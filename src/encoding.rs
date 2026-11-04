//! Text encodings and line endings: detection on load, conversion on save.
//!
//! The editor always works on a `String` whose line breaks are plain `\n`;
//! the original encoding and line-ending style are remembered separately and
//! applied again when saving.

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Globalization::{
    GetACP, MultiByteToWideChar, WideCharToMultiByte, CP_ACP, MB_ERR_INVALID_CHARS,
    WC_NO_BEST_FIT_CHARS,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Encoding {
    #[default]
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    /// The system ANSI code page (CP949 on Korean Windows).
    Ansi,
}

impl Encoding {
    pub const ALL: [Encoding; 5] = [
        Encoding::Utf8,
        Encoding::Utf8Bom,
        Encoding::Utf16Le,
        Encoding::Utf16Be,
        Encoding::Ansi,
    ];

    /// Short name for the status bar.
    pub fn label(self) -> String {
        match self {
            Encoding::Utf8 => "UTF-8".to_owned(),
            Encoding::Utf8Bom => "UTF-8 (BOM)".to_owned(),
            Encoding::Utf16Le => "UTF-16 LE".to_owned(),
            Encoding::Utf16Be => "UTF-16 BE".to_owned(),
            Encoding::Ansi => format!("ANSI ({})", ansi_name()),
        }
    }

    /// Name in menus, where there is room to spell out the BOM.
    pub fn menu_label(self) -> String {
        match self {
            Encoding::Utf8Bom => "UTF-8 (BOM 포함)".to_owned(),
            other => other.label(),
        }
    }
}

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
const UTF16LE_BOM: &[u8] = &[0xFF, 0xFE];
const UTF16BE_BOM: &[u8] = &[0xFE, 0xFF];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum LineEnding {
    #[default]
    CrLf,
    Lf,
    Cr,
}

impl LineEnding {
    pub const ALL: [LineEnding; 3] = [LineEnding::CrLf, LineEnding::Lf, LineEnding::Cr];

    pub fn label(self) -> &'static str {
        match self {
            LineEnding::CrLf => "Windows (CRLF)",
            LineEnding::Lf => "Unix (LF)",
            LineEnding::Cr => "Macintosh (CR)",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::CrLf => "\r\n",
            LineEnding::Lf => "\n",
            LineEnding::Cr => "\r",
        }
    }
}

/// The ANSI code page as users know it, e.g. `CP949`.
fn ansi_name() -> String {
    format!("CP{}", unsafe { GetACP() })
}

#[derive(Debug)]
pub struct Decoded {
    /// Text with `\n` line breaks only.
    pub text: String,
    pub encoding: Encoding,
    pub line_ending: LineEnding,
    /// Some bytes could not be decoded and were replaced with U+FFFD.
    pub lossy: bool,
    /// NUL characters were replaced (the editor cannot show them).
    pub had_nul: bool,
}

/// Decodes file contents, detecting the encoding unless `forced` is given.
pub fn decode(bytes: &[u8], forced: Option<Encoding>) -> Decoded {
    let encoding = forced.unwrap_or_else(|| detect(bytes));
    let (raw, lossy) = match encoding {
        Encoding::Utf8 | Encoding::Utf8Bom => {
            let body = bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes);
            match std::str::from_utf8(body) {
                Ok(s) => (s.to_owned(), false),
                Err(_) => (String::from_utf8_lossy(body).into_owned(), true),
            }
        }
        Encoding::Utf16Le => decode_utf16(bytes.strip_prefix(UTF16LE_BOM).unwrap_or(bytes), true),
        Encoding::Utf16Be => decode_utf16(bytes.strip_prefix(UTF16BE_BOM).unwrap_or(bytes), false),
        Encoding::Ansi => ansi_decode(bytes),
    };
    // A file read as UTF-8 that starts with a BOM keeps its BOM when saved.
    let encoding = match encoding {
        Encoding::Utf8 if bytes.starts_with(UTF8_BOM) => Encoding::Utf8Bom,
        other => other,
    };
    let line_ending = detect_line_ending(&raw);
    let had_nul = raw.contains('\0');
    let mut text = normalize_newlines(&raw);
    if had_nul {
        text = text.replace('\0', "\u{FFFD}");
    }
    Decoded {
        text,
        encoding,
        line_ending,
        lossy,
        had_nul,
    }
}

/// Guesses the encoding of file contents.
pub fn detect(bytes: &[u8]) -> Encoding {
    if bytes.starts_with(UTF8_BOM) {
        return Encoding::Utf8Bom;
    }
    if bytes.starts_with(UTF16LE_BOM) {
        return Encoding::Utf16Le;
    }
    if bytes.starts_with(UTF16BE_BOM) {
        return Encoding::Utf16Be;
    }
    if let Some(utf16) = detect_bomless_utf16(bytes) {
        return utf16;
    }
    if std::str::from_utf8(bytes).is_ok() {
        return Encoding::Utf8;
    }
    Encoding::Ansi
}

