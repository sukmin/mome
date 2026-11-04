//! Page setup and printing.
//!
//! egui cannot print, so this goes through Win32: the standard print dialogs, and an
//! invisible RichEdit control that lays out and paginates the text on the printer
//! (`EM_FORMATRANGE`). RichEdit also handles Hangul font fallback and tab stops.

use std::ops::Range;

use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL, HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontW, DeleteDC, DeleteObject, GetDeviceCaps, GetTextExtentPoint32W, SelectObject,
    SetBkMode, SetTextAlign, SetTextColor, TextOutW, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
    DEFAULT_PITCH, FW_NORMAL, HDC, LOGPIXELSX, LOGPIXELSY, OUT_DEFAULT_PRECIS, PHYSICALHEIGHT,
    PHYSICALOFFSETX, PHYSICALOFFSETY, PHYSICALWIDTH, CLEARTYPE_QUALITY, TA_CENTER, TA_TOP,
    TRANSPARENT,
};
use windows_sys::Win32::Storage::Xps::{EndDoc, EndPage, StartDocW, StartPage, AbortDoc, DOCINFOW};
use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;
use windows_sys::Win32::UI::Controls::Dialogs::{
    CommDlgExtendedError, PageSetupDlgW, PrintDlgW, PAGESETUPDLGW, PD_NOSELECTION, PD_PAGENUMS,
    PD_RETURNDC, PD_SELECTION, PD_USEDEVMODECOPIESANDCOLLATE, PRINTDLGW,
    PSD_INHUNDREDTHSOFMILLIMETERS, PSD_MARGINS,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, SendMessageW, ES_MULTILINE, WM_SETTEXT, WM_USER, WS_CHILD,
};

use crate::settings::{FontChoice, PageSettings};
use crate::win::wide;

// RichEdit definitions (not part of windows-sys).
const EM_EXLIMITTEXT: u32 = WM_USER + 53;
const EM_FORMATRANGE: u32 = WM_USER + 57;
const EM_SETCHARFORMAT: u32 = WM_USER + 68;
const EM_SETTEXTMODE: u32 = WM_USER + 89;
const EM_SETLANGOPTIONS: u32 = WM_USER + 120;
const TM_PLAINTEXT: usize = 1;
const SCF_ALL: usize = 4;
const CFM_BOLD: u32 = 0x0000_0001;
const CFM_ITALIC: u32 = 0x0000_0002;
const CFM_FACE: u32 = 0x2000_0000;
const CFM_COLOR: u32 = 0x4000_0000;
const CFM_SIZE: u32 = 0x8000_0000;
const CFE_AUTOCOLOR: u32 = 0x4000_0000;
const IMF_AUTOFONT: usize = 0x0002;
const IMF_DUALFONT: usize = 0x0080;

