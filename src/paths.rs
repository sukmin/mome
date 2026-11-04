//! Well-known locations used by mome.

use std::path::PathBuf;

fn env_dir(var: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(std::env::temp_dir)
}

/// `MOME_DATA_DIR` puts all state in one folder (used for testing).
fn override_dir() -> Option<PathBuf> {
    std::env::var_os("MOME_DATA_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// `%APPDATA%\mome` — settings that follow the user.
pub fn roaming_dir() -> PathBuf {
    override_dir().unwrap_or_else(|| env_dir("APPDATA").join("mome"))
}

/// `%LOCALAPPDATA%\mome` — machine-local state (sessions, log).
pub fn local_dir() -> PathBuf {
    override_dir().unwrap_or_else(|| env_dir("LOCALAPPDATA").join("mome"))
}

pub fn settings_file() -> PathBuf {
    roaming_dir().join("settings.json")
}

pub fn sessions_dir() -> PathBuf {
    local_dir().join("sessions")
}

pub fn log_file() -> PathBuf {
    local_dir().join("mome.log")
}

/// Suggested folder for revisions on first launch.
///
/// Not `%TEMP%`: Storage Sense may empty it long before the 30-day retention ends.
pub fn default_temp_dir() -> PathBuf {
    local_dir().join("temp")
}

pub fn windows_fonts_dir() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\Windows"))
        .join("Fonts")
}