/// UTF-16 text without a BOM is recognisable by the zero high bytes of ASCII characters.
fn detect_bomless_utf16(bytes: &[u8]) -> Option<Encoding> {
    let sample = &bytes[..bytes.len().min(4096) & !1];
    if sample.len() < 4 {
        return None;
    }
    let pairs = sample.len() / 2;
    let zero_even = sample.iter().step_by(2).filter(|&&b| b == 0).count();
    let zero_odd = sample.iter().skip(1).step_by(2).filter(|&&b| b == 0).count();
    if zero_odd * 3 > pairs && zero_even * 20 < pairs {
        Some(Encoding::Utf16Le)
    } else if zero_even * 3 > pairs && zero_odd * 20 < pairs {
        Some(Encoding::Utf16Be)
    } else {
        None
    }
}

/// Returns the text and whether anything had to be replaced.
fn decode_utf16(bytes: &[u8], little_endian: bool) -> (String, bool) {
    let (pairs, odd_tail) = bytes.as_chunks::<2>();
    let units: Vec<u16> = pairs
        .iter()
        .map(|&pair| {
            if little_endian {
                u16::from_le_bytes(pair)
            } else {
                u16::from_be_bytes(pair)
            }
        })
        .collect();
    let odd_tail = !odd_tail.is_empty();
    match String::from_utf16(&units) {
        Ok(s) => (s, odd_tail),
        Err(_) => (String::from_utf16_lossy(&units), true),
    }
}

/// Returns the text and whether anything had to be replaced.
fn ansi_decode(bytes: &[u8]) -> (String, bool) {
    if bytes.is_empty() {
        return (String::new(), false);
    }
    if let Some(s) = multi_byte_to_wide(bytes, MB_ERR_INVALID_CHARS) {
        return (s, false);
    }
    // Invalid sequences for this code page: decode anyway, accepting replacements.
    match multi_byte_to_wide(bytes, 0) {
        Some(s) => (s, true),
        None => (String::from_utf8_lossy(bytes).into_owned(), true),
    }
}

/// ANSI code page → text. With `MB_ERR_INVALID_CHARS`, fails on invalid sequences.
fn multi_byte_to_wide(bytes: &[u8], flags: u32) -> Option<String> {
    let len = i32::try_from(bytes.len()).ok()?;
    // First call: the length needed; second call: the conversion.
    let needed = unsafe { MultiByteToWideChar(CP_ACP, flags, bytes.as_ptr(), len, std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return None;
    }
    let mut buf = vec![0u16; needed as usize];
    let written = unsafe { MultiByteToWideChar(CP_ACP, flags, bytes.as_ptr(), len, buf.as_mut_ptr(), needed) };
    if written <= 0 {
        return None;
    }
    buf.truncate(written as usize);
    Some(String::from_utf16_lossy(&buf))
}

/// The text could not be stored in the requested encoding.
#[derive(Debug, Clone)]
pub struct Unrepresentable {
    /// The first character that has no equivalent in the target code page.
    pub sample: char,
}

/// Converts editor text (`\n` line breaks) into file bytes.
pub fn encode(
    text: &str,
    encoding: Encoding,
    line_ending: LineEnding,
) -> Result<Vec<u8>, Unrepresentable> {
    let with_breaks;
    let text = if line_ending == LineEnding::Lf {
        text
    } else {
        with_breaks = text.replace('\n', line_ending.as_str());
        with_breaks.as_str()
    };
    Ok(match encoding {
        Encoding::Utf8 => text.as_bytes().to_vec(),
        Encoding::Utf8Bom => [UTF8_BOM, text.as_bytes()].concat(),
        Encoding::Utf16Le => {
            let mut out = UTF16LE_BOM.to_vec();
            for unit in text.encode_utf16() {
                out.extend_from_slice(&unit.to_le_bytes());
            }
            out
        }
        Encoding::Utf16Be => {
            let mut out = UTF16BE_BOM.to_vec();
            for unit in text.encode_utf16() {
                out.extend_from_slice(&unit.to_be_bytes());
            }
            out
        }
        Encoding::Ansi => ansi_encode(text)?,
    })
}

fn ansi_encode(text: &str) -> Result<Vec<u8>, Unrepresentable> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let wide: Vec<u16> = text.encode_utf16().collect();
    let len = i32::try_from(wide.len()).map_err(|_| Unrepresentable { sample: '?' })?;
    // WC_NO_BEST_FIT_CHARS: a character the code page lacks becomes the default
    // character and sets `used_default`, instead of a look-alike ("é" → "e").
    let mut used_default = 0;
    let needed = unsafe {
        WideCharToMultiByte(
            CP_ACP,
            WC_NO_BEST_FIT_CHARS,
            wide.as_ptr(),
            len,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            &mut used_default,
        )
    };
    if needed <= 0 {
        return Err(Unrepresentable { sample: '?' });
    }
    let mut buf = vec![0u8; needed as usize];
    unsafe {
        WideCharToMultiByte(
            CP_ACP,
            WC_NO_BEST_FIT_CHARS,
            wide.as_ptr(),
            len,
            buf.as_mut_ptr(),
            needed,
            std::ptr::null(),
            &mut used_default,
        )
    };
    if used_default != 0 {
        return Err(Unrepresentable {
            sample: first_unrepresentable(text),
        });
    }
    Ok(buf)
}

