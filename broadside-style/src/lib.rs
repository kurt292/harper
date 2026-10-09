//! Broadside style guides.
//!
//! A style guide is a named, user-authored rule set stored as JSON. Zero or more guides are
//! active at once. Deterministic rules (`forbid_term`, `forbid_pattern`, `max_sentence_words`)
//! compile into Harper [`Linter`]s and run continuously next to the grammar rules. Model rules
//! are carried in the same file for the on-demand style pass (Lane B).
//!
//! ```
//! use broadside_style::{GuideStore, install, sample_guides};
//! use harper_core::linting::LintGroup;
//!
//! let guides = sample_guides();
//! let mut group = LintGroup::empty();
//! let conflicts = install(&mut group, &guides);
//! assert!(conflicts.is_empty()); // only one sample is active by default
//! assert!(group.iter_keys().any(|key| key.starts_with("Style:")));
//! ```

mod conflicts;
mod guide;
mod linters;
pub mod model;
mod store;
pub mod thesaurus;

pub use conflicts::{Conflict, EffectiveRule, active_by_precedence, resolve};
pub use guide::{Bindings, Rule, StyleGuide};
pub use linters::compile;
pub use store::{GuideStore, Loaded, StoreError, sample_guides};

use harper_core::linting::LintGroup;

/// Prefix of every linter key this crate adds to a [`LintGroup`].
pub const LINTER_KEY_PREFIX: &str = "Style:";

/// Builds the linter key for a rule: `Style:<guide id>:<rule number>:<rule kind>`.
pub fn linter_key(guide: &StyleGuide, index: usize, rule: &Rule) -> String {
    format!(
        "{LINTER_KEY_PREFIX}{}:{}:{}",
        guide.id,
        index + 1,
        rule.kind_name()
    )
}

/// Compiles the active guides' deterministic rules into `group`, enabled, after resolving
/// conflicts by precedence. Returns the conflicts so the caller can surface them.
pub fn install(group: &mut LintGroup, guides: &[StyleGuide]) -> Vec<Conflict> {
    let active = active_by_precedence(guides);
    let (effective, conflicts) = resolve(&active);

    for effective_rule in effective {
        let Some(linter) = compile(&effective_rule.guide.name, effective_rule.rule) else {
            continue;
        };
        let key = linter_key(effective_rule.guide, effective_rule.index, effective_rule.rule);
        if group.add(&key, BoxedLinter(linter)) {
            group.config.set_rule_enabled(&key, true);
        }
    }

    conflicts
}

/// `LintGroup::add` takes a concrete `impl Linter`; this forwards a boxed one.
struct BoxedLinter(Box<dyn harper_core::linting::Linter>);

impl harper_core::linting::Linter for BoxedLinter {
    fn lint(&mut self, document: &harper_core::Document) -> Vec<harper_core::linting::Lint> {
        self.0.lint(document)
    }

    fn description(&self) -> &str {
        self.0.description()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harper_core::Document;
    use harper_core::linting::Linter;
    use harper_core::spell::FstDictionary;

    #[test]
    fn switching_guides_changes_the_lints_on_identical_text() {
        let text = "We utilize cannabis and weed. Please purchase today.";
        let dictionary = FstDictionary::curated();
        let document = Document::new_plain_english(text, &dictionary);

        let mut guides = sample_guides();

        // Email guide only (the default): "utilize" and "weed" are flagged, "cannabis" is fine.
        let mut group = LintGroup::empty();
        install(&mut group, &guides);
        let email_lints: Vec<String> = flagged(&mut group, &document, text);
        assert_eq!(email_lints, vec!["utilize", "weed"]);

        // Landing guide only: "cannabis" and "purchase" are flagged instead.
        guides[0].active = false;
        guides[1].active = true;
        let mut group = LintGroup::empty();
        install(&mut group, &guides);
        let landing_lints = flagged(&mut group, &document, text);
        assert_eq!(landing_lints, vec!["cannabis", "purchase"]);

        // Both active: email wins (priority 10 < 20), so landing's "cannabis" rule is dropped
        // and reported; its "purchase" rule still applies.
        guides[0].active = true;
        let mut group = LintGroup::empty();
        let conflicts = install(&mut group, &guides);
        assert_eq!(conflicts.len(), 2, "{conflicts:?}");
        let both = flagged(&mut group, &document, text);
        assert_eq!(both, vec!["utilize", "weed", "purchase"]);
    }

    fn flagged(group: &mut LintGroup, document: &Document, text: &str) -> Vec<String> {
        let mut lints = group.lint(document);
        lints.sort_by_key(|lint| lint.span.start);
        lints
            .iter()
            .map(|lint| text.chars().skip(lint.span.start).take(lint.span.len()).collect())
            .collect()
    }
}