#[repr(C)]
struct CharFormatW {
    cb_size: u32,
    dw_mask: u32,
    dw_effects: u32,
    y_height: i32,
    y_offset: i32,
    cr_text_color: u32,
    b_char_set: u8,
    b_pitch_and_family: u8,
    sz_face_name: [u16; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CharRange {
    cp_min: i32,
    cp_max: i32,
}

#[repr(C)]
struct FormatRange {
    hdc: HDC,
    hdc_target: HDC,
    rc: RECT,
    rc_page: RECT,
    chrg: CharRange,
}

/// Printer choice and paper settings remembered while the app runs.
pub struct PrintState {
    dev_mode: HGLOBAL,
    dev_names: HGLOBAL,
}

impl Default for PrintState {
    fn default() -> Self {
        Self {
            dev_mode: std::ptr::null_mut(),
            dev_names: std::ptr::null_mut(),
        }
    }
}

impl Drop for PrintState {
    fn drop(&mut self) {
        unsafe {
            if !self.dev_mode.is_null() {
                GlobalFree(self.dev_mode);
            }
            if !self.dev_names.is_null() {
                GlobalFree(self.dev_names);
            }
        }
    }
}

impl PrintState {
    fn update_handles(&mut self, dev_mode: HGLOBAL, dev_names: HGLOBAL) {
        unsafe {
            if dev_mode != self.dev_mode && !self.dev_mode.is_null() {
                GlobalFree(self.dev_mode);
            }
            if dev_names != self.dev_names && !self.dev_names.is_null() {
                GlobalFree(self.dev_names);
            }
        }
        self.dev_mode = dev_mode;
        self.dev_names = dev_names;
    }
}

/// Shows the page setup dialog. Returns true if the user pressed OK.
pub fn page_setup(owner: HWND, state: &mut PrintState, page: &mut PageSettings) -> bool {
    let mut psd: PAGESETUPDLGW = unsafe { std::mem::zeroed() };
    psd.lStructSize = std::mem::size_of::<PAGESETUPDLGW>() as u32;
    psd.hwndOwner = owner;
    psd.hDevMode = state.dev_mode;
    psd.hDevNames = state.dev_names;
    psd.Flags = PSD_MARGINS | PSD_INHUNDREDTHSOFMILLIMETERS;
    let [l, t, r, b] = page.margins_mm;
    psd.rtMargin = RECT {
        left: (l * 100.0) as i32,
        top: (t * 100.0) as i32,
        right: (r * 100.0) as i32,
        bottom: (b * 100.0) as i32,
    };
    let ok = unsafe { PageSetupDlgW(&mut psd) } != 0;
    state.update_handles(psd.hDevMode, psd.hDevNames);
    if ok {
        page.margins_mm = [
            psd.rtMargin.left as f32 / 100.0,
            psd.rtMargin.top as f32 / 100.0,
            psd.rtMargin.right as f32 / 100.0,
            psd.rtMargin.bottom as f32 / 100.0,
        ];
    }
    ok
}

pub struct PrintJob<'a> {
    pub title: &'a str,
    /// Editor text with `\n` line breaks.
    pub text: &'a str,
    /// Selected characters, if any.
    pub selection: Option<Range<usize>>,
    pub font: &'a FontChoice,
    pub page: &'a PageSettings,
}

pub enum PrintOutcome {
    Printed { pages: u32 },
    Cancelled,
}

/// Shows the print dialog and prints.
pub fn print(owner: HWND, state: &mut PrintState, job: &PrintJob) -> Result<PrintOutcome, String> {
    let has_selection = job.selection.as_ref().is_some_and(|r| !r.is_empty());
    let mut pd: PRINTDLGW = unsafe { std::mem::zeroed() };
    pd.lStructSize = std::mem::size_of::<PRINTDLGW>() as u32;
    pd.hwndOwner = owner;
    pd.hDevMode = state.dev_mode;
    pd.hDevNames = state.dev_names;
    pd.Flags = PD_RETURNDC
        | PD_USEDEVMODECOPIESANDCOLLATE
        | if has_selection { 0 } else { PD_NOSELECTION };
    pd.nFromPage = 1;
    pd.nToPage = 1;
    pd.nMinPage = 1;
    pd.nMaxPage = 0xFFFF;
    pd.nCopies = 1;
    let ok = unsafe { PrintDlgW(&mut pd) } != 0;
    state.update_handles(pd.hDevMode, pd.hDevNames);
    if !ok {
        let err = unsafe { CommDlgExtendedError() };
        return if err == 0 {
            Ok(PrintOutcome::Cancelled)
        } else {
            Err(format!("인쇄 대화상자를 열 수 없습니다. (오류 코드: {err:#x})"))
        };
    }
    let hdc = pd.hDC;
    if hdc.is_null() {
        return Err("프린터에 연결할 수 없습니다.".to_owned());
    }

    let total = job.text.chars().count();
    let chars = match &job.selection {
        Some(selection) if pd.Flags & PD_SELECTION != 0 => selection.start.min(total)..selection.end.min(total),
        _ => 0..total,
    };
    let page_range = (pd.Flags & PD_PAGENUMS != 0).then_some((pd.nFromPage as u32, pd.nToPage as u32));
    let copies = pd.nCopies.max(1) as u32;

    // SAFETY: `hdc` is the printer DC the dialog just created; it is deleted below.
    let result = unsafe { print_to_dc(owner, hdc, job, chars, page_range, copies) };
    unsafe { DeleteDC(hdc) };
    result.map(|pages| PrintOutcome::Printed { pages })
}

struct PageGeometry {
    /// Page rectangle and text area in twips, relative to the printable origin.
    page: RECT,
    body: RECT,
    /// Device pixels.
    width_px: i32,
    height_px: i32,
    offset_x: i32,
    offset_y: i32,
    dpi_y: i32,
    margin_top_px: i32,
    margin_bottom_px: i32,
}

/// # Safety
/// `hdc` must be a valid printer device context.
unsafe fn page_geometry(hdc: HDC, page: &PageSettings) -> PageGeometry {
    let dpi_x = unsafe { GetDeviceCaps(hdc, LOGPIXELSX as _) }.max(1);
    let dpi_y = unsafe { GetDeviceCaps(hdc, LOGPIXELSY as _) }.max(1);
    let width_px = unsafe { GetDeviceCaps(hdc, PHYSICALWIDTH as _) };
    let height_px = unsafe { GetDeviceCaps(hdc, PHYSICALHEIGHT as _) };
    let offset_x = unsafe { GetDeviceCaps(hdc, PHYSICALOFFSETX as _) };
    let offset_y = unsafe { GetDeviceCaps(hdc, PHYSICALOFFSETY as _) };
    let tw_x = |px: i32| (px as i64 * 1440 / dpi_x as i64) as i32;
    let tw_y = |px: i32| (px as i64 * 1440 / dpi_y as i64) as i32;
    let mm_to_px = |mm: f32, dpi: i32| (mm / 25.4 * dpi as f32).round() as i32;
    let [l, t, r, b] = page.margins_mm;
    let (ml, mt, mr, mb) = (
        mm_to_px(l, dpi_x),
        mm_to_px(t, dpi_y),
        mm_to_px(r, dpi_x),
        mm_to_px(b, dpi_y),
    );
    let left = (ml - offset_x).max(0);
    let top = (mt - offset_y).max(0);
    let right = (width_px - mr - offset_x).max(left + dpi_x);
    let bottom = (height_px - mb - offset_y).max(top + dpi_y);
    PageGeometry {
        page: RECT {
            left: 0,
            top: 0,
            right: tw_x(width_px),
            bottom: tw_y(height_px),
        },
        body: RECT {
            left: tw_x(left),
            top: tw_y(top),
            right: tw_x(right),
            bottom: tw_y(bottom),
        },
        width_px,
        height_px,
        offset_x,
        offset_y,
        dpi_y,
        margin_top_px: mt,
        margin_bottom_px: mb,
    }
}

/// Creates the hidden RichEdit holding the characters `chars` of the text. Returns it
/// with the text length in UTF-16 units (RichEdit positions are UTF-16, not `char`s).
///
/// # Safety
/// `owner` must be a valid window; the caller destroys the returned control.
unsafe fn create_print_edit(owner: HWND, job: &PrintJob, chars: Range<usize>) -> Result<(HWND, i32), String> {
    let lib = wide("Msftedit.dll");
    if unsafe { LoadLibraryW(lib.as_ptr()) }.is_null() {
        return Err("인쇄 구성 요소(Msftedit.dll)를 불러올 수 없습니다.".to_owned());
    }
    let class = wide("RICHEDIT50W");
    let empty = wide("");
    let edit = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            empty.as_ptr(),
            WS_CHILD | ES_MULTILINE as u32,
            0,
            0,
            0,
            0,
            owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if edit.is_null() {
        return Err("인쇄용 편집 컨트롤을 만들 수 없습니다.".to_owned());
    }
    unsafe {
        SendMessageW(edit, EM_SETTEXTMODE, TM_PLAINTEXT, 0);
        SendMessageW(edit, EM_EXLIMITTEXT, 0, isize::MAX);
        SendMessageW(edit, EM_SETLANGOPTIONS, 0, (IMF_AUTOFONT | IMF_DUALFONT) as isize);
    }

    // RichEdit wants CR paragraph breaks.
    let body: String = job.text.chars().skip(chars.start).take(chars.len()).collect();
    let body = wide(&body.replace('\n', "\r"));
    let len = (body.len() - 1) as i32; // without the terminating NUL
    unsafe { SendMessageW(edit, WM_SETTEXT, 0, body.as_ptr() as isize) };

    let mut cf: CharFormatW = unsafe { std::mem::zeroed() };
    cf.cb_size = std::mem::size_of::<CharFormatW>() as u32;
    cf.dw_mask = CFM_FACE | CFM_SIZE | CFM_COLOR | CFM_BOLD | CFM_ITALIC;
    cf.dw_effects = CFE_AUTOCOLOR; // Black on paper, whatever the screen theme.
    // Editor sizes are in egui points (1/96 inch); RichEdit wants twips (1/1440 inch).
    cf.y_height = (job.font.size * 15.0).round() as i32;
    let face: Vec<u16> = job.font.family.encode_utf16().take(31).collect();
    cf.sz_face_name[..face.len()].copy_from_slice(&face);
    unsafe { SendMessageW(edit, EM_SETCHARFORMAT, SCF_ALL, &cf as *const _ as isize) };
    Ok((edit, len))
}

/// Prints header (document title) and footer (page number) in the margins.
///
/// # Safety
/// `hdc` must be a valid printer device context inside a page (`StartPage`).
unsafe fn draw_header_footer(hdc: HDC, geo: &PageGeometry, job: &PrintJob, page: u32) {
    /// Dark grey, as a COLORREF (0x00BBGGRR).
    const HEADER_COLOR: u32 = 0x0060_6060;
    let height = -(geo.dpi_y * 9 / 72);
    let face = wide("Malgun Gothic");
    let font = unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            DEFAULT_PITCH as u32,
            face.as_ptr(),
        )
    };
    let old = unsafe { SelectObject(hdc, font) };
    unsafe {
        SetBkMode(hdc, TRANSPARENT as i32);
        SetTextColor(hdc, HEADER_COLOR);
        SetTextAlign(hdc, TA_CENTER | TA_TOP);
    }
    let center_x = geo.width_px / 2 - geo.offset_x;
    let draw = |text: &str, y: i32| {
        let w: Vec<u16> = text.encode_utf16().collect();
        let mut size = windows_sys::Win32::Foundation::SIZE { cx: 0, cy: 0 };
        unsafe {
            GetTextExtentPoint32W(hdc, w.as_ptr(), w.len() as i32, &mut size);
            TextOutW(hdc, center_x, y - size.cy / 2, w.as_ptr(), w.len() as i32);
        }
    };
    if job.page.header && geo.margin_top_px > 0 {
        draw(job.title, geo.margin_top_px / 2 - geo.offset_y);
    }
    if job.page.footer && geo.margin_bottom_px > 0 {
        draw(&format!("- {page} -"), geo.height_px - geo.margin_bottom_px / 2 - geo.offset_y);
    }
    unsafe {
        SelectObject(hdc, old);
        DeleteObject(font);
    }
}

