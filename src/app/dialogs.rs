//! Modal dialogs.
//!
//! Every frame [`MomeApp::dialogs`] takes the open dialog out of `self`, so that its
//! UI can borrow the dialog and the app at the same time, shows it and gets back an
//! [`Outcome`]. [`MomeApp::finish_dialog`] then puts the dialog back, closes it, or
//! does what the user chose.

mod history_browser;

use std::path::PathBuf;

use egui::{
    Align, Button, Context, FontFamily, FontId, Frame, Id, Key, Layout, Margin, Modal, RichText, ScrollArea,
    TextEdit, Ui, Vec2,
};

pub use history_browser::HistoryBrowser;

use super::commands::SHORTCUT_HELP;
use super::{After, MomeApp};
use crate::document::DiskStamp;
use crate::encoding::Encoding;
use crate::fonts::{self, SystemFont, PREVIEW_FAMILY};
use crate::history::{self, RevisionKind};
use crate::paths;
use crate::session::{self, SessionRecord};
use crate::settings::{self, FontChoice, Settings};
use crate::theme::Palette;
use crate::win;
use crate::APP_VERSION;

pub enum Dialog {
    SavePrompt { after: After },
    GoTo(GoToForm),
    Settings(SettingsForm),
    History(Box<HistoryBrowser>),
    Font(Box<FontPicker>),
    Message { title: String, body: String },
    Reload,
    Recover { records: Vec<SessionRecord> },
    EncodingFallback { sample: char, path: PathBuf, after: Option<After> },
    /// Saving over a file that had bytes we could not read.
    LossySave { path: PathBuf, after: Option<After> },
    Shortcuts,
    About,
}

pub struct GoToForm {
    input: String,
    max: usize,
    error: Option<String>,
    focus: bool,
}

impl GoToForm {
    pub fn new(current: usize, max: usize) -> Self {
        Self {
            input: current.to_string(),
            max,
            error: None,
            focus: true,
        }
    }
}

pub struct SettingsForm {
    first_run: bool,
    temp_dir: String,
    interval: u64,
    retention: u32,
    autosave: bool,
    error: Option<String>,
}

impl SettingsForm {
    pub fn first_run() -> Self {
        let defaults = Settings::default();
        Self {
            first_run: true,
            temp_dir: paths::default_temp_dir().display().to_string(),
            interval: defaults.snapshot_interval_secs,
            retention: defaults.retention_days,
            autosave: false,
            error: None,
        }
    }

    pub fn from_settings(s: &Settings) -> Self {
        Self {
            first_run: false,
            temp_dir: s
                .temp_dir
                .clone()
                .unwrap_or_else(paths::default_temp_dir)
                .display()
                .to_string(),
            interval: s.snapshot_interval_secs,
            retention: s.retention_days,
            autosave: s.autosave_original,
            error: None,
        }
    }
}

pub struct FontPicker {
    fonts: Vec<SystemFont>,
    filter: String,
    selected: Option<usize>,
    size: f32,
    scroll_to_selected: bool,
}

impl FontPicker {
    pub fn new(current: &FontChoice) -> Self {
        let fonts = fonts::list_system_fonts();
        let selected = fonts.iter().position(|f| f.name.eq_ignore_ascii_case(&current.family));
        Self {
            fonts,
            filter: String::new(),
            selected,
            size: current.size,
            scroll_to_selected: true,
        }
    }

    pub fn selected_font(&self) -> Option<&SystemFont> {
        self.selected.and_then(|i| self.fonts.get(i))
    }
}

/// What the open dialog asks for after this frame.
enum Outcome {
    /// Nothing chosen yet: show the dialog again next frame.
    StayOpen,
    Close,
    Save(After),
    DontSave(After),
    GoToLine(usize),
    ApplySettings,
    BrowseFolder,
    Restore,
    OpenRevision,
    OpenFolder(PathBuf),
    PreviewFont,
    ApplyFont,
    CancelFont,
    Reload,
    KeepMine,
    Recover,
    Discard,
    SaveAsUtf8,
    SaveLossy,
}

/// The dialog's answer: the button the user chose, `on_dismiss` if they pressed Esc
/// or clicked outside the dialog, or else [`Outcome::StayOpen`].
fn answer(choice: Option<Outcome>, dismissed: bool, on_dismiss: Outcome) -> Outcome {
    match choice {
        Some(outcome) => outcome,
        None if dismissed => on_dismiss,
        None => Outcome::StayOpen,
    }
}

