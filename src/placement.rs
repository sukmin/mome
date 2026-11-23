//! The window's position and size, on systems with one or more monitors.
//!
//! * **Remembering where the window was** — positions are kept as Windows reports
//!   them (`GetWindowPlacement`, physical pixels), which mean the same thing on every
//!   monitor. egui points do not: their size depends on the monitor's scaling.
//! * **Moving onto a monitor with other scaling** — Windows sends `WM_DPICHANGED`
//!   with the rectangle the window should take there. winit 0.30 replaces it with one
//!   of its own and, while the window is being dragged, may nudge it back onto the
//!   monitor it is leaving. That switches the scaling back, the next mouse move
//!   switches it again, and the window jumps, flickers and grows
//!   (rust-windowing/winit#4600, emilk/egui#7648). A window subclass makes winit use
//!   Windows' rectangle, as the `WM_DPICHANGED` documentation asks.

use std::cell::Cell;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows_sys::Win32::UI::Shell::DefSubclassProc;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowPlacement, GetWindowRect, IsIconic, IsWindowVisible, IsZoomed, SetWindowPlacement, SetWindowPos,
    ShowWindow, SHOW_WINDOW_CMD, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_MAXIMIZE,
    SW_SHOWNA, WINDOWPLACEMENT, WINDOWPOS, WM_DPICHANGED, WM_NCDESTROY, WM_WINDOWPOSCHANGING,
    WPF_RESTORETOMAXIMIZED,
};

use crate::settings::WindowPlacement;
use crate::win;

const SUBCLASS_ID: usize = 0x6d6f_6470; // "modp"

/// How far a restored window may stick out of its monitor's work area. Windows'
/// frames have invisible resize borders (about 11 px at 150 % scaling) outside the
/// visible edge, so a window placed exactly at a screen edge sticks out a little.
const EDGE_SLACK: i32 = 32;

// ----- remembering the position --------------------------------------------------

fn rect_of(p: &WindowPlacement) -> RECT {
    RECT {
        left: p.left,
        top: p.top,
        right: p.right,
        bottom: p.bottom,
    }
}

fn read_placement(hwnd: HWND) -> Option<WINDOWPLACEMENT> {
    if hwnd.is_null() {
        return None;
    }
    let mut wp: WINDOWPLACEMENT = unsafe { std::mem::zeroed() };
    wp.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
    (unsafe { GetWindowPlacement(hwnd, &mut wp) } != 0).then_some(wp)
}

fn write_placement(hwnd: HWND, p: &WindowPlacement, show: SHOW_WINDOW_CMD) {
    let Some(mut wp) = read_placement(hwnd) else {
        return;
    };
    wp.flags = 0;
    wp.showCmd = show as u32;
    wp.rcNormalPosition = rect_of(p);
    unsafe { SetWindowPlacement(hwnd, &wp) };
}

/// Where the window is; while it is maximized or minimized, where it returns to.
pub fn current(hwnd: HWND) -> Option<WindowPlacement> {
    let wp = read_placement(hwnd)?;
    let r = wp.rcNormalPosition;
    if r.right <= r.left || r.bottom <= r.top {
        return None;
    }
    let maximized = unsafe {
        if IsIconic(hwnd) != 0 {
            wp.flags & WPF_RESTORETOMAXIMIZED != 0
        } else {
            IsZoomed(hwnd) != 0
        }
    };
    Some(WindowPlacement {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
        maximized,
    })
}

/// Rejects saved placements too small to be a real window (e.g. a damaged file).
/// A window whose monitor is gone is fine: [`restore`] brings it back on screen.
pub fn is_usable(p: &WindowPlacement) -> bool {
    p.right - p.left >= 100 && p.bottom - p.top >= 80
}

pub fn is_visible(hwnd: HWND) -> bool {
    unsafe { IsWindowVisible(hwnd) != 0 }
}

