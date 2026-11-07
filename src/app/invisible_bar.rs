//! 보이지 않는 문자 찾기: a bar listing the invisible characters in the document
//! by kind, with jumps to each one and a cleanup that can be undone.

use std::collections::HashMap;

use egui::{Align, Frame, Layout, Margin, RichText, ScrollArea, Ui};

use super::findbar::close_button;
use super::MomeApp;
use crate::invisible::{self, Found, Kind};

/// The kinds of invisible characters found, for the list.
struct Group {
    kind: Kind,
    count: usize,
    /// A few of the distinct characters, for their code points.
    chars: Vec<char>,
    distinct: usize,
}

#[derive(Default)]
pub struct InvisibleState {
    pub open: bool,
    /// Text version the results are for.
    version: Option<u64>,
    found: Vec<Found>,
    /// Character indices of the characters marked in the text area.
    pub marks: Vec<usize>,
    groups: Vec<Group>,
    /// Text hidden in tag characters or variation selectors: (character index, text).
    hidden: Vec<(usize, String)>,
    /// Kinds the user ticked or unticked for cleaning; others use `Kind::default_on`.
    picked: HashMap<&'static str, bool>,
    message: Option<String>,
}

const SHOWN_CODES: usize = 3;

impl InvisibleState {
    /// Scans `text` again if it changed since the last scan.
    pub fn refresh(&mut self, text: &str, version: u64) {
        if self.version == Some(version) {
            return;
        }
        self.version = Some(version);
        self.found = invisible::scan(text);
        self.marks = self.found.iter().filter(|f| f.kind.mark).map(|f| f.at).collect();
        self.groups.clear();
        for f in &self.found {
            match self.groups.iter_mut().find(|g| g.kind.name == f.kind.name) {
                Some(g) => {
                    g.count += 1;
                    if !g.chars.contains(&f.ch) {
                        g.distinct += 1;
                        if g.chars.len() < SHOWN_CODES {
                            g.chars.push(f.ch);
                        }
                    }
                }
                None => self.groups.push(Group {
                    kind: f.kind,
                    count: 1,
                    chars: vec![f.ch],
                    distinct: 1,
                }),
            }
        }
        self.hidden = if self.found.iter().any(|f| f.kind.can_hide_text()) {
            invisible::hidden_messages(text)
        } else {
            Vec::new()
        };
    }

    /// Forgets the results, e.g. while nobody looks at them.
    pub fn clear(&mut self) {
        if self.version.is_some() {
            *self = Self {
                open: self.open,
                picked: std::mem::take(&mut self.picked),
                ..Self::default()
            };
        }
    }

    fn picked(&self, kind: &Kind) -> bool {
        self.picked.get(kind.name).copied().unwrap_or(kind.default_on)
    }

    pub fn close(&mut self) {
        self.open = false;
        self.message = None;
    }
}

enum Action {
    Step { forward: bool, only: Option<&'static str> },
    Clean,
    Close,
}

/// `U+200B` or `U+202A, U+202E 외 3종`.
fn codes(group: &Group) -> String {
    let list: Vec<String> = group.chars.iter().map(|c| format!("U+{:04X}", *c as u32)).collect();
    let mut s = list.join(", ");
    if group.distinct > group.chars.len() {
        s.push_str(&format!(" 외 {}종", group.distinct - group.chars.len()));
    }
    s
}

impl MomeApp {
    /// Opens the bar (편집 > 보이지 않는 문자 찾기).
    pub(super) fn open_invisible(&mut self) {
        self.invisible.open = true;
        self.invisible.message = None;
        self.invisible.refresh(&self.doc.text, self.text_version);
        if self.invisible.found.is_empty() {
            self.notify("보이지 않는 문자가 없습니다.".to_owned());
        }
    }

    /// Scans the text if anything shows the results.
    pub(super) fn refresh_invisible(&mut self) {
        if self.invisible.open || self.settings.show_invisible {
            self.invisible.refresh(&self.doc.text, self.text_version);
        } else {
            self.invisible.clear();
        }
    }

