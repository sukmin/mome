//! 읽기 모드: Markdown shown formatted, and Obsidian canvases drawn as boards.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::epaint::CubicBezierShape;
use egui::{
    Align, Align2, Color32, CornerRadius, FontFamily, FontId, Frame, Layout, OutputCommand,
    Pos2, Rect, RichText, ScrollArea, Shape, Stroke, StrokeKind, TextStyle, Ui, UiBuilder, Vec2,
};
use egui_commonmark::{Alert, AlertBundle, CommonMarkCache, CommonMarkViewer};

use super::{Cmd, MomeApp};
use crate::canvas::{self, Canvas, NodeKind};
use crate::markdown::{self, LinkTarget};
use crate::theme::Palette;
use crate::win;

/// Documents the reader can show.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ViewKind {
    Markdown,
    Canvas,
}

impl ViewKind {
    pub fn of(path: Option<&Path>) -> Option<Self> {
        let ext = path?.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "md" | "markdown" => Some(Self::Markdown),
            "canvas" => Some(Self::Canvas),
            _ => None,
        }
    }
}

/// Width of the text column in the Markdown reader, before zoom.
const COLUMN_WIDTH: f32 = 820.0;
/// Space around a canvas when it is fitted into the window.
const FIT_MARGIN: f32 = 80.0;
const NODE_RADIUS: u8 = 8;

pub struct Viewer {
    /// The reader is showing instead of the editor.
    pub on: bool,
    cache: CommonMarkCache,
    alerts: AlertBundle,
    /// Markdown rewritten for the viewer, and the text version it was made from.
    prepared: Option<(u64, String)>,
    canvas: Option<(u64, Result<Canvas, String>)>,
    /// What part of the canvas is in view (canvas coordinates).
    scene_rect: Rect,
    /// Fit the whole canvas into view on the next frame.
    fit: bool,
    /// Contents of canvas file cards, by the path written in the canvas.
    files: HashMap<String, FileCard>,
}

enum FileCard {
    Note { path: PathBuf, text: String },
    Image { path: PathBuf, uri: String },
    Other { path: Option<PathBuf> },
}

impl Default for Viewer {
    fn default() -> Self {
        Self {
            on: false,
            cache: CommonMarkCache::default(),
            alerts: korean_alerts(),
            prepared: None,
            canvas: None,
            scene_rect: Rect::NOTHING,
            fit: true,
            files: HashMap::new(),
        }
    }
}

impl Viewer {
    /// A new document was loaded: forget what was made from the old one.
    pub fn reset(&mut self) {
        self.prepared = None;
        self.canvas = None;
        self.files.clear();
        self.fit = true;
    }

    pub fn fit_canvas(&mut self) {
        self.fit = true;
    }

    /// Zooms the canvas by `factor` around the middle of the view.
    pub fn zoom_canvas(&mut self, factor: f32) {
        if self.scene_rect.is_positive() {
            let c = self.scene_rect.center();
            self.scene_rect = Rect::from_center_size(c, self.scene_rect.size() / factor);
        }
    }
}

/// Obsidian's callouts (`> [!note]`), labelled in Korean.
fn korean_alerts() -> AlertBundle {
    AlertBundle::from_alerts(
        markdown::CALLOUTS
            .iter()
            .map(|&(id, label, icon, [r, g, b])| Alert {
                accent_color: Color32::from_rgb(r, g, b),
                icon,
                identifier: id.to_owned(),
                identifier_rendered: label.to_owned(),
            })
            .collect(),
    )
}