/// Puts the window back where it was. eframe keeps a new window hidden until its
/// first frame is painted; call this before that. Returns where the (restored, not
/// maximized) window ended up, for [`check_after_show`].
pub fn restore(hwnd: HWND, p: &WindowPlacement) -> Option<WindowPlacement> {
    if hwnd.is_null() {
        return None;
    }
    // The first call moves the window onto its monitor, where it is rescaled if that
    // monitor's scaling differs; the second sets the exact size there. A window
    // entirely off screen is moved onto the nearest monitor by Windows.
    write_placement(hwnd, p, SW_HIDE);
    write_placement(hwnd, p, SW_HIDE);
    fit_to_work_area(hwnd);
    let placed = current(hwnd);
    if p.maximized {
        // What winit does for a window created maximized: maximize it on its
        // monitor, and keep it hidden until eframe shows it.
        unsafe {
            ShowWindow(hwnd, SW_MAXIMIZE);
            ShowWindow(hwnd, SW_HIDE);
        }
    }
    placed
}

/// Windows may rescale a window only when it is first shown, if its monitor's
/// scaling differs from the one it was created on. Call once the window is visible;
/// puts it back where [`restore`] placed it if that happened.
pub fn check_after_show(hwnd: HWND, placed: &WindowPlacement) {
    let Some(now) = current(hwnd) else {
        return;
    };
    if now.maximized || now == *placed {
        return;
    }
    log::info!("window moved when shown ({now:?}); putting it back at {placed:?}");
    write_placement(hwnd, placed, SW_SHOWNA);
}

/// Shrinks and moves the window into the work area of the monitor it is (mostly)
/// on, e.g. when that monitor's resolution was lowered since the window was there.
fn fit_to_work_area(hwnd: HWND) {
    let mut r: RECT = unsafe { std::mem::zeroed() };
    if unsafe { GetWindowRect(hwnd, &mut r) } == 0 {
        return;
    }
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    let monitor = unsafe { MonitorFromRect(&r, MONITOR_DEFAULTTONEAREST) };
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return;
    }
    let area = RECT {
        left: info.rcWork.left - EDGE_SLACK,
        top: info.rcWork.top - EDGE_SLACK,
        right: info.rcWork.right + EDGE_SLACK,
        bottom: info.rcWork.bottom + EDGE_SLACK,
    };
    let width = (r.right - r.left).min(area.right - area.left);
    let height = (r.bottom - r.top).min(area.bottom - area.top);
    let left = r.left.clamp(area.left, area.right - width);
    let top = r.top.clamp(area.top, area.bottom - height);
    if (left, top, width, height) != (r.left, r.top, r.right - r.left, r.bottom - r.top) {
        unsafe { SetWindowPos(hwnd, std::ptr::null_mut(), left, top, width, height, SWP_NOZORDER | SWP_NOACTIVATE) };
    }
}

// ----- 새 창이 뜰 자리 -------------------------------------------------------------

/// 새 창을 띄운 창에서 비켜 놓는 거리(96 DPI에서의 픽셀): 제목 표시줄 높이쯤.
const CASCADE_STEP: i32 = 32;

/// `here`에 있는 창이 띄울 새 창의 자리: 오른쪽 아래로 조금 비켜난 곳. 같은 자리에 뜨면 새 창이 이 창을
/// 그대로 가려서, 새로 열린 것인지 내용이 바뀐 것인지 알 수 없다.
pub fn cascaded(hwnd: HWND, here: &WindowPlacement) -> WindowPlacement {
    let dpi = if hwnd.is_null() { 0 } else { unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForWindow(hwnd) } };
    let step = CASCADE_STEP * if dpi == 0 { 96 } else { dpi as i32 } / 96;
    let rect = rect_of(here);
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    let monitor = unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST) };
    let work = (unsafe { GetMonitorInfoW(monitor, &mut info) } != 0).then_some(info.rcWork);
    cascade(here, step, work.as_ref())
}

