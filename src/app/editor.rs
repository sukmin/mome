//! The text area: an egui `TextEdit` plus what Notepad users expect from it
//! (overwrite mode, programmatic selection, undoable edits, search highlights).

use std::sync::Arc;

use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::text_edit::TextEditState;
use egui::{
    Align, Color32, Context, Event, EventFilter, FontId, Frame, Galley, Id, ImeEvent, Margin, Pos2,
    Response, ScrollArea, Stroke, StrokeKind, TextBuffer, TextEdit, Ui, Vec2,
};

use crate::find::Match;

pub const ZOOM_MIN: u32 = 10;
pub const ZOOM_MAX: u32 = 500;
pub const ZOOM_STEP: u32 = 10;
const TEXT_MARGIN: Margin = Margin {
    left: 10,
    right: 10,
    top: 6,
    bottom: 6,
};

pub struct Editor {
    pub id: Id,
    /// Overwrite ("겹쳐쓰기") instead of insert mode, toggled with the Insert key.
    pub overwrite: bool,
    /// An IME composition (e.g. a Hangul syllable being assembled) is in progress.
    composing: bool,
    pub zoom: u32,
    pending_events: Vec<Event>,
    pending_focus: bool,
    pending_select: Option<(usize, usize)>,
    scroll_to_cursor: bool,
    /// Selection (anchor, cursor) as of the last frame the text area had focus.
    /// egui collapses the selection of an unfocused `TextEdit`, but menus, the
    /// right-click menu and the find bar still need it.
    last_selection: (usize, usize),
}

/// Undo steps kept by egui (each is a full copy of the text): fewer for big documents.
fn undo_depth(text_len: usize) -> usize {
    match text_len {
        n if n > 8 * 1024 * 1024 => 5,
        n if n > 1024 * 1024 => 20,
        _ => 100,
    }
}

pub struct ShowOptions<'a> {
    pub font: FontId,
    pub wrap: bool,
    pub text_color: Color32,
    pub highlights: &'a [Match],
    pub current: Option<usize>,
    pub match_bg: Color32,
    pub current_bg: Color32,
    /// Character indices of invisible characters to mark, ascending.
    pub marks: &'a [usize],
    pub mark_color: Color32,
}

