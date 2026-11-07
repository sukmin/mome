//! User settings, stored as JSON in `%APPDATA%\mome\settings.json`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths;

/// Held while one process reads, changes and writes the settings file.
struct SettingsLock(windows_sys::Win32::Foundation::HANDLE);

impl SettingsLock {
    /// Waits up to a few seconds; on failure the update goes ahead unlocked.
    fn acquire() -> Option<Self> {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
        let name = crate::win::wide("Local\\mome-settings");
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return None;
        }
        match unsafe { WaitForSingleObject(handle, 5000) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Some(Self(handle)),
            _ => {
                unsafe { CloseHandle(handle) };
                None
            }
        }
    }
}

impl Drop for SettingsLock {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::ReleaseMutex;
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

pub const DEFAULT_SNAPSHOT_SECS: u64 = 10;
pub const MIN_SNAPSHOT_SECS: u64 = 5;
pub const MAX_SNAPSHOT_SECS: u64 = 600;
pub const DEFAULT_RETENTION_DAYS: u32 = 30;
pub const MAX_RETENTION_DAYS: u32 = 30;
pub const MAX_RECENT_FILES: usize = 10;
/// Font sizes (in points) the font dialog offers and the settings accept.
pub const FONT_SIZES: std::ops::RangeInclusive<f32> = 6.0..=96.0;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FontChoice {
    /// Family name shown to the user, e.g. `Consolas`.
    pub family: String,
    /// Font file; `None` means "find it by family name".
    pub file: Option<PathBuf>,
    /// Face index inside a `.ttc` collection.
    pub index: u32,
    /// Size in points (egui points = pixels at 100 % scaling).
    pub size: f32,
}

impl Default for FontChoice {
    fn default() -> Self {
        Self {
            family: "Consolas".to_owned(),
            file: None,
            index: 0,
            size: 16.0,
        }
    }
}

/// Where the window was, as `GetWindowPlacement` reports it: the outer rectangle of the
/// restored (not maximized) window in physical pixels, which mean the same thing on
/// every monitor whatever its scaling.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct WindowPlacement {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub maximized: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PageSettings {
    /// Margins in millimetres: left, top, right, bottom.
    pub margins_mm: [f32; 4],
    pub header: bool,
    pub footer: bool,
}

