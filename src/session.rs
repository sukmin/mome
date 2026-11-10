//! Surviving Windows shutdown, and recovering after a crash.
//!
//! Every running window owns a *session*: a named mutex (alive while the process
//! lives) and a small JSON record in `%LOCALAPPDATA%\mome\sessions` describing the
//! open document. The record is refreshed whenever a revision is written.
//!
//! * **Windows shuts down** — winit does not report `WM_QUERYENDSESSION`, so the main
//!   window is subclassed. On that message we write a final revision and the record,
//!   and add a `RunOnce` registry entry `mome.exe --restore <id>`; Windows runs it
//!   once at the next sign-in and the window comes back as it was.
//! * **Crash or power loss** — the record stays behind while its mutex is gone. The
//!   next normal launch finds it and offers to recover the unsaved text.
//! * **Normal close** — the record and any `RunOnce` entry are removed.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegGetValueW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, OpenMutexW};
use windows_sys::Win32::UI::Shell::DefSubclassProc;
use windows_sys::Win32::UI::WindowsAndMessaging::{WM_ENDSESSION, WM_NCDESTROY, WM_QUERYENDSESSION};

use crate::encoding::{Encoding, LineEnding};
use crate::history::{self, DocInfo, RevisionKind, SnapshotJob};
use crate::paths;
use crate::settings::WindowPlacement;
use crate::time::LocalTime;
use crate::win::{self, wide};

const RUN_ONCE_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\RunOnce";
const SYNCHRONIZE: u32 = 0x0010_0000;
const SUBCLASS_ID: usize = 0x6d6f6d65; // "mome"

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SessionRecord {
    pub sid: String,
    pub doc_key: String,
    pub path: Option<PathBuf>,
    pub title: String,
    /// Encoding the document will be saved with.
    pub encoding: Encoding,
    /// Encoding the file on disk was read with (may differ after "인코딩 변경").
    pub file_encoding: Option<Encoding>,
    pub line_ending: LineEnding,
    /// The text differs from the file on disk (or there is no file).
    pub modified: bool,
    /// File holding the text when `modified`; otherwise the document is re-read from `path`.
    pub revision: Option<PathBuf>,
    /// Cursor position (character index).
    pub cursor: usize,
    /// Zoom in percent (0 = not recorded).
    pub zoom: u32,
    pub overwrite: bool,
    pub placement: Option<WindowPlacement>,
    /// Written while Windows was shutting down (as opposed to a crash). Only for
    /// reading the record when diagnosing a restore; mome does not act on it.
    pub ending_session: bool,
}

/// State shared between the UI and the shutdown hook.
#[derive(Default)]
pub struct Shared {
    pub record: SessionRecord,
    pub text: String,
    pub text_hash: u64,
    /// Hash of the text in `record.revision`, to avoid writing the same revision twice.
    pub revision_hash: Option<u64>,
    pub history_root: Option<PathBuf>,
    pub doc_info: DocInfo,
}

pub struct Session {
    sid: String,
    mutex: HANDLE,
    shared: Arc<Mutex<Shared>>,
    last_written: String,
    hooked: bool,
}

fn new_sid() -> String {
    format!("{}-{}", LocalTime::now().stamp(), std::process::id())
}

fn mutex_name(sid: &str) -> Vec<u16> {
    wide(&format!("Local\\mome-session-{sid}"))
}

fn record_path(sid: &str) -> PathBuf {
    paths::sessions_dir().join(format!("{sid}.json"))
}

/// Fallback location for the text when no temp folder has been chosen yet.
fn fallback_text_path(sid: &str) -> PathBuf {
    paths::sessions_dir().join(format!("{sid}.txt"))
}

fn run_once_name(sid: &str) -> Vec<u16> {
    wide(&format!("mome-{sid}"))
}

