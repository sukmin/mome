//! Find and replace bar under the menu, modelled on IntelliJ's: option toggles next
//! to the query (Alt+C / Alt+O / Alt+X), regular expressions with group references
//! in the replacement, a preview of the replacement under the highlighted match, and
//! Replace / Replace All / Exclude (Alt+P / Alt+A / Alt+L).

use std::ops::Range;
use std::rc::Rc;

use egui::text::{CCursor, CCursorRange};
use egui::text_edit::TextEditState;
use egui::{
    Align, Event, Frame, Id, Key, Layout, Margin, RichText, Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2,
};

use super::MomeApp;
use crate::find::{self, FindOptions, Match, Search};
use crate::settings::FindSettings;

const QUERY_ID: &str = "mome-find-query";
const REPLACE_ID: &str = "mome-find-replace";
/// Width of the "찾을 내용" / "바꿀 내용" labels.
const LABEL_WIDTH: f32 = 64.0;
/// Width of the match count ("3/12") next to the query.
const COUNT_WIDTH: f32 = 64.0;
/// Size of the option toggles next to the query.
const TOGGLE_SIZE: Vec2 = Vec2::new(28.0, 24.0);
/// Width of the first row without the query field (measured with Malgun Gothic).
const FIXED_WIDTH: f32 = 540.0;
/// Width of "바꾸기", "모두 바꾸기" and "제외" with their gaps.
const REPLACE_BUTTONS_WIDTH: f32 = 220.0;
/// A longer selection is not taken as the query when the bar opens.
const MAX_QUERY_FROM_SELECTION: usize = 200;

const REGEX_HELP: &str = "정규식 (Alt+X)\n\
\n\
찾을 내용\n\
  .  아무 글자     \\d 숫자     \\w 글자     \\s 공백\n\
  ^  줄 처음     $  줄 끝     \\b 단어 경계\n\
  (…) 그룹     (?<이름>…) 이름 붙인 그룹     \\1 앞 그룹 다시\n\
  (?=…) (?!…) (?<=…) (?<!…) 앞뒤 살피기\n\
\n\
바꿀 내용\n\
  $1 $2 …  그룹     ${이름}  이름 붙인 그룹     $0  찾은 전체\n\
  \\n 줄 바꿈     \\t 탭     \\$ \\\\  글자 그대로\n\
  \\u \\l  다음 한 글자를 대문자/소문자로\n\
  \\U … \\E   \\L … \\E  구간을 대문자/소문자로";

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

/// What `matches` was computed for: (text version, query, options).
type FindKey = (u64, String, FindOptions);
/// What the replacement preview was worked out for: the search, the highlighted
/// match, whether it is excluded, and the replacement.
type PreviewKey = (Option<FindKey>, Option<usize>, bool, String);

#[derive(Default)]
pub struct FindBar {
    pub open: bool,
    pub replace_mode: bool,
    pub query: String,
    pub replacement: String,
    pub options: FindSettings,
    pub matches: Vec<Match>,
    key: Option<FindKey>,
    /// Index into `matches` of the highlighted match.
    pub current: Option<usize>,
    /// Field to focus on the next frame.
    focus: Option<Field>,
    /// Select the whole query once its field has focus (egui collapses a selection
    /// set on an unfocused field).
    select_query: bool,
    pub message: Option<String>,
    /// The compiled query and what it was compiled from.
    compiled: Option<(String, FindOptions, Result<Rc<Search>, String>)>,
    /// Why the query cannot be searched (an invalid regular expression).
    pub error: Option<String>,
    /// Matches left out of replacing (Alt+L), as byte ranges in the text of
    /// `excluded_version`.
    excluded_ranges: Vec<Range<usize>>,
    excluded_version: u64,
    /// For each of `matches`: is it excluded?
    pub excluded: Vec<bool>,
    /// With a regular expression: what the highlighted match becomes, and the
    /// character index where it starts.
    pub preview: Option<(usize, String)>,
    preview_key: Option<PreviewKey>,
    /// Why the replacement cannot be used.
    pub replace_error: Option<String>,
    /// An action asked for with a keyboard shortcut, run by the bar.
    pending: Option<FindAction>,
    /// The selected text the query was taken from, while the query is still that
    /// text (escaped when regular expressions are on).
    taken_from_selection: Option<String>,
}

