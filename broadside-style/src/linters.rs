//! Harper linters compiled from deterministic style rules.

use harper_core::linting::{Lint, LintKind, Linter, Suggestion};
use harper_core::{Document, Span, TokenStringExt};
use regex::{Regex, RegexBuilder};

use crate::guide::Rule;

/// Lints from style guides sit below grammar errors but above Harper's softer advice.
const STYLE_LINT_PRIORITY: u8 = 63;

/// Compiles one deterministic rule into a linter. Returns `None` for model rules and for rules
/// whose configuration cannot compile (an invalid regex), which `StyleGuide::validate` reports.
pub fn compile(guide_name: &str, rule: &Rule) -> Option<Box<dyn Linter>> {
    match rule {
        Rule::ForbidTerm {
            terms,
            prefer,
            message,
        } => Some(Box::new(ForbidTermLinter::new(
            guide_name,
            terms,
            prefer.clone(),
            message.clone(),
        ))),
        Rule::ForbidPattern {
            regex,
            ignore_case,
            prefer,
            message,
        } => ForbidPatternLinter::new(guide_name, regex, *ignore_case, prefer.clone(), message.clone())
            .map(|linter| Box::new(linter) as Box<dyn Linter>),
        Rule::MaxSentenceWords { value } => {
            Some(Box::new(MaxSentenceWordsLinter::new(guide_name, *value)))
        }
        Rule::Voice { .. } | Rule::ReadingLevel { .. } | Rule::Freeform { .. } => None,
    }
}

fn lowercase(chars: &[char]) -> Vec<char> {
    chars.iter().flat_map(|c| c.to_lowercase()).collect()
}

/// Splits a term into lowercase words so multi-word terms ("a lot") match token sequences.
fn term_words(term: &str) -> Vec<Vec<char>> {
    term.split_whitespace()
        .map(|word| word.chars().flat_map(|c| c.to_lowercase()).collect())
        .collect()
}

fn quote(chars: &[char]) -> String {
    let text: String = chars.iter().collect();
    format!("“{text}”")
}

// ---------------------------------------------------------------------------

pub struct ForbidTermLinter {
    guide_name: String,
    terms: Vec<Vec<Vec<char>>>,
    prefer: Option<String>,
    message: Option<String>,
    description: String,
}

impl ForbidTermLinter {
    pub fn new(guide_name: &str, terms: &[String], prefer: Option<String>, message: Option<String>) -> Self {
        let compiled: Vec<Vec<Vec<char>>> = terms
            .iter()
            .map(|term| term_words(term))
            .filter(|words| !words.is_empty())
            .collect();
        let description = match &prefer {
            Some(prefer) => format!(
                "Style guide “{guide_name}”: avoid {} and use “{prefer}” instead.",
                terms.iter().map(|t| format!("“{t}”")).collect::<Vec<_>>().join(", ")
            ),
            None => format!(
                "Style guide “{guide_name}”: avoid {}.",
                terms.iter().map(|t| format!("“{t}”")).collect::<Vec<_>>().join(", ")
            ),
        };
        Self {
            guide_name: guide_name.to_string(),
            terms: compiled,
            prefer,
            message,
            description,
        }
    }
}