/// `here`를 `step`만큼 오른쪽 아래로 옮긴다. 모니터의 작업 영역(`work`)을 벗어나면 그 왼쪽 위 구석으로 돌아간다.
fn cascade(here: &WindowPlacement, step: i32, work: Option<&RECT>) -> WindowPlacement {
    let (width, height) = (here.right - here.left, here.bottom - here.top);
    let (mut left, mut top) = (here.left + step, here.top + step);
    if let Some(work) = work {
        if left + width > work.right + EDGE_SLACK || top + height > work.bottom + EDGE_SLACK {
            (left, top) = (work.left, work.top);
            // 이 창이 바로 그 구석에 있으면 겹치지 않게 한 칸 더 간다.
            if (left - here.left).abs() < step / 2 && (top - here.top).abs() < step / 2 {
                (left, top) = (left + step, top + step);
            }
        }
    }
    WindowPlacement {
        left,
        top,
        right: left + width,
        bottom: top + height,
        maximized: here.maximized,
    }
}

/// 명령줄(`--placement`)로 넘기는 글: `왼쪽,위,오른쪽,아래,최대화(0/1)`.
pub fn to_arg(p: &WindowPlacement) -> String {
    format!("{},{},{},{},{}", p.left, p.top, p.right, p.bottom, u8::from(p.maximized))
}

/// [`to_arg`]의 반대. 형식이 다르면 None.
pub fn from_arg(text: &str) -> Option<WindowPlacement> {
    let parts: Vec<i32> = text.split(',').map(|part| part.trim().parse().ok()).collect::<Option<_>>()?;
    match parts[..] {
        [left, top, right, bottom, maximized] => Some(WindowPlacement {
            left,
            top,
            right,
            bottom,
            maximized: maximized != 0,
        }),
        _ => None,
    }
}

// ----- moving onto a monitor with other scaling ----------------------------------------

/// State of the subclass from [`install_dpi_fix`].
#[derive(Default)]
struct DpiFix {
    /// The rectangle Windows suggested with the `WM_DPICHANGED` being handled.
    suggested: Cell<Option<RECT>>,
}

/// Makes the window take the size and position Windows asks for when its scaling
/// changes (see the module docs).
pub fn install_dpi_fix(hwnd: HWND) {
    if !win::add_subclass(hwnd, Some(dpi_fix_proc), SUBCLASS_ID, DpiFix::default()) {
        log::warn!("could not subclass the window; moving between monitors may misbehave");
    }
}

