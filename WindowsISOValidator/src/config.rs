//! Portable configuration stored next to the executable.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Where ESD / ISO downloads are stored.
    pub download_dir: PathBuf,
    /// Where MediaCreationTool.bat is written and run from (its output ISO lands here too).
    pub work_dir: PathBuf,
    /// Locale used for the Microsoft download page API (e.g. en-US).
    pub ms_locale: String,
    /// Last selected catalog version id, e.g. 11_26H2.
    pub last_vid: String,
}

impl Default for Config {
    fn default() -> Self {
        let base = portable_base_dir();
        Self {
            download_dir: base.join("downloads"),
            work_dir: base.join("mct"),
            ms_locale: "en-US".to_string(),
            last_vid: "11_26H2".to_string(),
        }
    }
}

static BASE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Directory of the executable when it is writable, otherwise a folder in the user's profile.
/// Resolved once per process (the probe creates and deletes a file).
pub fn portable_base_dir() -> PathBuf {
    BASE_DIR.get_or_init(resolve_base_dir).clone()
}

fn resolve_base_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if dir_writable(dir) {
                return dir.to_path_buf();
            }
        }
    }
    // Never fall back to the temp directory: MediaCreationTool.bat treats a temp location as
    // "run from a zip" and writes its ISO to C:\ESD instead of the work folder.
    for var in ["USERPROFILE", "HOME"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                let dir = PathBuf::from(v).join("WindowsISOValidator");
                if std::fs::create_dir_all(&dir).is_ok() && dir_writable(&dir) {
                    return dir;
                }
            }
        }
    }
    std::env::temp_dir().join("WindowsISOValidator")
}

fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(".wiv_write_probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

pub fn config_path() -> PathBuf {
    portable_base_dir().join("WindowsISOValidator.json")
}

pub fn load() -> Config {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

pub fn save(cfg: &Config) -> Result<()> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let text = serde_json::to_string_pretty(cfg)?;
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
}
