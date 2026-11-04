//! Menu bar, plus the dark-mode and JSON 정리 buttons in the top-right corner.

use egui::{Align, Button, Color32, Frame, Layout, Margin, RichText, Sense, Ui, Vec2};

use super::commands::shortcut_text;
use super::{Cmd, MomeApp};
use crate::encoding::{Encoding, LineEnding};
use crate::theme;

const MENU_WIDTH: f32 = 250.0;

impl MomeApp {
    pub(super) fn menu_bar(&mut self, ui: &mut Ui) {
        let p = self.palette();
        egui::Panel::top("mome-menu")
            .frame(Frame::NONE.fill(p.bar_bg).inner_margin(Margin::symmetric(4, 2)))
            .show(ui, |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    ui.menu_button("파일", |ui| self.file_menu(ui));
                    ui.menu_button("편집", |ui| self.edit_menu(ui));
                    ui.menu_button("서식", |ui| self.format_menu(ui));
                    ui.menu_button("보기", |ui| self.view_menu(ui));
                    ui.menu_button("인코딩", |ui| self.encoding_menu(ui));
                    ui.menu_button("도움말", |ui| self.help_menu(ui));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        self.theme_toggle(ui);
                        if self.is_json() {
                            self.json_button(ui);
                        }
                    });
                });
            });
    }

    /// A plain menu item with its shortcut on the right.
    fn item(&mut self, ui: &mut Ui, label: &str, cmd: Cmd, enabled: bool) {
        let shortcut = shortcut_text(&cmd);
        let mut button = Button::new(label);
        if !shortcut.is_empty() {
            button = button.shortcut_text(shortcut);
        }
        if ui.add_enabled(enabled, button).clicked() {
            self.pending_cmds.push(cmd);
            ui.close();
        }
    }

    /// A menu item with a check mark column.
    fn check_item(&mut self, ui: &mut Ui, label: &str, checked: bool, cmd: Cmd) {
        let mark_color = if checked {
            ui.visuals().text_color()
        } else {
            Color32::TRANSPARENT
        };
        let shortcut = shortcut_text(&cmd);
        let mut button = Button::new((RichText::new("✔").color(mark_color), label));
        if !shortcut.is_empty() {
            button = button.shortcut_text(shortcut);
        }
        if ui.add(button).clicked() {
            self.pending_cmds.push(cmd);
            ui.close();
        }
    }

    fn file_menu(&mut self, ui: &mut Ui) {
        ui.set_min_width(MENU_WIDTH);
        self.item(ui, "새로 만들기", Cmd::New, true);
        self.item(ui, "새 창", Cmd::NewWindow, true);
        self.item(ui, "열기…", Cmd::Open, true);
        let recent = self.settings.recent_files.clone();
        ui.add_enabled_ui(!recent.is_empty(), |ui| {
            ui.menu_button("최근에 연 파일", |ui| {
                ui.set_min_width(MENU_WIDTH);
                for (i, path) in recent.iter().enumerate() {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string());
                    let button = Button::new(format!("{}  {name}", i + 1));
                    if ui.add(button).on_hover_text(path.display().to_string()).clicked() {
                        self.pending_cmds.push(Cmd::OpenPath(path.clone()));
                        ui.close();
                    }
                }
                ui.separator();
                self.item(ui, "목록 지우기", Cmd::ClearRecent, true);
            });
        });
        self.item(ui, "저장", Cmd::Save, true);
        self.item(ui, "다른 이름으로 저장…", Cmd::SaveAs, true);
        ui.separator();
        let has_temp = self.settings.temp_dir.is_some();
        self.item(ui, "지금 임시 저장", Cmd::SnapshotNow, true);
        self.item(ui, "임시 저장 기록…", Cmd::History, true);
        self.item(ui, "임시 저장 폴더 열기", Cmd::OpenTempFolder, has_temp);
        let autosave = self.settings.autosave_original;
        self.check_item(ui, "원본 파일에도 자동 저장", autosave, Cmd::ToggleAutosave);
        ui.separator();
        self.item(ui, "페이지 설정…", Cmd::PageSetup, true);
        self.item(ui, "인쇄…", Cmd::Print, true);
        ui.separator();
        self.item(ui, "임시 저장 설정…", Cmd::Settings, true);
        ui.separator();
        self.item(ui, "끝내기", Cmd::Exit, true);
    }

    fn edit_menu(&mut self, ui: &mut Ui) {
        ui.set_min_width(MENU_WIDTH);
        let has_selection = !self.editor.selected_range(ui.ctx()).is_empty();
        self.item(ui, "실행 취소", Cmd::Undo, true);
        self.item(ui, "다시 실행", Cmd::Redo, true);
        ui.separator();
        self.item(ui, "잘라내기", Cmd::Cut, has_selection);
        self.item(ui, "복사", Cmd::Copy, has_selection);
        self.item(ui, "붙여넣기", Cmd::Paste, true);
        self.item(ui, "삭제", Cmd::Delete, has_selection);
        ui.separator();
        self.item(ui, "찾기…", Cmd::Find, true);
        self.item(ui, "다음 찾기", Cmd::FindNext, true);
        self.item(ui, "이전 찾기", Cmd::FindPrev, true);
        self.item(ui, "바꾸기…", Cmd::Replace, true);
        self.item(ui, "줄로 이동…", Cmd::GoTo, true);
        ui.separator();
        self.item(ui, "모두 선택", Cmd::SelectAll, true);
        self.item(ui, "시간/날짜", Cmd::TimeDate, true);
        ui.separator();
        let overwrite = self.editor.overwrite;
        self.check_item(ui, "겹쳐쓰기 모드", overwrite, Cmd::ToggleOverwrite);
    }

    /// The right-click menu of the text.
    pub(super) fn context_menu(&mut self, response: &egui::Response) {
        let has_selection = !self.editor.selected_range(&response.ctx).is_empty();
        response.context_menu(|ui| {
            ui.set_min_width(180.0);
            self.item(ui, "실행 취소", Cmd::Undo, true);
            self.item(ui, "다시 실행", Cmd::Redo, true);
            ui.separator();
            self.item(ui, "잘라내기", Cmd::Cut, has_selection);
            self.item(ui, "복사", Cmd::Copy, has_selection);
            self.item(ui, "붙여넣기", Cmd::Paste, true);
            self.item(ui, "삭제", Cmd::Delete, has_selection);
            ui.separator();
            self.item(ui, "모두 선택", Cmd::SelectAll, true);
        });
    }

    fn format_menu(&mut self, ui: &mut Ui) {
        ui.set_min_width(MENU_WIDTH);
        let wrap = self.settings.word_wrap;
        self.check_item(ui, "자동 줄 바꿈", wrap, Cmd::ToggleWrap);
        self.item(ui, "글꼴…", Cmd::Font, true);
        ui.separator();
        let json = self.is_json();
        self.item(ui, "JSON 정리", Cmd::FormatJson, json);
    }

    fn view_menu(&mut self, ui: &mut Ui) {
        ui.set_min_width(MENU_WIDTH);
        ui.menu_button("확대/축소", |ui| {
            ui.set_min_width(MENU_WIDTH);
            self.item(ui, "확대", Cmd::ZoomIn, true);
            self.item(ui, "축소", Cmd::ZoomOut, true);
            self.item(ui, "기본 크기로 복원", Cmd::ZoomReset, true);
        });
        let status = self.settings.show_status_bar;
        self.check_item(ui, "상태 표시줄", status, Cmd::ToggleStatusBar);
        let dark = self.settings.dark_mode;
        self.check_item(ui, "다크 모드", dark, Cmd::ToggleDark);
    }

    pub(super) fn encoding_items(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("저장할 인코딩").small().weak());
        let current = self.doc.encoding;
        for encoding in Encoding::ALL {
            self.check_item(ui, &encoding.menu_label(), current == encoding, Cmd::SetEncoding(encoding));
        }
    }

    pub(super) fn line_ending_items(&mut self, ui: &mut Ui) {
        let current = self.doc.line_ending;
        for eol in LineEnding::ALL {
            self.check_item(ui, eol.label(), current == eol, Cmd::SetLineEnding(eol));
        }
    }

    pub(super) fn reopen_menu(&mut self, ui: &mut Ui) {
        let has_file = self.doc.path.is_some();
        ui.add_enabled_ui(has_file, |ui| {
            ui.menu_button("다른 인코딩으로 다시 열기", |ui| {
                ui.set_min_width(200.0);
                // No "UTF-8 (BOM)": reading UTF-8 with or without a BOM is the same.
                for encoding in [Encoding::Utf8, Encoding::Ansi, Encoding::Utf16Le, Encoding::Utf16Be] {
                    self.item(ui, &encoding.menu_label(), Cmd::Reopen(encoding), true);
                }
            });
        });
    }

    fn encoding_menu(&mut self, ui: &mut Ui) {
        ui.set_min_width(MENU_WIDTH);
        self.encoding_items(ui);
        ui.separator();
        ui.menu_button("줄 끝 형식", |ui| {
            ui.set_min_width(200.0);
            self.line_ending_items(ui);
        });
        self.reopen_menu(ui);
    }

    fn help_menu(&mut self, ui: &mut Ui) {
        ui.set_min_width(MENU_WIDTH);
        self.item(ui, "단축키", Cmd::Shortcuts, true);
        self.item(ui, "mome 정보", Cmd::About, true);
    }

    /// Whether 서식 > JSON 정리 applies: a .json file, or untitled text that starts like JSON.
    fn is_json(&self) -> bool {
        match &self.doc.path {
            Some(path) => path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")),
            None => self.doc.text.trim_start().starts_with(['{', '[']),
        }
    }

    /// The "JSON 정리" button next to the theme switch, shown for JSON documents.
    fn json_button(&mut self, ui: &mut Ui) {
        let tip = "JSON을 들여쓰기해 보기 좋게 정리합니다. 키 순서와 값은 그대로 두며, Ctrl+Z로 되돌릴 수 있습니다.";
        if ui.button("{ } JSON 정리").on_hover_text(tip).clicked() {
            self.pending_cmds.push(Cmd::FormatJson);
        }
    }

    /// The one-click dark/light switch at the right end of the menu bar.
    fn theme_toggle(&mut self, ui: &mut Ui) {
        let p = self.palette();
        let dark = self.settings.dark_mode;
        let (rect, response) = ui.allocate_exact_size(Vec2::new(36.0, 26.0), Sense::click());
        let bg = if response.hovered() { p.hover } else { p.bar_bg };
        ui.painter().rect_filled(rect, 5.0, bg);
        let fg = ui.visuals().text_color();
        if dark {
            theme::paint_sun(ui.painter(), rect.center(), 20.0, fg);
        } else {
            theme::paint_moon(ui.painter(), rect.center(), 20.0, fg, bg);
        }
        let tip = if dark { "라이트 모드로 전환" } else { "다크 모드로 전환" };
        if response.on_hover_text(tip).clicked() {
            self.pending_cmds.push(Cmd::ToggleDark);
        }
    }
}