/// Text sizes for reading, scaled by `scale`.
fn reading_style(ui: &mut Ui, scale: f32) {
    let style = ui.style_mut();
    style.text_styles = [
        (TextStyle::Heading, FontId::new(28.0 * scale, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(16.0 * scale, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(14.5 * scale, FontFamily::Monospace)),
        (TextStyle::Button, FontId::new(16.0 * scale, FontFamily::Proportional)),
        (TextStyle::Small, FontId::new(13.0 * scale, FontFamily::Proportional)),
    ]
    .into();
    style.spacing.item_spacing = Vec2::new(8.0, 7.0) * scale;
}

/// Files a link may open in mome rather than in their own program.
fn opens_in_mome(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    matches!(
        ext.as_deref(),
        None | Some("txt" | "md" | "markdown" | "json" | "canvas" | "log" | "csv" | "ini" | "xml" | "yaml" | "yml")
    )
}

/// Removes the clicks on note and file links from egui's output and returns them.
fn take_local_links(ctx: &egui::Context) -> Vec<egui::OpenUrl> {
    ctx.output_mut(|o| {
        let mut taken = Vec::new();
        o.commands.retain(|c| match c {
            OutputCommand::OpenUrl(open) if is_local(&open.url) => {
                taken.push(open.clone());
                false
            }
            _ => true,
        });
        taken
    })
}

/// Links the reader handles itself (notes, files) rather than the browser.
fn is_local(url: &str) -> bool {
    url.starts_with(markdown::WIKI_SCHEME) || url.starts_with("file:") || !markdown::has_scheme(url)
}

impl MomeApp {
    pub(super) fn view_kind(&self) -> Option<ViewKind> {
        ViewKind::of(self.doc.path.as_deref())
    }

    /// Whether the reader is showing now.
    pub(super) fn reading(&self) -> Option<ViewKind> {
        self.view_kind().filter(|_| self.viewer.on)
    }

    pub(super) fn set_reading(&mut self, on: bool) {
        let on = on && self.view_kind().is_some();
        if on && self.find.open {
            self.find.close();
        }
        if on && !self.viewer.on {
            // Pick up edits made since the reader was last shown.
            self.viewer.files.clear();
        }
        self.viewer.on = on;
    }

    fn doc_dir(&self) -> Option<PathBuf> {
        self.doc.path.as_deref().and_then(Path::parent).map(Path::to_path_buf)
    }

    pub(super) fn reader_panel(&mut self, ui: &mut Ui, kind: ViewKind) {
        let p = self.palette();
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(p.editor_bg))
            .show(ui, |ui| match kind {
                ViewKind::Markdown => self.markdown_view(ui),
                ViewKind::Canvas => self.canvas_view(ui, &p),
            });
    }

    fn markdown_view(&mut self, ui: &mut Ui) {
        let dir = self.doc_dir();
        let version = self.text_version;
        if self.viewer.prepared.as_ref().is_none_or(|(v, _)| *v != version) {
            let text = markdown::prepare(&self.doc.text, dir.as_deref());
            self.viewer.prepared = Some((version, text));
        }
        let scale = self.editor.zoom as f32 / 100.0;
        let base = dir.as_deref().map(markdown::base_uri).unwrap_or_else(|| "file:///".to_owned());
        let viewer = &mut self.viewer;
        let text = viewer.prepared.as_ref().map(|(_, t)| t.as_str()).unwrap_or_default();
        ScrollArea::vertical()
            .id_salt("mome-reader")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let full = ui.available_width();
                let column = (COLUMN_WIDTH * scale).min(full - 48.0).max(200.0);
                let side = ((full - column) / 2.0).max(0.0);
                ui.add_space(28.0);
                ui.horizontal_top(|ui| {
                    ui.add_space(side);
                    ui.vertical(|ui| {
                        ui.set_width(column);
                        ui.set_max_width(column);
                        reading_style(ui, scale);
                        if text.trim().is_empty() {
                            ui.label(RichText::new("내용이 없습니다. [편집]을 눌러 글을 써 보세요.").weak());
                        }
                        CommonMarkViewer::new()
                            .default_implicit_uri_scheme(base)
                            .alerts(viewer.alerts.clone())
                            .max_image_width(Some(column as usize))
                            .show(ui, &mut viewer.cache, text);
                    });
                });
                ui.add_space(60.0);
            });
        self.follow_clicked_links(ui.ctx(), dir.as_deref());
    }

    /// Takes the clicks on note and file links out of egui's output (they would go
    /// to the browser) and opens them here.
    fn follow_clicked_links(&mut self, ctx: &egui::Context, dir: Option<&Path>) {
        for open in take_local_links(ctx) {
            match markdown::resolve_link(&open.url, dir) {
                LinkTarget::File(path) => self.open_link(&path, open.new_tab),
                LinkTarget::Url(url) => ctx.open_url(egui::OpenUrl { url, new_tab: open.new_tab }),
                LinkTarget::Missing(name) => {
                    self.notify_warning(format!("'{name}'을(를) 찾을 수 없습니다."));
                }
            }
        }
    }

    /// Opens a linked file: text in this window (a new one with Ctrl or the middle
    /// button), anything else in its own program.
    fn open_link(&mut self, path: &Path, new_window: bool) {
        if !path.exists() {
            self.notify_warning(format!("'{}'을(를) 찾을 수 없습니다.", path.display()));
        } else if !opens_in_mome(path) {
            win::open_folder(path);
        } else if new_window {
            win::spawn_self(&[path.as_os_str()]);
        } else {
            self.pending_cmds.push(Cmd::OpenPath(path.to_path_buf()));
        }
    }

    // ----- canvas ----------------------------------------------------------------

    fn canvas_view(&mut self, ui: &mut Ui, p: &Palette) {
        let dir = self.doc_dir();
        let root = dir.as_deref().map(|d| markdown::vault_root(d).unwrap_or_else(|| d.to_path_buf()));
        let version = self.text_version;
        if self.viewer.canvas.as_ref().is_none_or(|(v, _)| *v != version) {
            let parsed = Canvas::parse(&self.doc.text).map_err(|e| e.to_string()).map(|mut canvas| {
                for node in &mut canvas.nodes {
                    if let NodeKind::Text { text } = &mut node.kind {
                        *text = markdown::prepare(text, root.as_deref());
                    }
                }
                canvas
            });
            self.viewer.canvas = Some((version, parsed));
        }

        let canvas = match &self.viewer.canvas {
            Some((_, Ok(canvas))) => canvas,
            Some((_, Err(err))) => {
                let err = err.clone();
                self.canvas_message(ui, "캔버스 파일을 읽을 수 없습니다", &err);
                return;
            }
            None => return,
        };
        let Some(bounds) = canvas.bounds() else {
            self.canvas_message(ui, "빈 캔버스입니다", "옵시디언에서 카드를 추가하면 여기에 보입니다.");
            return;
        };
        // Cards that show files: read each file once.
        for node in &canvas.nodes {
            if let NodeKind::File { file, .. } = &node.kind {
                if !self.viewer.files.contains_key(file) {
                    let card = load_file_card(file, dir.as_deref(), root.as_deref());
                    self.viewer.files.insert(file.clone(), card);
                }
            }
        }
        if self.viewer.fit || !self.viewer.scene_rect.is_positive() {
            self.viewer.scene_rect = bounds.expand(FIT_MARGIN);
            self.viewer.fit = false;
        }

        let panel = ui.max_rect();
        let viewer = &mut self.viewer;
        let Some((_, Ok(canvas))) = &viewer.canvas else {
            return;
        };
        let mut clicked_file: Option<(PathBuf, bool)> = None;
        egui::Scene::new()
            .zoom_range(0.05..=2.0)
            .max_inner_size(Vec2::splat(1.0e6))
            .show(ui, &mut viewer.scene_rect, |ui| {
                ui.style_mut().interaction.selectable_labels = false;
                reading_style(ui, 1.0);
                let painter = ui.painter().clone();

                // Groups first (largest at the back), then edges, then cards.
                let mut groups: Vec<_> = canvas.nodes.iter().filter(|n| n.is_group()).collect();
                groups.sort_by(|a, b| (b.width * b.height).total_cmp(&(a.width * a.height)));
                for node in groups {
                    let NodeKind::Group { label } = &node.kind else { continue };
                    let rect = node.rect();
                    let color = canvas::color(node.color.as_deref()).unwrap_or(p.weak);
                    painter.rect(
                        rect,
                        CornerRadius::same(NODE_RADIUS),
                        color.gamma_multiply(0.07),
                        Stroke::new(2.0, color.gamma_multiply(0.6)),
                        StrokeKind::Inside,
                    );
                    if let Some(label) = label.as_deref().filter(|l| !l.is_empty()) {
                        painter.text(
                            rect.left_top() + Vec2::new(4.0, -6.0),
                            Align2::LEFT_BOTTOM,
                            label,
                            FontId::proportional(20.0),
                            p.editor_fg,
                        );
                    }
                }

                for edge in &canvas.edges {
                    let (Some(from), Some(to)) = (canvas.node(&edge.from_node), canvas.node(&edge.to_node)) else {
                        continue;
                    };
                    let color = canvas::color(edge.color.as_deref()).unwrap_or(p.weak);
                    paint_edge(&painter, edge, from.rect(), to.rect(), color, p);
                }

                for node in canvas.nodes.iter().filter(|n| !n.is_group()) {
                    let rect = node.rect();
                    let color = canvas::color(node.color.as_deref());
                    let fill = match color {
                        Some(c) => blend(p.editor_bg, c, 0.10),
                        None => p.editor_bg,
                    };
                    let border = color.unwrap_or(p.border);
                    painter.rect(rect, CornerRadius::same(NODE_RADIUS), fill, Stroke::new(2.0, border), StrokeKind::Inside);

                    let inner = rect.shrink2(Vec2::new(14.0, 10.0));
                    let builder = UiBuilder::new()
                        .max_rect(inner)
                        .layout(Layout::top_down(Align::Min))
                        .id_salt(("canvas-node", &node.id));
                    ui.scope_builder(builder, |ui| {
                        ui.set_clip_rect(rect.shrink(2.0).intersect(ui.clip_rect()));
                        match &node.kind {
                            NodeKind::Text { text } => {
                                let base = root.as_deref().map(markdown::base_uri).unwrap_or_default();
                                CommonMarkViewer::new()
                                    .default_implicit_uri_scheme(base)
                                    .alerts(viewer.alerts.clone())
                                    .max_image_width(Some(inner.width() as usize))
                                    .show(ui, &mut viewer.cache, text);
                            }
                            NodeKind::File { file, .. } => {
                                let card = viewer.files.get(file);
                                if let Some(open) = file_card(ui, file, card, &mut viewer.cache, &viewer.alerts, p) {
                                    clicked_file = Some(open);
                                }
                            }
                            NodeKind::Link { url } => {
                                ui.label(RichText::new("웹 링크").small().color(p.weak));
                                ui.hyperlink_to(url.as_str(), url.as_str());
                            }
                            NodeKind::Group { .. } | NodeKind::Unknown => {}
                        }
                    });
                }
            });

        self.canvas_buttons(ui, panel);
        if let Some((path, new_window)) = clicked_file {
            self.open_link(&path, new_window);
        }
        self.follow_clicked_links(ui.ctx(), root.as_deref());
    }

    /// "전체 보기" in the top-right corner of the canvas.
    fn canvas_buttons(&mut self, ui: &Ui, panel: Rect) {
        egui::Area::new(egui::Id::new("mome-canvas-buttons"))
            .order(egui::Order::Middle)
            .fixed_pos(panel.right_top() + Vec2::new(-12.0, 10.0))
            .pivot(Align2::RIGHT_TOP)
            .show(ui.ctx(), |ui| {
                ui.horizontal(|ui| {
                    if ui.button("−").on_hover_text("축소 (Ctrl+빼기)").clicked() {
                        self.viewer.zoom_canvas(1.0 / 1.25);
                    }
                    if ui.button("+").on_hover_text("확대 (Ctrl+더하기)").clicked() {
                        self.viewer.zoom_canvas(1.25);
                    }
                    if ui.button("전체 보기").on_hover_text("캔버스 전체가 보이게 맞춥니다 (Ctrl+0)").clicked() {
                        self.viewer.fit_canvas();
                    }
                });
            });
    }

    fn canvas_message(&mut self, ui: &mut Ui, title: &str, detail: &str) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.3);
            ui.label(RichText::new(title).size(18.0).strong());
            ui.add_space(4.0);
            ui.label(RichText::new(detail).weak());
            ui.add_space(12.0);
            if ui.button("편집 모드로 보기").clicked() {
                self.pending_cmds.push(Cmd::ToggleReader);
            }
        });
    }
}