impl FindBar {
    pub fn invalidate(&mut self) {
        self.key = None;
        self.current = None;
        self.pending = None;
    }

    /// Closing ends the search: exclusions and any action not yet run go.
    pub fn close(&mut self) {
        self.open = false;
        self.current = None;
        self.message = None;
        self.pending = None;
        self.excluded_ranges.clear();
        self.key = None;
    }

    /// Regular expressions were switched on or off: a query taken from the
    /// selection follows, escaped or not, so it still finds that text.
    fn regex_toggled(&mut self) {
        let Some(raw) = &self.taken_from_selection else {
            return;
        };
        let escaped = find::escape_regex(raw);
        if self.options.regex && self.query == *raw {
            self.query = escaped;
        } else if !self.options.regex && self.query == escaped {
            self.query = raw.clone();
        }
    }

    fn find_options(&self) -> FindOptions {
        FindOptions {
            match_case: self.options.match_case,
            whole_word: self.options.whole_word,
            regex: self.options.regex,
        }
    }

    /// The query, compiled once for each query and options.
    fn search(&mut self) -> Result<Rc<Search>, String> {
        let opts = self.find_options();
        let fresh = matches!(&self.compiled, Some((q, o, _)) if *q == self.query && *o == opts);
        if !fresh {
            let compiled = Search::new(&self.query, opts).map(Rc::new);
            self.compiled = Some((self.query.clone(), opts, compiled));
        }
        match &self.compiled {
            Some((_, _, compiled)) => compiled.clone(),
            None => Err(String::new()),
        }
    }

    /// Recomputes matches if the text, query or options changed.
    pub fn refresh(&mut self, text: &str, version: u64) {
        let key = (version, self.query.clone(), self.find_options());
        if self.key.as_ref() == Some(&key) {
            return;
        }
        // Exclusions belong to one query and one text (replacing keeps them).
        let same_query = self.key.as_ref().is_some_and(|(_, q, o)| *q == key.1 && *o == key.2);
        if !same_query || version != self.excluded_version {
            self.excluded_ranges.clear();
        }
        self.error = None;
        let found = self.search().and_then(|search| search.find_all(text));
        self.matches = found.unwrap_or_else(|e| {
            self.error = Some(e);
            Vec::new()
        });
        let excluded = self.matches.iter().map(|m| self.excluded_ranges.contains(&m.bytes)).collect();
        self.excluded = excluded;
        self.key = Some(key);
        self.current = None;
    }

    /// Exclusions moved by an edit the bar made itself, for the text of `version`.
    fn keep_excluded(&mut self, ranges: Vec<Range<usize>>, version: u64) {
        self.excluded_ranges = ranges;
        self.excluded_version = version;
    }

    fn is_excluded(&self, i: usize) -> bool {
        self.excluded.get(i).copied().unwrap_or(false)
    }

    /// With a regular expression in replace mode: checks the replacement and works
    /// out what the highlighted match becomes (shown under it).
    pub fn update_preview(&mut self, text: &str) {
        let active = self.open && self.replace_mode && self.options.regex && !self.query.is_empty();
        let excluded = self.current.is_some_and(|i| self.is_excluded(i));
        let key = active.then(|| (self.key.clone(), self.current, excluded, self.replacement.clone()));
        if key == self.preview_key {
            return;
        }
        self.preview_key = key;
        self.preview = None;
        self.replace_error = None;
        if !active {
            return;
        }
        // An invalid query is reported by itself.
        let Ok(search) = self.search() else {
            return;
        };
        if let Err(e) = search.check_replacement(&self.replacement) {
            self.replace_error = Some(e);
            return;
        }
        // Nothing to show until a replacement is written; an excluded match is not
        // replaced, so it gets no preview either.
        let m = self
            .current
            .filter(|_| !excluded && !self.replacement.is_empty())
            .and_then(|i| self.matches.get(i))
            .cloned();
        if let Some(m) = m {
            match search.replacement_for(text, &m, &self.replacement) {
                Ok(r) => self.preview = Some((m.chars.start, r)),
                Err(e) => self.replace_error = Some(e),
            }
        }
    }

