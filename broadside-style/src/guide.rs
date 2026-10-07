//! The user-facing style guide model and its JSON representation.
//!
//! The on-disk format is the one in the Broadside PRD: one JSON object per guide with an
//! ordered list of tagged rules. Deterministic rules compile into Harper linters; model rules
//! (`voice`, `reading_level`, `freeform`) are carried along for Lane B and never run
//! continuously.

use serde::{Deserialize, Serialize};

fn default_priority() -> u32 {
    100
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyleGuide {
    /// Stable identifier, also the file name. Lowercase letters, digits, `-` and `_`.
    pub id: String,
    /// Display name shown in the tray and the editor.
    pub name: String,
    /// Whether the guide currently contributes lints.
    #[serde(default)]
    pub active: bool,
    /// Precedence when active guides conflict. Lower wins.
    #[serde(default = "default_priority")]
    pub priority: u32,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default, skip_serializing_if = "Bindings::is_empty")]
    pub bindings: Bindings,
}

impl StyleGuide {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            active: false,
            priority: default_priority(),
            rules: Vec::new(),
            bindings: Bindings::default(),
        }
    }

    /// Terms this guide tells the writer to use, across all `forbid_term` rules.
    pub fn preferred_terms(&self) -> impl Iterator<Item = &str> {
        self.rules.iter().filter_map(|rule| match rule {
            Rule::ForbidTerm {
                prefer: Some(prefer),
                ..
            }
            | Rule::ForbidPattern {
                prefer: Some(prefer),
                ..
            } => Some(prefer.as_str()),
            _ => None,
        })
    }

    /// Terms this guide forbids, across all `forbid_term` rules.
    pub fn forbidden_terms(&self) -> impl Iterator<Item = &str> {
        self.rules.iter().flat_map(|rule| match rule {
            Rule::ForbidTerm { terms, .. } => terms.iter().map(String::as_str).collect(),
            _ => Vec::new(),
        })
    }

    /// Validates the identifier so it can double as a file name.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() {
            return Err("style guide id is empty".to_string());
        }
        if !self
            .id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            return Err(format!(
                "style guide id {:?} may only contain lowercase letters, digits, '-' and '_'",
                self.id
            ));
        }
        if self.name.trim().is_empty() {
            return Err(format!("style guide {:?} has no name", self.id));
        }
        for (index, rule) in self.rules.iter().enumerate() {
            rule.validate()
                .map_err(|error| format!("rule {} of {:?}: {error}", index + 1, self.id))?;
        }
        Ok(())
    }
}

/// Where a guide switches itself on. Honoured in P2's auto-activation work; stored now so the
/// format is stable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bindings {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub apps: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
}

impl Bindings {
    pub fn is_empty(&self) -> bool {
        self.apps.is_empty() && self.urls.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Rule {
    /// Flag any of `terms` (case-insensitive, whole words, multi-word allowed) and offer `prefer`.
    ForbidTerm {
        terms: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// Flag every match of a regular expression.
    ForbidPattern {
        regex: String,
        #[serde(default = "default_true")]
        ignore_case: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// Flag sentences longer than `value` words.
    MaxSentenceWords { value: usize },
    /// Model rule: preferred grammatical voice, e.g. `"active"`.
    Voice { value: String },
    /// Model rule: maximum reading grade level.
    ReadingLevel { max_grade: u8 },
    /// Model rule: a natural-language instruction for the style model.
    Freeform { instruction: String },
}

impl Rule {
    /// Deterministic rules run in plain code on every keystroke. Everything else needs the
    /// style model and is on-demand only.
    pub fn is_deterministic(&self) -> bool {
        matches!(
            self,
            Rule::ForbidTerm { .. } | Rule::ForbidPattern { .. } | Rule::MaxSentenceWords { .. }
        )
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            Rule::ForbidTerm { .. } => "forbid_term",
            Rule::ForbidPattern { .. } => "forbid_pattern",
            Rule::MaxSentenceWords { .. } => "max_sentence_words",
            Rule::Voice { .. } => "voice",
            Rule::ReadingLevel { .. } => "reading_level",
            Rule::Freeform { .. } => "freeform",
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Rule::ForbidTerm { terms, .. } => {
                if terms.iter().all(|term| term.trim().is_empty()) {
                    return Err("forbid_term has no terms".to_string());
                }
                Ok(())
            }
            Rule::ForbidPattern { regex, .. } => regex::Regex::new(regex)
                .map(|_| ())
                .map_err(|error| format!("invalid regex: {error}")),
            Rule::MaxSentenceWords { value } => {
                if *value == 0 {
                    return Err("max_sentence_words must be at least 1".to_string());
                }
                Ok(())
            }
            Rule::Voice { value } => {
                if value.trim().is_empty() {
                    return Err("voice has no value".to_string());
                }
                Ok(())
            }
            Rule::ReadingLevel { .. } => Ok(()),
            Rule::Freeform { instruction } => {
                if instruction.trim().is_empty() {
                    return Err("freeform has no instruction".to_string());
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_the_prd_example() {
        let json = r##"{
          "id": "nimbus-b2b-email",
          "name": "Nimbus B2B Email",
          "active": true,
          "priority": 10,
          "rules": [
            { "type": "forbid_term", "terms": ["weed", "pot"], "prefer": "cannabis" },
            { "type": "forbid_term", "terms": ["utilize"], "prefer": "use" },
            { "type": "max_sentence_words", "value": 25 },
            { "type": "voice", "value": "active" },
            { "type": "reading_level", "max_grade": 9 },
            { "type": "forbid_pattern", "regex": "\\b(very|really|just)\\b" },
            { "type": "freeform", "instruction": "Lead with the offer." }
          ],
          "bindings": { "apps": ["outlook.exe"], "urls": ["mail.google.com"] }
        }"##;

        let guide: StyleGuide = serde_json::from_str(json).unwrap();
        assert_eq!(guide.rules.len(), 7);
        assert_eq!(guide.rules.iter().filter(|r| r.is_deterministic()).count(), 4);
        assert_eq!(guide.bindings.apps, vec!["outlook.exe"]);
        guide.validate().unwrap();

        let again: StyleGuide =
            serde_json::from_str(&serde_json::to_string(&guide).unwrap()).unwrap();
        assert_eq!(guide, again);
    }

    #[test]
    fn rejects_bad_ids_and_regexes() {
        let mut guide = StyleGuide::new("Bad Id", "x");
        assert!(guide.validate().is_err());

        guide.id = "ok".into();
        guide.rules.push(Rule::ForbidPattern {
            regex: "(".into(),
            ignore_case: true,
            prefer: None,
            message: None,
        });
        assert!(guide.validate().is_err());
    }
}