fn load_file_card(file: &str, dir: Option<&Path>, root: Option<&Path>) -> FileCard {
    let direct = root.map(|r| r.join(file)).filter(|p| p.is_file());
    let path = direct.or_else(|| dir.and_then(|d| markdown::find_file(file, d)));
    let Some(path) = path else {
        return FileCard::Other { path: None };
    };
    if markdown::is_image(&path) {
        let uri = markdown::file_uri(&path);
        return FileCard::Image { path, uri };
    }
    let is_note = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("txt"));
    if is_note {
        if let Ok(bytes) = std::fs::read(&path) {
            let decoded = crate::encoding::decode(&bytes, None);
            let base = path.parent().map(Path::to_path_buf);
            let text = markdown::prepare(&decoded.text, base.as_deref());
            return FileCard::Note { path, text };
        }
    }
    FileCard::Other { path: Some(path) }
}

/// The inside of a file card. Returns the file to open when its name is clicked.
fn file_card(
    ui: &mut Ui,
    file: &str,
    card: Option<&FileCard>,
    cache: &mut CommonMarkCache,
    alerts: &AlertBundle,
    p: &Palette,
) -> Option<(PathBuf, bool)> {
    let name = Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.to_owned());
    let path = match card {
        Some(FileCard::Note { path, .. } | FileCard::Image { path, .. } | FileCard::Other { path: Some(path) }) => {
            Some(path.clone())
        }
        _ => None,
    };
    let mut open = None;
    if let Some(path) = &path {
        let response = ui
            .link(RichText::new(name.clone()).small().color(p.accent))
            .on_hover_text("클릭하면 이 창에서, Ctrl+클릭하면 새 창에서 엽니다.");
        if response.clicked() || response.middle_clicked() {
            let new_window = response.middle_clicked() || ui.input(|i| i.modifiers.command);
            open = Some((path.clone(), new_window));
        }
    } else {
        ui.label(RichText::new(format!("{name} (찾을 수 없음)")).small().color(p.warning));
    }
    match card {
        Some(FileCard::Note { path, text }) => {
            let base = path.parent().map(markdown::base_uri).unwrap_or_default();
            CommonMarkViewer::new()
                .default_implicit_uri_scheme(base)
                .alerts(alerts.clone())
                .max_image_width(Some(ui.available_width() as usize))
                .show(ui, cache, text);
        }
        Some(FileCard::Image { uri, .. }) => {
            let size = ui.available_size();
            ui.add(egui::Image::from_uri(uri.as_str()).max_size(size).maintain_aspect_ratio(true));
        }
        _ => {}
    }
    open
}