impl Session {
    /// Starts a new session with a fresh id.
    pub fn new() -> Self {
        let sid = new_sid();
        let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, mutex_name(&sid).as_ptr()) };
        Self::with(sid, mutex)
    }

    /// Takes over the id of a session being restored, so its record and `RunOnce`
    /// entry stay this window's. `None` if another process already owns it.
    pub fn resume(sid: &str) -> Option<Self> {
        let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, mutex_name(sid).as_ptr()) };
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe { CloseHandle(mutex) };
            return None;
        }
        Some(Self::with(sid.to_owned(), mutex))
    }

    fn with(sid: String, mutex: HANDLE) -> Self {
        let shared = Shared {
            record: SessionRecord {
                sid: sid.clone(),
                ..Default::default()
            },
            ..Default::default()
        };
        Self {
            sid,
            mutex,
            shared: Arc::new(Mutex::new(shared)),
            last_written: String::new(),
            hooked: false,
        }
    }

    pub fn sid(&self) -> &str {
        &self.sid
    }

    /// Runs `f` on the shared state. `f` must not show a dialog or otherwise pump
    /// window messages: the shutdown hook would find the state locked and fall back
    /// to the record written last.
    pub fn update<R>(&self, f: impl FnOnce(&mut Shared) -> R) -> R {
        let mut guard = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }

    /// Subclasses the main window to catch `WM_QUERYENDSESSION` / `WM_ENDSESSION`.
    pub fn install_shutdown_hook(&mut self, hwnd: HWND) {
        if self.hooked {
            return;
        }
        let data = HookData {
            shared: Arc::clone(&self.shared),
            sid: self.sid.clone(),
            prepared: Cell::new(false),
        };
        self.hooked = win::add_subclass(hwnd, Some(shutdown_hook_proc), SUBCLASS_ID, data);
        if !self.hooked {
            log::error!("could not subclass the window; Windows shutdown will not be detected");
        }
    }

    /// Writes the record to disk if it changed since the last write.
    pub fn write_record(&mut self) {
        let json = self.update(|s| serde_json::to_string_pretty(&s.record).unwrap_or_default());
        if json.is_empty() || json == self.last_written {
            return;
        }
        if let Err(err) = write_json(&self.sid, &json) {
            log::warn!("could not write session record: {err}");
            return;
        }
        self.last_written = json;
    }

    /// The window was closed by the user: nothing needs restoring.
    pub fn end_normally(&mut self) {
        delete_record(&self.sid);
        unregister_run_once(&self.sid);
        self.last_written.clear();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.mutex.is_null() {
            unsafe { CloseHandle(self.mutex) };
        }
    }
}

fn write_json(sid: &str, json: &str) -> std::io::Result<()> {
    let path = record_path(sid);
    std::fs::create_dir_all(paths::sessions_dir())?;
    let tmp = path.with_extension("json.tmp");
    crate::document::write_synced(&tmp, json.as_bytes())?;
    std::fs::rename(&tmp, &path)
}

pub fn load_record(sid: &str) -> Option<SessionRecord> {
    let text = std::fs::read_to_string(record_path(sid)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The revisions every session record points to (as [`crate::history::path_key`]):
/// text that windows, running or to be restored or recovered, still need.
pub fn revisions_in_use() -> std::collections::HashSet<String> {
    let Ok(entries) = std::fs::read_dir(paths::sessions_dir()) else {
        return Default::default();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let record = load_record(name.strip_suffix(".json")?)?;
            record.revision.map(|p| crate::history::path_key(&p))
        })
        .collect()
}

pub fn delete_record(sid: &str) {
    let _ = std::fs::remove_file(record_path(sid));
    let _ = std::fs::remove_file(fallback_text_path(sid));
}

fn is_alive(sid: &str) -> bool {
    let name = mutex_name(sid);
    let handle = unsafe { OpenMutexW(SYNCHRONIZE, 0, name.as_ptr()) };
    if handle.is_null() {
        false
    } else {
        unsafe { CloseHandle(handle) };
        true
    }
}

/// Records left behind by windows that are no longer running and are not
/// waiting for a `RunOnce` restore.
fn find_orphans(own_sid: &str) -> Vec<SessionRecord> {
    let Ok(entries) = std::fs::read_dir(paths::sessions_dir()) else {
        return Vec::new();
    };
    let mut orphans: Vec<SessionRecord> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let sid = name.strip_suffix(".json")?.to_owned();
            if sid == own_sid || is_alive(&sid) || run_once_exists(&sid) {
                return None;
            }
            match load_record(&sid) {
                Some(record) => Some(record),
                None => {
                    delete_record(&sid);
                    None
                }
            }
        })
        .collect();
    orphans.sort_by(|a, b| a.sid.cmp(&b.sid));
    orphans
}

/// Records left behind by windows that crashed or lost power and still have unsaved
/// text to offer. Records with nothing to recover are deleted.
pub fn recoverable_orphans(own_sid: &str) -> Vec<SessionRecord> {
    let mut recoverable = Vec::new();
    for record in find_orphans(own_sid) {
        let has_text = record.modified && record.revision.as_ref().is_some_and(|p| p.exists());
        if has_text {
            recoverable.push(record);
        } else {
            delete_record(&record.sid);
        }
    }
    recoverable
}

fn run_once_command(sid: &str) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\" --restore {sid}", exe.display()))
}

fn register_run_once(sid: &str) -> bool {
    let Some(command) = run_once_command(sid) else {
        return false;
    };
    let key_name = wide(RUN_ONCE_KEY);
    let value_name = run_once_name(sid);
    let data = wide(&command);
    let mut key: HKEY = std::ptr::null_mut();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key_name.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if status != 0 {
        log::error!("RegCreateKeyExW(RunOnce) failed: {status}");
        return false;
    }
    let data_bytes = (data.len() * 2) as u32; // including the terminating NUL
    let status = unsafe { RegSetValueExW(key, value_name.as_ptr(), 0, REG_SZ, data.as_ptr().cast(), data_bytes) };
    unsafe { RegCloseKey(key) };
    if status != 0 {
        log::error!("RegSetValueExW(RunOnce) failed: {status}");
        return false;
    }
    true
}

