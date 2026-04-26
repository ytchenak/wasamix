//! Configuration module — loads and saves the user's mic selection to JSON.
//!
//! RUST CONCEPT: `serde` and derive macros
//! ----------------------------------------
//! `#[derive(Serialize, Deserialize)]` automatically generates code to
//! convert our struct to/from JSON. In Python you'd manually call
//! json.load/json.dump — Rust does this at compile time via "derive macros".
//!
//! RUST CONCEPT: `Option<T>`
//! -------------------------
//! `Option<i32>` means "either Some(i32) or None". It's Rust's way of
//! handling nullable values WITHOUT null pointer exceptions. The compiler
//! forces you to check for None before using the value.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Our config struct. `#[derive(...)]` auto-generates serialization code.
/// In Python this would be a dict — in Rust it's a typed struct.
#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    /// Selected microphone (capture device). `None` means "use first available".
    pub mic_device_id: Option<String>,

    /// Render device whose playback we capture via WASAPI loopback. `None`
    /// means "Windows default". `#[serde(default)]` keeps legacy configs
    /// (from wasamix ≤ 0.1.0, which didn't have this field) loadable.
    #[serde(default)]
    pub system_source_device_id: Option<String>,

    /// Destination render device — where the mixed audio is written. `None`
    /// means "auto-detect VB-Cable". Users who want to target a different
    /// virtual cable (OBS Virtual Audio, CABLE-B, a real device for
    /// debugging) can pin a specific ID here.
    #[serde(default)]
    pub output_device_id: Option<String>,

    /// Whether the tray icon color-codes the current output level.
    /// `#[serde(default = "...")]` lets older config.json files (missing this
    /// field) still deserialize — they get `default_show_level_meter()`.
    #[serde(default = "default_show_level_meter")]
    pub show_level_meter: bool,
}

fn default_show_level_meter() -> bool {
    true
}

/// `impl` blocks attach methods to a struct — similar to defining methods
/// inside a Python class, but Rust separates data (struct) from behavior (impl).
impl Config {
    /// Load config from disk. If the file doesn't exist or is corrupt, return defaults.
    ///
    /// RUST CONCEPT: `Result<T, E>` and the `?` operator
    /// --------------------------------------------------
    /// Functions that can fail return `Result<T, E>` — either `Ok(value)` or `Err(error)`.
    /// The `?` operator is sugar: if the result is Err, return early with that error.
    /// If it's Ok, unwrap the value and continue. It replaces Python's try/except.
    pub fn load() -> Self {
        let path = Self::default_path();
        Self::load_from(&path).unwrap_or_default()
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)?;
        let config: Config = serde_json::from_str(&text)?;
        Ok(config)
    }

    /// Save config to disk.
    pub fn save(&self) -> Result<()> {
        let path = Self::default_path();
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        fs::write(path, text)?;
        Ok(())
    }

    fn default_path() -> PathBuf {
        // `env::current_exe()` gets the path of the running .exe.
        // We store config.json next to it, just like the Python version.
        let mut path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
        path.pop(); // remove the exe filename
        path.push("config.json");
        path
    }
}

/// `Default` is a trait (like a Python protocol/interface) that provides
/// a default value. `Config::default()` returns this.
/// `unwrap_or_default()` above uses this when loading fails.
impl Default for Config {
    fn default() -> Self {
        Config {
            mic_device_id: None,
            system_source_device_id: None,
            output_device_id: None,
            show_level_meter: default_show_level_meter(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_load_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        let config = Config::load_from(&path);
        // `is_err()` checks if Result is Err — file doesn't exist
        assert!(config.is_err());
    }

    #[test]
    fn test_save_and_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        let config = Config {
            mic_device_id: Some("mic-123".to_string()),
            system_source_device_id: Some("src-456".to_string()),
            output_device_id: Some("out-789".to_string()),
            show_level_meter: false,
        };
        config.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.mic_device_id.as_deref(), Some("mic-123"));
        assert_eq!(loaded.system_source_device_id.as_deref(), Some("src-456"));
        assert_eq!(loaded.output_device_id.as_deref(), Some("out-789"));
        assert!(!loaded.show_level_meter);
    }

    #[test]
    fn test_load_legacy_without_new_fields() {
        // A config.json from wasamix 0.1.0 has only `mic_device_id`.
        // It must still load — new fields default to None / true.
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"mic_device_id":"abc"}"#).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.mic_device_id.as_deref(), Some("abc"));
        assert_eq!(loaded.system_source_device_id, None);
        assert_eq!(loaded.output_device_id, None);
        assert!(loaded.show_level_meter);
    }

    #[test]
    fn test_load_corrupt_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, "not json{{{").unwrap();
        let config = Config::load_from(&path);
        assert!(config.is_err());
    }
}
