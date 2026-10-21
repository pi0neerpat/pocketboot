//! `<app_config_dir>/settings.json` (A6). A missing or unreadable file reads
//! as the defaults.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "settings.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// R53, Q7: on by default.
    pub success_sound: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            success_sound: true,
        }
    }
}

pub fn load(path: &Path) -> Settings {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, s: &Settings) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(s)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/settings.json");
        assert_eq!(load(&path), Settings::default());
        assert!(load(&path).success_sound);

        save(
            &path,
            &Settings {
                success_sound: false,
            },
        )
        .unwrap();
        assert!(!load(&path).success_sound);

        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(load(&path), Settings::default());
    }
}
