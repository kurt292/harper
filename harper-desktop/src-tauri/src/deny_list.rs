//! Broadside's deny-list: applications and sites Harper must never read, whatever the
//! integration list says. Password managers, banking, payroll and sign-in pages by default.
//!
//! Stored as `broadside-deny-list.json` next to Harper's config, created with the defaults on
//! first use and editable from the Style Guides settings page. The highlighter re-reads it when
//! the file changes.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::config::Config;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DenyList {
    /// Executable names (or full paths) that are never read.
    #[serde(default)]
    pub apps: Vec<String>,
    /// Substrings matched, case-insensitively, against the browser's current address.
    #[serde(default)]
    pub urls: Vec<String>,
}

impl Default for DenyList {
    fn default() -> Self {
        Self {
            apps: [
                "1password.exe",
                "bitwarden.exe",
                "keepass.exe",
                "keepassxc.exe",
                "lastpass.exe",
                "dashlane.exe",
                "enpass.exe",
                "nordpass.exe",
                "roboform.exe",
                "credentialuibroker.exe",
            ]
            .map(String::from)
            .to_vec(),
            urls: [
                "bank",
                "chase.com",
                "wellsfargo.com",
                "capitalone.com",
                "citi.com",
                "usbank.com",
                "fidelity.com",
                "schwab.com",
                "vanguard.com",
                "paypal.com",
                "venmo.com",
                "coinbase.com",
                "adp.com",
                "gusto.com",
                "paychex.com",
                "workday.com",
                "quickbooks.intuit.com",
                "irs.gov",
                "ssa.gov",
                "accounts.google.com",
                "login.microsoftonline.com",
                "login.",
                "signin.",
                "/login",
                "/signin",
                "/password",
                "/checkout",
            ]
            .map(String::from)
            .to_vec(),
        }
    }
}

impl DenyList {
    pub fn path() -> Option<PathBuf> {
        Config::style_guides_dir().map(|dir| {
            dir.parent()
                .map(|parent| parent.join("broadside-deny-list.json"))
                .unwrap_or_else(|| dir.join("broadside-deny-list.json"))
        })
    }

    /// Whether `app_id` (an executable path or name) is denied. Entries without a path
    /// separator match the file name; others the whole path. Case-insensitive.
    pub fn denies_app(&self, app_id: &str) -> bool {
        let file_name = app_id.rsplit(['\\', '/']).next().unwrap_or(app_id);
        self.apps.iter().any(|entry| {
            if entry.contains(['\\', '/']) {
                entry.eq_ignore_ascii_case(app_id)
            } else {
                entry.eq_ignore_ascii_case(file_name)
            }
        })
    }

    /// Whether a browser address is denied: any pattern appearing in the lowercased URL.
    pub fn denies_url(&self, url: &str) -> bool {
        let url = url.to_lowercase();
        self.urls
            .iter()
            .map(|pattern| pattern.trim().to_lowercase())
            .filter(|pattern| !pattern.is_empty())
            .any(|pattern| url.contains(&pattern))
    }

    /// Reads the list, creating the file with defaults when it is missing. A corrupt file is
    /// reported and treated as the defaults so protection never silently switches off.
    pub fn load() -> DenyList {
        let Some(path) = Self::path() else {
            return DenyList::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<DenyList>(&text) {
                Ok(list) => list,
                Err(error) => {
                    eprintln!(
                        "Deny-list {} could not be parsed ({error}); using the defaults",
                        path.display()
                    );
                    DenyList::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let list = DenyList::default();
                if let Err(error) = list.save() {
                    eprintln!("Could not write the default deny-list: {error}");
                }
                list
            }
            Err(error) => {
                eprintln!(
                    "Deny-list {} could not be read ({error}); using the defaults",
                    path.display()
                );
                DenyList::default()
            }
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path().ok_or_else(|| "config directory unavailable".to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    /// The current list, re-read only when the file's modification time changes. Cheap enough
    /// to call on every highlighter tick.
    pub fn current() -> DenyList {
        static CACHE: Mutex<Option<(Option<SystemTime>, DenyList)>> = Mutex::new(None);

        let modified = Self::path().and_then(|path| std::fs::metadata(path).ok()?.modified().ok());
        let mut cache = match CACHE.lock() {
            Ok(cache) => cache,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some((cached_modified, list)) = cache.as_ref()
            && *cached_modified == modified
            && modified.is_some()
        {
            return list.clone();
        }
        let list = Self::load();
        *cache = Some((modified, list.clone()));
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cover_password_managers_and_banks() {
        let list = DenyList::default();
        assert!(list.denies_app(r"C:\Program Files\1Password\app\8\1Password.exe"));
        assert!(list.denies_app("KeePassXC.exe"));
        assert!(!list.denies_app(r"C:\Windows\System32\notepad.exe"));

        assert!(list.denies_url("https://secure.chase.com/web/auth/dashboard"));
        assert!(list.denies_url("https://www.mybank.example/accounts"));
        assert!(list.denies_url("https://accounts.google.com/signin/v2"));
        assert!(list.denies_url("https://example.com/login?next=/"));
        assert!(!list.denies_url("https://mail.google.com/mail/u/0/#inbox"));
        assert!(!list.denies_url("https://docs.google.com/document/d/abc"));
    }

    #[test]
    fn user_entries_match_like_defaults() {
        let list = DenyList {
            apps: vec![r"C:\Tools\secret.exe".into(), "hr.exe".into()],
            urls: vec!["intranet.example".into(), " ".into()],
        };
        assert!(list.denies_app(r"c:\tools\SECRET.EXE"));
        assert!(!list.denies_app(r"c:\other\secret.exe"));
        assert!(list.denies_app(r"D:\apps\HR.exe"));
        assert!(list.denies_url("https://Intranet.Example/payroll"));
        assert!(!list.denies_url("https://example.com"));
    }
}