impl Linter for ForbidTermLinter {
    fn lint(&mut self, document: &Document) -> Vec<Lint> {
        let tokens = document.get_tokens();

        // Word tokens with their lowercase text and position in the token stream.
        let words: Vec<(usize, Vec<char>)> = tokens
            .iter()
            .enumerate()
            .filter(|(_, token)| token.kind.is_word_like())
            .map(|(index, token)| (index, lowercase(document.get_span_content(&token.span))))
            .collect();

        let mut lints = Vec::new();

        for term in &self.terms {
            let n = term.len();
            if words.len() < n {
                continue;
            }
            for start in 0..=(words.len() - n) {
                let window = &words[start..start + n];
                if !window.iter().zip(term).all(|((_, word), want)| word == want) {
                    continue;
                }
                // Consecutive words must be separated only by whitespace.
                let adjacent = window.windows(2).all(|pair| {
                    tokens[pair[0].0 + 1..pair[1].0]
                        .iter()
                        .all(|token| token.kind.is_whitespace())
                });
                if !adjacent {
                    continue;
                }

                let span = Span::new(
                    tokens[window[0].0].span.start,
                    tokens[window[n - 1].0].span.end,
                );
                let original = document.get_span_content(&span);

                let suggestions = match &self.prefer {
                    Some(prefer) => vec![Suggestion::replace_with_match_case_str(
                        prefer,
                        original.iter().copied(),
                    )],
                    None => Vec::new(),
                };
                let message = match (&self.message, &self.prefer) {
                    (Some(message), _) => message.clone(),
                    (None, Some(prefer)) => format!(
                        "{} is not allowed by the “{}” style guide. Use “{prefer}”.",
                        quote(original),
                        self.guide_name
                    ),
                    (None, None) => format!(
                        "{} is not allowed by the “{}” style guide.",
                        quote(original),
                        self.guide_name
                    ),
                };

                lints.push(Lint {
                    span,
                    lint_kind: LintKind::Style,
                    suggestions,
                    message,
                    priority: STYLE_LINT_PRIORITY,
                });
            }
        }

        lints.sort_by_key(|lint| lint.span.start);
        lints
    }

    fn description(&self) -> &str {
        &self.description
    }
}

// ---------------------------------------------------------------------------

pub struct ForbidPatternLinter {
    guide_name: String,
    regex: Regex,
    source: String,
    prefer: Option<String>,
    message: Option<String>,
    description: String,
}

impl ForbidPatternLinter {
    pub fn new(
        guide_name: &str,
        pattern: &str,
        ignore_case: bool,
        prefer: Option<String>,
        message: Option<String>,
    ) -> Option<Self> {
        let regex = RegexBuilder::new(pattern)
            .case_insensitive(ignore_case)
            .build()
            .ok()?;
        Some(Self {
            guide_name: guide_name.to_string(),
            regex,
            source: pattern.to_string(),
            prefer,
            message,
            description: format!("Style guide “{guide_name}”: text matching /{pattern}/ is not allowed."),
        })
    }
}

impl Linter for ForbidPatternLinter {
    fn lint(&mut self, document: &Document) -> Vec<Lint> {
        let text = document.get_full_string();

        // Regex offsets are bytes; Harper spans are chars.
        let mut byte_to_char = vec![0usize; text.len() + 1];
        let mut char_index = 0;
        for (byte_index, ch) in text.char_indices() {
            for b in byte_index..byte_index + ch.len_utf8() {
                byte_to_char[b] = char_index;
            }
            char_index += 1;
        }
        byte_to_char[text.len()] = char_index;

        let mut lints = Vec::new();
        for found in self.regex.find_iter(&text) {
            if found.is_empty() {
                continue;
            }
            let span = Span::new(byte_to_char[found.start()], byte_to_char[found.end()]);
            let original = document.get_span_content(&span);

            let suggestions = match &self.prefer {
                Some(prefer) => vec![Suggestion::replace_with_match_case_str(
                    prefer,
                    original.iter().copied(),
                )],
                None => Vec::new(),
            };
            let message = match (&self.message, &self.prefer) {
                (Some(message), _) => message.clone(),
                (None, Some(prefer)) => format!(
                    "{} is not allowed by the “{}” style guide. Use “{prefer}”.",
                    quote(original),
                    self.guide_name
                ),
                (None, None) => format!(
                    "{} matches a pattern the “{}” style guide forbids (/{}/).",
                    quote(original),
                    self.guide_name,
                    self.source
                ),
            };

            lints.push(Lint {
                span,
                lint_kind: LintKind::Style,
                suggestions,
                message,
                priority: STYLE_LINT_PRIORITY,
            });
        }
        lints
    }

    fn description(&self) -> &str {
        &self.description
    }
}

// ---------------------------------------------------------------------------

pub struct MaxSentenceWordsLinter {
    guide_name: String,
    max_words: usize,
    description: String,
}

