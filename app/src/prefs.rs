//! Remembered choices between launches: your name, avatar, last room settings and
//! whether you publish a group source. Saved as a small JSON file in
//! %APPDATA%\VTubeCollab\settings.json.

use std::path::PathBuf;

use collab::RoomSettings;
use serde::{Deserialize, Serialize};

/// `#[serde(default)]` = anything missing from an older/edited file gets its default.
#[derive(Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Prefs {
    pub name: String,
    pub source: Option<String>,
    pub room_settings: RoomSettings,
    pub group_enabled: bool,
}

fn path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("VTubeCollab").join("settings.json")
}

/// Load the saved choices (defaults if there's no file or it can't be read).
pub fn load() -> Prefs {
    std::fs::read_to_string(path())
        .ok()
        // Editors like Notepad may put an invisible marker (BOM) at the start; skip it.
        .and_then(|text| serde_json::from_str(text.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_default()
}

/// Save the choices. Failing to save isn't worth bothering the user about.
pub fn save(prefs: &Prefs) {
    let file = path();
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(prefs) {
        let _ = std::fs::write(file, text);
    }
}
