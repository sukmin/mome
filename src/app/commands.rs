//! Commands from menus, shortcuts and the status bar.

use std::path::PathBuf;

use egui::{Event, Key, KeyboardShortcut, Modifiers};

use super::dialogs::{Dialog, FontPicker, GoToForm, HistoryBrowser, SettingsForm};
use super::findbar::Direction;
use super::viewer::ViewKind;
use super::{After, MomeApp};
use crate::encoding::{Encoding, LineEnding};
use crate::history::RevisionKind;
use crate::print::{self, PrintJob, PrintOutcome};
use crate::theme;
use crate::win;

#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    // 파일
    New,
    NewWindow,
    Open,
    /// Open dialog without asking about unsaved changes (already asked).
    OpenUnchecked,
    OpenPath(PathBuf),
    ClearRecent,
    Save,
    SaveAs,
    ToggleAutosave,
    SnapshotNow,
    History,
    OpenTempFolder,
    PageSetup,
    Print,
    Settings,
    Exit,
    // 편집
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Delete,
    Find,
    FindNext,
    FindPrev,
    Replace,
    GoTo,
    SelectAll,
    TimeDate,
    ToggleOverwrite,
    FindInvisible,
    // 서식 / 보기
    ToggleWrap,
    Font,
    FormatJson,
    ToggleReader,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ToggleStatusBar,
    ToggleShowInvisible,
    ToggleDark,
    // 인코딩
    SetEncoding(Encoding),
    SetLineEnding(LineEnding),
    Reopen(Encoding),
    // 도움말
    Shortcuts,
    About,
}

const CMD: Modifiers = Modifiers::COMMAND;
const CMD_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);

/// Keyboard shortcuts. More specific modifiers must come first: egui ignores
/// extra Shift/Alt when matching.
const SHORTCUTS: &[(Modifiers, Key, Cmd)] = &[
    (CMD_SHIFT, Key::N, Cmd::NewWindow),
    (CMD, Key::N, Cmd::New),
    (CMD, Key::O, Cmd::Open),
    (CMD_SHIFT, Key::S, Cmd::SaveAs),
    (CMD, Key::S, Cmd::Save),
    (CMD_SHIFT, Key::R, Cmd::History),
    // IntelliJ's replace key; Ctrl+H (Notepad's) stays too.
    (CMD, Key::R, Cmd::Replace),
    (CMD, Key::P, Cmd::Print),
    (CMD_SHIFT, Key::I, Cmd::FindInvisible),
    (CMD, Key::F, Cmd::Find),
    (Modifiers::SHIFT, Key::F3, Cmd::FindPrev),
    (Modifiers::NONE, Key::F3, Cmd::FindNext),
    (CMD, Key::H, Cmd::Replace),
    (CMD, Key::G, Cmd::GoTo),
    (CMD, Key::E, Cmd::ToggleReader),
    (Modifiers::NONE, Key::F5, Cmd::TimeDate),
    (CMD, Key::Plus, Cmd::ZoomIn),
    (CMD, Key::Equals, Cmd::ZoomIn),
    (CMD, Key::Minus, Cmd::ZoomOut),
    (CMD, Key::Num0, Cmd::ZoomReset),
    (Modifiers::NONE, Key::F1, Cmd::Shortcuts),
];

/// The table in 도움말 > 단축키 (F1).
pub(super) const SHORTCUT_HELP: &[(&str, &str)] = &[
    ("Ctrl+N", "새로 만들기"),
    ("Ctrl+Shift+N", "새 창"),
    ("Ctrl+O", "열기"),
    ("Ctrl+S", "저장"),
    ("Ctrl+Shift+S", "다른 이름으로 저장"),
    ("Ctrl+Shift+R", "임시 저장 기록"),
    ("Ctrl+P", "인쇄"),
    ("Ctrl+Z / Ctrl+Y", "실행 취소 / 다시 실행"),
    ("Ctrl+F / Ctrl+R", "찾기 / 바꾸기 (Ctrl+H도 바꾸기)"),
    ("F3 / Shift+F3", "다음 찾기 / 이전 찾기"),
    ("Enter / Shift+Enter", "찾기 표시줄에서 다음 / 이전 찾기"),
    ("Alt+C / Alt+O / Alt+X", "찾기 표시줄: 대/소문자 구분 / 단어 단위 / 정규식"),
    ("Alt+P / Alt+A / Alt+L", "찾기 표시줄: 바꾸기 / 모두 바꾸기 / 제외"),
    ("Ctrl+Shift+I", "보이지 않는 문자 찾기"),
    ("Ctrl+G", "줄로 이동"),
    ("Ctrl+E", "읽기 모드 / 편집 모드 전환 (Markdown·캔버스)"),
    ("F5", "시간/날짜 삽입"),
    ("Insert", "삽입 / 겹쳐쓰기 전환"),
    ("Ctrl+더하기 / 빼기 / 0", "확대 / 축소 / 기본 크기로 복원"),
    ("Ctrl+마우스 휠", "확대 / 축소"),
    ("Esc", "찾기 닫기"),
    ("F1", "단축키 보기"),
];