impl MaxSentenceWordsLinter {
    pub fn new(guide_name: &str, max_words: usize) -> Self {
        Self {
            guide_name: guide_name.to_string(),
            max_words,
            description: format!(
                "Style guide “{guide_name}”: sentences may be at most {max_words} words long."
            ),
        }
    }
}

impl Linter for MaxSentenceWordsLinter {
    fn lint(&mut self, document: &Document) -> Vec<Lint> {
        let mut lints = Vec::new();

        for sentence in document.iter_sentences() {
            let word_count = sentence.iter_words().count();
            if word_count <= self.max_words {
                continue;
            }
            let (Some(first), Some(last)) = (sentence.first_word(), sentence.last()) else {
                continue;
            };
            lints.push(Lint {
                span: Span::new(first.span.start, last.span.end),
                lint_kind: LintKind::Style,
                suggestions: Vec::new(),
                message: format!(
                    "This sentence is {word_count} words long. The “{}” style guide allows at most {}.",
                    self.guide_name, self.max_words
                ),
                priority: STYLE_LINT_PRIORITY,
            });
        }

        lints
    }

    fn description(&self) -> &str {
        &self.description
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harper_core::spell::FstDictionary;

    fn lint_with(rule: &Rule, text: &str) -> Vec<Lint> {
        let mut linter = compile("Test Guide", rule).expect("deterministic rule compiles");
        let document = Document::new_plain_english(text, &FstDictionary::curated());
        linter.lint(&document)
    }

    fn text_of(text: &str, lint: &Lint) -> String {
        text.chars()
            .skip(lint.span.start)
            .take(lint.span.len())
            .collect()
    }

    #[test]
    fn forbid_term_matches_whole_words_case_insensitively() {
        let rule = Rule::ForbidTerm {
            terms: vec!["weed".into(), "pot".into()],
            prefer: Some("cannabis".into()),
            message: None,
        };
        let text = "Weed is great. The potted plant is not pot. Buy weed.";
        let lints = lint_with(&rule, text);
        let found: Vec<String> = lints.iter().map(|l| text_of(text, l)).collect();
        assert_eq!(found, vec!["Weed", "pot", "weed"]);

        // Case of the replacement follows the original.
        match &lints[0].suggestions[0] {
            Suggestion::ReplaceWith(chars) => {
                assert_eq!(chars.iter().collect::<String>(), "Cannabis")
            }
            other => panic!("unexpected suggestion {other:?}"),
        }
    }

    #[test]
    fn forbid_term_matches_multi_word_terms() {
        let rule = Rule::ForbidTerm {
            terms: vec!["a lot".into()],
            prefer: Some("many".into()),
            message: None,
        };
        let text = "There were a lot of people, a   lot of them. A lottery.";
        let lints = lint_with(&rule, text);
        let found: Vec<String> = lints.iter().map(|l| text_of(text, l)).collect();
        assert_eq!(found, vec!["a lot", "a   lot"]);
    }

    #[test]
    fn forbid_pattern_maps_byte_offsets_to_chars() {
        let rule = Rule::ForbidPattern {
            regex: r"\b(very|really|just)\b".into(),
            ignore_case: true,
            prefer: None,
            message: None,
        };
        let text = "Café is Very good, really. Justice is just fine.";
        let lints = lint_with(&rule, text);
        let found: Vec<String> = lints.iter().map(|l| text_of(text, l)).collect();
        assert_eq!(found, vec!["Very", "really", "just"]);
    }

    #[test]
    fn max_sentence_words_flags_only_long_sentences() {
        let rule = Rule::MaxSentenceWords { value: 5 };
        let text = "Short one here. This sentence has far too many words in it for the rule.";
        let lints = lint_with(&rule, text);
        assert_eq!(lints.len(), 1);
        assert!(text_of(text, &lints[0]).starts_with("This sentence"));
        assert!(lints[0].message.contains("at most 5"));
    }

    #[test]
    fn model_rules_do_not_compile_to_linters() {
        assert!(compile("g", &Rule::Voice { value: "active".into() }).is_none());
        assert!(compile("g", &Rule::ReadingLevel { max_grade: 9 }).is_none());
        assert!(compile("g", &Rule::Freeform { instruction: "x".into() }).is_none());
    }
}
