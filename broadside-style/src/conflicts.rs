//! Resolution of conflicting rules between simultaneously active guides.
//!
//! The PRD rule: when two active guides conflict, the one with higher precedence (lower
//! `priority`) wins, and the conflict is reported rather than silently resolved.

use std::fmt;

use crate::guide::{Rule, StyleGuide};

/// A rule that survived conflict resolution, with the guide it came from.
#[derive(Debug, Clone)]
pub struct EffectiveRule<'a> {
    pub guide: &'a StyleGuide,
    pub rule: &'a Rule,
    /// Position of the rule inside its guide, for stable linter names.
    pub index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub winner: String,
    pub loser: String,
    pub detail: String,
}

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "“{}” overrides “{}”: {}",
            self.winner, self.loser, self.detail
        )
    }
}

/// Orders `guides` by precedence (priority, then name) and keeps only the active ones.
pub fn active_by_precedence(guides: &[StyleGuide]) -> Vec<&StyleGuide> {
    let mut active: Vec<&StyleGuide> = guides.iter().filter(|guide| guide.active).collect();
    active.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.name.cmp(&b.name)));
    active
}

/// Drops lower-precedence rules that contradict a higher-precedence guide and reports each drop.
///
/// Two kinds of contradiction are detected:
/// - a term one guide forbids that a higher-precedence guide prefers (or the reverse);
/// - duplicated `max_sentence_words` limits, where only the winner's limit is kept so the same
///   sentence is not flagged twice.
pub fn resolve<'a>(active: &[&'a StyleGuide]) -> (Vec<EffectiveRule<'a>>, Vec<Conflict>) {
    let mut effective = Vec::new();
    let mut conflicts = Vec::new();
    let mut sentence_limit_owner: Option<&StyleGuide> = None;

    for (position, guide) in active.iter().enumerate() {
        let winners = &active[..position];

        for (index, rule) in guide.rules.iter().enumerate() {
            match rule {
                Rule::ForbidTerm { terms, prefer, .. } => {
                    if let Some(conflict) = term_conflict(guide, terms, prefer.as_deref(), winners) {
                        conflicts.push(conflict);
                        continue;
                    }
                }
                Rule::ForbidPattern { prefer: Some(prefer), .. } => {
                    if let Some(winner) = winners.iter().find(|w| forbids(w, prefer)) {
                        conflicts.push(Conflict {
                            winner: winner.name.clone(),
                            loser: guide.name.clone(),
                            detail: format!(
                                "a forbid_pattern rule prefers “{prefer}”, which “{}” forbids; the rule was dropped",
                                winner.name
                            ),
                        });
                        continue;
                    }
                }
                Rule::MaxSentenceWords { value } => match sentence_limit_owner {
                    Some(owner) if !std::ptr::eq(owner, *guide) => {
                        let owner_limit = sentence_limit(owner).unwrap_or(0);
                        if owner_limit != *value {
                            conflicts.push(Conflict {
                                winner: owner.name.clone(),
                                loser: guide.name.clone(),
                                detail: format!(
                                    "both set max_sentence_words ({owner_limit} vs {value}); {owner_limit} applies"
                                ),
                            });
                        }
                        continue;
                    }
                    _ => sentence_limit_owner = Some(guide),
                },
                _ => {}
            }

            effective.push(EffectiveRule { guide, rule, index });
        }
    }

    (effective, conflicts)
}

fn eq_term(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn forbids(guide: &StyleGuide, term: &str) -> bool {
    guide.forbidden_terms().any(|t| eq_term(t, term))
}

fn prefers(guide: &StyleGuide, term: &str) -> bool {
    guide.preferred_terms().any(|t| eq_term(t, term))
}

fn sentence_limit(guide: &StyleGuide) -> Option<usize> {
    guide.rules.iter().find_map(|rule| match rule {
        Rule::MaxSentenceWords { value } => Some(*value),
        _ => None,
    })
}

fn term_conflict(
    loser: &StyleGuide,
    terms: &[String],
    prefer: Option<&str>,
    winners: &[&StyleGuide],
) -> Option<Conflict> {
    for winner in winners {
        if let Some(term) = terms.iter().find(|term| prefers(winner, term)) {
            return Some(Conflict {
                winner: winner.name.clone(),
                loser: loser.name.clone(),
                detail: format!(
                    "forbids “{term}”, which “{}” prefers; the rule was dropped",
                    winner.name
                ),
            });
        }
        if let Some(prefer) = prefer
            && forbids(winner, prefer)
        {
            return Some(Conflict {
                winner: winner.name.clone(),
                loser: loser.name.clone(),
                detail: format!(
                    "prefers “{prefer}”, which “{}” forbids; the rule was dropped",
                    winner.name
                ),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guide(id: &str, priority: u32, rules: Vec<Rule>) -> StyleGuide {
        let mut guide = StyleGuide::new(id, id.to_uppercase());
        guide.active = true;
        guide.priority = priority;
        guide.rules = rules;
        guide
    }

    fn forbid(terms: &[&str], prefer: &str) -> Rule {
        Rule::ForbidTerm {
            terms: terms.iter().map(|t| t.to_string()).collect(),
            prefer: Some(prefer.to_string()),
            message: None,
        }
    }

    #[test]
    fn higher_precedence_guide_wins_term_conflicts() {
        let email = guide("email", 10, vec![forbid(&["weed", "pot"], "cannabis")]);
        let landing = guide(
            "landing",
            20,
            vec![forbid(&["cannabis"], "weed"), forbid(&["utilize"], "use")],
        );
        let guides = vec![landing.clone(), email.clone()];

        let active = active_by_precedence(&guides);
        assert_eq!(active[0].id, "email");

        let (effective, conflicts) = resolve(&active);
        let kept: Vec<(&str, usize)> = effective.iter().map(|e| (e.guide.id.as_str(), e.index)).collect();
        assert_eq!(kept, vec![("email", 0), ("landing", 1)]);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].winner, "EMAIL");
        assert_eq!(conflicts[0].loser, "LANDING");
    }

    #[test]
    fn only_the_winning_sentence_limit_survives() {
        let a = guide("a", 1, vec![Rule::MaxSentenceWords { value: 25 }]);
        let b = guide("b", 2, vec![Rule::MaxSentenceWords { value: 15 }]);
        let guides = vec![a, b];
        let active = active_by_precedence(&guides);
        let (effective, conflicts) = resolve(&active);
        assert_eq!(effective.len(), 1);
        assert_eq!(effective[0].guide.id, "a");
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].detail.contains("25 applies"));
    }

    #[test]
    fn inactive_guides_are_ignored() {
        let mut off = guide("off", 1, vec![forbid(&["x"], "y")]);
        off.active = false;
        let on = guide("on", 2, vec![forbid(&["y"], "x")]);
        let guides = vec![off, on];
        let active = active_by_precedence(&guides);
        let (effective, conflicts) = resolve(&active);
        assert_eq!(effective.len(), 1);
        assert!(conflicts.is_empty());
    }
}