/// Text shown next to menu items.
pub fn shortcut_text(cmd: &Cmd) -> &'static str {
    match cmd {
        Cmd::New => "Ctrl+N",
        Cmd::NewWindow => "Ctrl+Shift+N",
        Cmd::Open => "Ctrl+O",
        Cmd::Save => "Ctrl+S",
        Cmd::SaveAs => "Ctrl+Shift+S",
        Cmd::History => "Ctrl+Shift+R",
        Cmd::Print => "Ctrl+P",
        Cmd::Undo => "Ctrl+Z",
        Cmd::Redo => "Ctrl+Y",
        Cmd::Cut => "Ctrl+X",
        Cmd::Copy => "Ctrl+C",
        Cmd::Paste => "Ctrl+V",
        Cmd::Delete => "Del",
        Cmd::Find => "Ctrl+F",
        Cmd::FindNext => "F3",
        Cmd::FindPrev => "Shift+F3",
        Cmd::Replace => "Ctrl+R",
        Cmd::FindInvisible => "Ctrl+Shift+I",
        Cmd::GoTo => "Ctrl+G",
        Cmd::ToggleReader => "Ctrl+E",
        Cmd::SelectAll => "Ctrl+A",
        Cmd::TimeDate => "F5",
        Cmd::ToggleOverwrite => "Insert",
        Cmd::ZoomIn => "Ctrl+더하기",
        Cmd::ZoomOut => "Ctrl+빼기",
        Cmd::ZoomReset => "Ctrl+0",
        Cmd::Shortcuts => "F1",
        _ => "",
    }
}

/// Alt+letter types nothing, as in Notepad: Windows sends the letter with the key
/// (WM_SYSCHAR) and egui would insert it. AltGr is not reported as Alt, and
/// Alt+numpad codes arrive when Alt is released, so neither is affected.
fn drop_alt_letters(ctx: &egui::Context) {
    ctx.input_mut(|i| {
        let mut k = 0;
        while k + 1 < i.events.len() {
            let alt_key = matches!(&i.events[k], egui::Event::Key { pressed: true, modifiers, .. }
                if modifiers.alt && !modifiers.ctrl && !modifiers.command);
            if alt_key && matches!(i.events[k + 1], egui::Event::Text(_)) {
                i.events.remove(k + 1);
            }
            k += 1;
        }
    });
}