pub struct ShowOutput {
    pub changed: bool,
    pub response: Response,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            id: Id::new("mome-editor"),
            overwrite: false,
            composing: false,
            zoom: 100,
            pending_events: Vec::new(),
            pending_focus: true,
            pending_select: None,
            scroll_to_cursor: false,
            last_selection: (0, 0),
        }
    }

    /// Gives the text area keyboard focus on the next frame, with its last selection.
    pub fn request_focus(&mut self) {
        self.pending_focus = true;
    }

    /// Feeds an input event (undo, cut, paste, …) to the text area on the next frame.
    /// The selection it had before losing focus (e.g. to a menu) is restored first.
    pub fn inject(&mut self, event: Event) {
        self.pending_events.push(event);
        self.pending_focus = true;
    }

    /// Selects a character range on the next frame and scrolls it into view.
    pub fn select(&mut self, from: usize, to: usize) {
        self.pending_select = Some((from, to));
        self.pending_focus = true;
    }

    /// Like [`Self::select`] but leaves keyboard focus where it is (e.g. in the find bar).
    pub fn select_quiet(&mut self, from: usize, to: usize) {
        self.pending_select = Some((from, to));
    }

    fn stored_selection(&self, ctx: &Context) -> Option<(usize, usize)> {
        TextEditState::load(ctx, self.id)
            .and_then(|s| s.cursor.char_range())
            .map(|r| (r.secondary.index.0, r.primary.index.0))
    }

    fn store_selection(&self, ctx: &Context, (a, b): (usize, usize)) {
        let mut state = TextEditState::load(ctx, self.id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(CCursorRange::two(CCursor::new(a), CCursor::new(b))));
        state.store(ctx, self.id);
    }

    /// Current selection as (anchor, cursor) character indices, also while another
    /// widget (menu, find bar) has the focus.
    pub fn selection(&self, ctx: &Context) -> (usize, usize) {
        if let Some(selection) = self.pending_select {
            return selection;
        }
        if ctx.memory(|m| m.has_focus(self.id)) {
            if let Some(selection) = self.stored_selection(ctx) {
                return selection;
            }
        }
        self.last_selection
    }

    /// Character index of the text cursor (the moving end of the selection).
    pub fn cursor(&self, ctx: &Context) -> usize {
        self.selection(ctx).1
    }

    /// Selection as an ordered range.
    pub fn selected_range(&self, ctx: &Context) -> std::ops::Range<usize> {
        let (a, b) = self.selection(ctx);
        a.min(b)..a.max(b)
    }

    /// Forgets undo history and puts the cursor at `cursor` (used when a document is loaded).
    pub fn reset(&mut self, ctx: &Context, cursor: usize, text_len: usize) {
        let mut state = TextEditState::load(ctx, self.id).unwrap_or_default();
        state.set_undoer(egui::util::undoer::Undoer::with_settings(egui::util::undoer::Settings {
            max_undos: undo_depth(text_len),
            ..Default::default()
        }));
        state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(cursor))));
        state.store(ctx, self.id);
        self.composing = false;
        self.pending_events.clear();
        self.pending_select = None;
        self.last_selection = (cursor, cursor);
        self.scroll_to_cursor = true;
        self.pending_focus = true;
    }

    pub fn zoom_by(&mut self, steps: i32) {
        let z = self.zoom as i32 + steps * ZOOM_STEP as i32;
        self.zoom = z.clamp(ZOOM_MIN as i32, ZOOM_MAX as i32) as u32;
    }

    /// Replaces the whole text as one undoable step and selects `selection` afterwards.
    pub fn apply_edit(&mut self, ctx: &Context, text: &mut String, new_text: String, selection: (usize, usize)) {
        let mut state = TextEditState::load(ctx, self.id).unwrap_or_default();
        let before = state.cursor.char_range().unwrap_or_default();
        let after = CCursorRange::two(CCursor::new(selection.0), CCursor::new(selection.1));
        let mut undoer = state.undoer();
        undoer.add_undo(&(before, text.clone()));
        if new_text != *text {
            // A new edit makes what was undone before it unreachable, as in any editor;
            // otherwise Ctrl+Y would bring it back over this edit. `redo` with a state
            // other than the last undo point clears the redo list.
            let _ = undoer.redo(&(after, new_text.clone()));
        }
        *text = new_text;
        undoer.add_undo(&(after, text.clone()));
        state.set_undoer(undoer);
        state.cursor.set_char_range(Some(after));
        state.store(ctx, self.id);
        self.pending_select = None;
        self.last_selection = selection;
        self.scroll_to_cursor = true;
    }

    /// Runs before the ui pass: overwrite mode, injected events and pending selection.
    /// Returns true if `text` was modified.
    pub fn before_frame(&mut self, ctx: &Context, text: &mut String) -> bool {
        let mut changed = false;
        // Ctrl+V: winit turns only "\r\n" into "\n"; a lone CR must become a line
        // break too, or it would be saved as a bare CR (menu paste does the same).
        ctx.input_mut(|i| {
            for event in &mut i.events {
                if let Event::Paste(pasted) = event {
                    if pasted.contains('\r') {
                        *pasted = crate::encoding::normalize_newlines(pasted);
                    }
                }
            }
        });
        let has_focus = ctx.memory(|m| m.has_focus(self.id));
        let range = TextEditState::load(ctx, self.id).and_then(|s| s.cursor.char_range());
        if range.is_some_and(|r| r.is_empty()) {
            // A composition always has a non-empty preedit range.
            self.composing = false;
        }

        // Count how many characters the typed input will add, tracking IME state.
        let was_composing = self.composing;
        let mut composing = self.composing;
        let mut typed = 0usize;
        ctx.input(|i| {
            for event in &i.events {
                match event {
                    Event::Text(t) if !composing && t != "\n" && t != "\r" => typed += t.chars().count(),
                    Event::Ime(ImeEvent::Preedit { text, .. }) => {
                        if !text.is_empty() && !composing {
                            typed += 1; // A new syllable starts and takes one cell.
                        }
                        composing = !text.is_empty();
                    }
                    Event::Ime(ImeEvent::Commit(_)) => composing = false,
                    _ => {}
                }
            }
        });
        self.composing = composing;

        if self.overwrite && has_focus && typed > 0 {
            if let Some(range) = range {
                let [lo, hi] = range.sorted_cursors();
                // While composing, the preedit text is "selected"; overwrite after it.
                let at = if was_composing {
                    Some(hi.index.0)
                } else if range.is_empty() {
                    Some(lo.index.0)
                } else {
                    None // A real selection is simply replaced by the typed text.
                };
                if let Some(at) = at {
                    changed |= delete_forward_in_line(text, at, typed);
                }
            }
        }

        if let Some(selection) = self.pending_select.take() {
            self.store_selection(ctx, selection);
            self.last_selection = selection;
            self.scroll_to_cursor = true;
        } else if self.pending_focus && !has_focus {
            // Coming back from a menu, dialog or the find bar: bring the selection back.
            self.store_selection(ctx, self.last_selection);
        }
        if self.pending_focus {
            ctx.memory_mut(|m| m.request_focus(self.id));
            self.pending_focus = false;
        }
        if !self.pending_events.is_empty() {
            let events = std::mem::take(&mut self.pending_events);
            ctx.input_mut(|i| i.events.extend(events));
        }
        changed
    }

    pub fn show(&mut self, ui: &mut Ui, text: &mut String, opts: &ShowOptions) -> ShowOutput {
        let wrap = opts.wrap;
        let mut layouter = |ui: &Ui, buf: &dyn TextBuffer, wrap_width: f32| -> Arc<Galley> {
            let job = layout_job(buf.as_str(), opts, if wrap { wrap_width } else { f32::INFINITY });
            ui.fonts_mut(|f| f.layout_job(job))
        };

        let scroll = if wrap {
            ScrollArea::vertical()
        } else {
            ScrollArea::both()
        };
        let id = self.id;
        let scroll_to_cursor = std::mem::take(&mut self.scroll_to_cursor);
        let ctx = ui.ctx().clone();
        let selection_before = if ctx.memory(|m| m.has_focus(id)) {
            self.stored_selection(&ctx).unwrap_or(self.last_selection)
        } else {
            self.last_selection
        };
        let output = scroll
            .id_salt("mome-editor-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let available = ui.available_size();
                let margins = TEXT_MARGIN.sum();
                let width = if wrap {
                    available.x
                } else {
                    // Without wrapping the text can be wider than the window; size the
                    // widget to the text so the horizontal scroll bar works.
                    let galley = layouter(ui, &*text as &dyn TextBuffer, f32::INFINITY);
                    (galley.size().x + margins.x + 20.0).max(available.x)
                };
                let edit = TextEdit::multiline(text)
                    .id(id)
                    .font(opts.font.clone())
                    .text_color(opts.text_color)
                    .frame(Frame::NONE.inner_margin(TEXT_MARGIN))
                    .event_filter(EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    })
                    .desired_width(width)
                    .min_size(Vec2::new(width, available.y))
                    .layouter(&mut layouter);
                let out = edit.show(ui);
                paint_marks(ui, &out.galley, out.galley_pos, opts.marks, opts.mark_color);
                if scroll_to_cursor {
                    if let Some(range) = TextEditState::load(ui.ctx(), id).and_then(|s| s.cursor.char_range()) {
                        let rect = out
                            .galley
                            .pos_from_cursor(range.primary)
                            .translate(out.galley_pos.to_vec2());
                        ui.scroll_to_rect(rect.expand(4.0), Some(Align::Center));
                    }
                }
                out
            })
            .inner;

        let response = output.response.response;
        let right_click = response.hovered()
            && ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary));
        if right_click {
            // egui moves the cursor on any mouse button; a right-click must keep the
            // selection so the context menu can cut or copy it.
            self.store_selection(&ctx, selection_before);
            self.last_selection = selection_before;
        } else if response.has_focus() {
            if let Some(selection) = self.stored_selection(&ctx) {
                self.last_selection = selection;
            }
        }
        ShowOutput {
            changed: response.changed(),
            response,
        }
    }
}

