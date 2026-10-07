//! Style guides on disk: one JSON file per guide in a single directory.

use std::fs;
use std::path::{Path, PathBuf};

use crate::guide::{Rule, StyleGuide};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("style guide directory {0} could not be read: {1}")]
    Directory(PathBuf, std::io::Error),
    #[error("style guide {0} could not be written: {1}")]
    Write(PathBuf, std::io::Error),
    #[error("style guide {0} is invalid: {1}")]
    Invalid(String, String),
    #[error("no style guide with id {0:?}")]
    NotFound(String),
    #[error("{0}")]
    Serialize(#[from] serde_json::Error),
}

/// Result of scanning the directory. Files that fail to parse are reported, not fatal, so one
/// bad edit never disables every other guide.
#[derive(Debug, Default)]
pub struct Loaded {
    /// Sorted by priority, then name.
    pub guides: Vec<StyleGuide>,
    pub errors: Vec<(PathBuf, String)>,
}

#[derive(Debug, Clone)]
pub struct GuideStore {
    dir: PathBuf,
}

impl GuideStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path_for(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Reads every `*.json` file in the directory. A missing directory is an empty store.
    pub fn load(&self) -> Result<Loaded, StoreError> {
        let mut loaded = Loaded::default();

        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(loaded),
            Err(error) => return Err(StoreError::Directory(self.dir.clone(), error)),
        };

        for entry in entries {
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(error) => {
                    loaded.errors.push((self.dir.clone(), error.to_string()));
                    continue;
                }
            };
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match Self::read_guide(&path) {
                Ok(guide) => loaded.guides.push(guide),
                Err(error) => loaded.errors.push((path, error)),
            }
        }

        loaded
            .guides
            .sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.name.cmp(&b.name)));
        Ok(loaded)
    }

    fn read_guide(path: &Path) -> Result<StyleGuide, String> {
        let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
        let guide: StyleGuide = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        guide.validate()?;
        Ok(guide)
    }

    /// Writes a guide as pretty JSON to `<id>.json`, creating the directory if needed.
    pub fn save(&self, guide: &StyleGuide) -> Result<PathBuf, StoreError> {
        guide
            .validate()
            .map_err(|error| StoreError::Invalid(guide.id.clone(), error))?;
        fs::create_dir_all(&self.dir).map_err(|e| StoreError::Write(self.dir.clone(), e))?;

        let path = self.path_for(&guide.id);
        let json = serde_json::to_string_pretty(guide)?;
        // Write to a sibling first so a reader polling the directory never sees a half-written file.
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json).map_err(|e| StoreError::Write(tmp.clone(), e))?;
        fs::rename(&tmp, &path).map_err(|e| StoreError::Write(path.clone(), e))?;
        Ok(path)
    }

    pub fn get(&self, id: &str) -> Result<StyleGuide, StoreError> {
        let path = self.path_for(id);
        if !path.exists() {
            return Err(StoreError::NotFound(id.to_string()));
        }
        Self::read_guide(&path).map_err(|error| StoreError::Invalid(id.to_string(), error))
    }

    /// Flips a guide's `active` flag on disk and returns the new value.
    pub fn set_active(&self, id: &str, active: bool) -> Result<StyleGuide, StoreError> {
        let mut guide = self.get(id)?;
        guide.active = active;
        self.save(&guide)?;
        Ok(guide)
    }

    pub fn toggle_active(&self, id: &str) -> Result<StyleGuide, StoreError> {
        let guide = self.get(id)?;
        self.set_active(id, !guide.active)
    }

    pub fn delete(&self, id: &str) -> Result<(), StoreError> {
        let path = self.path_for(id);
        fs::remove_file(&path).map_err(|e| StoreError::Write(path, e))
    }

    /// Writes the two sample guides when the store holds no guides at all. Returns whether it did.
    pub fn ensure_samples(&self) -> Result<bool, StoreError> {
        if !self.load()?.guides.is_empty() {
            return Ok(false);
        }
        for guide in sample_guides() {
            self.save(&guide)?;
        }
        Ok(true)
    }
}

