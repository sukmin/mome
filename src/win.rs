//! Win32 glue: UTF-16 strings, the window handle, window subclasses, and starting
//! Explorer or another mome.

use std::path::Path;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::UI::Shell::{RemoveWindowSubclass, SetWindowSubclass, ShellExecuteW, SUBCLASSPROC};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible, SW_SHOWNORMAL,
};

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

/// `path` for the shell: with `\` only, and for a folder with one at the end.
fn shell_path(path: &Path, folder: bool) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt as _;
    let (slash, backslash) = (u16::from(b'/'), u16::from(b'\\'));
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().map(|c| if c == slash { backslash } else { c }).collect();
    if folder && wide.last() != Some(&backslash) {
        wide.push(backslash);
    }
    wide.push(0);
    wide
}

/// Opens a folder in Explorer. Asked to "open" `C:\노트\자료`, the shell runs a
/// `자료.bat` or `자료.exe` lying next to the folder instead of opening it; the
/// `\` at the end and the "explore" verb, which programs do not have, leave
/// nothing but the folder.
pub fn open_folder(path: &Path) {
    shell_execute("explore", &shell_path(path, true));
}

/// Opens a file with the program Windows has for it. Only for files that have
/// been found safe to hand over.
pub fn open_file(path: &Path) {
    shell_execute("open", &shell_path(path, false));
}

/// 웹 주소를 기본 브라우저로 연다. `https://`로 시작하는 주소만 연다(셸은 다른 글이면 프로그램도 실행한다).
pub fn open_url(url: &str) {
    if url.starts_with("https://") && !url.contains(char::is_whitespace) {
        shell_execute("open", &wide(url));
    } else {
        log::warn!("not opening {url:?}: not an https address");
    }
}

fn shell_execute(verb: &str, target: &[u16]) {
    let verb = wide(verb);
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

/// Opens Explorer on the folder holding `path`, with the file selected, without
/// opening the file itself.
pub fn show_in_folder(path: &Path) {
    use std::os::windows::process::CommandExt;
    // Explorer wants `/select,"path"` as one raw argument; paths cannot contain quotes.
    // It does not know `/` (as in a link to `sub/a.exe`) and would show another folder.
    let arg = format!("/select,\"{}\"", path.display().to_string().replace('/', "\\"));
    if let Err(err) = std::process::Command::new("explorer.exe").raw_arg(arg).spawn() {
        log::error!("could not start Explorer: {err}");
    }
}

/// 프로세스 `pid`의 본창이 화면에 보이는지(새로 띄운 mome의 창이 떴는지 알아본다).
/// 창 라이브러리가 본창보다 먼저 띄우는 작은 보조 창(22×22 픽셀쯤)은 세지 않는다.
pub fn has_main_window(pid: u32) -> bool {
    /// 본창으로 볼 가장 작은 크기(픽셀). mome 창은 이보다 작아지지 않는다.
    const MIN_SIZE: (i32, i32) = (200, 100);
    struct Find {
        pid: u32,
        found: bool,
    }
    unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> i32 {
        // SAFETY: `data`는 아래 `EnumWindows`에 넘긴 `Find`이고, 그 호출이 끝날 때까지 살아 있다.
        let find = unsafe { &mut *(data as *mut Find) };
        let mut owner = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
        if owner == find.pid && unsafe { IsWindowVisible(hwnd) } != 0 {
            let mut rect: RECT = unsafe { std::mem::zeroed() };
            unsafe { GetWindowRect(hwnd, &mut rect) };
            if rect.right - rect.left >= MIN_SIZE.0 && rect.bottom - rect.top >= MIN_SIZE.1 {
                find.found = true;
                return 0; // 찾았다: 그만 돈다.
            }
        }
        1
    }
    let mut find = Find { pid, found: false };
    unsafe { EnumWindows(Some(each), &mut find as *mut Find as LPARAM) };
    find.found
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_end_in_a_backslash_for_the_shell() {
        let text = |wide: Vec<u16>| String::from_utf16(&wide).unwrap();
        assert_eq!(text(shell_path(Path::new(r"C:\노트/자료"), true)), "C:\\노트\\자료\\\0");
        assert_eq!(text(shell_path(Path::new(r"C:\노트\자료\"), true)), "C:\\노트\\자료\\\0");
        assert_eq!(text(shell_path(Path::new("C:/노트/그림.png"), false)), "C:\\노트\\그림.png\0");
    }
}