fn layout_job(text: &str, opts: &ShowOptions, wrap_width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;
    job.keep_trailing_whitespace = true;
    let base = TextFormat {
        font_id: opts.font.clone(),
        color: opts.text_color,
        line_height: Some(opts.font.size * crate::fonts::EDITOR_LINE_HEIGHT),
        ..Default::default()
    };
    // Matches may be one frame behind the text while typing; never slice with them then.
    let valid = !opts.highlights.is_empty()
        && opts.highlights.iter().all(|m| {
            m.bytes.end <= text.len()
                && text.is_char_boundary(m.bytes.start)
                && text.is_char_boundary(m.bytes.end)
        });
    if !valid {
        job.append(text, 0.0, base);
        return job;
    }
    let mut last = 0;
    for (i, m) in opts.highlights.iter().enumerate() {
        if m.bytes.start < last || m.bytes.is_empty() {
            continue;
        }
        if m.bytes.start > last {
            job.append(&text[last..m.bytes.start], 0.0, base.clone());
        }
        let mut format = base.clone();
        format.background = if Some(i) == opts.current {
            opts.current_bg
        } else {
            opts.match_bg
        };
        job.append(&text[m.bytes.clone()], 0.0, format);
        last = m.bytes.end;
    }
    if last < text.len() || text.is_empty() {
        job.append(&text[last..], 0.0, base);
    }
    job
}