/// Shows a centred modal with a title. Returns the content result and whether
/// the user dismissed it (Esc or a click outside).
fn modal<R>(ctx: &Context, id: &str, width: f32, title: &str, add: impl FnOnce(&mut Ui) -> R) -> (R, bool) {
    let frame = Frame::window(&ctx.global_style()).inner_margin(Margin::same(18));
    // Never wider than the window (minus the frame margins).
    let width = width.min(ctx.content_rect().width() - 72.0).max(200.0);
    let response = Modal::new(Id::new(id)).frame(frame).show(ctx, |ui| {
        ui.set_width(width);
        ui.label(RichText::new(title).size(17.0).strong());
        ui.add_space(10.0);
        add(ui)
    });
    let dismissed = response.should_close();
    (response.inner, dismissed)
}

fn enter_pressed(ctx: &Context) -> bool {
    ctx.input(|i| i.key_pressed(Key::Enter))
}

const BUTTON_SIZE: Vec2 = Vec2::new(88.0, 30.0);

/// The highlighted button of a dialog (usually what Enter does).
fn primary_button<'a>(text: &'a str, p: &Palette) -> Button<'a> {
    Button::new(RichText::new(text).color(p.on_accent))
        .fill(p.accent)
        .min_size(BUTTON_SIZE)
}

fn secondary_button(text: &str) -> Button<'_> {
    Button::new(text).min_size(BUTTON_SIZE)
}

fn primary(ui: &mut Ui, text: &str, p: &Palette) -> egui::Response {
    ui.add(primary_button(text, p))
}

fn secondary(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(secondary_button(text))
}

/// Right-aligned button row (first button added is the rightmost).
fn buttons<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.add_space(14.0);
    ui.with_layout(Layout::right_to_left(Align::Center), add).inner
}

/// A small grey line under the main text.
fn hint(ui: &mut Ui, text: &str, p: &Palette) {
    ui.label(RichText::new(text).small().color(p.weak));
}

impl MomeApp {
    pub(super) fn dialogs(&mut self, ctx: &Context, frame: &mut eframe::Frame) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        // Keys typed while a dialog is open must not reach the text.
        ctx.memory_mut(|m| m.surrender_focus(self.editor.id));