/// Finds the first character that does not survive a round trip through the ANSI code page.
fn first_unrepresentable(text: &str) -> char {
    for c in text.chars() {
        let mut buf = [0u16; 2];
        let units = c.encode_utf16(&mut buf);
        let mut used_default = 0;
        let mut out = [0u8; 8];
        unsafe {
            WideCharToMultiByte(
                CP_ACP,
                WC_NO_BEST_FIT_CHARS,
                units.as_ptr(),
                units.len() as i32,
                out.as_mut_ptr(),
                out.len() as i32,
                std::ptr::null(),
                &mut used_default,
            );
        }
        if used_default != 0 {
            return c;
        }
    }
    '?'
}

/// The most frequent line-break style; CRLF when there are none.
fn detect_line_ending(text: &str) -> LineEnding {
    let bytes = text.as_bytes();
    let (mut crlf, mut lf, mut cr) = (0usize, 0usize, 0usize);
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                crlf += 1;
                i += 1;
            }
            b'\r' => cr += 1,
            b'\n' => lf += 1,
            _ => {}
        }
        i += 1;
    }
    // Ties, including no line breaks at all, go to Windows' CRLF.
    if crlf >= lf && crlf >= cr {
        LineEnding::CrLf
    } else if lf >= cr {
        LineEnding::Lf
    } else {
        LineEnding::Cr
    }
}

/// Converts CRLF and lone CR into LF.
pub fn normalize_newlines(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_boms() {
        assert_eq!(detect(&[0xEF, 0xBB, 0xBF, b'a']), Encoding::Utf8Bom);
        assert_eq!(detect(&[0xFF, 0xFE, b'a', 0]), Encoding::Utf16Le);
        assert_eq!(detect(&[0xFE, 0xFF, 0, b'a']), Encoding::Utf16Be);
    }

    #[test]
    fn detects_utf8_and_ansi() {
        assert_eq!(detect("hello".as_bytes()), Encoding::Utf8);
        assert_eq!(detect("안녕하세요".as_bytes()), Encoding::Utf8);
        // "안녕" in CP949 is not valid UTF-8.
        assert_eq!(detect(&[0xBE, 0xC8, 0xB3, 0xE7]), Encoding::Ansi);
    }

    #[test]
    fn detects_bomless_utf16() {
        let le: Vec<u8> = "hello world".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(detect(&le), Encoding::Utf16Le);
        let be: Vec<u8> = "hello world".encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        assert_eq!(detect(&be), Encoding::Utf16Be);
    }

    #[test]
    fn line_endings() {
        assert_eq!(detect_line_ending("a\r\nb\r\nc\n"), LineEnding::CrLf);
        assert_eq!(detect_line_ending("a\nb\nc\r\n"), LineEnding::Lf);
        assert_eq!(detect_line_ending("a\rb\rc"), LineEnding::Cr);
        assert_eq!(detect_line_ending("abc"), LineEnding::CrLf);
        assert_eq!(normalize_newlines("a\r\nb\rc\n"), "a\nb\nc\n");
    }

    #[test]
    fn round_trips() {
        let text = "첫 줄\n두 번째 줄 😀\n";
        for enc in [Encoding::Utf8, Encoding::Utf8Bom, Encoding::Utf16Le, Encoding::Utf16Be] {
            for eol in LineEnding::ALL {
                let bytes = encode(text, enc, eol).unwrap();
                let back = decode(&bytes, None);
                assert_eq!(back.text, text, "{enc:?} {eol:?}");
                assert_eq!(back.encoding, enc);
                assert_eq!(back.line_ending, eol);
            }
        }
    }

    #[test]
    fn utf8_without_bom_stays_without_bom() {
        let d = decode("abc".as_bytes(), None);
        assert_eq!(d.encoding, Encoding::Utf8);
        let d = decode("abc".as_bytes(), Some(Encoding::Utf8Bom));
        assert_eq!(d.encoding, Encoding::Utf8Bom);
    }

    #[test]
    fn invalid_utf8_forced_is_lossy() {
        let d = decode(&[0xBE, 0xC8], Some(Encoding::Utf8));
        assert!(d.lossy);
    }

    #[test]
    fn nul_is_replaced() {
        let d = decode(b"a\0b", None);
        assert!(d.had_nul);
        assert_eq!(d.text, "a\u{FFFD}b");
    }
}
