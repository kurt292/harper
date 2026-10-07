//! The style linters must survive inside Harper's curated group, which is what the desktop
//! app actually builds, not just an empty group.

use broadside_style::{install, sample_guides};
use harper_core::linting::{FlatConfig, LintGroup, Linter};
use harper_core::spell::FstDictionary;
use harper_core::{Dialect, Document};

#[test]
fn style_lints_appear_in_a_curated_lint_group() {
    let dictionary = FstDictionary::curated();
    let mut group = LintGroup::new_curated(dictionary.clone(), Dialect::American)
        .with_lint_config(FlatConfig::new_curated());
    install(&mut group, &sample_guides());

    let text = "We utilize cannabis and weed. Please purchase today.";
    let document = Document::new_markdown_default(text, &dictionary);
    let organized = group.organized_lints(&document);

    let style_keys: Vec<&String> = organized
        .iter()
        .filter(|(key, lints)| key.starts_with("Style:") && !lints.is_empty())
        .map(|(key, _)| key)
        .collect();
    assert!(
        !style_keys.is_empty(),
        "no Style lints; keys with lints: {:?}",
        organized.iter().filter(|(_, l)| !l.is_empty()).map(|(k, _)| k).collect::<Vec<_>>()
    );

    let flagged: Vec<String> = group
        .lint(&document)
        .iter()
        .map(|lint| text.chars().skip(lint.span.start).take(lint.span.len()).collect())
        .collect();
    assert!(flagged.iter().any(|w| w == "utilize"), "flagged: {flagged:?}");
    assert!(flagged.iter().any(|w| w == "weed"), "flagged: {flagged:?}");
}