        let outcome = match &mut dialog {
            Dialog::SavePrompt { after } => self.save_prompt_ui(ctx, after),
            Dialog::GoTo(form) => self.goto_ui(ctx, form),
            Dialog::Settings(form) => self.settings_ui(ctx, form),
            Dialog::History(browser) => self.history_ui(ctx, browser),
            Dialog::Font(picker) => self.font_ui(ctx, picker),
            Dialog::Message { title, body } => self.message_ui(ctx, title, body),
            Dialog::Reload => self.reload_ui(ctx),
            Dialog::Recover { records } => self.recover_ui(ctx, records),
            Dialog::EncodingFallback { sample, .. } => self.encoding_fallback_ui(ctx, *sample),
            Dialog::LossySave { .. } => self.lossy_save_ui(ctx),
            Dialog::Shortcuts => self.shortcuts_ui(ctx),
            Dialog::About => self.about_ui(ctx),
        };
        self.finish_dialog(ctx, frame, dialog, outcome);
    }

    fn save_prompt_ui(&self, ctx: &Context, after: &After) -> Outcome {
        let p = self.palette();
        let enter = enter_pressed(ctx);
        let title = self.doc.title();
        let (choice, dismissed) = modal(ctx, "dlg-save", 400.0, "mome", |ui| {
            ui.label(format!("변경 내용을 '{title}'에 저장하시겠습니까?"));
            if self.settings.temp_dir.is_some() {
                ui.add_space(4.0);
                hint(ui, "저장하지 않아도 지금 내용은 임시 저장 기록에 남습니다.", &p);
            }
            buttons(ui, |ui| {
                if secondary(ui, "취소").clicked() {
                    return Some(Outcome::Close);
                }
                if secondary(ui, "저장 안 함").clicked() {
                    return Some(Outcome::DontSave(after.clone()));
                }
                if primary(ui, "저장", &p).clicked() || enter {
                    return Some(Outcome::Save(after.clone()));
                }
                None
            })
        });
        answer(choice, dismissed, Outcome::Close)
    }

    fn goto_ui(&self, ctx: &Context, form: &mut GoToForm) -> Outcome {
        let p = self.palette();
        let enter = enter_pressed(ctx);
        let (choice, dismissed) = modal(ctx, "dlg-goto", 300.0, "줄로 이동", |ui| {
            ui.label(format!("줄 번호(1~{})", form.max));
            let field = ui.add(TextEdit::singleline(&mut form.input).desired_width(f32::INFINITY));
            if form.focus {
                field.request_focus();
                form.focus = false;
            }
            if let Some(error) = &form.error {
                ui.label(RichText::new(error).color(p.warning).small());
            }
            let submit = field.lost_focus() && enter;
            buttons(ui, |ui| {
                if secondary(ui, "취소").clicked() {
                    return Some(Outcome::Close);
                }
                if primary(ui, "이동", &p).clicked() || submit {
                    match form.input.trim().parse::<usize>() {
                        Ok(n) if (1..=form.max).contains(&n) => return Some(Outcome::GoToLine(n)),
                        _ => {
                            form.error = Some(format!("1부터 {}까지의 숫자를 입력하세요.", form.max));
                            form.focus = true;
                        }
                    }
                }
                None
            })
        });
        answer(choice, dismissed, Outcome::Close)
    }

    fn settings_ui(&self, ctx: &Context, form: &mut SettingsForm) -> Outcome {
        let p = self.palette();
        let first_run = form.first_run;
        let title = if first_run { "mome에 오신 것을 환영합니다" } else { "임시 저장 설정" };
        let (choice, dismissed) = modal(ctx, "dlg-settings", 500.0, title, |ui| {
            if first_run {
                ui.label("편집 중인 문서를 주기적으로 임시 저장할 폴더를 선택하세요.");
                hint(
                    ui,
                    "내용이 변경되면 설정한 간격마다 새 버전으로 저장하며, 이전 버전을 덮어쓰거나 지우지 않습니다. \
                     보관 기간이 지난 버전만 자동으로 삭제합니다.",
                    &p,
                );
                ui.add_space(8.0);
            }
            ui.label(RichText::new("임시 저장 폴더").strong());
            let mut browse = false;
            ui.horizontal(|ui| {
                ui.add(TextEdit::singleline(&mut form.temp_dir).desired_width(340.0));
                browse = ui.button("찾아보기…").clicked();
            });
            ui.add_space(8.0);
            egui::Grid::new("settings-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("임시 저장 간격");
                ui.horizontal(|ui| {
                    let range = settings::MIN_SNAPSHOT_SECS..=settings::MAX_SNAPSHOT_SECS;
                    ui.add(egui::DragValue::new(&mut form.interval).range(range));
                    ui.label("초 (내용이 변경되었을 때만)");
                });
                ui.end_row();
                ui.label("보관 기간");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut form.retention).range(1..=settings::MAX_RETENTION_DAYS));
                    ui.label(format!("일 (최대 {}일)", settings::MAX_RETENTION_DAYS));
                });
                ui.end_row();
            });
            ui.add_space(4.0);
            ui.checkbox(&mut form.autosave, "파일로 저장한 문서는 원본 파일에도 자동 저장 (임시 저장 간격마다)");
            if let Some(error) = &form.error {
                ui.add_space(4.0);
                ui.label(RichText::new(error).color(p.warning));
            }
            if browse {
                return Some(Outcome::BrowseFolder);
            }
            buttons(ui, |ui| {
                if !first_run && secondary(ui, "취소").clicked() {
                    return Some(Outcome::Close);
                }
                let label = if first_run { "시작하기" } else { "확인" };
                if primary(ui, label, &p).clicked() {
                    return Some(Outcome::ApplySettings);
                }
                None
            })
        });
        // The first-run choice cannot be skipped by pressing Esc.
        answer(choice, dismissed && !first_run, Outcome::Close)
    }

    fn font_ui(&self, ctx: &Context, picker: &mut FontPicker) -> Outcome {
        let p = self.palette();
        let (choice, dismissed) = modal(ctx, "dlg-font", 560.0, "글꼴", |ui| {
            let filter_hint = "글꼴 이름으로 찾기";
            ui.add(TextEdit::singleline(&mut picker.filter).hint_text(filter_hint).desired_width(f32::INFINITY));
            ui.add_space(6.0);
            let mut picked = None;
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(240.0);
                    if font_list(ui, picker) {
                        picked = Some(Outcome::PreviewFont);
                    }
                });
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("크기");
                        let size = egui::DragValue::new(&mut picker.size).range(settings::FONT_SIZES);
                        ui.add(size.speed(0.5).suffix(" pt"));
                    });
                    ui.add_space(6.0);
                    font_preview(ui, picker.size, &p);
                    hint(ui, "한글이 없는 글꼴은 한글만 맑은 고딕으로 표시합니다.", &p);
                });
            });
            let button = buttons(ui, |ui| {
                if secondary(ui, "취소").clicked() {
                    return Some(Outcome::CancelFont);
                }
                if primary(ui, "확인", &p).clicked() {
                    return Some(Outcome::ApplyFont);
                }
                None
            });
            button.or(picked)
        });
        answer(choice, dismissed, Outcome::CancelFont)
    }

    fn message_ui(&self, ctx: &Context, title: &str, body: &str) -> Outcome {
        let p = self.palette();
        let enter = enter_pressed(ctx);
        let (choice, dismissed) = modal(ctx, "dlg-message", 420.0, title, |ui| {
            ui.label(body);
            buttons(ui, |ui| (primary(ui, "확인", &p).clicked() || enter).then_some(Outcome::Close))
        });
        answer(choice, dismissed, Outcome::Close)
    }

    fn reload_ui(&self, ctx: &Context) -> Outcome {
        let p = self.palette();
        let title = self.doc.title();
        let (choice, dismissed) = modal(ctx, "dlg-reload", 440.0, "파일이 변경되었습니다", |ui| {
            ui.label(format!("'{title}' 파일이 다른 프로그램에서 변경되었습니다. 다시 불러오시겠습니까?"));
            if self.doc.is_modified() {
                ui.add_space(4.0);
                let warning = "다시 불러오면 지금 편집 중인 내용은 임시 저장 기록에만 남습니다.";
                ui.label(RichText::new(warning).small().color(p.warning));
            }
            buttons(ui, |ui| {
                if secondary(ui, "현재 내용 유지").clicked() {
                    return Some(Outcome::KeepMine);
                }
                if primary(ui, "다시 불러오기", &p).clicked() {
                    return Some(Outcome::Reload);
                }
                None
            })
        });
        answer(choice, dismissed, Outcome::KeepMine)
    }

    fn recover_ui(&self, ctx: &Context, records: &[SessionRecord]) -> Outcome {
        let p = self.palette();
        let enter = enter_pressed(ctx);
        let (choice, dismissed) = modal(ctx, "dlg-recover", 460.0, "작업 복구", |ui| {
            ui.label("mome가 지난번에 정상적으로 종료되지 않았습니다. 저장하지 않은 문서를 복구할 수 있습니다.");
            ui.add_space(6.0);
            for record in records {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&record.title).strong());
                    ui.label(RichText::new(revision_time(record)).small().color(p.weak));
                });
                if let Some(path) = &record.path {
                    hint(ui, &path.display().to_string(), &p);
                }
            }
            ui.add_space(4.0);
            if records.len() > 1 {
                hint(ui, "문서가 여러 개이면 나머지는 새 창에서 엽니다.", &p);
            }
            hint(ui, "복구하지 않아도 내용은 [임시 저장 기록]에 남아 있습니다.", &p);
            buttons(ui, |ui| {
                if secondary(ui, "복구 안 함").clicked() {
                    return Some(Outcome::Discard);
                }
                if primary(ui, "복구", &p).clicked() || enter {
                    return Some(Outcome::Recover);
                }
                None
            })
        });
        // A click outside or Esc only puts the question off to the next start;
        // "복구 안 함" is the one way to let the text go.
        answer(choice, dismissed, Outcome::Close)
    }

    fn encoding_fallback_ui(&self, ctx: &Context, sample: char) -> Outcome {
        let p = self.palette();
        let enter = enter_pressed(ctx);
        let label = self.doc.encoding.label();
        let (choice, dismissed) = modal(ctx, "dlg-encoding", 440.0, "저장할 수 없는 문자", |ui| {
            ui.label(format!(
                "현재 인코딩({label})으로는 '{sample}' 등 일부 문자를 저장할 수 없습니다.\nUTF-8로 바꿔서 저장하시겠습니까?"
            ));
            buttons(ui, |ui| {
                if secondary(ui, "취소").clicked() {
                    return Some(Outcome::Close);
                }
                if primary(ui, "UTF-8로 저장", &p).clicked() || enter {
                    return Some(Outcome::SaveAsUtf8);
                }
                None
            })
        });
        answer(choice, dismissed, Outcome::Close)
    }

    fn lossy_save_ui(&self, ctx: &Context) -> Outcome {
        let p = self.palette();
        let enter = enter_pressed(ctx);
        let title = self.doc.title();
        let label = self.doc.encoding.label();
        let unread = self.doc.unread;
        let (choice, dismissed) = modal(ctx, "dlg-lossy", 460.0, "원래 내용이 손상될 수 있습니다", |ui| {
            if unread {
                ui.label(format!(
                    "'{title}' 파일을 읽지 못한 채로 열려 있습니다. \
                     이대로 저장하면 파일의 원래 내용이 지금 화면의 내용으로 바뀝니다."
                ));
                ui.add_space(4.0);
                hint(ui, "드라이브가 연결되었다면 [파일 > 열기]로 파일을 다시 열어 보세요.", &p);
            } else {
                ui.label(format!(
                    "'{title}' 파일에는 {label} 인코딩으로 읽지 못한 내용이 있어 '�'로 표시되어 있습니다. \
                     이대로 저장하면 그 부분의 원래 내용이 사라집니다."
                ));
                ui.add_space(4.0);
                hint(ui, "텍스트 파일이라면 [인코딩 > 다른 인코딩으로 다시 열기]를 먼저 사용해 보세요.", &p);
            }
            // The safe choice is the highlighted one here.
            buttons(ui, |ui| {
                if primary(ui, "취소", &p).clicked() || enter {
                    return Some(Outcome::Close);
                }
                if secondary(ui, "그래도 저장").clicked() {
                    return Some(Outcome::SaveLossy);
                }
                None
            })
        });
        answer(choice, dismissed, Outcome::Close)
    }

    fn shortcuts_ui(&self, ctx: &Context) -> Outcome {
        let p = self.palette();
        let enter = enter_pressed(ctx);
        // The list scrolls when the window is too short for it (title and button stay).
        let list_height = (ctx.content_rect().height() - 200.0).max(160.0);
        let (choice, dismissed) = modal(ctx, "dlg-shortcuts", 520.0, "단축키", |ui| {
            ScrollArea::vertical().max_height(list_height).show(ui, |ui| {
                egui::Grid::new("shortcut-grid").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                    for (keys, what) in SHORTCUT_HELP {
                        ui.label(RichText::new(*keys).monospace());
                        ui.label(*what);
                        ui.end_row();
                    }
                });
            });
            buttons(ui, |ui| (primary(ui, "닫기", &p).clicked() || enter).then_some(Outcome::Close))
        });
        answer(choice, dismissed, Outcome::Close)
    }

    fn about_ui(&self, ctx: &Context) -> Outcome {
        let p = self.palette();
        let enter = enter_pressed(ctx);
        let temp_dir = match &self.settings.temp_dir {
            Some(dir) => dir.display().to_string(),
            None => "(설정되지 않음)".to_owned(),
        };
        let (choice, dismissed) = modal(ctx, "dlg-about", 440.0, "mome 정보", |ui| {
            ui.label(RichText::new(format!("mome {APP_VERSION}")).size(15.0).strong());
            ui.label("메모장을 살짝 비튼 가벼운 텍스트 편집기");
            ui.add_space(8.0);
            let rows = [
                ("임시 저장 폴더", temp_dir),
                ("설정 파일", paths::settings_file().display().to_string()),
                ("로그 파일", paths::log_file().display().to_string()),
            ];
            for (name, value) in rows {
                hint(ui, name, &p);
                // Long paths wrap instead of widening the dialog past the window.
                ui.add(egui::Label::new(value).wrap());
                ui.add_space(2.0);
            }
            buttons(ui, |ui| (primary(ui, "닫기", &p).clicked() || enter).then_some(Outcome::Close))
        });
        answer(choice, dismissed, Outcome::Close)
    }

    fn close_dialog(&mut self) {
        self.dialog = self.dialog_queue.pop_front();
        if self.dialog.is_none() {
            self.editor.request_focus();
        }
    }

    fn finish_dialog(&mut self, ctx: &Context, frame: &mut eframe::Frame, dialog: Dialog, outcome: Outcome) {
        match outcome {
            Outcome::StayOpen => self.dialog = Some(dialog),
            Outcome::Close => self.close_dialog(),
            Outcome::Save(after) => {
                self.close_dialog();
                self.save(ctx, frame, Some(after));
            }
            Outcome::DontSave(after) => {
                self.close_dialog();
                self.discard_then(ctx, after);
            }
            Outcome::GoToLine(line) => {
                self.close_dialog();
                if let Some(start) = self.stats.line_start(line) {
                    self.editor.select(start, start);
                }
            }
            Outcome::BrowseFolder => {
                if let Dialog::Settings(mut form) = dialog {
                    let mut picker = rfd::FileDialog::new().set_title("임시 저장 폴더 선택").set_parent(frame);
                    let current = PathBuf::from(form.temp_dir.trim());
                    if current.is_dir() {
                        picker = picker.set_directory(&current);
                    }
                    if let Some(dir) = picker.pick_folder() {
                        form.temp_dir = dir.display().to_string();
                        form.error = None;
                    }
                    self.dialog = Some(Dialog::Settings(form));
                }
            }
            Outcome::ApplySettings => {
                if let Dialog::Settings(mut form) = dialog {
                    match self.apply_settings(&form) {
                        Ok(()) => self.close_dialog(),
                        Err(error) => {
                            form.error = Some(error);
                            self.dialog = Some(Dialog::Settings(form));
                        }
                    }
                }
            }
            Outcome::Restore => {
                if let Dialog::History(browser) = dialog {
                    if let Some(rev) = browser.selected_revision().cloned() {
                        self.close_dialog();
                        self.restore_revision(ctx, &rev);
                    } else {
                        self.dialog = Some(Dialog::History(browser));
                    }
                }
            }
            Outcome::OpenRevision => {
                if let Dialog::History(browser) = dialog {
                    if let Some(rev) = browser.selected_revision() {
                        win::spawn_self(&["--revision".as_ref(), rev.path.as_os_str()]);
                    }
                    self.dialog = Some(Dialog::History(browser));
                }
            }
            Outcome::OpenFolder(dir) => {
                win::open_folder(&dir);
                self.dialog = Some(dialog);
            }
            Outcome::PreviewFont => {
                if let Dialog::Font(picker) = &dialog {
                    self.preview_font = picker.selected_font().cloned();
                    self.fonts_dirty = true;
                }
                self.dialog = Some(dialog);
            }
            Outcome::ApplyFont => {
                if let Dialog::Font(picker) = dialog {
                    let font = match picker.selected_font() {
                        Some(font) => FontChoice {
                            family: font.name.clone(),
                            file: Some(font.file.clone()),
                            index: font.index,
                            size: picker.size,
                        },
                        None => FontChoice {
                            size: picker.size,
                            ..self.settings.font.clone()
                        },
                    };
                    self.change_settings(|s| s.font = font.clone());
                }
                self.preview_font = None;
                self.fonts_dirty = true;
                self.close_dialog();
            }
            Outcome::CancelFont => {
                self.preview_font = None;
                self.fonts_dirty = true;
                self.close_dialog();
            }
            Outcome::Reload => {
                self.close_dialog();
                if let Some(path) = self.doc.path.clone() {
                    if self.snapshot_sync(RevisionKind::Reload) {
                        self.open_path(ctx, &path, None);
                    } else {
                        self.show_error(
                            "다시 불러오지 않았습니다",
                            "지금 내용을 임시 저장 폴더에 남기지 못해서, 다시 불러오면 사라지게 됩니다. \
                             [다른 이름으로 저장]으로 먼저 저장해 주세요.",
                        );
                    }
                }
            }
            Outcome::KeepMine => {
                self.close_dialog();
                if let Some(path) = &self.doc.path {
                    self.doc.disk_stamp = DiskStamp::of(path);
                }
            }
            Outcome::Recover => {
                if let Dialog::Recover { records } = dialog {
                    self.close_dialog();
                    self.recover(ctx, records);
                }
            }
            Outcome::Discard => {
                if let Dialog::Recover { records } = dialog {
                    for r in &records {
                        session::delete_record(&r.sid);
                    }
                }
                self.close_dialog();
            }
            Outcome::SaveAsUtf8 => {
                if let Dialog::EncodingFallback { path, after, .. } = dialog {
                    self.close_dialog();
                    self.doc.encoding = Encoding::Utf8;
                    self.save_to(ctx, &path, after);
                }
            }
            Outcome::SaveLossy => {
                if let Dialog::LossySave { path, after } = dialog {
                    self.close_dialog();
                    // The user accepted that unreadable bytes become '�' (or that
                    // the unread file is replaced).
                    self.doc.lossy = false;
                    self.doc.unread = false;
                    self.save_to(ctx, &path, after);
                }
            }
        }
    }

    fn apply_settings(&mut self, form: &SettingsForm) -> Result<(), String> {
        let text = form.temp_dir.trim();
        let dir = PathBuf::from(text);
        if text.is_empty() || !dir.is_absolute() {
            return Err("폴더의 전체 경로를 입력하세요. (예: D:\\mome-temp)".to_owned());
        }
        std::fs::create_dir_all(&dir).map_err(|e| format!("폴더를 만들 수 없습니다: {e}"))?;
        let probe = dir.join(".mome-write-test");
        std::fs::write(&probe, b"ok").map_err(|e| format!("이 폴더에 파일을 쓸 수 없습니다: {e}"))?;
        let _ = std::fs::remove_file(&probe);

        let old = self.settings.temp_dir.clone();
        let interval = form
            .interval
            .clamp(settings::MIN_SNAPSHOT_SECS, settings::MAX_SNAPSHOT_SECS);
        let retention = form.retention.clamp(1, settings::MAX_RETENTION_DAYS);
        let autosave = form.autosave;
        self.change_settings(|s| {
            s.temp_dir = Some(dir.clone());
            s.snapshot_interval_secs = interval;
            s.retention_days = retention;
            s.autosave_original = autosave;
        });
        if old.as_ref() != Some(&dir) {
            self.snapshot.last_hash = None;
            self.last_cleanup = None;
            if self.doc.is_modified() {
                self.snapshot.dirty_since.get_or_insert_with(std::time::Instant::now);
            }
            if let Some(old) = old {
                self.notify(format!(
                    "이제 새 폴더에 임시 저장합니다. 이전 기록은 {}에 그대로 있습니다.",
                    old.display()
                ));
            }
        }
        Ok(())
    }
}

