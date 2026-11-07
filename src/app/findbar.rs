//! Find and replace bar under the menu.

use egui::text::{CCursor, CCursorRange};
use egui::text_edit::TextEditState;
use egui::{Align, Frame, Id, Key, Layout, Margin, RichText, Sense, Stroke, TextEdit, Ui, Vec2};

use super::MomeApp;
use crate::find::{self, FindOptions, Match};
use crate::settings::FindSettings;

const QUERY_ID: &str = "mome-find-query";
const REPLACE_ID: &str = "mome-find-replace";
/// Width of the "찾을 내용" / "바꿀 내용" labels.
const LABEL_WIDTH: f32 = 64.0;
/// Width of the match count ("3/12") next to the query.
const COUNT_WIDTH: f32 = 64.0;
/// A longer selection is not taken as the query when the bar opens.
const MAX_QUERY_FROM_SELECTION: usize = 200;

/// The find bar's text fields.
#[derive(Clone, Copy, PartialEq)]
enum Field {
    Query,
    Replacement,
}

/// Is `id` one of the find bar's text fields?
pub fn is_field(id: Id) -> bool {
    id == Id::new(QUERY_ID) || id == Id::new(REPLACE_ID)
}

#[derive(Clone, Copy, PartialEq)]
pub enum Direction {
    Forward,
    Backward,
}

/// Where the keyboard focus goes when a match is selected.
#[derive(Clone, Copy, PartialEq)]
enum FocusAfter {
    /// Into the text (F3 pressed in the text).
    Text,
    /// Stays where it is, e.g. in the find bar while the user types the query.
    Unchanged,
}

#[derive(Default)]
pub struct FindBar {
    pub open: bool,
    pub replace_mode: bool,
    pub query: String,
    pub replacement: String,
    pub options: FindSettings,
    pub matches: Vec<Match>,
    /// What `matches` was computed for: (text version, query, options).
    key: Option<(u64, String, FindOptions)>,
    /// Index into `matches` of the highlighted match.
    pub current: Option<usize>,
    /// Field to focus on the next frame.
    focus: Option<Field>,
    /// Select the whole query once its field has focus (egui collapses a selection
    /// set on an unfocused field).
    select_query: bool,
    pub message: Option<String>,
}

impl FindBar {
    pub fn invalidate(&mut self) {
        self.key = None;
        self.current = None;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.current = None;
        self.message = None;
    }

    fn find_options(&self) -> FindOptions {
        FindOptions {
            match_case: self.options.match_case,
            whole_word: self.options.whole_word,
        }
    }

    /// Recomputes matches if the text, query or options changed.
    pub fn refresh(&mut self, text: &str, version: u64) {
        let key = (version, self.query.clone(), self.find_options());
        if self.key.as_ref() == Some(&key) {
            return;
        }
        self.matches = find::find_all(text, &self.query, self.find_options());
        self.key = Some(key);
        self.current = None;
    }

    fn count_text(&self) -> String {
        if self.query.is_empty() {
            String::new()
        } else if self.matches.is_empty() {
            "결과 없음".to_owned()
        } else {
            let total = if self.matches.len() >= find::MAX_MATCHES {
                format!("{}+", self.matches.len())
            } else {
                self.matches.len().to_string()
            };
            match self.current {
                Some(i) => format!("{}/{total}", i + 1),
                None => format!("{total}개"),
            }
        }
    }
}

/// Selects all text in a single-line field, so typing replaces it.
fn select_all_in(ctx: &egui::Context, id: Id, text: &str) {
    let mut state = TextEditState::load(ctx, id).unwrap_or_default();
    let end = CCursor::new(text.chars().count());
    state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), end)));
    state.store(ctx, id);
}

impl MomeApp {
    /// Ctrl+F / Ctrl+H.
    pub(super) fn open_find(&mut self, ctx: &egui::Context, replace: bool) {
        let range = self.editor.selected_range(ctx);
        if !range.is_empty() && range.len() < MAX_QUERY_FROM_SELECTION {
            let start = super::editor::byte_index(&self.doc.text, range.start);
            let end = super::editor::byte_index(&self.doc.text, range.end);
            let selected = &self.doc.text[start..end];
            if !selected.contains('\n') {
                self.find.query = selected.to_owned();
            }
        }
        self.find.open = true;
        self.find.replace_mode = replace;
        self.find.message = None;
        self.find.focus = Some(Field::Query);
        self.find.select_query = true;
    }

    /// Does a find bar field have keyboard focus?
    pub(super) fn find_has_focus(&self, ctx: &egui::Context) -> bool {
        self.find.open && ctx.memory(|m| m.focused().is_some_and(is_field))
    }

    /// F3 / Shift+F3: moves to the next match. From the text the focus stays in the
    /// text; from the find bar it stays in the find bar.
    pub(super) fn find_step(&mut self, ctx: &egui::Context, direction: Direction) {
        if self.find.query.is_empty() {
            self.open_find(ctx, false);
            return;
        }
        let focus = if self.find_has_focus(ctx) { FocusAfter::Unchanged } else { FocusAfter::Text };
        self.find_from_selection(ctx, direction, focus);
    }