/// Two guides with deliberately conflicting rules, so the P2 exit criterion (switching guides
/// changes the suggestions on identical text) can be demonstrated immediately.
pub fn sample_guides() -> Vec<StyleGuide> {
    let mut email = StyleGuide::new("nimbus-b2b-email", "Nimbus B2B Email");
    email.active = true;
    email.priority = 10;
    email.rules = vec![
        Rule::ForbidTerm {
            terms: vec!["weed".into(), "pot".into(), "marijuana".into()],
            prefer: Some("cannabis".into()),
            message: None,
        },
        Rule::ForbidTerm {
            terms: vec!["utilize".into()],
            prefer: Some("use".into()),
            message: None,
        },
        Rule::ForbidTerm {
            terms: vec!["reach out".into()],
            prefer: Some("contact".into()),
            message: None,
        },
        Rule::MaxSentenceWords { value: 25 },
        Rule::ForbidPattern {
            regex: r"\b(very|really|just)\b".into(),
            ignore_case: true,
            prefer: None,
            message: Some("Intensifiers weaken a sales email. Cut the word or pick a stronger one.".into()),
        },
        Rule::Voice {
            value: "active".into(),
        },
        Rule::ReadingLevel { max_grade: 9 },
        Rule::Freeform {
            instruction: "Lead with the offer. No pleasantries before the first sentence of substance."
                .into(),
        },
    ];
    email.bindings.apps = vec!["outlook.exe".into(), "olk.exe".into()];
    email.bindings.urls = vec!["mail.google.com".into()];

    let mut landing = StyleGuide::new("brand-landing-page", "Brand Landing Page");
    landing.active = false;
    landing.priority = 20;
    landing.rules = vec![
        Rule::ForbidTerm {
            terms: vec!["cannabis".into()],
            prefer: Some("weed".into()),
            message: Some("Landing pages talk like customers do. Say “weed”.".into()),
        },
        Rule::ForbidTerm {
            terms: vec!["purchase".into()],
            prefer: Some("buy".into()),
            message: None,
        },
        Rule::MaxSentenceWords { value: 15 },
        Rule::ForbidPattern {
            regex: r"\b(leverage|synergy|solutions?)\b".into(),
            ignore_case: true,
            prefer: None,
            message: Some("Corporate filler. Say what it does.".into()),
        },
        Rule::Voice {
            value: "active".into(),
        },
        Rule::Freeform {
            instruction: "Second person, present tense, short punchy lines. One idea per sentence."
                .into(),
        },
    ];

    vec![email, landing]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_round_trip_through_disk_and_toggle() {
        let dir = tempfile::tempdir().unwrap();
        let store = GuideStore::new(dir.path().join("guides"));

        assert!(store.load().unwrap().guides.is_empty());
        assert!(store.ensure_samples().unwrap());
        assert!(!store.ensure_samples().unwrap());

        let loaded = store.load().unwrap();
        assert!(loaded.errors.is_empty());
        assert_eq!(loaded.guides.len(), 2);
        assert_eq!(loaded.guides[0].id, "nimbus-b2b-email");
        assert!(loaded.guides[0].active);
        assert!(!loaded.guides[1].active);

        let toggled = store.toggle_active("brand-landing-page").unwrap();
        assert!(toggled.active);
        assert!(store.get("brand-landing-page").unwrap().active);
        assert_eq!(store.load().unwrap().guides.iter().filter(|g| g.active).count(), 2);
    }

    #[test]
    fn bad_files_are_reported_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let store = GuideStore::new(dir.path());
        store.ensure_samples().unwrap();
        fs::write(dir.path().join("broken.json"), "{ not json").unwrap();
        fs::write(dir.path().join("notes.txt"), "ignored").unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.guides.len(), 2);
        assert_eq!(loaded.errors.len(), 1);
        assert!(loaded.errors[0].0.ends_with("broken.json"));
    }
}