/// Marks the invisible characters at character indices `marks` (ascending) that are
/// on screen: a box around those that take up space, and a bar with a small
/// triangle on top where a zero-width one sits.
fn paint_marks(ui: &Ui, galley: &Galley, origin: Pos2, marks: &[usize], color: Color32) {
    if marks.is_empty() {
        return;
    }
    let clip = ui.clip_rect();
    let painter = ui.painter();
    let stroke = Stroke::new(1.5, color);
    let mut next = 0; // Index into `marks`.
    let mut row_start = 0; // Character index of the row's first character.
    for row in &galley.rows {
        let row_end = row_start + row.char_count_including_newline().0;
        let row_rect = row.rect().translate(origin.to_vec2());
        if row_rect.top() > clip.bottom() || next >= marks.len() {
            break;
        }
        if row_rect.bottom() < clip.top() {
            next += marks[next..].partition_point(|&m| m < row_end);
        } else {
            while next < marks.len() && marks[next] < row_end {
                if let Some(glyph) = row.glyphs.get(marks[next] - row_start) {
                    let r = glyph.logical_rect().translate(row.pos.to_vec2() + origin.to_vec2());
                    if r.width() >= 3.0 {
                        painter.rect(r.shrink(1.0), 2.0, color.gamma_multiply(0.18), stroke, StrokeKind::Inside);
                    } else {
                        let x = r.left();
                        painter.line_segment([Pos2::new(x, r.top() + 2.0), Pos2::new(x, r.bottom() - 2.0)], stroke);
                        let top = r.top() + 1.0;
                        painter.add(egui::Shape::convex_polygon(
                            vec![Pos2::new(x - 4.0, top), Pos2::new(x + 4.0, top), Pos2::new(x, top + 5.0)],
                            color,
                            Stroke::NONE,
                        ));
                    }
                }
                next += 1;
            }
        }
        row_start = row_end;
    }
}

/// Deletes up to `count` characters starting at character index `at`, stopping at a
/// line break (overwrite mode never joins lines). Returns true if anything was removed.
fn delete_forward_in_line(text: &mut String, at: usize, count: usize) -> bool {
    let Some((start, _)) = text.char_indices().nth(at) else {
        return false; // At the very end: nothing to overwrite.
    };
    let mut end = start;
    for (taken, (i, c)) in text[start..].char_indices().enumerate() {
        if taken == count || c == '\n' {
            break;
        }
        end = start + i + c.len_utf8();
    }
    if end > start {
        text.replace_range(start..end, "");
        true
    } else {
        false
    }
}

/// Character index → byte index (clamped to the end).
pub fn byte_index(text: &str, char_index: usize) -> usize {
    text.char_indices().nth(char_index).map_or(text.len(), |(b, _)| b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overwrite_stays_in_line() {
        let mut s = "abc\ndef".to_owned();
        assert!(delete_forward_in_line(&mut s, 1, 5));
        assert_eq!(s, "a\ndef");
        let mut s = "가나다".to_owned();
        assert!(delete_forward_in_line(&mut s, 0, 1));
        assert_eq!(s, "나다");
        let mut s = "ab".to_owned();
        assert!(!delete_forward_in_line(&mut s, 2, 1));
        let mut s = "a\nb".to_owned();
        assert!(!delete_forward_in_line(&mut s, 1, 1));
    }

    #[test]
    fn byte_indices() {
        assert_eq!(byte_index("가a", 1), 3);
        assert_eq!(byte_index("가a", 5), 4);
    }
}