    /// The highlighted match, if it still belongs to the current text.
    fn current_match(&self) -> Option<Match> {
        self.find.current.and_then(|i| self.find.matches.get(i)).cloned()
    }

    fn find_from_selection(&mut self, ctx: &egui::Context, direction: Direction, focus: FocusAfter) {
        self.find.refresh(&self.doc.text, self.text_version);
        // Continue from the highlighted match; the text's own selection is only a
        // fallback because egui forgets it while the find bar has the focus.
        let selection = match self.current_match() {
            Some(m) => m.chars,
            None => self.editor.selected_range(ctx),
        };
        self.go_to_match(selection, direction, focus);
    }

    /// Selects the match after (or before) `selection` and reports wrapping or failure.
    fn go_to_match(&mut self, selection: std::ops::Range<usize>, direction: Direction, focus: FocusAfter) {
        let forward = direction == Direction::Forward;
        let wrap = self.find.options.wrap_around;
        match find::next_match(&self.find.matches, selection.clone(), forward, wrap) {
            Some(i) => {
                let m = self.find.matches[i].chars.clone();
                let wrapped = if forward {
                    m.start < selection.end
                } else {
                    m.end > selection.start
                };
                self.find.current = Some(i);
                match focus {
                    FocusAfter::Text => self.editor.select(m.start, m.end),
                    FocusAfter::Unchanged => self.editor.select_quiet(m.start, m.end),
                }
                self.find.message = wrapped.then(|| {
                    if forward {
                        "문서 끝에 도달하여 처음부터 다시 찾았습니다.".to_owned()
                    } else {
                        "문서 처음에 도달하여 끝부터 다시 찾았습니다.".to_owned()
                    }
                });
            }
            None => {
                let text = if self.find.matches.is_empty() {
                    format!("'{}'을(를) 찾을 수 없습니다.", self.find.query)
                } else if forward {
                    "문서 끝까지 찾았습니다.".to_owned()
                } else {
                    "문서 처음까지 찾았습니다.".to_owned()
                };
                if !self.find.open {
                    self.notify(text.clone());
                }
                self.find.message = Some(text);
            }
        }
    }

    fn replace_one(&mut self, ctx: &egui::Context) {
        self.find.refresh(&self.doc.text, self.text_version);
        let selection = self.editor.selected_range(ctx);
        let hit = self
            .current_match()
            .or_else(|| self.find.matches.iter().find(|m| m.chars == selection).cloned());
        let Some(m) = hit else {
            // No match highlighted yet: first show the next one, like Notepad.
            self.go_to_match(selection, Direction::Forward, FocusAfter::Unchanged);
            return;
        };
        let mut text = self.doc.text.clone();
        text.replace_range(m.bytes.clone(), &self.find.replacement);
        let end = m.chars.start + self.find.replacement.chars().count();
        self.editor.apply_edit(ctx, &mut self.doc.text, text, (end, end));
        self.on_text_changed();
        self.find.refresh(&self.doc.text, self.text_version);
        self.go_to_match(end..end, Direction::Forward, FocusAfter::Unchanged);
    }

    fn replace_all(&mut self, ctx: &egui::Context) {
        // Not the highlighted matches, which stop at find::MAX_MATCHES.
        let matches = find::find_every(&self.doc.text, &self.find.query, self.find.find_options());
        let count = matches.len();
        if count == 0 {
            self.find.message = Some(format!("'{}'을(를) 찾을 수 없습니다.", self.find.query));
            return;
        }
        let text = find::replace_all(&self.doc.text, &matches, &self.find.replacement);
        // Keep the cursor next to the same text: shift it by what changed before it.
        let cursor = self.editor.cursor(ctx);
        let replacement_len = self.find.replacement.chars().count() as isize;
        let shift: isize = matches
            .iter()
            .filter(|m| m.chars.end <= cursor)
            .map(|m| replacement_len - m.chars.len() as isize)
            .sum();
        let cursor = ((cursor as isize + shift).max(0) as usize).min(text.chars().count());
        self.editor.apply_edit(ctx, &mut self.doc.text, text, (cursor, cursor));
        self.on_text_changed();
        // Put the focus in the text so Ctrl+Z undoes the replacement, not the field.
        self.editor.request_focus();
        self.find.message = Some(format!("{count}개 항목을 바꿨습니다. Ctrl+Z로 실행 취소할 수 있습니다."));
    }

