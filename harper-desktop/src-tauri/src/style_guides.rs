//! Broadside style guides: where they live on disk and how they join the lint group.
//!
//! Guides are plain JSON files in `<config dir>/harper-desktop/style-guides/`. Both the Tauri
//! process and the highlighter read them straight from disk whenever a linter is (re)built, so a
//! toggle from the tray reaches the highlighter on its next config refresh without any IPC
//! changes.

use std::path::PathBuf;

use broadside_style::{GuideStore, StyleGuide};
use harper_core::linting::LintGroup;
use tracing::{info, warn};

use crate::config::Config;

pub fn dir() -> Option<PathBuf> {
    Config::style_guides_dir()
}

pub fn store() -> Option<GuideStore> {
    dir().map(GuideStore::new)
}

/// Guides sorted by precedence, or an empty list when the directory is unreadable.
pub fn load_guides() -> Vec<StyleGuide> {
    let Some(store) = store() else {
        return Vec::new();
    };
    match store.load() {
        Ok(loaded) => {
            for (path, error) in &loaded.errors {
                warn!("Skipping style guide {}: {error}", path.display());
            }
            loaded.guides
        }
        Err(error) => {
            warn!("Style guides unavailable: {error}");
            Vec::new()
        }
    }
}

/// Adds the active guides' deterministic rules to `group`, logging conflicts when they change.
/// Linters are rebuilt every second, so logging on every build would repeat forever.
pub fn install_into(group: &mut LintGroup) {
    static LAST_CONFLICTS: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

    let guides = load_guides();
    let conflicts = broadside_style::install(group, &guides);
    let summary = conflicts
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    if let Ok(mut last) = LAST_CONFLICTS.lock()
        && last.as_deref() != Some(summary.as_str())
    {
        for conflict in &conflicts {
            warn!("Style guide conflict: {conflict}");
        }
        *last = Some(summary);
    }
}

/// Seeds the two sample guides on a fresh install.
pub fn ensure_samples() {
    let Some(store) = store() else {
        return;
    };
    match store.ensure_samples() {
        Ok(true) => info!("Wrote sample style guides to {}", store.dir().display()),
        Ok(false) => {}
        Err(error) => warn!("Could not write sample style guides: {error}"),
    }
}

/// Flips a guide's active flag on disk. Returns the updated guide.
pub fn toggle(id: &str) -> Result<StyleGuide, String> {
    let store = store().ok_or_else(|| "config directory unavailable".to_string())?;
    store.toggle_active(id).map_err(|error| error.to_string())
}