/// The font list of the font dialog, filtered by name. Returns whether the user
/// picked another font.
fn font_list(ui: &mut Ui, picker: &mut FontPicker) -> bool {
    let mut picked = false;
    let filter = picker.filter.to_lowercase();
    ScrollArea::vertical()
        .id_salt("font-list")
        .max_height(280.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, font) in picker.fonts.iter().enumerate() {
                let name = font.display_name();
                if !filter.is_empty() && !name.to_lowercase().contains(&filter) {
                    continue;
                }
                let selected = picker.selected == Some(i);
                let response = ui.selectable_label(selected, name);
                if picker.scroll_to_selected && selected {
                    response.scroll_to_me(Some(Align::Center));
                }
                if response.clicked() && !selected {
                    picker.selected = Some(i);
                    picked = true;
                }
            }
        });
    picker.scroll_to_selected = false;
    picked
}

/// Sample text in the font being previewed (installed as [`PREVIEW_FAMILY`]).
fn font_preview(ui: &mut Ui, size: f32, p: &Palette) {
    const SAMPLE: &str = "가나다라마바사 아자차카타파하\n다람쥐 헌 쳇바퀴에 타고파\nThe quick brown fox\n0123456789 (){}[] .,;:'\"";
    Frame::NONE
        .fill(p.editor_bg)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(4)
        .inner_margin(10)
        .show(ui, |ui| {
            ui.set_min_size(Vec2::new(260.0, 230.0));
            let font = FontId::new(size, FontFamily::Name(PREVIEW_FAMILY.into()));
            ui.label(RichText::new(SAMPLE).font(font).color(p.editor_fg));
        });
}

/// When the unsaved text of a crashed session was last written, for the recovery list.
fn revision_time(record: &SessionRecord) -> String {
    record
        .revision
        .as_deref()
        .and_then(|path| path.file_name())
        .and_then(|name| history::parse_revision_name(&name.to_string_lossy()))
        .map(|(time, _)| time.display())
        .unwrap_or_default()
}