/// An edge as Obsidian draws it: a curve leaving and entering the cards at right
/// angles, with arrowheads and an optional label in the middle.
fn paint_edge(painter: &egui::Painter, edge: &canvas::Edge, from: Rect, to: Rect, color: Color32, p: &Palette) {
    let (from_side, to_side) = edge.sides(from, to);
    let start = from_side.anchor(from);
    let end = to_side.anchor(to);
    let reach = (start.distance(end) * 0.4).clamp(40.0, 300.0);
    let points = [start, start + from_side.normal() * reach, end + to_side.normal() * reach, end];
    let curve = CubicBezierShape::from_points_stroke(points, false, Color32::TRANSPARENT, Stroke::new(2.5, color));
    let middle = curve.sample(0.5);
    painter.add(curve);
    if edge.arrow_at_end() {
        paint_arrow(painter, end, -to_side.normal(), color);
    }
    if edge.arrow_at_start() {
        paint_arrow(painter, start, -from_side.normal(), color);
    }
    if let Some(label) = edge.label.as_deref().filter(|l| !l.is_empty()) {
        let galley = painter.layout_no_wrap(label.to_owned(), FontId::proportional(15.0), p.editor_fg);
        let rect = Rect::from_center_size(middle, galley.size()).expand2(Vec2::new(6.0, 3.0));
        painter.rect_filled(rect, CornerRadius::same(4), p.editor_bg);
        painter.galley(rect.min + Vec2::new(6.0, 3.0), galley, p.editor_fg);
    }
}

