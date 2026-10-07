//! Lane B: the on-demand style pass against a local model served by Ollama.
//!
//! The active guides' model rules (`voice`, `reading_level`, `freeform`) become the system
//! prompt, the user's text is the user message, and the model must answer with structured JSON.
//! Nothing here runs continuously: a 3B model on a 15 W CPU takes seconds per paragraph, so this
//! is the pass behind a "Check" button.
//!
//! Precision over recall: results below the confidence floor are dropped, and so is any
//! violation whose quoted `original` cannot be found verbatim in the text, because a suggestion
//! that cannot be anchored cannot be applied safely.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::guide::{Rule, StyleGuide};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelConfig {
    /// Ollama's HTTP endpoint.
    pub endpoint: String,
    /// Model tag as `ollama list` shows it.
    pub model: String,
    /// Whole-request timeout. A cold 3B model can take 20 s to load on this hardware.
    pub timeout_secs: u64,
    /// Violations the model rates below this are discarded.
    pub confidence_floor: f32,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://127.0.0.1:11434".to_string(),
            model: "qwen2.5:3b".to_string(),
            timeout_secs: 90,
            confidence_floor: 0.6,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("no active style guide has model rules; nothing to check")]
    NothingToCheck,
    #[error("could not reach the model server at {endpoint}: {source}")]
    Unreachable {
        endpoint: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("model server returned {status}: {body}")]
    Server { status: u16, body: String },
    #[error("model answer was not the expected JSON: {0}")]
    BadAnswer(String),
}

/// One finding from the model, anchored to the text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelViolation {
    /// Which rule the model says was broken, e.g. `voice:active` or `freeform`.
    pub rule: String,
    /// Exact text from the input that the finding is about.
    pub original: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    pub explanation: String,
    pub confidence: f32,
    /// Char offsets of `original` in the checked text, `[start, end)`.
    pub span: [usize; 2],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StyleCheckReport {
    pub model: String,
    pub guides: Vec<String>,
    pub violations: Vec<ModelViolation>,
    pub elapsed_ms: u128,
    /// Findings that were dropped and why, so a quiet report is explainable.
    pub dropped: Vec<String>,
}

/// Model rules of the active guides, in precedence order, as `(guide name, rule)`.
pub fn model_rules<'a>(guides: &'a [StyleGuide]) -> Vec<(&'a StyleGuide, &'a Rule)> {
    crate::conflicts::active_by_precedence(guides)
        .into_iter()
        .flat_map(|guide| {
            guide
                .rules
                .iter()
                .filter(|rule| !rule.is_deterministic())
                .map(move |rule| (guide, rule))
        })
        .collect()
}

/// A model rule with the short id the prompt asks the model to echo back.
pub struct PromptRule<'a> {
    pub id: String,
    pub guide: &'a StyleGuide,
    pub rule: &'a Rule,
}

impl PromptRule<'_> {
    /// Human-readable label for reports: `Nimbus B2B Email · voice (active)`.
    pub fn label(&self) -> String {
        let detail = match self.rule {
            Rule::Voice { value } => format!("voice ({value})"),
            Rule::ReadingLevel { max_grade } => format!("reading level (grade {max_grade})"),
            Rule::Freeform { .. } => "freeform".to_string(),
            other => other.kind_name().to_string(),
        };
        format!("{} · {detail}", self.guide.name)
    }
}

/// Model rules of the active guides with ids `voice-1`, `freeform-2`, … in prompt order.
pub fn prompt_rules(guides: &[StyleGuide]) -> Vec<PromptRule<'_>> {
    model_rules(guides)
        .into_iter()
        .enumerate()
        .map(|(index, (guide, rule))| PromptRule {
            id: format!("{}-{}", rule.kind_name(), index + 1),
            guide,
            rule,
        })
        .collect()
}