unsafe extern "system" fn dpi_fix_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    if msg == WM_NCDESTROY {
        // SAFETY: `data` is ours (from `install_dpi_fix`) and not used after this.
        unsafe { win::remove_subclass::<DpiFix>(hwnd, Some(dpi_fix_proc), id, data) };
        return unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
    }
    // SAFETY: `data` points to our `DpiFix`, which lives until WM_NCDESTROY.
    let fix = unsafe { &*(data as *const DpiFix) };
    match msg {
        // A maximized window is sized by Windows itself; winit leaves it alone too.
        WM_DPICHANGED if lparam != 0 && unsafe { IsZoomed(hwnd) == 0 && IsIconic(hwnd) == 0 } => {
            // winit takes the new scale factor from this message and then moves the
            // window with SetWindowPos; WM_WINDOWPOSCHANGING below swaps its rectangle
            // for the one Windows suggested.
            // SAFETY: for WM_DPICHANGED, `lparam` points to the suggested RECT.
            fix.suggested.set(Some(unsafe { *(lparam as *const RECT) }));
            let result = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
            fix.suggested.set(None);
            return result;
        }
        WM_WINDOWPOSCHANGING if lparam != 0 => {
            // SAFETY: for WM_WINDOWPOSCHANGING, `lparam` points to a WINDOWPOS that
            // the handler may change.
            let pos = unsafe { &mut *(lparam as *mut WINDOWPOS) };
            if pos.flags & (SWP_NOMOVE | SWP_NOSIZE) == 0 {
                if let Some(r) = fix.suggested.take() {
                    pos.x = r.left;
                    pos.y = r.top;
                    pos.cx = r.right - r.left;
                    pos.cy = r.bottom - r.top;
                }
            }
        }
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// Debug builds only: pretends the window is dragged onto monitors with these DPIs, one
/// every 1.5 s, the way Windows reports it, and logs where the window ends up.
#[cfg(debug_assertions)]
pub fn simulate_drags(hwnd: HWND, dpis: Vec<u32>) {
    use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;

    // Sent from another thread, so the messages arrive through winit's message loop
    // as real ones do, not in the middle of a frame.
    let hwnd = hwnd as usize;
    std::thread::spawn(move || {
        let hwnd = hwnd as HWND;
        let mut from = unsafe { GetDpiForWindow(hwnd) }.max(1);
        for to in dpis {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            simulate_drag(hwnd, from, to);
            from = to;
        }
    });
}

#[cfg(debug_assertions)]
fn simulate_drag(hwnd: HWND, from_dpi: u32, dpi: u32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE};

    let mut before: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetWindowRect(hwnd, &mut before) };
    // Windows scales the whole window rectangle, keeping its top-left corner.
    let scale = |v: i32| (v as i64 * dpi as i64 / from_dpi as i64) as i32;
    let suggested = RECT {
        left: before.left,
        top: before.top,
        right: before.left + scale(before.right - before.left),
        bottom: before.top + scale(before.bottom - before.top),
    };
    let dpis = ((dpi << 16) | dpi) as WPARAM;
    unsafe {
        SendMessageW(hwnd, WM_ENTERSIZEMOVE, 0, 0);
        SendMessageW(hwnd, WM_DPICHANGED, dpis, &suggested as *const RECT as LPARAM);
        SendMessageW(hwnd, WM_EXITSIZEMOVE, 0, 0);
    }
    let mut after: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetWindowRect(hwnd, &mut after) };
    let show = |r: &RECT| format!("{},{},{},{}", r.left, r.top, r.right, r.bottom);
    log::info!(
        "test dpi {from_dpi}->{dpi}: before {} suggested {} after {}",
        show(&before),
        show(&suggested),
        show(&after)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(left: i32, top: i32, right: i32, bottom: i32) -> WindowPlacement {
        WindowPlacement { left, top, right, bottom, maximized: false }
    }

    #[test]
    fn 새_창은_조금_비켜난_자리에_뜬다() {
        let work = RECT { left: 0, top: 0, right: 3840, bottom: 2100 };
        // 크기는 그대로, 오른쪽 아래로 한 칸.
        assert_eq!(cascade(&at(100, 100, 1000, 800), 48, Some(&work)), at(148, 148, 1048, 848));
        // 작업 영역을 벗어나면 왼쪽 위 구석으로 돌아간다.
        assert_eq!(cascade(&at(2900, 1400, 3800, 2100), 48, Some(&work)), at(0, 0, 900, 700));
        // 이미 그 구석에 있는 큰 창은 한 칸 비켜 놓는다(가장자리 밖으로 조금 나가도 된다).
        assert_eq!(cascade(&at(0, 0, 3840, 2100), 48, Some(&work)), at(48, 48, 3888, 2148));
        // 모니터를 알 수 없으면 그냥 옮긴다. 최대화 여부는 물려받는다.
        let maximized = WindowPlacement { maximized: true, ..at(-11, -11, 900, 700) };
        assert_eq!(cascade(&maximized, 32, None), WindowPlacement { maximized: true, ..at(21, 21, 932, 732) });
    }

    #[test]
    fn 창의_자리를_명령줄로_주고받는다() {
        let p = WindowPlacement { maximized: true, ..at(-11, -1200, 900, 700) };
        assert_eq!(to_arg(&p), "-11,-1200,900,700,1");
        assert_eq!(from_arg(&to_arg(&p)), Some(p));
        assert_eq!(from_arg("1,2,3,4,0"), Some(at(1, 2, 3, 4)));
        for bad in ["", "1,2,3,4", "1,2,3,4,5,6", "a,b,c,d,e", "1,2,,4,0"] {
            assert_eq!(from_arg(bad), None, "{bad}");
        }
    }
}