impl MomeApp {
    pub(super) fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        for (modifiers, key, cmd) in SHORTCUTS {
            if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(*modifiers, *key))) {
                self.pending_cmds.push(cmd.clone());
            }
        }
        // egui's text field has Emacs-style Ctrl+K/U/W deletions; Windows users do not expect them.
        for key in [Key::K, Key::U, Key::W] {
            ctx.input_mut(|i| i.consume_key(CMD, key));
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Insert)) {
            self.pending_cmds.push(Cmd::ToggleOverwrite);
        }
        self.find_bar_keys(ctx);
        drop_alt_letters(ctx);
        if self.find.open && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.find.close();
            self.editor.request_focus();
        } else if self.invisible.open && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.invisible.close();
            self.editor.request_focus();
        }
        // Ctrl + mouse wheel zooms the text (the canvas zooms itself).
        if self.reading() != Some(ViewKind::Canvas) {
            self.editor.zoom_by_factor(ctx.input(|i| i.zoom_delta()));
        }
    }

    pub(super) fn run(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame, cmd: Cmd) {
        if let Some(kind) = self.reading() {
            match cmd {
                // Overwrite mode means nothing while reading.
                Cmd::ToggleOverwrite => return,
                // The menu cannot reach the text selected in the reader; Ctrl+C can.
                Cmd::Copy => {
                    self.notify("읽기 화면에서는 글을 끌어 선택한 뒤 Ctrl+C를 누르면 복사됩니다.".to_owned());
                    return;
                }
                Cmd::ZoomIn | Cmd::ZoomOut | Cmd::ZoomReset if kind == ViewKind::Canvas => {
                    match cmd {
                        Cmd::ZoomIn => self.viewer.zoom_canvas(1.25),
                        Cmd::ZoomOut => self.viewer.zoom_canvas(1.0 / 1.25),
                        _ => self.viewer.fit_canvas(),
                    }
                    return;
                }
                // Commands that work on the text go back to the editor first.
                Cmd::Undo
                | Cmd::Redo
                | Cmd::Cut
                | Cmd::Paste
                | Cmd::Delete
                | Cmd::Find
                | Cmd::Replace
                | Cmd::FindNext
                | Cmd::FindPrev
                | Cmd::GoTo
                | Cmd::SelectAll
                | Cmd::TimeDate
                | Cmd::FindInvisible
                | Cmd::FormatJson => self.set_reading(false),
                _ => {}
            }
        }
        match cmd {
            Cmd::New => self.confirm_then(ctx, frame, After::New),
            Cmd::NewWindow => win::spawn_self(&[]),
            Cmd::Open => self.confirm_then(ctx, frame, After::Open(None)),
            Cmd::OpenUnchecked => {
                if let Some(path) = self.pick_open_path(frame) {
                    self.open_path(ctx, &path, None);
                }
            }
            Cmd::OpenPath(path) => self.confirm_then(ctx, frame, After::Open(Some(path))),
            Cmd::ClearRecent => self.change_settings(|s| s.recent_files.clear()),
            Cmd::Save => {
                self.save(ctx, frame, None);
            }
            Cmd::SaveAs => {
                self.save_as(ctx, frame);
            }
            Cmd::ToggleAutosave => {
                let on = !self.settings.autosave_original;
                self.change_settings(|s| s.autosave_original = on);
                self.notify(if on {
                    "이제 파일로 저장한 문서는 임시 저장 간격마다 원본 파일에도 자동 저장합니다.".to_owned()
                } else {
                    "원본 파일 자동 저장을 껐습니다. 임시 저장은 계속됩니다.".to_owned()
                });
            }
            Cmd::SnapshotNow => {
                if self.settings.temp_dir.is_none() {
                    self.open_dialog(Dialog::Settings(SettingsForm::first_run()));
                } else if self.take_snapshot(RevisionKind::Manual) {
                    self.snapshot.dirty_since = None;
                    self.notify("지금 내용을 임시 저장했습니다.".to_owned());
                } else {
                    self.notify("마지막 임시 저장 이후 변경된 내용이 없습니다.".to_owned());
                }
            }
            Cmd::History => match &self.settings.temp_dir {
                Some(root) => {
                    let browser = HistoryBrowser::new(root.clone(), &self.doc.doc_key);
                    self.open_dialog(Dialog::History(Box::new(browser)));
                }
                None => self.open_dialog(Dialog::Settings(SettingsForm::first_run())),
            },
            Cmd::OpenTempFolder => match &self.settings.temp_dir {
                Some(dir) => {
                    let _ = std::fs::create_dir_all(dir);
                    win::open_folder(dir);
                }
                None => self.open_dialog(Dialog::Settings(SettingsForm::first_run())),
            },
            Cmd::PageSetup => {
                let mut page = self.settings.page.clone();
                if print::page_setup(self.hwnd, &mut self.print_state, &mut page) {
                    self.change_settings(|s| s.page = page.clone());
                }
            }
            Cmd::Print => self.print(ctx),
            Cmd::Settings => self.open_dialog(Dialog::Settings(SettingsForm::from_settings(&self.settings))),
            Cmd::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),

            Cmd::Undo => self.editor.inject(key_event(Key::Z, CMD)),
            Cmd::Redo => self.editor.inject(key_event(Key::Y, CMD)),
            Cmd::Cut => self.editor.inject(Event::Cut),
            Cmd::Copy => self.editor.inject(Event::Copy),
            Cmd::Paste => {
                let text = arboard::Clipboard::new().and_then(|mut c| c.get_text());
                match text {
                    Ok(text) if !text.is_empty() => {
                        self.editor.inject(Event::Paste(crate::encoding::normalize_newlines(&text)));
                    }
                    _ => self.editor.request_focus(),
                }
            }
            Cmd::Delete => self.editor.inject(key_event(Key::Delete, Modifiers::NONE)),
            Cmd::Find => self.open_find(ctx, false),
            Cmd::Replace => self.open_find(ctx, true),
            Cmd::FindNext => self.find_step(ctx, Direction::Forward),
            Cmd::FindPrev => self.find_step(ctx, Direction::Backward),
            Cmd::GoTo => {
                let (line, _) = self.stats.line_col(self.editor.cursor(ctx));
                self.open_dialog(Dialog::GoTo(GoToForm::new(line, self.stats.line_count())));
            }
            Cmd::SelectAll => self.editor.select(0, self.stats.total_chars()),
            Cmd::TimeDate => self.editor.inject(Event::Paste(crate::time::time_date_string())),
            Cmd::ToggleOverwrite => {
                self.editor.overwrite = !self.editor.overwrite;
                self.editor.request_focus();
            }
            Cmd::FindInvisible => self.open_invisible(),

            Cmd::ToggleWrap => {
                let wrap = !self.settings.word_wrap;
                self.change_settings(|s| s.word_wrap = wrap);
            }
            Cmd::Font => {
                let picker = FontPicker::new(&self.settings.font);
                self.preview_font = picker.selected_font().cloned();
                self.fonts_dirty = true;
                self.open_dialog(Dialog::Font(Box::new(picker)));
            }
            Cmd::FormatJson => self.format_json(ctx),
            Cmd::ToggleReader => {
                if self.view_kind().is_some() {
                    let on = !self.viewer.on;
                    self.set_reading(on);
                } else {
                    self.notify("읽기 모드는 Markdown(.md)과 옵시디언 캔버스(.canvas) 파일에서 쓸 수 있습니다.".to_owned());
                }
            }
            Cmd::ZoomIn => self.editor.zoom_by(1),
            Cmd::ZoomOut => self.editor.zoom_by(-1),
            Cmd::ZoomReset => self.editor.zoom = 100,
            Cmd::ToggleStatusBar => {
                let show = !self.settings.show_status_bar;
                self.change_settings(|s| s.show_status_bar = show);
            }
            Cmd::ToggleShowInvisible => {
                let show = !self.settings.show_invisible;
                self.change_settings(|s| s.show_invisible = show);
            }
            Cmd::ToggleDark => {
                let dark = !self.settings.dark_mode;
                theme::apply(ctx, dark);
                self.change_settings(|s| s.dark_mode = dark);
            }

            Cmd::SetEncoding(encoding) => {
                self.doc.encoding = encoding;
                self.notify(format!("다음에 저장할 때 {} 인코딩으로 저장합니다.", encoding.label()));
            }
            Cmd::SetLineEnding(eol) => {
                self.doc.line_ending = eol;
                self.notify(format!("다음에 저장할 때 줄 끝을 {} 형식으로 저장합니다.", eol.label()));
            }
            Cmd::Reopen(encoding) => {
                if self.doc.path.is_some() {
                    self.confirm_then(ctx, frame, After::Reopen(encoding));
                }
            }

            Cmd::Shortcuts => self.open_dialog(Dialog::Shortcuts),
            Cmd::About => self.open_dialog(Dialog::About),
        }
    }

    /// Re-indents the document as JSON in one undoable step, or shows where it is invalid.
    fn format_json(&mut self, ctx: &egui::Context) {
        match crate::json::pretty(&self.doc.text) {
            Ok(text) if text == self.doc.text => self.notify("이미 정리된 JSON입니다.".to_owned()),
            Ok(text) => {
                self.editor.apply_edit(ctx, &mut self.doc.text, text, (0, 0));
                self.on_text_changed();
                self.editor.request_focus();
                self.notify("JSON을 정리했습니다. Ctrl+Z로 되돌릴 수 있습니다.".to_owned());
            }
            Err(err) => {
                let at = crate::json::error_char_index(&self.doc.text, err.line(), err.column());
                self.editor.select(at, at);
                // As the status bar counts: serde_json's column is in bytes, which
                // is further right than the characters once there is Korean before it.
                let (line, column) = self.stats.line_col(at);
                self.show_error(
                    "JSON을 정리할 수 없습니다",
                    &format!("{line}줄 {column}열 근처의 JSON 형식이 올바르지 않습니다. 커서를 그 위치로 옮겼습니다."),
                );
            }
        }
    }

    fn print(&mut self, ctx: &egui::Context) {
        let range = self.editor.selected_range(ctx);
        let selection = (!range.is_empty()).then_some(range);
        let title = self.doc.title();
        let job = PrintJob {
            title: &title,
            text: &self.doc.text,
            selection,
            font: &self.settings.font,
            page: &self.settings.page,
        };
        match print::print(self.hwnd, &mut self.print_state, &job) {
            Ok(PrintOutcome::Printed { pages }) => self.notify(format!("{pages}쪽을 프린터로 보냈습니다.")),
            Ok(PrintOutcome::Cancelled) => {}
            Err(err) => self.show_error("인쇄할 수 없습니다", &err),
        }
    }
}

fn key_event(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_letters_type_nothing() {
        let key = |key, modifiers| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        let text = |s: &str| egui::Event::Text(s.to_owned());
        let altgr = Modifiers { alt: true, ctrl: true, ..Default::default() };
        let events = vec![
            key(Key::Q, Modifiers::ALT),
            text("q"),
            key(Key::W, Modifiers::NONE),
            text("w"),
            key(Key::E, altgr),
            text("€"),
        ];
        let ctx = egui::Context::default();
        let mut left = Vec::new();
        let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
            drop_alt_letters(ui.ctx());
            left = ui.input(|i| i.events.iter().filter_map(|e| match e {
                egui::Event::Text(t) => Some(t.clone()),
                _ => None,
            }).collect::<Vec<_>>());
        });
        output.textures_delta.clear();
        assert_eq!(left, ["w", "€"]);
    }
}
