//! Fonts: finding Windows fonts and installing them into egui.
//!
//! egui draws text itself and needs the font *files*. Installed fonts are listed in
//! the registry (`...\Windows NT\CurrentVersion\Fonts`, machine-wide and per user).
//! Hangul always falls back to Malgun Gothic, so any Latin font can be chosen.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegEnumValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
    KEY_READ, REG_SZ,
};

use crate::paths;
use crate::settings::FontChoice;
use crate::win::wide;

pub const EDITOR_FAMILY: &str = "editor";
pub const PREVIEW_FAMILY: &str = "preview";
const KOREAN_FALLBACK: &str = "malgun.ttf";
const UI_FONT: &str = "segoeui.ttf";

#[derive(Clone, Debug, PartialEq)]
pub struct SystemFont {
    /// Registry family name, e.g. `Consolas` or `GulimChe`.
    pub name: String,
    pub file: PathBuf,
    /// Face index inside a `.ttc` collection.
    pub index: u32,
}

impl SystemFont {
    /// Name to show, with the Korean name for the common Korean fonts.
    pub fn display_name(&self) -> String {
        display_name(&self.name)
    }
}

pub fn display_name(family: &str) -> String {
    let korean = match family {
        "Malgun Gothic" => "맑은 고딕",
        "Malgun Gothic Semilight" | "Malgun Gothic SemiLight" => "맑은 고딕 Semilight",
        "Gulim" => "굴림",
        "GulimChe" => "굴림체",
        "Dotum" => "돋움",
        "DotumChe" => "돋움체",
        "Batang" => "바탕",
        "BatangChe" => "바탕체",
        "Gungsuh" => "궁서",
        "GungsuhChe" => "궁서체",
        _ => return family.to_owned(),
    };
    format!("{korean} ({family})")
}

/// Regular faces of all installed TrueType/OpenType fonts, sorted by name.
pub fn list_system_fonts() -> Vec<SystemFont> {
    let mut fonts = Vec::new();
    let subkey = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Fonts";
    for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        for (name, file) in read_registry_values(root, subkey) {
            fonts.extend(parse_font_entry(&name, &file));
        }
    }
    fonts.sort_by_key(|f| f.name.to_lowercase());
    fonts.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
    fonts
}

/// Turns `Gulim & GulimChe (TrueType)` = `gulim.ttc` into one entry per face.
fn parse_font_entry(value_name: &str, file: &str) -> Vec<SystemFont> {
    let Some(names) = value_name
        .strip_suffix(" (TrueType)")
        .or_else(|| value_name.strip_suffix(" (OpenType)"))
    else {
        return Vec::new();
    };
    let lower = file.to_lowercase();
    if !(lower.ends_with(".ttf") || lower.ends_with(".otf") || lower.ends_with(".ttc")) {
        return Vec::new();
    }
    let path = if Path::new(file).is_absolute() {
        PathBuf::from(file)
    } else {
        paths::windows_fonts_dir().join(file)
    };
    names
        .split(" & ")
        .enumerate()
        .filter(|(_, name)| !is_style_variant(name))
        .map(|(index, name)| SystemFont {
            name: name.trim().to_owned(),
            file: path.clone(),
            index: index as u32,
        })
        .collect()
}

/// Bold/italic/light files are separate registry entries; only regular faces are offered.
fn is_style_variant(name: &str) -> bool {
    const STYLES: [&str; 12] = [
        " Bold", " Italic", " Light", " Semibold", " SemiBold", " Black", " Thin", " Medium",
        " Oblique", " ExtraLight", " Heavy", " Demibold",
    ];
    STYLES.iter().any(|s| name.contains(s))
}

/// The string values (name, data) of a registry key.
fn read_registry_values(root: HKEY, subkey: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let subkey = wide(subkey);
    let mut key: HKEY = std::ptr::null_mut();
    if unsafe { RegOpenKeyExW(root, subkey.as_ptr(), 0, KEY_READ, &mut key) } != 0 {
        return out;
    }
    for index in 0.. {
        let mut name = [0u16; 512];
        let mut name_len = name.len() as u32;
        let mut data = [0u16; 1024];
        let mut data_len = (data.len() * 2) as u32; // in bytes
        let mut kind = 0u32;
        let status = unsafe {
            RegEnumValueW(
                key,
                index,
                name.as_mut_ptr(),
                &mut name_len,
                std::ptr::null(),
                &mut kind,
                data.as_mut_ptr().cast(),
                &mut data_len,
            )
        };
        if status != 0 {
            break; // ERROR_NO_MORE_ITEMS
        }
        if kind != REG_SZ {
            continue;
        }
        let name = String::from_utf16_lossy(&name[..name_len as usize]);
        let units = (data_len as usize / 2).min(data.len());
        let value = String::from_utf16_lossy(&data[..units]);
        out.push((name, value.trim_end_matches('\0').to_owned()));
    }
    unsafe { RegCloseKey(key) };
    out
}

/// Looks up an installed font by family name.
fn find_font(family: &str) -> Option<SystemFont> {
    list_system_fonts()
        .into_iter()
        .find(|f| f.name.eq_ignore_ascii_case(family))
}