/// Builds the system prompt from the active guides' model rules.
pub fn system_prompt(guides: &[StyleGuide]) -> Option<String> {
    let rules = prompt_rules(guides);
    if rules.is_empty() {
        return None;
    }

    let mut prompt = String::from(
        "You are a strict copy editor enforcing a house style guide. Read the user's text and \
         report each passage that breaks one of the numbered rules below.\n\n\
         How to report:\n\
         - One violation per problem. A sentence that breaks two rules is two violations.\n\
         - `original` is the shortest passage that shows the problem: a phrase or a single \
         sentence, copied exactly as it appears in the text, including capitalization and \
         punctuation. Never quote the whole text or more than one sentence.\n\
         - `suggestion` is a concrete rewrite of exactly that passage, or omit it if the fix is \
         to delete the passage.\n\
         - `rule` is the id of the rule broken, copied exactly, for example `voice-1`.\n\
         - `confidence` is 0 to 1. Report only what you are confident about; fewer, surer \
         findings are better than many weak ones.\n\
         - Ignore spelling, grammar and punctuation. Another tool handles those.\n\
         - If the text follows every rule, return an empty `violations` list.\n\n\
         Rules:\n",
    );

    for prompt_rule in &rules {
        let text = match prompt_rule.rule {
            Rule::Voice { value } => format!(
                "Write in the {value} voice. A sentence in any other voice breaks this rule."
            ),
            Rule::ReadingLevel { max_grade } => format!(
                "Keep the text readable at US grade {max_grade} or below. A sentence that needs a \
                 higher reading level breaks this rule."
            ),
            Rule::Freeform { instruction } => instruction.clone(),
            _ => continue,
        };
        prompt.push_str(&format!(
            "- id: {} (from the guide “{}”): {text}\n",
            prompt_rule.id, prompt_rule.guide.name
        ));
    }

    prompt.push_str(
        "\nExample. For the text “Hope all is well! The report was reviewed by the team.” with \
         rules voice-1 (active voice) and freeform-2 (no pleasantries), answer:\n\
         {\"violations\":[\
         {\"rule\":\"freeform-2\",\"original\":\"Hope all is well!\",\"explanation\":\"Pleasantry \
         before the substance.\",\"confidence\":0.95},\
         {\"rule\":\"voice-1\",\"original\":\"The report was reviewed by the team.\",\
         \"suggestion\":\"The team reviewed the report.\",\"explanation\":\"Passive voice.\",\
         \"confidence\":0.9}]}",
    );
    Some(prompt)
}

/// The JSON schema the model must answer with. Ollama's structured outputs constrain decoding.
fn answer_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "violations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "rule": { "type": "string" },
                        "original": { "type": "string" },
                        "suggestion": { "type": "string" },
                        "explanation": { "type": "string" },
                        "confidence": { "type": "number" }
                    },
                    "required": ["rule", "original", "explanation", "confidence"]
                }
            }
        },
        "required": ["violations"]
    })
}

#[derive(Deserialize)]
struct RawAnswer {
    #[serde(default)]
    violations: Vec<RawViolation>,
}

#[derive(Deserialize)]
struct RawViolation {
    rule: String,
    original: String,
    #[serde(default)]
    suggestion: Option<String>,
    #[serde(default)]
    explanation: String,
    #[serde(default)]
    confidence: f32,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: String,
}

/// Models the Ollama server currently has available.
pub fn available_models(config: &ModelConfig) -> Result<Vec<String>, ModelError> {
    #[derive(Deserialize)]
    struct Tags {
        #[serde(default)]
        models: Vec<Tag>,
    }
    #[derive(Deserialize)]
    struct Tag {
        name: String,
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|source| ModelError::Unreachable {
            endpoint: config.endpoint.clone(),
            source,
        })?;
    let response = client
        .get(format!("{}/api/tags", config.endpoint.trim_end_matches('/')))
        .send()
        .map_err(|source| ModelError::Unreachable {
            endpoint: config.endpoint.clone(),
            source,
        })?;
    let tags: Tags = response
        .json()
        .map_err(|error| ModelError::BadAnswer(error.to_string()))?;
    Ok(tags.models.into_iter().map(|tag| tag.name).collect())
}

/// Runs the style pass. Blocking; call it off the UI thread.
pub fn check(
    text: &str,
    guides: &[StyleGuide],
    config: &ModelConfig,
) -> Result<StyleCheckReport, ModelError> {
    let system = system_prompt(guides).ok_or(ModelError::NothingToCheck)?;
    let rules = prompt_rules(guides);
    let guide_names: Vec<String> = rules
        .iter()
        .map(|rule| rule.guide.name.clone())
        .fold(Vec::new(), |mut names, name| {
            if !names.contains(&name) {
                names.push(name);
            }
            names
        });

    let started = Instant::now();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(config.timeout_secs))
        .build()
        .map_err(|source| ModelError::Unreachable {
            endpoint: config.endpoint.clone(),
            source,
        })?;

    let body = serde_json::json!({
        "model": config.model,
        "stream": false,
        "format": answer_schema(),
        "options": { "temperature": 0.0 },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": text }
        ]
    });

    let response = client
        .post(format!("{}/api/chat", config.endpoint.trim_end_matches('/')))
        .json(&body)
        .send()
        .map_err(|source| ModelError::Unreachable {
            endpoint: config.endpoint.clone(),
            source,
        })?;

    let status = response.status();
    if !status.is_success() {
        return Err(ModelError::Server {
            status: status.as_u16(),
            body: response.text().unwrap_or_default(),
        });
    }

    let chat: ChatResponse = response
        .json()
        .map_err(|error| ModelError::BadAnswer(error.to_string()))?;
    let raw: RawAnswer = serde_json::from_str(&chat.message.content)
        .map_err(|error| ModelError::BadAnswer(format!("{error}: {}", chat.message.content)))?;

    let (mut violations, dropped) = anchor(text, raw.violations, config.confidence_floor);

    // Replace echoed ids with readable labels; keep whatever the model said otherwise.
    for violation in &mut violations {
        if let Some(rule) = rules.iter().find(|rule| rule.id == violation.rule) {
            violation.rule = rule.label();
        }
    }

    Ok(StyleCheckReport {
        model: config.model.clone(),
        guides: guide_names,
        violations,
        elapsed_ms: started.elapsed().as_millis(),
        dropped,
    })
}

