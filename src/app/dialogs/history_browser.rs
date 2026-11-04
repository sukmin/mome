//! The "임시 저장 기록" dialog: documents, their revisions, and a preview.

use std::path::PathBuf;

use egui::{Align, Context, FontFamily, FontId, Frame, Layout, RichText, ScrollArea, Ui, Vec2};

use super::{answer, buttons, modal, primary_button, secondary, secondary_button, Outcome};
use crate::app::MomeApp;
use crate::fonts::{self, EDITOR_FAMILY};
use crate::history::{self, DocEntry, Revision, RevisionKind};
use crate::theme::Palette;
use crate::time::LocalTime;

/// Longer revisions are cut in the preview to keep the dialog responsive.
const PREVIEW_LIMIT: usize = 256 * 1024;
const PREVIEW_FONT_SIZE: f32 = 13.0;

pub struct HistoryBrowser {
    root: PathBuf,
    /// Key of the document open in this window.
    current_key: String,
    docs: Vec<DocEntry>,
    selected_doc: Option<usize>,
    /// Revisions of the selected document, newest first.
    revisions: Vec<Revision>,
    selected_rev: Option<usize>,
    preview: String,
    /// When the dialog was opened, for "n분 전".
    now: LocalTime,
}

impl HistoryBrowser {
    pub fn new(root: PathBuf, current_key: &str) -> Self {
        let docs = history::list_docs(&root);
        let initial = docs
            .iter()
            .position(|d| d.key == current_key)
            .or(if docs.is_empty() { None } else { Some(0) });
        let mut browser = Self {
            root,
            current_key: current_key.to_owned(),
            docs,
            selected_doc: None,
            revisions: Vec::new(),
            selected_rev: None,
            preview: String::new(),
            now: LocalTime::now(),
        };
        if let Some(i) = initial {
            browser.select_doc(i);
        }
        browser
    }

    fn select_doc(&mut self, i: usize) {
        self.selected_doc = Some(i);
        self.revisions = history::list_revisions(&self.docs[i].dir);
        self.selected_rev = None;
        self.preview.clear();
        if !self.revisions.is_empty() {
            self.select_rev(0);
        }
    }

    fn select_rev(&mut self, i: usize) {
        self.selected_rev = Some(i);
        self.preview = match history::read_revision(&self.revisions[i].path) {
            Ok(mut text) => {
                if text.len() > PREVIEW_LIMIT {
                    let mut cut = PREVIEW_LIMIT;
                    while !text.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    text.truncate(cut);
                    text.push_str("\n\n… (미리 보기에는 앞부분만 표시됩니다)");
                }
                text
            }
            Err(err) => format!("이 버전을 열 수 없습니다: {err}"),
        };
    }

    pub(super) fn selected_revision(&self) -> Option<&Revision> {
        self.selected_rev.and_then(|i| self.revisions.get(i))
    }

    fn is_current_doc(&self) -> bool {
        self.selected_doc.is_some_and(|i| self.docs[i].key == self.current_key)
    }

    /// Folder of the selected document, or the whole history.
    fn selected_folder(&self) -> PathBuf {
        match self.selected_doc {
            Some(i) => self.docs[i].dir.clone(),
            None => self.root.clone(),
        }
    }
}

/// Sizes of the dialog's parts. Every part gets a fixed size derived from the window:
/// a scroll area that may take "all available space" inside a modal makes the modal
/// grow a little every frame until it runs off the screen.
struct DialogSizes {
    width: f32,
    list_height: f32,
    /// Height of each column: its heading plus the list.
    column_height: f32,
    docs_width: f32,
    revs_width: f32,
    preview_width: f32,
}

impl DialogSizes {
    fn new(ctx: &Context, gap: f32) -> Self {
        let screen = ctx.content_rect();
        let width = (screen.width() - 80.0).clamp(480.0, 900.0);
        let list_height = (screen.height() - 280.0).clamp(120.0, 400.0);
        let docs_width = (width * 0.27).round();
        let revs_width = (width * 0.25).round();
        // Two 1-pixel separators, each with spacing on both sides.
        let preview_width = (width - docs_width - revs_width - 4.0 * gap - 2.0).max(120.0);
        Self {
            width,
            list_height,
            column_height: list_height + 28.0,
            docs_width,
            revs_width,
            preview_width,
        }
    }
}

impl MomeApp {
    pub(super) fn history_ui(&self, ctx: &Context, browser: &mut HistoryBrowser) -> Outcome {
        let p = self.palette();
        let gap = ctx.global_style().spacing.item_spacing.x;
        let size = DialogSizes::new(ctx, gap);
        let (choice, dismissed) = modal(ctx, "dlg-history", size.width, "임시 저장 기록", |ui| {
            ui.label(RichText::new(browser.root.display().to_string()).small().color(p.weak));
            ui.add_space(6.0);
            let row = Vec2::new(size.width, size.column_height);
            ui.allocate_ui_with_layout(row, Layout::left_to_right(Align::Min), |ui| {
                ui.set_height(size.column_height);
                column(ui, size.docs_width, size.column_height, "문서", |ui| {
                    doc_list(ui, browser, size.docs_width, size.list_height, &p);
                });
                separator(ui, size.column_height, &p);
                column(ui, size.revs_width, size.column_height, "버전", |ui| {
                    revision_list(ui, browser, size.revs_width, size.list_height);
                });
                separator(ui, size.column_height, &p);
                column(ui, size.preview_width, size.column_height, "미리 보기", |ui| {
                    preview(ui, &browser.preview, size.preview_width, size.list_height, &p);
                });
            });
            history_buttons(ui, browser, &p)
        });
        answer(choice, dismissed, Outcome::Close)
    }