/// Prints the characters `chars` of the text; returns the number of pages printed.
///
/// # Safety
/// `owner` must be a valid window and `hdc` a valid printer device context.
unsafe fn print_to_dc(
    owner: HWND,
    hdc: HDC,
    job: &PrintJob,
    chars: Range<usize>,
    page_range: Option<(u32, u32)>,
    copies: u32,
) -> Result<u32, String> {
    let (edit, len) = unsafe { create_print_edit(owner, job, chars)? };
    let geo = unsafe { page_geometry(hdc, job.page) };

    let title = wide(job.title);
    let doc_info = DOCINFOW {
        cbSize: std::mem::size_of::<DOCINFOW>() as i32,
        lpszDocName: title.as_ptr(),
        lpszOutput: std::ptr::null(),
        lpszDatatype: std::ptr::null(),
        fwType: 0,
    };
    if unsafe { StartDocW(hdc, &doc_info) } <= 0 {
        unsafe { DestroyWindow(edit) };
        return Err("인쇄 작업을 시작할 수 없습니다.".to_owned());
    }

    let mut printed = 0;
    let mut failed = false;
    'copies: for _ in 0..copies {
        let mut cp = 0i32;
        let mut page = 1u32;
        loop {
            let wanted = page_range.is_none_or(|(from, to)| page >= from && page <= to);
            if wanted && unsafe { StartPage(hdc) } <= 0 {
                failed = true;
                break 'copies;
            }
            let mut fr = FormatRange {
                hdc,
                hdc_target: hdc,
                rc: geo.body,
                rc_page: geo.page,
                chrg: CharRange {
                    cp_min: cp,
                    cp_max: len,
                },
            };
            let next = unsafe { SendMessageW(edit, EM_FORMATRANGE, wanted as usize, &mut fr as *mut _ as isize) } as i32;
            if wanted {
                unsafe { draw_header_footer(hdc, &geo, job, page) };
                if unsafe { EndPage(hdc) } <= 0 {
                    failed = true;
                    break 'copies;
                }
                printed += 1;
            }
            if next <= cp || next >= len {
                break;
            }
            cp = next;
            page += 1;
            if page_range.is_some_and(|(_, to)| page > to) {
                break;
            }
        }
        unsafe { SendMessageW(edit, EM_FORMATRANGE, 0, 0) };
    }

    unsafe {
        if failed {
            AbortDoc(hdc);
        } else {
            EndDoc(hdc);
        }
        DestroyWindow(edit);
    }
    if failed {
        Err("인쇄 중 오류가 발생했습니다.".to_owned())
    } else {
        Ok(printed)
    }
}