/// A filled arrowhead with its tip at `tip`, pointing along `dir`.
fn paint_arrow(painter: &egui::Painter, tip: Pos2, dir: Vec2, color: Color32) {
    let back = tip - dir * 14.0;
    let side = dir.rot90() * 7.0;
    painter.add(Shape::convex_polygon(vec![tip, back + side, back - side], color, Stroke::NONE));
}

/// `base` tinted with `amount` of `tint`.
fn blend(base: Color32, tint: Color32, amount: f32) -> Color32 {
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount).round() as u8;
    Color32::from_rgb(mix(base.r(), tint.r()), mix(base.g(), tint.g()), mix(base.b(), tint.b()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(ViewKind::of(Some(Path::new("a.MD"))), Some(ViewKind::Markdown));
        assert_eq!(ViewKind::of(Some(Path::new("보드.canvas"))), Some(ViewKind::Canvas));
        assert_eq!(ViewKind::of(Some(Path::new("a.txt"))), None);
        assert_eq!(ViewKind::of(None), None);
    }

    #[test]
    fn takes_note_links_from_output() {
        let ctx = egui::Context::default();
        let mut taken = Vec::new();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.ctx().open_url(egui::OpenUrl::same_tab("wiki:노트"));
            ui.ctx().open_url(egui::OpenUrl::new_tab("https://example.com"));
            taken = take_local_links(ui.ctx());
        });
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].url, "wiki:노트");
        let urls: Vec<_> = output
            .platform_output
            .commands
            .iter()
            .filter_map(|c| match c {
                OutputCommand::OpenUrl(o) => Some(o.url.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(urls, ["https://example.com"]);
        output.textures_delta.clear();
    }

    #[test]
    fn local_links() {
        assert!(is_local("wiki:노트"));
        assert!(is_local("다른 노트.md"));
        assert!(is_local("file:///C:/a.md"));
        assert!(!is_local("https://example.com"));
        assert!(!is_local("mailto:a@b.c"));
        assert!(opens_in_mome(Path::new("a.md")));
        assert!(!opens_in_mome(Path::new("a.pdf")));
    }
}