    /// After a file is opened: says so if invisible characters are marked in it.
    pub(super) fn warn_invisible(&mut self) {
        self.refresh_invisible();
        let count = self.marked_invisible_count();
        if count > 0 {
            self.notify_warning(format!(
                "보이지 않는 문자가 {count}개 있어 분홍색으로 표시했습니다. [편집 > 보이지 않는 문자 찾기]에서 확인하세요."
            ));
        }
    }

    /// How many invisible characters are marked in the text (for the status bar).
    pub(super) fn marked_invisible_count(&self) -> usize {
        if self.settings.show_invisible {
            self.invisible.marks.len()
        } else {
            0
        }
    }

    pub(super) fn invisible_bar(&mut self, ui: &mut Ui) {
        let p = self.palette();
        let ctx = ui.ctx().clone();
        let mut action = None;
        egui::Panel::top("mome-invisible-bar")
            .frame(Frame::NONE.fill(p.bar_bg).inner_margin(Margin::symmetric(8, 5)))
            .show(ui, |ui| {
                let state = &mut self.invisible;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("보이지 않는 문자").strong());
                    let total = state.found.len();
                    let summary = if total == 0 {
                        "없습니다".to_owned()
                    } else {
                        format!("{total}개 · {}종류", state.groups.len())
                    };
                    ui.label(RichText::new(summary).color(p.weak));
                    if total > 0 {
                        if ui.button("이전").on_hover_text("커서 앞의 보이지 않는 문자로").clicked() {
                            action = Some(Action::Step { forward: false, only: None });
                        }
                        if ui.button("다음").on_hover_text("커서 뒤의 보이지 않는 문자로").clicked() {
                            action = Some(Action::Step { forward: true, only: None });
                        }
                    }
                    if let Some(message) = &state.message {
                        ui.label(RichText::new(message).color(p.weak));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if close_button(ui, p.weak).on_hover_text("닫기 (Esc)").clicked() {
                            action = Some(Action::Close);
                        }
                    });
                });
                if state.found.is_empty() {
                    return;
                }
                for (at, text) in &state.hidden {
                    let (line, _) = self.stats.line_col(*at);
                    let shown: String = text.chars().take(120).collect();
                    let more = if text.chars().count() > 120 { "…" } else { "" };
                    ui.label(RichText::new(format!("숨은 글자 ({line}줄): \"{shown}{more}\"")).color(p.warning))
                        .on_hover_text("보이지 않는 문자 속에 숨겨진 글자를 풀어 본 것입니다. 정리하면 함께 지워집니다.");
                }

