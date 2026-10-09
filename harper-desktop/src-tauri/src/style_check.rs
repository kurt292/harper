//! Broadside's on-demand passes, driven by global hotkeys in the highlighter:
//!
//! - **Ctrl+Alt+H, model style check (Lane B):** runs the active guides' model rules against
//!   the focused field's text on a background thread.
//! - **Ctrl+Alt+T, thesaurus:** five ranked alternatives for the selected word, or the word at
//!   the caret.
//!
//! Both produce [`Findings`]: passages stored with the exact text they quote, re-anchored to
//! whatever the field holds now and appended to every lint pass as ordinary lints. The overlay
//! then shows them with the usual card, and applying one finding leaves the rest in place.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread;

use broadside_style::model::{ModelConfig, StyleCheckReport, check};
use broadside_style::thesaurus::Lookup;
use harper_core::Span;
use harper_core::linting::{Lint, LintKind, Suggestion};

use crate::style_guides;

/// Lint group key for model findings. Shows up as the rule name in the popup card.
pub const MODEL_RULE_NAME: &str = "Style guide (model)";
/// Lint group key for thesaurus lookups.
pub const THESAURUS_RULE_NAME: &str = "Thesaurus";

/// Lints from the model sit below deterministic style lints.
const MODEL_LINT_PRIORITY: u8 = 70;
/// A thesaurus lookup is something the user asked for; show it above everything.
const THESAURUS_LINT_PRIORITY: u8 = 10;

/// One finding, kept in a form that can be located again after the text changes.
#[derive(Debug, Clone)]
pub struct Finding {
    pub original: String,
    pub suggestions: Vec<String>,
    pub message: String,
    /// Where `original` was when the finding was made. Used while the text is unchanged, so a
    /// word that occurs several times is highlighted at the right one.
    pub start_hint: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct Findings {
    /// The text the findings were made against. Used as the first anchor; later texts are searched.
    pub text: String,
    pub findings: Vec<Finding>,
    pub lint_kind: LintKind,
    pub priority: u8,
    /// Only anchor by position in the unchanged text. A thesaurus lookup must vanish once the
    /// word is replaced rather than jump to another occurrence of the same word.
    pub hint_only: bool,
}

impl Findings {
    pub fn from_report(text: String, report: &StyleCheckReport) -> Self {
        let findings = report
            .violations
            .iter()
            .map(|violation| Finding {
                original: violation.original.clone(),
                suggestions: violation.suggestion.iter().cloned().collect(),
                message: format!(
                    "**{}** ({}% sure): {}",
                    violation.rule,
                    (violation.confidence * 100.0).round() as u32,
                    violation.explanation
                ),
                start_hint: Some(violation.span[0]),
            })
            .collect();
        Self {
            text,
            findings,
            lint_kind: LintKind::Style,
            priority: MODEL_LINT_PRIORITY,
            hint_only: false,
        }
    }

    pub fn from_lookup(text: String, lookup: &Lookup) -> Self {
        let options = lookup
            .options
            .iter()
            .map(|option| format!("{} ({})", option.word, option.register))
            .collect::<Vec<_>>()
            .join(", ");
        let message = if lookup.options.is_empty() {
            format!("No alternatives found for **{}**.", lookup.word)
        } else {
            format!("Alternatives for **{}**: {options}", lookup.word)
        };
        Self {
            text,
            findings: vec![Finding {
                original: lookup.word.clone(),
                suggestions: lookup.options.iter().map(|o| o.word.clone()).collect(),
                message,
                start_hint: Some(lookup.span.start),
            }],
            lint_kind: LintKind::WordChoice,
            priority: THESAURUS_LINT_PRIORITY,
            hint_only: true,
        }
    }

