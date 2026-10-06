use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Integration {
    pub bundle_id: String,
    pub enabled: bool,
}

impl Integration {
    /// Whether this entry applies to `bundle_id`.
    ///
    /// An exact match always applies. On Windows, an entry that is a bare executable name (no path
    /// separator) also matches any full path whose file name equals it, case-insensitively.
    pub fn matches(&self, bundle_id: &str) -> bool {
        if self.bundle_id == bundle_id {
            return true;
        }

        if !cfg!(target_os = "windows") || self.bundle_id.contains(['\\', '/']) {
            return false;
        }

        bundle_id
            .rsplit(['\\', '/'])
            .next()
            .is_some_and(|file_name| file_name.eq_ignore_ascii_case(&self.bundle_id))
    }

    pub fn curated_integrations() -> Vec<Self> {
        #[cfg(target_os = "macos")]
        let integrations = [
            "com.apple.TextEdit",
            "com.apple.mail",
            "com.apple.MobileSMS",
            "com.apple.Notes",
            "com.tinyspeck.slackmacgap",
            "com.hnc.Discord",
            "com.bloombuilt.dayone-mac",
        ];

        // Broadside: Windows identifies apps by executable path, which is machine-specific for
        // Store apps (Notepad) and per-user installs (Notion). Bare file names match any path with
        // that name; see `Integration::matches`.
        #[cfg(target_os = "windows")]
        let integrations = [
            "notepad.exe",
            "chrome.exe",
            "msedge.exe",
            "outlook.exe",
            "olk.exe",
            "notion.exe",
            "winword.exe",
            "slack.exe",
        ];

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let integrations: [&str; 0] = [];

        integrations
            .into_iter()
            .map(|bundle_id| Integration {
                bundle_id: bundle_id.to_string(),
                enabled: true,
            })
            .collect()
    }
}