                ScrollArea::vertical()
                    .id_salt("mome-invisible-list")
                    .max_height(200.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for group in &state.groups {
                            ui.horizontal(|ui| {
                                let mut on = state.picked.get(group.kind.name).copied().unwrap_or(group.kind.default_on);
                                let tip = format!("정리할 때 {}", group.kind.fix.label());
                                if ui.checkbox(&mut on, group.kind.name).on_hover_text(tip).changed() {
                                    state.picked.insert(group.kind.name, on);
                                }
                                ui.label(RichText::new(codes(group)).color(p.weak).monospace());
                                ui.label(format!("{}개", group.count));
                                ui.label(RichText::new(format!("→ {}", group.kind.fix.label())).color(p.weak));
                                if ui.small_button("찾기").on_hover_text("커서 뒤의 이 문자로 이동").clicked() {
                                    action = Some(Action::Step { forward: true, only: Some(group.kind.name) });
                                }
                            });
                        }
                    });

                ui.horizontal(|ui| {
                    let any = state.groups.iter().any(|g| state.picked(&g.kind));
                    let clean = ui
                        .add_enabled(any, egui::Button::new("선택한 문자 정리"))
                        .on_hover_text("체크한 종류를 지우거나 일반 공백·줄 바꿈으로 바꿉니다. Ctrl+Z로 되돌릴 수 있습니다.");
                    if clean.clicked() {
                        action = Some(Action::Clean);
                    }
                    let show = self.settings.show_invisible;
                    let hint = if show {
                        "본문에 분홍색 표시로 보입니다. [보기 > 보이지 않는 문자 표시]로 끌 수 있습니다."
                    } else {
                        "[보기 > 보이지 않는 문자 표시]를 켜면 본문에 위치가 표시됩니다."
                    };
                    ui.label(RichText::new(hint).small().color(p.weak));
                });
            });

        match action {
            Some(Action::Step { forward, only }) => self.invisible_step(&ctx, forward, only),
            Some(Action::Clean) => self.clean_invisible(&ctx),
            Some(Action::Close) => {
                self.invisible.close();
                self.editor.request_focus();
            }
            None => {}
        }
    }

    /// Selects the next (or previous) invisible character from the cursor,
    /// optionally only those of the kind named `only`.
    fn invisible_step(&mut self, ctx: &egui::Context, forward: bool, only: Option<&'static str>) {
        self.invisible.refresh(&self.doc.text, self.text_version);
        let list: Vec<usize> = self
            .invisible
            .found
            .iter()
            .filter(|f| only.is_none_or(|name| f.kind.name == name))
            .map(|f| f.at)
            .collect();
        if list.is_empty() {
            return;
        }
        let selection = self.editor.selected_range(ctx);
        let (index, wrapped) = if forward {
            match list.iter().position(|&at| at >= selection.end) {
                Some(i) => (i, false),
                None => (0, true),
            }
        } else {
            match list.iter().rposition(|&at| at < selection.start) {
                Some(i) => (i, false),
                None => (list.len() - 1, true),
            }
        };
        let at = list[index];
        self.editor.select(at, at + 1);
        let (line, col) = self.stats.line_col(at);
        let mut message = format!("{}/{}번째 · {line}줄 {col}열", index + 1, list.len());
        if wrapped {
            message.push_str(if forward { " (처음부터 다시)" } else { " (끝부터 다시)" });
        }
        self.invisible.message = Some(message);
    }

    /// Removes or replaces the ticked kinds in one undoable step.
    fn clean_invisible(&mut self, ctx: &egui::Context) {
        self.invisible.refresh(&self.doc.text, self.text_version);
        let state = &self.invisible;
        let count = state.found.iter().filter(|f| state.picked(&f.kind)).count();
        if count == 0 {
            return;
        }
        let cursor = self.editor.cursor(ctx);
        let (text, cursor) = invisible::clean(&self.doc.text, &state.found, cursor, |k| state.picked(k));
        self.editor.apply_edit(ctx, &mut self.doc.text, text, (cursor, cursor));
        self.on_text_changed();
        self.editor.request_focus();
        self.invisible.refresh(&self.doc.text, self.text_version);
        self.invisible.message = Some(format!("{count}개를 정리했습니다. Ctrl+Z로 되돌릴 수 있습니다."));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_by_kind() {
        let mut state = InvisibleState::default();
        state.refresh("a\u{200B}b\u{200B}\u{202A}\u{202E}\u{2066}\u{2067}\u{2068}\u{3000}", 1);
        assert_eq!(state.found.len(), 8);
        // 전각 공백 is listed but not marked in the text.
        assert_eq!(state.marks, [1, 3, 4, 5, 6, 7, 8]);
        assert_eq!(state.groups.len(), 3);
        assert_eq!(state.groups[0].count, 2);
        assert_eq!(codes(&state.groups[0]), "U+200B");
        assert_eq!(codes(&state.groups[1]), "U+202A, U+202E, U+2066 외 2종");
        assert!(!state.picked(&state.groups[2].kind));
        // Same version: not scanned again.
        state.refresh("", 1);
        assert_eq!(state.found.len(), 8);
        state.clear();
        assert!(state.found.is_empty());
    }
}