    fn count_text(&self) -> String {
        if self.query.is_empty() {
            String::new()
        } else if self.error.is_some() {
            "오류".to_owned()
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

    fn not_found(&self) -> String {
        match &self.error {
            Some(e) => e.clone(),
            None => format!("'{}'을(를) 찾을 수 없습니다.", self.query),
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

/// Consumes Alt+`key` (without Ctrl or Shift) and the letter Windows types with it.
/// Some(repeat) if it was pressed; `repeat` is true for a key held down.
fn consume_alt(ctx: &egui::Context, key: Key) -> Option<bool> {
    ctx.input_mut(|i| {
        let at = i.events.iter().position(|e| {
            matches!(e, Event::Key { key: k, pressed: true, modifiers, .. }
                if *k == key && modifiers.alt && !modifiers.ctrl && !modifiers.shift && !modifiers.command)
        })?;
        let repeat = matches!(i.events.remove(at), Event::Key { repeat: true, .. });
        if matches!(i.events.get(at), Some(Event::Text(_))) {
            i.events.remove(at);
        }
        Some(repeat)
    })
}

/// Moves `ranges` that lie after `edited` (replaced by `new_len` bytes) along.
fn shift_ranges(ranges: &[Range<usize>], edited: &Range<usize>, new_len: usize) -> Vec<Range<usize>> {
    ranges
        .iter()
        .filter(|r| r.end <= edited.start || r.start >= edited.end)
        .map(|r| {
            if r.start >= edited.end {
                r.start + new_len - edited.len()..r.end + new_len - edited.len()
            } else {
                r.clone()
            }
        })
        .collect()
}

/// A toggle next to the query, like IntelliJ's "Cc", "W" and ".*".
fn option_toggle(ui: &mut Ui, on: &mut bool, label: &str, tip: &str) -> bool {
    let button = egui::Button::new(RichText::new(label).strong()).selected(*on).min_size(TOGGLE_SIZE);
    let clicked = ui.add(button).on_hover_text(tip).clicked();
    if clicked {
        *on = !*on;
    }
    clicked
}

impl MomeApp {
    /// Ctrl+F / Ctrl+R (Ctrl+H).
    pub(super) fn open_find(&mut self, ctx: &egui::Context, replace: bool) {
        let range = self.editor.selected_range(ctx);
        // A match the bar itself selected is no new query: taking it would turn a
        // regular expression into the text it found.
        let is_match = self.find.open && self.find.matches.iter().any(|m| m.chars == range);
        if !range.is_empty() && !is_match && range.len() < MAX_QUERY_FROM_SELECTION {
            let start = super::editor::byte_index(&self.doc.text, range.start);
            let end = super::editor::byte_index(&self.doc.text, range.end);
            let selected = &self.doc.text[start..end];
            if !selected.contains('\n') {
                // With regular expressions on, the selection is searched for as it is.
                self.find.query = if self.find.options.regex {
                    find::escape_regex(selected)
                } else {
                    selected.to_owned()
                };
                self.find.taken_from_selection = Some(selected.to_owned());
            }
        }
        self.find.open = true;
        self.find.replace_mode = replace;
        self.find.message = None;
        self.find.focus = Some(Field::Query);
        self.find.select_query = true;
    }

    /// Debug builds only: fills in the bar from `query|||replacement|||flags`; flags
    /// are c (match case), w (words), r (regular expression), x (exclude the
    /// highlighted match once the bar shows) and a (replace all).
    #[cfg(debug_assertions)]
    pub(super) fn test_find(&mut self, spec: &str) {
        let mut parts = spec.split("|||");
        self.find.query = parts.next().unwrap_or_default().to_owned();
        self.find.replacement = parts.next().unwrap_or_default().to_owned();
        let flags = parts.next().unwrap_or_default();
        self.find.options.match_case = flags.contains('c');
        self.find.options.whole_word = flags.contains('w');
        self.find.options.regex = flags.contains('r');
        if flags.contains('x') {
            self.find.pending = Some(FindAction::Exclude);
        } else if flags.contains('a') {
            self.find.pending = Some(FindAction::ReplaceAll);
        }
    }

    /// Does a find bar field have keyboard focus?
    pub(super) fn find_has_focus(&self, ctx: &egui::Context) -> bool {
        self.find.open && ctx.memory(|m| m.focused().is_some_and(is_field))
    }

    /// IntelliJ's keys while the find bar is open: Alt+C / Alt+O / Alt+X switch the
    /// options; Alt+P / Alt+A / Alt+L replace, replace all and exclude in replace
    /// mode, but not while typing in the text, where a slip would change it. The
    /// keys are taken either way, so their letters are never typed.
    pub(super) fn find_bar_keys(&mut self, ctx: &egui::Context) {
        if !self.find.open || self.reading().is_some() {
            return;
        }
        let in_text = ctx.memory(|m| m.has_focus(self.editor.id));
        for key in [Key::C, Key::O, Key::X] {
            // A held key switches once.
            if consume_alt(ctx, key) == Some(false) {
                let o = &mut self.find.options;
                match key {
                    Key::C => o.match_case = !o.match_case,
                    Key::O => o.whole_word = !o.whole_word,
                    _ => {
                        o.regex = !o.regex;
                        self.find.regex_toggled();
                    }
                }
                // From the text only the highlights change; the cursor stays put.
                self.find.pending = Some(if in_text { FindAction::Refresh } else { FindAction::Incremental });
            }
        }
        // Replace and exclude step on while held; replacing all happens once.
        for (key, action, repeats) in [
            (Key::P, FindAction::Replace, true),
            (Key::A, FindAction::ReplaceAll, false),
            (Key::L, FindAction::Exclude, true),
        ] {
            if let Some(repeat) = consume_alt(ctx, key) {
                if self.find.replace_mode && !in_text && (repeats || !repeat) {
                    self.find.pending = Some(action);
                }
            }
        }
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

    /// Index of the match to work on: the highlighted one, or the one selected in
    /// the text.
    fn match_at_selection(&self, ctx: &egui::Context) -> Option<usize> {
        self.find.current.filter(|&i| i < self.find.matches.len()).or_else(|| {
            let selection = self.editor.selected_range(ctx);
            self.find.matches.iter().position(|m| m.chars == selection)
        })
    }

    /// The highlighted match is the one the bar selected last. Once the cursor or
    /// the selection has been put elsewhere in the text, searching, replacing and
    /// excluding go on from there, as in Notepad and IntelliJ.
    fn forget_current_if_moved(&mut self, ctx: &egui::Context) {
        if let Some(m) = self.find.current.and_then(|i| self.find.matches.get(i)) {
            if m.chars != self.editor.selected_range(ctx) {
                self.find.current = None;
            }
        }
    }

    fn find_from_selection(&mut self, ctx: &egui::Context, direction: Direction, focus: FocusAfter) {
        self.find.refresh(&self.doc.text, self.text_version);
        self.forget_current_if_moved(ctx);
        // Continue from the highlighted match, passing over an empty one (`^`) there.
        let (selection, past) = match self.find.current.and_then(|i| self.find.matches.get(i)) {
            Some(m) => (m.chars.clone(), true),
            None => (self.editor.selected_range(ctx), false),
        };
        self.go_to_match(selection, direction, focus, past);
    }

    /// Selects the match after (or before) `selection` and reports wrapping or failure.
    fn go_to_match(&mut self, selection: Range<usize>, direction: Direction, focus: FocusAfter, past_selection: bool) {
        let forward = direction == Direction::Forward;
        let wrap = self.find.options.wrap_around;
        match find::next_match(&self.find.matches, selection.clone(), forward, wrap, past_selection) {
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
                    self.find.not_found()
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

    /// Alt+P: replaces the highlighted match and moves to the next one. An
    /// excluded match is left as it is.
    fn replace_one(&mut self, ctx: &egui::Context) {
        self.find.refresh(&self.doc.text, self.text_version);
        self.forget_current_if_moved(ctx);
        let Some(i) = self.match_at_selection(ctx) else {
            // No match highlighted yet: first show the next one, like Notepad.
            let selection = self.editor.selected_range(ctx);
            self.go_to_match(selection, Direction::Forward, FocusAfter::Unchanged, false);
            return;
        };
        let m = self.find.matches[i].clone();
        if self.find.is_excluded(i) {
            self.go_to_match(m.chars, Direction::Forward, FocusAfter::Unchanged, true);
            return;
        }
        let replacement = self
            .find
            .search()
            .and_then(|search| search.replacement_for(&self.doc.text, &m, &self.find.replacement));
        let replacement = match replacement {
            Ok(r) => r,
            Err(e) => {
                self.find.message = Some(e);
                return;
            }
        };
        let excluded = shift_ranges(&self.find.excluded_ranges, &m.bytes, replacement.len());
        let mut text = self.doc.text.clone();
        text.replace_range(m.bytes.clone(), &replacement);
        let end = m.chars.start + replacement.chars().count();
        self.editor.apply_edit(ctx, &mut self.doc.text, text, (end, end));
        self.on_text_changed();
        self.find.keep_excluded(excluded, self.text_version);
        self.find.refresh(&self.doc.text, self.text_version);
        self.go_to_match(end..end, Direction::Forward, FocusAfter::Unchanged, true);
    }

    /// Alt+A: replaces every match that is not excluded.
    fn replace_all(&mut self, ctx: &egui::Context) {
        self.find.refresh(&self.doc.text, self.text_version);
        // Not the highlighted matches, which stop at find::MAX_MATCHES.
        let found = self.find.search().and_then(|search| {
            let every = search.find_every(&self.doc.text)?;
            let (skipped, matches): (Vec<Match>, Vec<Match>) =
                every.into_iter().partition(|m| self.find.excluded_ranges.contains(&m.bytes));
            let replaced = search.replace_all(&self.doc.text, &matches, &self.find.replacement)?;
            Ok((matches, skipped.len(), replaced))
        });
        let (matches, skipped, (text, spans)) = match found {
            Ok(found) => found,
            Err(e) => {
                self.find.message = Some(e);
                return;
            }
        };
        if matches.is_empty() {
            self.find.message = Some(if skipped > 0 {
                "바꿀 항목이 없습니다. 찾은 항목을 모두 제외했습니다.".to_owned()
            } else {
                self.find.not_found()
            });
            return;
        }
        // Keep the cursor next to the same text: shift it by what changed before it.
        let cursor = self.editor.cursor(ctx);
        let shift: isize = matches
            .iter()
            .zip(&spans)
            .take_while(|(m, _)| m.chars.end <= cursor)
            .map(|(m, span)| text[span.clone()].chars().count() as isize - m.chars.len() as isize)
            .sum();
        let cursor = ((cursor as isize + shift).max(0) as usize).min(text.chars().count());
        // The excluded matches stay excluded where they moved to.
        let excluded = self
            .find
            .excluded_ranges
            .iter()
            .map(|r| {
                let delta: isize = matches
                    .iter()
                    .zip(&spans)
                    .take_while(|(m, _)| m.bytes.end <= r.start)
                    .map(|(m, span)| span.len() as isize - m.bytes.len() as isize)
                    .sum();
                (r.start as isize + delta) as usize..(r.end as isize + delta) as usize
            })
            .collect();
        self.editor.apply_edit(ctx, &mut self.doc.text, text, (cursor, cursor));
        self.on_text_changed();
        self.find.keep_excluded(excluded, self.text_version);
        // Put the focus in the text so Ctrl+Z undoes the replacement, not the field.
        self.editor.request_focus();
        let count = matches.len();
        self.find.message = Some(if skipped > 0 {
            format!("{count}개 항목을 바꾸고 제외한 {skipped}개는 그대로 두었습니다. Ctrl+Z로 실행 취소할 수 있습니다.")
        } else {
            format!("{count}개 항목을 바꿨습니다. Ctrl+Z로 실행 취소할 수 있습니다.")
        });
    }

    /// Alt+L: leaves the highlighted match out of replacing and moves on; on an
    /// excluded match it takes it back in.
    fn exclude_one(&mut self, ctx: &egui::Context) {
        self.find.refresh(&self.doc.text, self.text_version);
        self.forget_current_if_moved(ctx);
        let Some(i) = self.match_at_selection(ctx) else {
            let selection = self.editor.selected_range(ctx);
            self.go_to_match(selection, Direction::Forward, FocusAfter::Unchanged, false);
            return;
        };
        let m = self.find.matches[i].clone();
        let exclude = !self.find.is_excluded(i);
        if self.find.excluded_version != self.text_version {
            self.find.excluded_ranges.clear();
            self.find.excluded_version = self.text_version;
        }
        if exclude {
            self.find.excluded_ranges.push(m.bytes.clone());
        } else {
            self.find.excluded_ranges.retain(|r| *r != m.bytes);
        }
        if let Some(flag) = self.find.excluded.get_mut(i) {
            *flag = exclude;
        }
        if exclude {
            self.go_to_match(m.chars, Direction::Forward, FocusAfter::Unchanged, true);
        } else {
            self.find.current = Some(i);
        }
    }

    pub(super) fn find_bar(&mut self, ui: &mut Ui) {
        let p = self.palette();
        let ctx = ui.ctx().clone();
        self.find.refresh(&self.doc.text, self.text_version);
        // So that the count ("3/12"), the highlight and the preview follow too.
        self.forget_current_if_moved(&ctx);
        self.find.update_preview(&self.doc.text);
        let mut action: Option<FindAction> = self.find.pending.take();
        egui::Panel::top("mome-find-bar")
            .frame(Frame::NONE.fill(p.bar_bg).inner_margin(Margin::symmetric(8, 5)))
            .show(ui, |ui| {
                // The first row's fixed parts (labels, toggles, count, buttons, gaps)
                // take about FIXED_WIDTH; in a narrow window "바꾸기 표시" goes
                // (Ctrl+R / Ctrl+F still switch) so nothing overlaps.
                let width = ui.available_width();
                let narrow = width < FIXED_WIDTH + 80.0;
                let fixed = if narrow { FIXED_WIDTH - 120.0 } else { FIXED_WIDTH };
                let field_width = (width - fixed).clamp(60.0, 260.0);
                // Where "이전" starts, so "바꾸기" lines up under it.
                let mut buttons_x = 0.0;
                ui.horizontal(|ui| {
                    ui.add_sized([LABEL_WIDTH, 24.0], egui::Label::new("찾을 내용"));
                    let query = ui.add(
                        TextEdit::singleline(&mut self.find.query)
                            .id(Id::new(QUERY_ID))
                            .hint_text(if self.find.options.regex { "정규식" } else { "" })
                            .desired_width(field_width),
                    );
                    if self.find.error.is_some() {
                        ui.painter()
                            .rect_stroke(query.rect, 2.0, Stroke::new(1.5, p.warning), StrokeKind::Outside);
                    }
                    if self.find.focus == Some(Field::Query) {
                        query.request_focus();
                        self.find.focus = None;
                    } else if self.find.select_query && query.has_focus() {
                        select_all_in(&ctx, Id::new(QUERY_ID), &self.find.query);
                        self.find.select_query = false;
                        ui.ctx().request_repaint();
                    }
                    if query.changed() {
                        self.find.taken_from_selection = None;
                        action = Some(FindAction::Incremental);
                    }
                    if query.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        let back = ui.input(|i| i.modifiers.shift);
                        action = Some(if back { FindAction::Prev } else { FindAction::Next });
                        self.find.focus = Some(Field::Query);
                    }
                    let o = &mut self.find.options;
                    let toggled = option_toggle(ui, &mut o.match_case, "Cc", "대/소문자 구분 (Alt+C)")
                        | option_toggle(ui, &mut o.whole_word, "W", "단어 단위로 (Alt+O)");
                    if option_toggle(ui, &mut o.regex, ".*", REGEX_HELP) {
                        self.find.regex_toggled();
                        action = Some(FindAction::Incremental);
                    }
                    if toggled {
                        action = Some(FindAction::Incremental);
                    }
                    let count_color = if self.find.error.is_some() { p.warning } else { p.weak };
                    let count = self.find.count_text();
                    // "12345/100000+" needs more room than "3/12".
                    let count_width = COUNT_WIDTH.max(count.chars().count() as f32 * 8.0 + 8.0);
                    ui.add_sized(
                        [count_width, 24.0],
                        egui::Label::new(RichText::new(count).color(count_color)),
                    );
                    buttons_x = ui.cursor().min.x;
                    if ui.button("이전").on_hover_text("이전 찾기 (Shift+Enter, Shift+F3)").clicked() {
                        action = Some(FindAction::Prev);
                    }
                    if ui.button("다음").on_hover_text("다음 찾기 (Enter, F3)").clicked() {
                        action = Some(FindAction::Next);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if close_button(ui, p.weak).on_hover_text("닫기 (Esc)").clicked() {
                            action = Some(FindAction::Close);
                        }
                        let label = if self.find.replace_mode { "바꾸기 숨기기" } else { "바꾸기 표시" };
                        let tip = if self.find.replace_mode { "찾기만 하기 (Ctrl+F)" } else { "바꾸기 (Ctrl+R)" };
                        if !narrow && ui.button(label).on_hover_text(tip).clicked() {
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
                        let hint = if self.find.options.regex { "$1, ${이름}, \\n 등을 쓸 수 있음" } else { "" };
                        let replace = ui.add(
                            TextEdit::singleline(&mut self.find.replacement)
                                .id(Id::new(REPLACE_ID))
                                .hint_text(hint)
                                .desired_width(field_width),
                        );
                        if self.find.replace_error.is_some() {
                            ui.painter()
                                .rect_stroke(replace.rect, 2.0, Stroke::new(1.5, p.warning), StrokeKind::Outside);
                        }
                        if self.find.focus == Some(Field::Replacement) {
                            replace.request_focus();
                            self.find.focus = None;
                        }
                        if replace.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            action = Some(FindAction::Replace);
                            self.find.focus = Some(Field::Replacement);
                        }
                        // Line the buttons up with "이전" / "다음" above, if they still fit.
                        if buttons_x + REPLACE_BUTTONS_WIDTH <= ui.max_rect().right() {
                            ui.add_space((buttons_x - ui.cursor().min.x).max(0.0));
                        }
                        if ui.button("바꾸기").on_hover_text("바꾸고 다음으로 (Alt+P, Enter)").clicked() {
                            action = Some(FindAction::Replace);
                        }
                        if ui.button("모두 바꾸기").on_hover_text("제외한 항목 말고 모두 바꾸기 (Alt+A)").clicked() {
                            action = Some(FindAction::ReplaceAll);
                        }
                        let excluded = self.find.current.is_some_and(|i| self.find.is_excluded(i));
                        let (label, tip) = if excluded {
                            ("포함", "제외했던 이 항목을 다시 바꾸기 대상에 넣습니다. (Alt+L)")
                        } else {
                            ("제외", "이 항목은 바꾸지 않고 다음으로 넘어갑니다. (Alt+L)")
                        };
                        if ui.button(label).on_hover_text(tip).clicked() {
                            action = Some(FindAction::Exclude);
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.add_space(LABEL_WIDTH + ui.spacing().item_spacing.x);
                    let before = self.find.options.wrap_around;
                    ui.toggle_value(&mut self.find.options.wrap_around, "끝에서 처음으로 이어 찾기")
                        .on_hover_text("문서 끝(또는 처음)에 도달하면 반대쪽부터 계속 찾습니다.");
                    if before != self.find.options.wrap_around {
                        action = Some(FindAction::Incremental);
                    }
                    let shown = match (&self.find.error, &self.find.replace_error, &self.find.message) {
                        (Some(e), _, _) | (None, Some(e), _) => Some(RichText::new(e).color(p.warning)),
                        (None, None, Some(m)) => Some(RichText::new(m).color(p.weak)),
                        _ => None,
                    };
                    if let Some(text) = shown {
                        ui.add_space(8.0);
                        ui.add(egui::Label::new(text.small()).truncate());
                    }
                });
            });

        match action {
            Some(FindAction::Next) => self.find_from_selection(&ctx, Direction::Forward, FocusAfter::Unchanged),
            Some(FindAction::Prev) => self.find_from_selection(&ctx, Direction::Backward, FocusAfter::Unchanged),
            Some(FindAction::Refresh) => {
                self.find.message = None;
                self.find.refresh(&self.doc.text, self.text_version);
            }
            Some(FindAction::Incremental) => {
                self.find.message = None;
                self.find.refresh(&self.doc.text, self.text_version);
                if !self.find.query.is_empty() {
                    // Search again from where the current selection starts.
                    let start = self.editor.selected_range(&ctx).start;
                    self.go_to_match(start..start, Direction::Forward, FocusAfter::Unchanged, false);
                }
            }
            Some(FindAction::Replace) => self.replace_one(&ctx),
            Some(FindAction::ReplaceAll) => self.replace_all(&ctx),
            Some(FindAction::Exclude) => self.exclude_one(&ctx),
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
    /// Search again and go to the first match from the cursor (the query changed).
    Incremental,
    /// Search again without moving the cursor (an option switched from the text).
    Refresh,
    Replace,
    ReplaceAll,
    Exclude,
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

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Modifiers;

    fn key(key: Key, modifiers: Modifiers, repeat: bool) -> Event {
        Event::Key { key, physical_key: None, pressed: true, repeat, modifiers }
    }

    /// Runs `f` on a frame of `ctx` whose input is `events`; returns the events left.
    fn with_events(ctx: &egui::Context, events: Vec<Event>, mut f: impl FnMut(&egui::Context)) -> Vec<Event> {
        let raw = egui::RawInput { events, ..Default::default() };
        let mut left = Vec::new();
        let mut output = ctx.run_ui(raw, |ui| {
            f(ui.ctx());
            left = ui.input(|i| i.events.clone());
        });
        output.textures_delta.clear();
        left
    }

    #[test]
    fn alt_keys_take_their_letter() {
        let ctx = egui::Context::default();
        let text = |s: &str| Event::Text(s.to_owned());
        // Alt+X is taken with the "x" Windows types; the next letter stays.
        let mut got = None;
        let left = with_events(&ctx, vec![key(Key::X, Modifiers::ALT, false), text("x"), text("y")], |ctx| {
            got = consume_alt(ctx, Key::X);
        });
        assert_eq!(got, Some(false));
        assert_eq!(left, vec![text("y")]);
        // A held key: egui marks the second press of a key still down as a repeat.
        let alt_a = || vec![key(Key::A, Modifiers::ALT, false), text("a")];
        with_events(&ctx, alt_a(), |ctx| got = consume_alt(ctx, Key::A));
        assert_eq!(got, Some(false));
        let left = with_events(&ctx, alt_a(), |ctx| got = consume_alt(ctx, Key::A));
        assert_eq!((got, left.len()), (Some(true), 0));
        // Ctrl+Alt (AltGr on some layouts) is not Alt+letter.
        let altgr = Modifiers { alt: true, ctrl: true, ..Default::default() };
        let left = with_events(&ctx, vec![key(Key::E, altgr, false), text("€")], |ctx| {
            got = consume_alt(ctx, Key::E);
        });
        assert_eq!((got, left.len()), (None, 2));
    }

    #[test]
    fn query_from_selection_follows_regex() {
        let mut bar = FindBar {
            query: "a.b".to_owned(),
            taken_from_selection: Some("a.b".to_owned()),
            ..Default::default()
        };
        bar.options.regex = true;
        bar.regex_toggled();
        assert_eq!(bar.query, r"a\.b");
        bar.options.regex = false;
        bar.regex_toggled();
        assert_eq!(bar.query, "a.b");
        // A query the user wrote is left alone.
        bar.taken_from_selection = None;
        bar.options.regex = true;
        bar.regex_toggled();
        assert_eq!(bar.query, "a.b");
    }

    #[test]
    fn exclusions_follow_a_replacement() {
        let ranges = vec![0..2, 5..7, 10..12];
        // "5..7" itself was replaced by four bytes: it goes, the later one moves on.
        assert_eq!(shift_ranges(&ranges, &(5..7), 4), vec![0..2, 12..14]);
        // A replacement before everything that shrinks the text.
        let later: Vec<Range<usize>> = std::iter::once(4..6).collect();
        assert_eq!(shift_ranges(&later, &(0..3), 1), std::iter::once(2..4).collect::<Vec<_>>());
    }
}
