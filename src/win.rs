//! Win32 glue: UTF-16 strings, the window handle, window subclasses, and starting
//! Explorer or another mome.

use std::path::Path;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::{RemoveWindowSubclass, SetWindowSubclass, ShellExecuteW, SUBCLASSPROC};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// Null-terminated UTF-16.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn wide_path(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt as _;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

pub fn hwnd_of(window: &impl HasWindowHandle) -> Option<HWND> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
        _ => None,
    }
}

/// Puts `proc` in front of the window procedure of `hwnd` (winit's), to see messages
/// winit does not report. `proc` gets a pointer to `data` as its last argument and
/// must call [`remove_subclass`] with the same `T` on `WM_NCDESTROY`.
/// Returns false if the subclass could not be installed.
pub fn add_subclass<T>(hwnd: HWND, proc: SUBCLASSPROC, id: usize, data: T) -> bool {
    if hwnd.is_null() {
        return false;
    }
    let data = Box::into_raw(Box::new(data));
    if unsafe { SetWindowSubclass(hwnd, proc, id, data as usize) } == 0 {
        // SAFETY: not handed out; this is the only owner.
        drop(unsafe { Box::from_raw(data) });
        return false;
    }
    true
}

/// Undoes [`add_subclass`]. Call it from the subclass proc on `WM_NCDESTROY`.
///
/// # Safety
/// `data` must be the pointer the proc received, from `add_subclass::<T>`; the proc
/// must not use it afterwards.
pub unsafe fn remove_subclass<T>(hwnd: HWND, proc: SUBCLASSPROC, id: usize, data: usize) {
    unsafe {
        RemoveWindowSubclass(hwnd, proc, id);
        drop(Box::from_raw(data as *mut T));
    }
}

/// Opens a folder in Explorer.
pub fn open_folder(path: &Path) {
    let verb = wide("open");
    let target = wide_path(path);
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// Starts another mome window.
pub fn spawn_self(args: &[&std::ffi::OsStr]) {
    match std::env::current_exe() {
        Ok(exe) => {
            if let Err(err) = std::process::Command::new(exe).args(args).spawn() {
                log::error!("could not start a new window: {err}");
            }
        }
        Err(err) => log::error!("could not find our own executable: {err}"),
    }
}
