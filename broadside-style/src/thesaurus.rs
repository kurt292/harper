//! On-demand thesaurus: five alternatives for a word, ranked for the sentence they sit in.
//!
//! Stage one, the candidate pool, is Harper's offline synonym table. Stage two ranks and cuts:
//!
//! 1. grammatical fit, measured as the distance between the original token's dictionary
//!    metadata (part of speech, inflection) and the candidate's;
//! 2. register, approximated by frequency rank and dictionary membership;
//! 3. style-guide compliance: candidates an active guide forbids are dropped outright;
//! 4. diversity: one candidate per stem, so five options are five ideas.
//!
//! Contextual plausibility from the model is a P3 follow-up; the scoring here is plain code
//! and answers in well under a millisecond once the dictionary is loaded.

use harper_core::spell::{Dictionary, FstDictionary};
use harper_core::{Document, Span, TokenKind};
use serde::Serialize;

use crate::conflicts::active_by_precedence;
use crate::guide::StyleGuide;

/// How many raw synonyms to consider before ranking.
const CANDIDATE_POOL: usize = 40;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Alternative {
    pub word: String,
    /// "neutral" for common dictionary words, "formal" for rarer ones.
    pub register: &'static str,
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Lookup {
    pub word: String,
    pub span: Span<char>,
    pub options: Vec<Alternative>,
}

/// The word to look up for a selection `(start, end)` in `text`: the selection itself when it
/// is exactly one word, otherwise the word under the selection's start (the caret).
pub fn word_at(text: &str, selection: (usize, usize)) -> Option<(String, Span<char>)> {
    let document = Document::new_plain_english(text, &FstDictionary::curated());
    let (start, end) = selection;
    let chars: Vec<char> = text.chars().collect();

    if end > start && end <= chars.len() {
        let slice: String = chars[start..end].iter().collect();
        let trimmed = slice.trim();
        if !trimmed.is_empty() && !trimmed.contains(char::is_whitespace) {
            let lead = slice.len() - slice.trim_start().len();
            let span = Span::new(start + lead, start + lead + trimmed.chars().count());
            return Some((trimmed.to_string(), span));
        }
    }

    // Caret, or a selection that is not a single word: use the word token at the caret, and
    // tolerate a caret sitting just past the end of a word.
    let probe = start.min(chars.len().saturating_sub(1));
    let token = document
        .get_token_at_char_index(probe)
        .filter(|token| token.kind.is_word_like())
        .or_else(|| {
            probe
                .checked_sub(1)
                .and_then(|index| document.get_token_at_char_index(index))
                .filter(|token| token.kind.is_word_like())
        })?;
    let word: String = chars[token.span.start..token.span.end].iter().collect();
    Some((word, token.span))
}

/// Ranked alternatives for the word at `span` in `text`, at most `max` of them.
pub fn alternatives(text: &str, span: Span<char>, guides: &[StyleGuide], max: usize) -> Lookup {
    let chars: Vec<char> = text.chars().collect();
    let word: String = chars[span.start.min(chars.len())..span.end.min(chars.len())]
        .iter()
        .collect();
    let lower = word.to_lowercase();

    let dictionary = FstDictionary::curated();
    let document = Document::new_plain_english(text, &dictionary);
    let word_metadata = document
        .get_token_at_char_index(span.start)
        .and_then(|token| match &token.kind {
            TokenKind::Word(Some(metadata)) => Some(metadata.clone()),
            _ => None,
        })
        .or_else(|| dictionary.get_word_metadata_str(&lower).map(|m| m.into_owned()));

    let forbidden: Vec<String> = active_by_precedence(guides)
        .iter()
        .flat_map(|guide| guide.forbidden_terms().map(str::to_lowercase).collect::<Vec<_>>())
        .collect();

    let Some(candidates) = harper_thesaurus::thesaurus().get_synonyms_freq_sorted(&lower) else {
        return Lookup {
            word,
            span,
            options: Vec::new(),
        };
    };

    let mut scored: Vec<Alternative> = Vec::new();
    for (rank, candidate) in candidates.iter().take(CANDIDATE_POOL).enumerate() {
        let candidate = candidate.trim();
        let candidate_lower = candidate.to_lowercase();

        if candidate_lower == lower
            || candidate_lower.starts_with(&lower)
            || lower.starts_with(&candidate_lower)
            || candidate.split_whitespace().count() > 2
            || !candidate
                .chars()
                .all(|c| c.is_alphabetic() || c == ' ' || c == '-' || c == '\'')
            || forbidden.iter().any(|f| f == &candidate_lower)
        {
            continue;
        }

        let candidate_metadata = dictionary.get_word_metadata_str(&candidate_lower);
        let in_dictionary = candidate_metadata.is_some();
        let grammatical_distance = match (&word_metadata, candidate_metadata) {
            (Some(original), Some(candidate)) => original.difference(&candidate) as f32,
            (Some(_), None) => 12.0,
            (None, _) => 0.0,
        };

        let score = 1.0 / (1.0 + 0.35 * grammatical_distance + 0.04 * rank as f32);
        let register = if in_dictionary && rank < 25 {
            "neutral"
        } else {
            "formal"
        };

        scored.push(Alternative {
            word: candidate.to_string(),
            register,
            score,
        });
    }

    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    // Diversity: one option per stem.
    let mut options: Vec<Alternative> = Vec::new();
    for alternative in scored {
        let stem = stem_of(&alternative.word);
        if options.iter().any(|kept| stem_of(&kept.word) == stem) {
            continue;
        }
        options.push(alternative);
        if options.len() == max {
            break;
        }
    }

    Lookup {
        word,
        span,
        options,
    }
}

fn stem_of(word: &str) -> String {
    word.to_lowercase().chars().take(4).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guide::Rule;

    #[test]
    fn picks_the_selected_word_or_the_word_at_the_caret() {
        let text = "The strong strain has a strong nose.";
        assert_eq!(
            word_at(text, (4, 10)),
            Some(("strong".to_string(), Span::new(4, 10)))
        );
        // Caret inside "strain".
        assert_eq!(
            word_at(text, (13, 13)),
            Some(("strain".to_string(), Span::new(11, 17)))
        );
        // Caret just past "nose" (before the period).
        assert_eq!(
            word_at(text, (35, 35)),
            Some(("nose".to_string(), Span::new(31, 35)))
        );
        // A multi-word selection falls back to the word at its start.
        assert_eq!(
            word_at(text, (4, 17)).map(|(w, _)| w),
            Some("strong".to_string())
        );
    }

    #[test]
    fn ranks_five_varied_alternatives_and_honours_guides() {
        let text = "The strong strain has a strong nose.";
        let span = Span::new(24, 30);

        let plain = alternatives(text, span, &[], 5);
        assert_eq!(plain.word, "strong");
        assert_eq!(plain.options.len(), 5, "{plain:?}");
        let words: Vec<&str> = plain.options.iter().map(|o| o.word.as_str()).collect();
        assert!(!words.iter().any(|w| w.starts_with("strong")), "{words:?}");
        let stems: std::collections::HashSet<String> =
            words.iter().map(|w| stem_of(w)).collect();
        assert_eq!(stems.len(), words.len(), "stems repeat: {words:?}");
        assert!(plain.options.windows(2).all(|w| w[0].score >= w[1].score));

        // A guide that forbids the top pick removes it from the options.
        let top = words[0].to_string();
        let mut guide = StyleGuide::new("g", "G");
        guide.active = true;
        guide.rules.push(Rule::ForbidTerm {
            terms: vec![top.clone()],
            prefer: None,
            message: None,
        });
        let guided = alternatives(text, span, &[guide], 5);
        assert!(
            !guided.options.iter().any(|o| o.word == top),
            "{top} should be filtered: {guided:?}"
        );
    }

    #[test]
    fn unknown_words_have_no_alternatives() {
        let text = "Try the zxqvbn blend.";
        let lookup = alternatives(text, Span::new(8, 14), &[], 5);
        assert!(lookup.options.is_empty());
    }
}