/// Caches font file contents so switching fonts does not re-read files.
#[derive(Default)]
pub struct FontStore {
    /// Font files read so far, by (file, face index).
    cache: HashMap<(PathBuf, u32), Arc<FontData>>,
    /// Malgun Gothic: `None` = not looked for yet, `Some(None)` = not installed.
    korean: Option<Option<&'static [u8]>>,
}

impl FontStore {
    fn load(&mut self, file: &Path, index: u32) -> Option<Arc<FontData>> {
        let key = (file.to_path_buf(), index);
        if let Some(data) = self.cache.get(&key) {
            return Some(Arc::clone(data));
        }
        let bytes = std::fs::read(file)
            .map_err(|err| log::warn!("cannot read font {}: {err}", file.display()))
            .ok()?;
        // egui panics on a font it cannot parse; a damaged file must never reach it
        // (once chosen, it would stop mome from starting at all).
        if let Err(err) = skrifa::FontRef::from_index(&bytes, index) {
            log::warn!("cannot use font {} #{index}: {err}", file.display());
            return None;
        }
        let mut data = FontData::from_owned(bytes);
        data.index = index;
        let data = Arc::new(data);
        self.cache.insert(key, Arc::clone(&data));
        Some(data)
    }

    fn load_windows_font(&mut self, file_name: &str) -> Option<Arc<FontData>> {
        self.load(&paths::windows_fonts_dir().join(file_name), 0)
    }

    /// Resolves the editor font; falls back to Consolas when the file is gone.
    fn resolve_editor_font(&mut self, choice: &FontChoice) -> Option<Arc<FontData>> {
        if let Some(file) = &choice.file {
            if let Some(data) = self.load(file, choice.index) {
                return Some(data);
            }
        }
        if let Some(font) = find_font(&choice.family) {
            if let Some(data) = self.load(&font.file, font.index) {
                return Some(data);
            }
        }
        self.load_windows_font("consola.ttf")
    }

    /// Malgun Gothic, read once and kept for the life of the process so the several
    /// differently-tweaked copies below can share the bytes.
    fn korean_bytes(&mut self) -> Option<&'static [u8]> {
        *self.korean.get_or_insert_with(|| {
            let path = paths::windows_fonts_dir().join(KOREAN_FALLBACK);
            match std::fs::read(&path) {
                Ok(bytes) => Some(&*Box::leak(bytes.into_boxed_slice())),
                Err(err) => {
                    log::warn!("no Korean fallback font {}: {err}", path.display());
                    None
                }
            }
        })
    }

    /// Builds the egui font set: UI fonts, the editor font and an optional preview font.
    pub fn definitions(&mut self, editor: &FontChoice, preview: Option<&SystemFont>) -> FontDefinitions {
        let mut defs = FontDefinitions::default();
        let emoji: Vec<String> = defs
            .families
            .get(&FontFamily::Proportional)
            .map(|v| v.iter().filter(|n| is_emoji(n)).cloned().collect())
            .unwrap_or_default();
        let korean = self.korean_bytes();

        // UI: Segoe UI for Latin, Malgun Gothic for Hangul.
        let ui = self.load_windows_font(UI_FONT);
        if let Some(data) = &ui {
            defs.font_data.insert("ui".to_owned(), Arc::clone(data));
            defs.families
                .entry(FontFamily::Proportional)
                .or_default()
                .insert(0, "ui".to_owned());
        }
        add_korean_fallback(&mut defs, korean, FontFamily::Proportional, "korean-ui", ui.as_deref(), 0.0);
        let hack = defs.font_data.get("Hack").cloned();
        add_korean_fallback(&mut defs, korean, FontFamily::Monospace, "korean-mono", hack.as_deref(), 0.0);

        let editor_data = self.resolve_editor_font(editor);
        let editor_family = (EDITOR_FAMILY, Some(EDITOR_LINE_HEIGHT));
        install_family(&mut defs, korean, &emoji, editor_family, editor_data);
        let preview_data = preview.and_then(|p| self.load(&p.file, p.index));
        install_family(&mut defs, korean, &emoji, (PREVIEW_FAMILY, None), preview_data);
        defs
    }
}

fn is_emoji(font_name: &str) -> bool {
    font_name.contains("moji")
}

/// Adds Malgun Gothic (`korean`, if installed) to `family` as a fallback for Hangul,
/// before its emoji fonts. Its baseline is matched to `primary`, then moved down by
/// `shift` ems.
fn add_korean_fallback(
    defs: &mut FontDefinitions,
    korean: Option<&'static [u8]>,
    family: FontFamily,
    name: &str,
    primary: Option<&FontData>,
    shift: f32,
) {
    let Some(bytes) = korean else {
        return;
    };
    let mut data = fallback_font(bytes, primary);
    data.tweak.y_offset_factor += shift;
    defs.font_data.insert(name.to_owned(), Arc::new(data));
    let list = defs.families.entry(family).or_default();
    let before_emoji = list.iter().position(|n| is_emoji(n)).unwrap_or(list.len());
    list.insert(before_emoji, name.to_owned());
}