    pub(super) fn find_bar(&mut self, ui: &mut Ui) {
        let p = self.palette();
        let ctx = ui.ctx().clone();
        let mut action: Option<FindAction> = None;
        egui::Panel::top("mome-find-bar")
            .frame(Frame::NONE.fill(p.bar_bg).inner_margin(Margin::symmetric(8, 5)))
            .show(ui, |ui| {
                let field_width = (ui.available_width() - 360.0).clamp(120.0, 260.0);
                ui.horizontal(|ui| {
                    ui.add_sized([LABEL_WIDTH, 24.0], egui::Label::new("찾을 내용"));
                    let query = ui.add(
                        TextEdit::singleline(&mut self.find.query)
                            .id(Id::new(QUERY_ID))
                            .desired_width(field_width),
                    );
                    if self.find.focus == Some(Field::Query) {
                        query.request_focus();
                        self.find.focus = None;
                    } else if self.find.select_query && query.has_focus() {
                        select_all_in(&ctx, Id::new(QUERY_ID), &self.find.query);
                        self.find.select_query = false;
                        ui.ctx().request_repaint();
                    }
                    if query.changed() {
                        action = Some(FindAction::Incremental);
                    }
                    if query.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        let back = ui.input(|i| i.modifiers.shift);
                        action = Some(if back { FindAction::Prev } else { FindAction::Next });
                        self.find.focus = Some(Field::Query);
                    }
                    ui.add_sized(
                        [COUNT_WIDTH, 24.0],
                        egui::Label::new(RichText::new(self.find.count_text()).color(p.weak)),
                    );
                    if ui.button("이전").on_hover_text("이전 찾기 (Shift+F3)").clicked() {
                        action = Some(FindAction::Prev);
                    }
                    if ui.button("다음").on_hover_text("다음 찾기 (F3)").clicked() {
                        action = Some(FindAction::Next);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if close_button(ui, p.weak).on_hover_text("닫기 (Esc)").clicked() {
                            action = Some(FindAction::Close);
                        }
                        let label = if self.find.replace_mode { "바꾸기 숨기기" } else { "바꾸기 표시" };
                        if ui.button(label).clicked() {
                            self.find.replace_mode = !self.find.replace_mode;
                            if self.find.replace_mode {
                                self.find.focus = Some(Field::Replacement);
                            }
                        }
                    });
                });
                if self.find.replace_mode {
                    ui.horizontal(|ui| {
                        ui.add_sized([LABEL_WIDTH, 24.0], egui::Label::new("바꿀 내용"));
                        let replace = ui.add(
                            TextEdit::singleline(&mut self.find.replacement)
                                .id(Id::new(REPLACE_ID))
                                .desired_width(field_width),
                        );
                        if self.find.focus == Some(Field::Replacement) {
                            replace.request_focus();
                            self.find.focus = None;
                        }
                        if replace.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            action = Some(FindAction::Replace);
                            self.find.focus = Some(Field::Replacement);
                        }
                        // Line the buttons up with "이전" / "다음" above.
                        ui.add_space(COUNT_WIDTH + ui.spacing().item_spacing.x);
                        if ui.button("바꾸기").clicked() {
                            action = Some(FindAction::Replace);
                        }
                        if ui.button("모두 바꾸기").clicked() {
                            action = Some(FindAction::ReplaceAll);
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.add_space(LABEL_WIDTH + ui.spacing().item_spacing.x);
                    let before = self.find.options.clone();
                    ui.toggle_value(&mut self.find.options.match_case, "대/소문자 구분");
                    ui.toggle_value(&mut self.find.options.whole_word, "단어 단위로");
                    ui.toggle_value(&mut self.find.options.wrap_around, "끝에서 처음으로 이어 찾기")
                        .on_hover_text("문서 끝(또는 처음)에 도달하면 반대쪽부터 계속 찾습니다.");
                    if before != self.find.options {
                        action = Some(FindAction::Incremental);
                    }
                    if let Some(message) = &self.find.message {
                        ui.add_space(8.0);
                        ui.add(egui::Label::new(RichText::new(message).color(p.weak).small()).truncate());
                    }
                });
            });

        match action {
            Some(FindAction::Next) => self.find_from_selection(&ctx, Direction::Forward, FocusAfter::Unchanged),
            Some(FindAction::Prev) => self.find_from_selection(&ctx, Direction::Backward, FocusAfter::Unchanged),
            Some(FindAction::Incremental) => {
                self.find.message = None;
                self.find.refresh(&self.doc.text, self.text_version);
                if !self.find.query.is_empty() {
                    // Search again from where the current selection starts.
                    let start = self.editor.selected_range(&ctx).start;
                    self.go_to_match(start..start, Direction::Forward, FocusAfter::Unchanged);
                }
            }
            Some(FindAction::Replace) => self.replace_one(&ctx),
            Some(FindAction::ReplaceAll) => self.replace_all(&ctx),
            Some(FindAction::Close) => {
                self.find.close();
                self.editor.request_focus();
            }
            None => {}
        }
    }
}

enum FindAction {
    Next,
    Prev,
    Incremental,
    Replace,
    ReplaceAll,
    Close,
}

pub(super) fn close_button(ui: &mut Ui, color: egui::Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 4.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let c = rect.center();
    let d = 5.0;
    let stroke = Stroke::new(1.5, color);
    ui.painter().line_segment([c + Vec2::new(-d, -d), c + Vec2::new(d, d)], stroke);
    ui.painter().line_segment([c + Vec2::new(-d, d), c + Vec2::new(d, -d)], stroke);
    response
}