impl Default for PageSettings {
    fn default() -> Self {
        // The same margins as Windows Notepad.
        Self {
            margins_mm: [20.0, 25.0, 20.0, 25.0],
            header: true,
            footer: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FindSettings {
    pub match_case: bool,
    pub whole_word: bool,
    pub wrap_around: bool,
}

impl Default for FindSettings {
    fn default() -> Self {
        Self {
            match_case: false,
            whole_word: false,
            wrap_around: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Folder for revisions. `None` until chosen on first launch.
    pub temp_dir: Option<PathBuf>,
    pub snapshot_interval_secs: u64,
    pub retention_days: u32,
    /// Also write named documents back to their own file periodically.
    pub autosave_original: bool,
    pub dark_mode: bool,
    pub word_wrap: bool,
    pub show_status_bar: bool,
    /// Mark zero-width characters, special spaces and the like in the text.
    pub show_invisible: bool,
    pub font: FontChoice,
    /// Named `placement` rather than `window`, which held egui points before; an old
    /// `window` entry is ignored.
    pub placement: Option<WindowPlacement>,
    pub recent_files: Vec<PathBuf>,
    pub page: PageSettings,
    pub find: FindSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            temp_dir: None,
            snapshot_interval_secs: DEFAULT_SNAPSHOT_SECS,
            retention_days: DEFAULT_RETENTION_DAYS,
            autosave_original: false,
            dark_mode: false,
            word_wrap: true,
            show_status_bar: true,
            show_invisible: true,
            font: FontChoice::default(),
            placement: None,
            recent_files: Vec::new(),
            page: PageSettings::default(),
            find: FindSettings::default(),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        Self::load_from(&paths::settings_file())
    }

    pub fn load_from(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        // Tolerate a BOM in case the file was edited by hand.
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
        match serde_json::from_str::<Settings>(text) {
            Ok(mut settings) => {
                settings.sanitize();
                settings
            }
            Err(err) => {
                // Defaults are used, and the next change writes them over the file:
                // keep a copy so the old settings can still be looked at.
                let backup = path.with_extension("json.bad");
                log::warn!(
                    "ignoring unreadable settings {}: {err}; copy kept as {}",
                    path.display(),
                    backup.display()
                );
                let _ = std::fs::copy(path, &backup);
                Self::default()
            }
        }
    }

    /// Applies `change` to the settings file as it is on disk now and saves it.
    ///
    /// Several mome windows (processes) share the file; writing back a whole stale
    /// copy would undo what another window changed in the meantime, so each window
    /// only writes the fields it actually changed.
    pub fn update_on_disk(change: impl Fn(&mut Settings)) {
        let path = paths::settings_file();
        // Windows closing together must not read and write the file over each other.
        let _lock = SettingsLock::acquire();
        let mut on_disk = Self::load_from(&path);
        change(&mut on_disk);
        if let Err(err) = on_disk.save_to(&path) {
            log::error!("failed to save settings: {err}");
        }
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        // Write next to the target, flush, and rename, so a crash or power cut never
        // leaves half a file. The process id keeps windows from sharing the file.
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        if let Err(err) = crate::document::write_synced(&tmp, json.as_bytes()) {
            let _ = std::fs::remove_file(&tmp);
            return Err(err);
        }
        std::fs::rename(&tmp, path)
    }

    fn sanitize(&mut self) {
        self.snapshot_interval_secs = self
            .snapshot_interval_secs
            .clamp(MIN_SNAPSHOT_SECS, MAX_SNAPSHOT_SECS);
        self.retention_days = self.retention_days.clamp(1, MAX_RETENTION_DAYS);
        if !FONT_SIZES.contains(&self.font.size) {
            self.font.size = FontChoice::default().size;
        }
        self.recent_files.truncate(MAX_RECENT_FILES);
        for m in &mut self.page.margins_mm {
            *m = m.clamp(0.0, 100.0);
        }
    }

    pub fn push_recent(&mut self, path: &Path) {
        self.recent_files
            .retain(|p| !p.as_os_str().eq_ignore_ascii_case(path.as_os_str()));
        self.recent_files.insert(0, path.to_path_buf());
        self.recent_files.truncate(MAX_RECENT_FILES);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let dir = std::env::temp_dir().join(format!("mome-settings-test-{}", std::process::id()));
        let path = dir.join("settings.json");
        let s = Settings {
            dark_mode: true,
            temp_dir: Some(PathBuf::from("D:\\temp")),
            ..Settings::default()
        };
        s.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path), s);

        // Unknown or missing fields fall back to defaults; a BOM is tolerated.
        std::fs::write(&path, "\u{FEFF}{\"dark_mode\": true, \"retention_days\": 999}").unwrap();
        let loaded = Settings::load_from(&path);
        assert!(loaded.dark_mode);
        assert_eq!(loaded.retention_days, MAX_RETENTION_DAYS);
        assert_eq!(loaded.snapshot_interval_secs, DEFAULT_SNAPSHOT_SECS);

        // The window position of older versions (in egui points) is dropped, nothing else.
        std::fs::write(
            &path,
            r#"{"temp_dir": "D:\\temp", "window": {"x": 10.0, "y": 20.0, "width": 900.0, "height": 640.0, "maximized": false}}"#,
        )
        .unwrap();
        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.temp_dir, Some(PathBuf::from("D:\\temp")));
        assert_eq!(loaded.placement, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recent_files_are_unique() {
        let mut s = Settings::default();
        s.push_recent(Path::new("C:\\a.txt"));
        s.push_recent(Path::new("C:\\b.txt"));
        s.push_recent(Path::new("c:\\A.TXT"));
        assert_eq!(s.recent_files.len(), 2);
        assert_eq!(s.recent_files[0], PathBuf::from("c:\\A.TXT"));
    }
}