fn unregister_run_once(sid: &str) {
    let key_name = wide(RUN_ONCE_KEY);
    let value_name = run_once_name(sid);
    unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key_name.as_ptr(), value_name.as_ptr()) };
}

fn run_once_exists(sid: &str) -> bool {
    let key_name = wide(RUN_ONCE_KEY);
    let value_name = run_once_name(sid);
    let mut size = 0u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key_name.as_ptr(),
            value_name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    status == 0
}

/// What the shutdown hook needs; owned by the window subclass.
struct HookData {
    shared: Arc<Mutex<Shared>>,
    /// Kept outside the mutex so the restore can be registered even if it is busy.
    sid: String,
    /// The current shutdown has already been prepared for.
    prepared: Cell<bool>,
}

/// Saves everything needed to bring this window back after the next sign-in.
fn prepare_for_shutdown(hwnd: HWND, data: &HookData) {
    if data.prepared.replace(true) {
        return;
    }
    let sid = &data.sid;
    // `try_lock`: if the UI thread holds the lock (it is inside a modal dialog that
    // pumped this message), fall back to the record written by the last revision.
    match data.shared.try_lock() {
        Ok(mut s) => {
            // Moving a window does not always run a frame, so ask Windows directly.
            if let Some(placement) = crate::placement::current(hwnd) {
                s.record.placement = Some(placement);
            }
            if !s.record.modified {
                s.record.revision = None;
            } else if s.revision_hash != Some(s.text_hash) || !s.record.revision.as_ref().is_some_and(|p| p.exists()) {
                // (Also when the revision file has gone missing since it was written.)
                match save_text_for_restore(&s, sid) {
                    Ok(path) => {
                        s.record.revision = Some(path);
                        s.revision_hash = Some(s.text_hash);
                    }
                    Err(err) => log::error!("could not save text at shutdown: {err}"),
                }
            }
            s.record.ending_session = true;
            write_record_now(&s.record);
        }
        Err(_) => log::warn!("session state busy at shutdown; using the last saved record"),
    }
    if register_run_once(sid) {
        log::info!("Windows is ending the session; registered restore of {sid}");
    }
}

/// Writes the unsaved text as a revision, or next to the session record when no
/// temp folder has been chosen yet or it cannot be written to now (a drive that
/// was unplugged or is full).
fn save_text_for_restore(s: &Shared, sid: &str) -> std::io::Result<PathBuf> {
    if let Some(root) = s.history_root.clone() {
        let written = history::write_snapshot(&SnapshotJob {
            root,
            key: s.record.doc_key.clone(),
            info: s.doc_info.clone(),
            kind: RevisionKind::Session,
            time: LocalTime::now(),
            text: s.text.clone(),
        });
        match written {
            Ok(path) => return Ok(path),
            Err(err) => log::warn!("could not write a revision at shutdown ({err}); keeping the text with the session"),
        }
    }
    let path = fallback_text_path(sid);
    std::fs::create_dir_all(paths::sessions_dir())?;
    crate::document::write_file_safely(&path, s.text.replace('\n', "\r\n").as_bytes())?;
    Ok(path)
}

fn write_record_now(record: &SessionRecord) {
    let result = serde_json::to_string_pretty(record)
        .map_err(std::io::Error::other)
        .and_then(|json| write_json(&record.sid, &json));
    if let Err(err) = result {
        log::error!("could not write session record at shutdown: {err}");
    }
}

unsafe extern "system" fn shutdown_hook_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    if msg == WM_NCDESTROY {
        // SAFETY: `data` is ours (from `install_shutdown_hook`) and not used after this.
        unsafe { win::remove_subclass::<HookData>(hwnd, Some(shutdown_hook_proc), id, data) };
        return unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
    }
    // SAFETY: `data` points to our `HookData`, which lives until WM_NCDESTROY.
    let hook = unsafe { &*(data as *const HookData) };
    match msg {
        WM_QUERYENDSESSION => {
            prepare_for_shutdown(hwnd, hook);
            // Never block the shutdown; everything is already on disk.
            return 1;
        }
        WM_ENDSESSION => {
            if wparam == 0 {
                // Shutdown was cancelled: keep running as if nothing happened.
                hook.prepared.set(false);
                unregister_run_once(&hook.sid);
                if let Ok(mut s) = hook.shared.try_lock() {
                    s.record.ending_session = false;
                }
                log::info!("Windows shutdown was cancelled");
            } else {
                // Some forced shutdowns skip WM_QUERYENDSESSION.
                prepare_for_shutdown(hwnd, hook);
                log::info!("Windows session is ending");
                log::logger().flush();
            }
            return 0;
        }
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}