/// Turns raw model output into anchored violations, dropping what cannot be trusted.
fn anchor(
    text: &str,
    raw: Vec<RawViolation>,
    confidence_floor: f32,
) -> (Vec<ModelViolation>, Vec<String>) {
    let chars: Vec<char> = text.chars().collect();
    let mut violations = Vec::new();
    let mut dropped = Vec::new();
    let mut search_from = 0;

    for item in raw {
        let original = item.original.trim();
        if original.is_empty() {
            dropped.push(format!("[{}] empty quote", item.rule));
            continue;
        }
        if item.confidence < confidence_floor {
            dropped.push(format!(
                "[{}] {:?}: confidence {:.2} below floor {:.2}",
                item.rule, original, item.confidence, confidence_floor
            ));
            continue;
        }

        let needle: Vec<char> = original.chars().collect();
        // A quote that is most of the text cannot be acted on and usually means the model
        // ignored the "shortest passage" instruction.
        let trimmed_len = text.trim().chars().count();
        if trimmed_len > 40 && needle.len() * 10 >= trimmed_len * 8 {
            dropped.push(format!(
                "[{}] quote covers {} of {} chars; too broad to anchor",
                item.rule,
                needle.len(),
                trimmed_len
            ));
            continue;
        }
        let found = find_chars(&chars, &needle, search_from).or_else(|| find_chars(&chars, &needle, 0));
        let Some(start) = found else {
            dropped.push(format!(
                "[{}] {:?}: not found verbatim in the text",
                item.rule, original
            ));
            continue;
        };
        let end = start + needle.len();
        search_from = end;

        let suggestion = item
            .suggestion
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s != original);

        violations.push(ModelViolation {
            rule: item.rule,
            original: original.to_string(),
            suggestion,
            explanation: item.explanation.trim().to_string(),
            confidence: item.confidence,
            span: [start, end],
        });
    }

    violations.sort_by_key(|v| v.span[0]);
    (violations, dropped)
}

fn find_chars(haystack: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() || from > haystack.len() - needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::sample_guides;

    #[test]
    fn prompt_lists_only_model_rules_of_active_guides() {
        let guides = sample_guides();
        let prompt = system_prompt(&guides).unwrap();
        assert!(prompt.contains("id: voice-1 (from the guide “Nimbus B2B Email”)"));
        assert!(prompt.contains("id: reading_level-2"));
        assert!(prompt.contains("id: freeform-3"));
        assert!(prompt.contains("Lead with the offer"));
        assert_eq!(prompt_rules(&guides)[0].label(), "Nimbus B2B Email · voice (active)");
        // Landing page is inactive by default.
        assert!(!prompt.contains("Second person"));
        // Deterministic rules never reach the model.
        assert!(!prompt.contains("utilize"));
    }

    #[test]
    fn no_model_rules_means_nothing_to_check() {
        let mut guides = sample_guides();
        for guide in &mut guides {
            guide.active = false;
        }
        assert!(system_prompt(&guides).is_none());
    }

    #[test]
    fn anchoring_drops_unfound_and_low_confidence_findings() {
        let text = "The report was reviewed by the team. It was very good. Café time.";
        let raw = vec![
            RawViolation {
                rule: "voice:active".into(),
                original: "was reviewed by the team".into(),
                suggestion: Some("the team reviewed".into()),
                explanation: "Passive voice".into(),
                confidence: 0.9,
            },
            RawViolation {
                rule: "freeform".into(),
                original: "not in the text".into(),
                suggestion: None,
                explanation: "".into(),
                confidence: 0.95,
            },
            RawViolation {
                rule: "freeform".into(),
                original: "very good".into(),
                suggestion: None,
                explanation: "weak".into(),
                confidence: 0.3,
            },
            RawViolation {
                rule: "freeform".into(),
                original: "Café time.".into(),
                suggestion: Some("Café time.".into()),
                explanation: "same text, suggestion should be dropped".into(),
                confidence: 0.8,
            },
        ];

        let (violations, dropped) = anchor(text, raw, 0.6);
        assert_eq!(violations.len(), 2);
        assert_eq!(violations[0].span, [11, 35]);
        assert_eq!(
            text.chars().skip(11).take(24).collect::<String>(),
            "was reviewed by the team"
        );
        assert_eq!(violations[1].original, "Café time.");
        assert_eq!(violations[1].suggestion, None);
        assert_eq!(dropped.len(), 2);
    }
}
