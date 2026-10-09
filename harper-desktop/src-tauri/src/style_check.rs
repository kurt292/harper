//! Broadside Lane B in the field: a global hotkey runs the active guides' model rules against the
//! focused field's text and turns the findings into overlay lints.
//!
//! The model call takes seconds, so it runs on its own thread and reports back through a channel
//! the highlighter polls each tick. Findings are stored with the exact passage they quote and are
//! re-anchored to whatever the field holds now, so applying one finding does not throw the rest
//! away.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread;

use broadside_style::model::{ModelConfig, StyleCheckReport, check};
use harper_core::Span;
use harper_core::linting::{Lint, LintKind, Suggestion};

use crate::style_guides;

/// Lint group key the findings appear under. Shows up as the rule name in the popup card.
pub const MODEL_RULE_NAME: &str = "Style guide (model)";

/// Lints from the model sit below deterministic style lints.
const MODEL_LINT_PRIORITY: u8 = 70;

/// One finding, kept in a form that can be located again after the text changes.
#[derive(Debug, Clone)]
pub struct Finding {
    pub original: String,
    pub suggestion: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct Findings {
    /// The text the model saw. Used as the first anchor; later texts are searched.
    pub text: String,
    pub findings: Vec<Finding>,
}

impl Findings {
    pub fn from_report(text: String, report: &StyleCheckReport) -> Self {
        let findings = report
            .violations
            .iter()
            .map(|violation| Finding {
                original: violation.original.clone(),
                suggestion: violation.suggestion.clone(),
                message: format!(
                    "**{}** ({}% sure): {}",
                    violation.rule,
                    (violation.confidence * 100.0).round() as u32,
                    violation.explanation
                ),
            })
            .collect();
        Self { text, findings }
    }

    /// Lints for `text`: each finding located by its quoted passage, in order, left to right. A
    /// finding whose passage is no longer present is skipped, not guessed.
    pub fn lints_for(&self, text: &str) -> Vec<Lint> {
        let chars: Vec<char> = text.chars().collect();
        let mut lints = Vec::new();
        let mut search_from = 0;

        for finding in &self.findings {
            let needle: Vec<char> = finding.original.chars().collect();
            let found =
                find_chars(&chars, &needle, search_from).or_else(|| find_chars(&chars, &needle, 0));
            let Some(start) = found else {
                continue;
            };
            let end = start + needle.len();
            search_from = end;

            lints.push(Lint {
                span: Span::new(start, end),
                lint_kind: LintKind::Style,
                suggestions: finding
                    .suggestion
                    .as_ref()
                    .map(|s| vec![Suggestion::ReplaceWith(s.chars().collect())])
                    .unwrap_or_default(),
                message: finding.message.clone(),
                priority: MODEL_LINT_PRIORITY,
            });
        }

        lints
    }
}

fn find_chars(haystack: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() || from > haystack.len() - needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// Findings shared between the highlighter's lint closure and its event loop.
pub type SharedFindings = Rc<RefCell<Option<Findings>>>;

pub enum StyleCheckEvent {
    Finished { findings: Findings, summary: String },
    Failed(String),
}

/// Owns the hotkey listener and the in-flight model run.
pub struct StyleChecker {
    hotkey: Receiver<()>,
    results: Receiver<StyleCheckEvent>,
    results_tx: Sender<StyleCheckEvent>,
    running: bool,
}

impl StyleChecker {
    /// Starts listening for the hotkey. On platforms without a global hotkey the checker simply
    /// never fires.
    pub fn start() -> Self {
        let (hotkey_tx, hotkey) = channel();
        let (results_tx, results) = channel();

        #[cfg(target_os = "windows")]
        thread::spawn(move || hotkey_thread(hotkey_tx));
        #[cfg(not(target_os = "windows"))]
        drop(hotkey_tx);

        Self {
            hotkey,
            results,
            results_tx,
            running: false,
        }
    }

    /// Whether the hotkey was pressed since the last call. Drains repeated presses.
    pub fn hotkey_pressed(&mut self) -> bool {
        let mut pressed = false;
        loop {
            match self.hotkey.try_recv() {
                Ok(()) => pressed = true,
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        pressed
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Runs the model check for `text` on a background thread. Ignored while one is running.
    pub fn run(&mut self, text: String) {
        if self.running {
            return;
        }
        self.running = true;
        let tx = self.results_tx.clone();
        thread::spawn(move || {
            let guides = style_guides::load_guides();
            let config = ModelConfig::default();
            let event = match check(&text, &guides, &config) {
                Ok(report) => {
                    let findings = Findings::from_report(text, &report);
                    let summary = format!(
                        "Style check: {} finding{} in {:.1} s ({} dropped)",
                        findings.findings.len(),
                        if findings.findings.len() == 1 {
                            ""
                        } else {
                            "s"
                        },
                        report.elapsed_ms as f64 / 1000.0,
                        report.dropped.len()
                    );
                    StyleCheckEvent::Finished { findings, summary }
                }
                Err(error) => StyleCheckEvent::Failed(format!("Style check failed: {error}")),
            };
            let _ = tx.send(event);
        });
    }

    /// The finished run, if any.
    pub fn poll(&mut self) -> Option<StyleCheckEvent> {
        match self.results.try_recv() {
            Ok(event) => {
                self.running = false;
                Some(event)
            }
            Err(_) => None,
        }
    }
}

/// Ctrl+Alt+H, registered on a thread of its own because Windows delivers WM_HOTKEY to the
/// registering thread's message queue and winit owns the overlay's.
#[cfg(target_os = "windows")]
fn hotkey_thread(tx: Sender<()>) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, RegisterHotKey,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

    const HOTKEY_ID: i32 = 0x4842; // "HB"

    unsafe {
        if let Err(error) = RegisterHotKey(
            None,
            HOTKEY_ID,
            MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
            u32::from(b'H'),
        ) {
            eprintln!("Style check hotkey (Ctrl+Alt+H) could not be registered: {error}");
            return;
        }
        eprintln!("Style check hotkey registered: Ctrl+Alt+H");

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            if message.message == WM_HOTKEY
                && message.wParam.0 as i32 == HOTKEY_ID
                && tx.send(()).is_err()
            {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn findings_follow_their_passages_across_edits() {
        let findings = Findings {
            text: "Hope all is well! The report was reviewed by the team.".into(),
            findings: vec![
                Finding {
                    original: "Hope all is well!".into(),
                    suggestion: None,
                    message: "pleasantry".into(),
                },
                Finding {
                    original: "was reviewed by the team".into(),
                    suggestion: Some("the team reviewed".into()),
                    message: "passive".into(),
                },
            ],
        };

        let lints = findings.lints_for(&findings.text);
        assert_eq!(lints.len(), 2);
        assert_eq!(lints[0].span, Span::new(0, 17));

        // After removing the first passage, the second is found at its new position.
        let edited = "The report was reviewed by the team.";
        let lints = findings.lints_for(edited);
        assert_eq!(lints.len(), 1);
        assert_eq!(lints[0].span, Span::new(11, 35));
        assert!(
            matches!(&lints[0].suggestions[0], Suggestion::ReplaceWith(c) if c.iter().collect::<String>() == "the team reviewed")
        );
    }
}
