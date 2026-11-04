//! Obsidian canvas files (JSON Canvas 1.0, `.canvas`): reading and geometry.
//! <https://jsoncanvas.org/spec/1.0/>

use egui::{Color32, Pos2, Rect, Vec2};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct Canvas {
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

#[derive(Debug, Deserialize)]
pub struct Node {
    pub id: String,
    #[serde(flatten)]
    pub kind: NodeKind,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub color: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum NodeKind {
    Text {
        #[serde(default)]
        text: String,
    },
    File {
        file: String,
    },
    Link {
        url: String,
    },
    Group {
        label: Option<String>,
    },
    /// A node type from a newer version or a plugin: drawn as an empty card.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    pub from_node: String,
    pub from_side: Option<Side>,
    pub from_end: Option<End>,
    pub to_node: String,
    pub to_side: Option<Side>,
    pub to_end: Option<End>,
    pub color: Option<String>,
    pub label: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum End {
    None,
    Arrow,
}

impl Canvas {
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(text)
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// The area all nodes cover, or `None` for an empty canvas.
    pub fn bounds(&self) -> Option<Rect> {
        self.nodes.iter().map(Node::rect).reduce(|a, b| a.union(b))
    }
}

impl Node {
    pub fn rect(&self) -> Rect {
        Rect::from_min_size(Pos2::new(self.x, self.y), Vec2::new(self.width.max(1.0), self.height.max(1.0)))
    }

    pub fn is_group(&self) -> bool {
        matches!(self.kind, NodeKind::Group { .. })
    }
}

impl Edge {
    /// Where the edge leaves `from` and enters `to`: the given sides, or else the
    /// sides that face each other.
    pub fn sides(&self, from: Rect, to: Rect) -> (Side, Side) {
        let d = to.center() - from.center();
        let facing = if d.x.abs() >= d.y.abs() {
            if d.x >= 0.0 {
                (Side::Right, Side::Left)
            } else {
                (Side::Left, Side::Right)
            }
        } else if d.y >= 0.0 {
            (Side::Bottom, Side::Top)
        } else {
            (Side::Top, Side::Bottom)
        };
        (self.from_side.unwrap_or(facing.0), self.to_side.unwrap_or(facing.1))
    }

    pub fn arrow_at_start(&self) -> bool {
        self.from_end == Some(End::Arrow)
    }

    pub fn arrow_at_end(&self) -> bool {
        self.to_end != Some(End::None)
    }
}

impl Side {
    /// The middle of this side of `rect`.
    pub fn anchor(self, rect: Rect) -> Pos2 {
        match self {
            Side::Top => rect.center_top(),
            Side::Right => rect.right_center(),
            Side::Bottom => rect.center_bottom(),
            Side::Left => rect.left_center(),
        }
    }

    /// Unit vector pointing out of the node.
    pub fn normal(self) -> Vec2 {
        match self {
            Side::Top => Vec2::new(0.0, -1.0),
            Side::Right => Vec2::new(1.0, 0.0),
            Side::Bottom => Vec2::new(0.0, 1.0),
            Side::Left => Vec2::new(-1.0, 0.0),
        }
    }
}

/// A canvas color: the presets "1"–"6" (Obsidian's red, orange, yellow, green,
/// cyan, purple) or "#rrggbb".
pub fn color(value: Option<&str>) -> Option<Color32> {
    let value = value?.trim();
    let preset = match value {
        "1" => Some((0xE9, 0x3F, 0x47)),
        "2" => Some((0xEC, 0x75, 0x00)),
        "3" => Some((0xE0, 0xAC, 0x00)),
        "4" => Some((0x08, 0xB9, 0x4E)),
        "5" => Some((0x00, 0xBF, 0xBC)),
        "6" => Some((0x78, 0x52, 0xEE)),
        _ => None,
    };
    if let Some((r, g, b)) = preset {
        return Some(Color32::from_rgb(r, g, b));
    }
    let hex = value.strip_prefix('#')?;
    let hex = match hex.len() {
        3 => hex.chars().flat_map(|c| [c, c]).collect(),
        6 => hex.to_owned(),
        _ => return None,
    };
    let v = u32::from_str_radix(&hex, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"{
        "nodes": [
            {"id": "a", "type": "text", "text": "# 안녕", "x": 0, "y": 0, "width": 200, "height": 100},
            {"id": "b", "type": "file", "file": "노트.md", "x": 400, "y": 50, "width": 200, "height": 100, "color": "4"},
            {"id": "c", "type": "link", "url": "https://obsidian.md", "x": 0, "y": 300, "width": 200, "height": 60},
            {"id": "g", "type": "group", "label": "묶음", "x": -20, "y": -20, "width": 700, "height": 400, "color": "#abc"},
            {"id": "z", "type": "fancy", "x": 0, "y": 0, "width": 10, "height": 10}
        ],
        "edges": [
            {"id": "e", "fromNode": "a", "toNode": "b", "toEnd": "none"},
            {"id": "f", "fromNode": "a", "fromSide": "bottom", "toNode": "c", "toSide": "top", "label": "참고"}
        ]
    }"##;

    #[test]
    fn parses_sample() {
        let canvas = Canvas::parse(SAMPLE).unwrap();
        assert_eq!(canvas.nodes.len(), 5);
        assert!(matches!(&canvas.nodes[0].kind, NodeKind::Text { text } if text == "# 안녕"));
        assert!(matches!(&canvas.nodes[1].kind, NodeKind::File { file, .. } if file == "노트.md"));
        assert!(matches!(canvas.nodes[4].kind, NodeKind::Unknown));
        assert!(canvas.nodes[3].is_group());
        assert_eq!(canvas.bounds(), Some(Rect::from_min_max(Pos2::new(-20.0, -20.0), Pos2::new(680.0, 380.0))));

        let e = &canvas.edges[0];
        assert!(!e.arrow_at_end() && !e.arrow_at_start());
        let (a, b) = (canvas.node("a").unwrap().rect(), canvas.node("b").unwrap().rect());
        assert_eq!(e.sides(a, b), (Side::Right, Side::Left));
        let f = &canvas.edges[1];
        assert!(f.arrow_at_end());
        assert_eq!(f.sides(a, b), (Side::Bottom, Side::Top));
        assert_eq!(f.label.as_deref(), Some("참고"));
    }

    #[test]
    fn empty_and_invalid() {
        assert!(Canvas::parse("").unwrap().nodes.is_empty());
        assert!(Canvas::parse("{}").unwrap().bounds().is_none());
        assert!(Canvas::parse("{\"nodes\": 3}").is_err());
    }

    #[test]
    fn colors() {
        assert_eq!(color(Some("1")), Some(Color32::from_rgb(0xE9, 0x3F, 0x47)));
        assert_eq!(color(Some("#abc")), Some(Color32::from_rgb(0xAA, 0xBB, 0xCC)));
        assert_eq!(color(Some("#102030")), Some(Color32::from_rgb(0x10, 0x20, 0x30)));
        assert_eq!(color(Some("빨강")), None);
        assert_eq!(color(None), None);
    }
}
