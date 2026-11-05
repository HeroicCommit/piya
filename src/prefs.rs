//! User-facing prefs stored under the Piya data dir (`prefs.json`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserPrefs {
    /// Free-form “about you” profile injected into every chat system prompt.
    #[serde(default)]
    pub profile: String,
    /// When true, chat may auto-save clear preference statements to memory.
    #[serde(default)]
    pub auto_capture_prefs: bool,
    /// Stub: morning digest preference (scheduling not wired yet).
    #[serde(default)]
    pub digest_enabled: bool,
    /// Stub: preferred digest hour local (0–23); unused until scheduler lands.
    #[serde(default = "default_digest_hour")]
    pub digest_hour: u32,
}

fn default_digest_hour() -> u32 {
    8
}

impl Default for UserPrefs {
    fn default() -> Self {
        Self {
            profile: String::new(),
            auto_capture_prefs: false,
            digest_enabled: false,
            digest_hour: default_digest_hour(),
        }
    }
}

impl UserPrefs {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("prefs.json")
    }

    pub fn load(data_dir: &Path) -> Result<Self> {
        let path = Self::path(data_dir);
        if !path.is_file() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let path = Self::path(data_dir);
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json + "\n").with_context(|| format!("write {}", path.display()))
    }
}

/// Heuristic: looks like an explicit preference the user stated.
pub fn looks_like_preference(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    if lower.len() < 12 || lower.len() > 400 {
        return false;
    }
    // Avoid treating reminders / tasks as prefs.
    if lower.starts_with("remind ")
        || lower.starts_with("remind me")
        || lower.starts_with("remember to ")
        || lower.contains("every day")
        || lower.contains("every morning")
    {
        return false;
    }
    let cues = [
        "i prefer ",
        "i always ",
        "i never ",
        "please always ",
        "please never ",
        "call me ",
        "my name is ",
        "don't ",
        "do not ",
        "prefer ",
        "always use ",
        "never use ",
    ];
    cues.iter().any(|c| lower.contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_prefs() {
        assert!(looks_like_preference("I prefer markdown reports"));
        assert!(!looks_like_preference("Remind me at 9pm"));
        assert!(!looks_like_preference("hi"));
    }
}
