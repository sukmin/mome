//! Status bar: cursor position, character count, manuscript paper sheets,
//! insert/overwrite mode, zoom, line endings and encoding.

use egui::{Align, Button, Color32, FontId, Frame, Layout, Margin, Popup, RichText, Ui};

use super::{Cmd, MomeApp};
use crate::stats::{MANUSCRIPT_COLUMNS, MANUSCRIPT_ROWS};

const TEXT_SIZE: f32 = 13.0;
/// egui's default separator spacing.
const SEPARATOR_WIDTH: f32 = 6.0;

/// Width a segment and the separator after it take up in the bar.
fn segment_width(ui: &Ui, text: &str) -> f32 {
    let font = FontId::proportional(TEXT_SIZE);
    let text_width = ui.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER).size().x);
    let spacing = ui.spacing();
    text_width + 2.0 * spacing.button_padding.x + 2.0 * spacing.item_spacing.x + SEPARATOR_WIDTH
}

/// `1234567` → `1,234,567`
fn group_digits(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

impl MomeApp {
    pub(super) fn status_bar(&mut self, ui: &mut Ui) {
        let p = self.palette();
        let ctx = ui.ctx().clone();
        let (anchor, cursor) = self.editor.selection(&ctx);
        let (line, col) = self.stats.line_col(cursor);
        let selected = self.stats.chars_between(anchor, cursor);

        let position = format!("줄 {}, 열 {}", group_digits(line), group_digits(col));
        let total = group_digits(self.stats.chars_without_breaks());
        let count = if selected > 0 {
            format!("{total}자 중 {}자 선택", group_digits(selected))
        } else {
            format!("{total}자")
        };
        let sheets = format!("원고지 {}매", group_digits(self.stats.manuscript_sheets()));

        egui::Panel::bottom("mome-status")
            .exact_size(26.0)
            .frame(Frame::NONE.fill(p.bar_bg).inner_margin(Margin::symmetric(6, 0)))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let segment = |text: String| Button::new(RichText::new(text).size(TEXT_SIZE)).frame_when_inactive(false);

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        // Rightmost first.
                        let enc = ui.add(segment(self.doc.encoding.label())).on_hover_text("인코딩 - 클릭하여 변경");
                        Popup::menu(&enc).show(|ui| {
                            ui.set_min_width(220.0);
                            self.encoding_items(ui);
                            ui.separator();
                            self.reopen_menu(ui);
                        });
                        ui.separator();

                        let eol = ui.add(segment(self.doc.line_ending.label().to_owned())).on_hover_text("줄 끝 형식 - 클릭하여 변경");
                        Popup::menu(&eol).show(|ui| {
                            ui.set_min_width(200.0);
                            self.line_ending_items(ui);
                        });
                        ui.separator();

                        let zoom = ui
                            .add(segment(format!("{}%", self.editor.zoom)))
                            .on_hover_text("확대/축소 - 클릭하여 100%로 복원");
                        if zoom.clicked() {
                            self.pending_cmds.push(Cmd::ZoomReset);
                        }
                        ui.separator();

                        let (mode, mode_tip) = if self.editor.overwrite {
                            (
                                RichText::new("겹쳐쓰기").size(TEXT_SIZE).color(p.warning),
                                "겹쳐쓰기 모드: 입력하는 글자가 커서 뒤의 글자를 대체합니다. - 클릭하거나 Insert 키를 눌러 전환",
                            )
                        } else {
                            (RichText::new("삽입").size(TEXT_SIZE), "삽입 모드 - 클릭하거나 Insert 키를 눌러 전환")
                        };
                        if ui.add(Button::new(mode).frame_when_inactive(false)).on_hover_text(mode_tip).clicked() {
                            self.pending_cmds.push(Cmd::ToggleOverwrite);
                        }
                        ui.separator();

                        // The least important segment: left out when the window is too narrow
                        // for it and the segments left of it, so the cursor position stays
                        // visible. The count tooltip still has the number.
                        let room_needed: f32 = [&sheets, &count, &position].iter().map(|t| segment_width(ui, t)).sum();
                        if ui.available_width() >= room_needed {
                            ui.add(segment(sheets)).on_hover_text(format!(
                                "200자 원고지({MANUSCRIPT_COLUMNS}칸 × {MANUSCRIPT_ROWS}줄) 기준 매수\n\
                                 공백도 한 칸이고, 문단이 끝나거나 줄이 바뀌면 남은 칸도 채운 것으로 셉니다.\n\
                                 원고지 {}줄 사용. 한글(HWP) 문서 정보의 값과는 조금 다를 수 있습니다.",
                                group_digits(self.stats.manuscript_rows()),
                            ));
                            ui.separator();
                        }

                        let count = ui.add(segment(count));
                        if count.hovered() {
                            count.on_hover_text(format!(
                                "글자 수 (줄 바꿈 제외)\n공백 제외: {}자\n줄 수: {}\n200자 원고지: {}매",
                                group_digits(self.char_count_without_spaces()),
                                group_digits(self.stats.line_count()),
                                group_digits(self.stats.manuscript_sheets()),
                            ));
                        }
                        ui.separator();

                        let position = ui.add(segment(position)).on_hover_text("클릭하여 줄로 이동 (Ctrl+G)");
                        if position.clicked() {
                            self.pending_cmds.push(Cmd::GoTo);
                        }

                        // Left side: the latest message, or when the last revision was written.
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add_space(4.0);
                            let (text, color) = match &self.message {
                                Some(m) => (m.text.clone(), if m.warning { p.warning } else { p.weak }),
                                None => (self.snapshot_status(), p.weak),
                            };
                            ui.add(egui::Label::new(RichText::new(text).size(TEXT_SIZE).color(color)).truncate());
                        });
                    });
                });
            });
    }

    fn snapshot_status(&self) -> String {
        match (&self.settings.temp_dir, self.snapshot.last_time) {
            (None, _) => "임시 저장 폴더가 설정되지 않았습니다".to_owned(),
            (Some(_), Some(t)) => format!("{}에 임시 저장됨", t.time_string()),
            (Some(_), None) => String::new(),
        }
    }

    fn char_count_without_spaces(&self) -> usize {
        // Only computed while the tooltip is shown.
        self.doc.text.chars().filter(|c| !c.is_whitespace()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::group_digits;

    #[test]
    fn groups() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(1234567), "1,234,567");
    }
}