    /// Lints for `text`: each finding located by its quoted passage, in order, left to right. A
    /// finding whose passage is no longer present is skipped, not guessed.
    pub fn lints_for(&self, text: &str) -> Vec<Lint> {
        let chars: Vec<char> = text.chars().collect();
        let unchanged = text == self.text;
        if self.hint_only && !unchanged {
            return Vec::new();
        }
        let mut lints = Vec::new();
        let mut search_from = 0;

        for finding in &self.findings {
            let needle: Vec<char> = finding.original.chars().collect();
            let hinted = finding.start_hint.filter(|&start| {
                unchanged
                    && start + needle.len() <= chars.len()
                    && chars[start..start + needle.len()] == needle[..]
            });
            let found = hinted
                .or_else(|| find_chars(&chars, &needle, search_from))
                .or_else(|| find_chars(&chars, &needle, 0));
            let Some(start) = found else {
                continue;
            };
            let end = start + needle.len();
            search_from = end;
            let original: Vec<char> = chars[start..end].to_vec();

            lints.push(Lint {
                span: Span::new(start, end),
                lint_kind: self.lint_kind,
                suggestions: finding
                    .suggestions
                    .iter()
                    .map(|s| Suggestion::replace_with_match_case_str(s, original.iter().copied()))
                    .collect(),
                message: finding.message.clone(),
                priority: self.priority,
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

/// Both finding sets, shared between the highlighter's lint closure and its event loop.
#[derive(Debug, Default)]
pub struct FindingSets {
    pub model: Option<Findings>,
    pub thesaurus: Option<Findings>,
}

impl FindingSets {
    /// `(rule name, findings)` pairs for every set that has findings.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &Findings)> {
        [
            (MODEL_RULE_NAME, self.model.as_ref()),
            (THESAURUS_RULE_NAME, self.thesaurus.as_ref()),
        ]
        .into_iter()
        .filter_map(|(name, findings)| findings.map(|f| (name, f)))
    }
}

pub type SharedFindings = Rc<RefCell<FindingSets>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hotkey {
    StyleCheck,
    Thesaurus,
}

pub enum StyleCheckEvent {
    Finished { findings: Findings, summary: String },
    Failed(String),
}

/// Owns the hotkey listener and the in-flight model run.
pub struct StyleChecker {
    hotkeys: Receiver<Hotkey>,
    results: Receiver<StyleCheckEvent>,
    results_tx: Sender<StyleCheckEvent>,
    running: bool,
}

impl StyleChecker {
    /// Starts listening for the hotkeys. On platforms without global hotkeys the checker simply
    /// never fires.
    pub fn start() -> Self {
        let (hotkey_tx, hotkeys) = channel();
        let (results_tx, results) = channel();

        #[cfg(target_os = "windows")]
        thread::spawn(move || hotkey_thread(hotkey_tx));
        #[cfg(not(target_os = "windows"))]
        drop(hotkey_tx);

        Self {
            hotkeys,
            results,
            results_tx,
            running: false,
        }
    }

    /// Hotkeys pressed since the last call, in order.
    pub fn hotkeys_pressed(&mut self) -> Vec<Hotkey> {
        let mut pressed = Vec::new();
        loop {
            match self.hotkeys.try_recv() {
                Ok(hotkey) => pressed.push(hotkey),
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

/// Ctrl+Alt+H and Ctrl+Alt+T, registered on a thread of their own because Windows delivers
/// WM_HOTKEY to the registering thread's message queue and winit owns the overlay's.
#[cfg(target_os = "windows")]
fn hotkey_thread(tx: Sender<Hotkey>) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, RegisterHotKey,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

    const STYLE_CHECK_ID: i32 = 0x4842; // "HB"
    const THESAURUS_ID: i32 = 0x4843;

    unsafe {
        for (id, key, label) in [
            (STYLE_CHECK_ID, b'H', "style check (Ctrl+Alt+H)"),
            (THESAURUS_ID, b'T', "thesaurus (Ctrl+Alt+T)"),
        ] {
            match RegisterHotKey(
                None,
                id,
                MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
                u32::from(key),
            ) {
                Ok(()) => eprintln!("Hotkey registered: {label}"),
                Err(error) => eprintln!("Hotkey for {label} could not be registered: {error}"),
            }
        }

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            if message.message != WM_HOTKEY {
                continue;
            }
            let hotkey = match message.wParam.0 as i32 {
                STYLE_CHECK_ID => Hotkey::StyleCheck,
                THESAURUS_ID => Hotkey::Thesaurus,
                _ => continue,
            };
            if tx.send(hotkey).is_err() {
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
                    suggestions: Vec::new(),
                    message: "pleasantry".into(),
                    start_hint: Some(0),
                },
                Finding {
                    original: "was reviewed by the team".into(),
                    suggestions: vec!["the team reviewed".into()],
                    message: "passive".into(),
                    start_hint: Some(29),
                },
            ],
            lint_kind: LintKind::Style,
            priority: MODEL_LINT_PRIORITY,
            hint_only: false,
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

    #[test]
    fn thesaurus_findings_anchor_at_the_requested_occurrence() {
        let text = "The strong strain has a strong nose.".to_string();
        let lookup = Lookup {
            word: "strong".into(),
            span: Span::new(24, 30),
            options: vec![broadside_style::thesaurus::Alternative {
                word: "potent".into(),
                register: "neutral",
                score: 0.9,
            }],
        };
        let findings = Findings::from_lookup(text.clone(), &lookup);
        let lints = findings.lints_for(&text);
        assert_eq!(lints.len(), 1);
        assert_eq!(
            lints[0].span,
            Span::new(24, 30),
            "second occurrence, from the hint"
        );
        assert_eq!(lints[0].suggestions.len(), 1);
    }
}