/// Installs the named family `name` (editor or preview): `data`, then emoji, then
/// Korean. With a `line_height` (in ems) taller than the font's own row height, the
/// extra space would all go below the text; the glyphs are moved down by half of it
/// to centre them.
fn install_family(
    defs: &mut FontDefinitions,
    korean: Option<&'static [u8]>,
    emoji: &[String],
    (name, line_height): (&str, Option<f32>),
    data: Option<Arc<FontData>>,
) {
    let shift = match (&data, line_height) {
        (Some(d), Some(lh)) => vertical_metrics(&d.font, d.index)
            .map(|(_, row_height)| ((lh - row_height) / 2.0).max(0.0))
            .unwrap_or(0.0),
        _ => 0.0,
    };
    let mut list = Vec::new();
    if let Some(data) = &data {
        let data = if shift > 0.0 {
            let mut shifted = FontData::clone(data);
            shifted.tweak.y_offset_factor += shift;
            Arc::new(shifted)
        } else {
            Arc::clone(data)
        };
        let main = format!("{name}-main");
        defs.font_data.insert(main.clone(), data);
        list.push(main);
    }
    list.extend(emoji.iter().cloned());
    let family = FontFamily::Name(name.into());
    defs.families.insert(family.clone(), list);
    add_korean_fallback(defs, korean, family, &format!("{name}-korean"), data.as_deref(), shift);
}

/// Line height of the editor text, in ems. Generous, because Hangul is taller than Latin.
pub const EDITOR_LINE_HEIGHT: f32 = 1.45;

/// Ascent and row height (ascent - descent + line gap) in ems, as egui computes them.
fn vertical_metrics(data: &[u8], index: u32) -> Option<(f32, f32)> {
    use skrifa::MetadataProvider as _;
    let font = skrifa::FontRef::from_index(data, index).ok()?;
    let m = font.metrics(skrifa::instance::Size::unscaled(), skrifa::instance::LocationRef::default());
    let upem = m.units_per_em as f32;
    if upem <= 0.0 {
        return None;
    }
    let ascent = m.ascent / upem;
    Some((ascent, ascent - m.descent / upem + m.leading / upem))
}

/// The Korean fallback font, shifted so its baseline lines up with `primary`.
///
/// egui centres a fallback font's row within the primary font's row, which puts
/// Malgun Gothic's baseline below that of fonts like Consolas.
fn fallback_font(bytes: &'static [u8], primary: Option<&FontData>) -> FontData {
    let mut data = FontData::from_static(bytes);
    let (Some(primary), Some((fallback_ascent, fallback_height))) = (primary, vertical_metrics(bytes, 0)) else {
        return data;
    };
    if let Some((ascent, height)) = vertical_metrics(&primary.font, primary.index) {
        data.tweak.y_offset_factor = (ascent - fallback_ascent) - 0.5 * (height - fallback_height);
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_vertical_metrics() {
        let dir = paths::windows_fonts_dir();
        let (Ok(consolas), Ok(malgun)) = (
            std::fs::read(dir.join("consola.ttf")),
            std::fs::read(dir.join(KOREAN_FALLBACK)),
        ) else {
            return; // Fonts not installed on this machine.
        };
        let (a1, h1) = vertical_metrics(&consolas, 0).unwrap();
        let (a2, h2) = vertical_metrics(&malgun, 0).unwrap();
        assert!(a1 > 0.5 && a1 < 1.5 && h1 > a1, "consolas {a1} {h1}");
        assert!(a2 > 0.5 && a2 < 1.5 && h2 > a2, "malgun {a2} {h2}");
        let primary = FontData::from_owned(consolas);
        let tweaked = fallback_font(Box::leak(malgun.into_boxed_slice()), Some(&primary));
        // Baseline of the fallback = its ascent + egui's centring offset + our shift.
        let baseline = a2 + 0.5 * (h1 - h2) + tweaked.tweak.y_offset_factor;
        assert!((baseline - a1).abs() < 1e-4);
    }

    #[test]
    fn parses_registry_entries() {
        let fonts = parse_font_entry("Gulim & GulimChe & Dotum & DotumChe (TrueType)", "gulim.ttc");
        let names: Vec<_> = fonts.iter().map(|f| (f.name.as_str(), f.index)).collect();
        assert_eq!(
            names,
            vec![("Gulim", 0), ("GulimChe", 1), ("Dotum", 2), ("DotumChe", 3)]
        );
        assert!(parse_font_entry("Consolas Bold (TrueType)", "consolab.ttf").is_empty());
        assert!(parse_font_entry("Courier 10,12,15", "COURE.FON").is_empty());
        let user = parse_font_entry("D2Coding (TrueType)", "C:\\Users\\me\\Fonts\\D2Coding.ttf");
        assert_eq!(user[0].file, PathBuf::from("C:\\Users\\me\\Fonts\\D2Coding.ttf"));
    }

    #[test]
    fn korean_display_names() {
        assert_eq!(display_name("Malgun Gothic"), "맑은 고딕 (Malgun Gothic)");
        assert_eq!(display_name("Consolas"), "Consolas");
    }
}