    pub(super) fn restore_revision(&mut self, ctx: &Context, rev: &Revision) {
        let text = match history::read_revision(&rev.path) {
            Ok(text) => text,
            Err(err) => {
                self.show_error("이 버전을 열 수 없습니다", &format!("{}\n\n{err}", rev.path.display()));
                return;
            }
        };
        // Keep what is on screen now, so the restore itself can be undone later too.
        self.take_snapshot(RevisionKind::Restore);
        self.editor.apply_edit(ctx, &mut self.doc.text, text, (0, 0));
        self.editor.request_focus();
        self.on_text_changed();
        self.notify(format!(
            "{} {} 버전으로 되돌렸습니다. Ctrl+Z로 실행 취소할 수 있습니다.",
            rev.time.date_string(),
            rev.time.time_string()
        ));
    }
}

/// A fixed-size column with a heading.
fn column(ui: &mut Ui, width: f32, height: f32, heading: &str, add: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(Vec2::new(width, height), Layout::top_down(Align::Min), |ui| {
        ui.set_width(width);
        ui.label(RichText::new(heading).strong());
        add(ui);
    });
}

/// A fixed-height vertical line: `ui.separator()` in a row takes all available height.
fn separator(ui: &mut Ui, height: f32, p: &Palette) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, height), egui::Sense::hover());
    ui.painter().vline(rect.center().x, rect.y_range(), egui::Stroke::new(1.0, p.border));
}

fn list_area(id: &str, width: f32, height: f32) -> ScrollArea {
    ScrollArea::vertical()
        .id_salt(id)
        .max_height(height)
        .max_width(width)
        .auto_shrink([false, false])
}

fn doc_list(ui: &mut Ui, browser: &mut HistoryBrowser, width: f32, height: f32, p: &Palette) {
    list_area("hist-docs", width, height).show(ui, |ui| {
        if browser.docs.is_empty() {
            ui.label(RichText::new("아직 임시 저장된 문서가 없습니다.").color(p.weak));
        }
        let mut clicked = None;
        for (i, doc) in browser.docs.iter().enumerate() {
            let mut title = doc.info.title.clone();
            if doc.key == browser.current_key {
                title.push_str("  (현재 문서)");
            }
            let ago = doc.latest.map(|t| t.ago(&browser.now)).unwrap_or_default();
            let text = format!("{title}\n버전 {}개 · {ago}", doc.count);
            let response = ui.selectable_label(browser.selected_doc == Some(i), text);
            let response = match &doc.info.path {
                Some(path) => response.on_hover_text(path.display().to_string()),
                None => response.on_hover_text("저장한 적 없는 문서"),
            };
            if response.clicked() {
                clicked = Some(i);
            }
        }
        if let Some(i) = clicked {
            browser.select_doc(i);
        }
    });
}

fn revision_list(ui: &mut Ui, browser: &mut HistoryBrowser, width: f32, height: f32) {
    list_area("hist-revs", width, height).show(ui, |ui| {
        let mut clicked = None;
        for (i, rev) in browser.revisions.iter().enumerate() {
            let text = format!(
                "{}  {}\n{} · {} · {}",
                rev.time.date_string(),
                rev.time.time_string(),
                rev.kind.label(),
                rev.time.ago(&browser.now),
                format_size(rev.size)
            );
            if ui.selectable_label(browser.selected_rev == Some(i), text).clicked() {
                clicked = Some(i);
            }
        }
        if let Some(i) = clicked {
            browser.select_rev(i);
        }
    });
}

fn preview(ui: &mut Ui, text: &str, width: f32, height: f32, p: &Palette) {
    Frame::NONE.fill(p.editor_bg).corner_radius(4).inner_margin(8).show(ui, |ui| {
        let inner = Vec2::new(width - 16.0, height - 16.0);
        ui.set_min_size(inner);
        ui.set_max_size(inner);
        ScrollArea::both()
            .id_salt("hist-preview")
            .max_width(inner.x)
            .max_height(inner.y)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let font = FontId::new(PREVIEW_FONT_SIZE, FontFamily::Name(EDITOR_FAMILY.into()));
                let text = RichText::new(text)
                    .font(font)
                    .line_height(Some(PREVIEW_FONT_SIZE * fonts::EDITOR_LINE_HEIGHT))
                    .color(p.editor_fg);
                ui.add(egui::Label::new(text).extend());
            });
    });
}

fn history_buttons(ui: &mut Ui, browser: &HistoryBrowser, p: &Palette) -> Option<Outcome> {
    let selected = browser.selected_revision().is_some();
    let current = browser.is_current_doc();
    buttons(ui, |ui| {
        if secondary(ui, "닫기").clicked() {
            return Some(Outcome::Close);
        }
        let open = ui
            .add_enabled(selected, secondary_button("새 창에서 열기"))
            .on_hover_text("이 버전을 새 창에서 엽니다. 그 창에서 저장하면 원래 파일에 저장됩니다.");
        if open.clicked() {
            return Some(Outcome::OpenRevision);
        }
        let restore = ui.add_enabled(selected && current, primary_button("이 버전으로 되돌리기", p));
        let restore = if current {
            restore.on_hover_text("지금 내용을 먼저 임시 저장한 뒤 되돌립니다. Ctrl+Z로 실행 취소할 수 있습니다.")
        } else {
            restore.on_disabled_hover_text("다른 문서의 버전은 [새 창에서 열기]로 열 수 있습니다.")
        };
        if restore.clicked() {
            return Some(Outcome::Restore);
        }
        // Left end of the row.
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.button("폴더 열기")
                .clicked()
                .then(|| Outcome::OpenFolder(browser.selected_folder()))
        })
        .inner
    })
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{} KB", bytes.div_ceil(1024))
    } else {
        format!("{bytes} B")
    }
}
