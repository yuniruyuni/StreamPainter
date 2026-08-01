//! ローカル WebSocket プロトコルの serde 型 (docs/protocol.md)。
//! TypeScript 側の `client/src/protocol.ts` と JSON 表現を揃える。
//! フィールドは camelCase、type タグは snake_case、点は [u, v, p, dt] の配列。

use serde::{Deserialize, Serialize};

/// [u, v, p, dt] — 正規化座標・筆圧 0..1・stroke_begin からの相対 ms
pub type Point = (f64, f64, f64, f64);

/// painter / local hub / overlay が同じ値で適用する上限。
pub const PROTOCOL_VERSION: u32 = 5;
pub const MAX_ITEMS: usize = 500;
pub const MAX_TOTAL_POINTS: usize = 200_000;
pub const MAX_STROKE_POINTS: usize = 10_000;
pub const MAX_POINTS_PER_MESSAGE: usize = 512;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Pen,
    Marker,
    Eraser,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Brush {
    pub tool: Tool,
    pub color: String,
    pub opacity: f64,
    pub width_n: f64,
    pub pressure_width: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stroke {
    pub stroke_id: String,
    pub brush: Brush,
    pub pts: Vec<Point>,
    pub done: bool,
    pub ended_at: Option<f64>,
}

/// キャンバス内の正規化座標 [u, v]。
pub type Position = (f64, f64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    Line,
    Arrow,
    Rectangle,
    Ellipse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineStyle {
    pub color: String,
    pub opacity: f64,
    pub width_n: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShapeItem {
    pub item_id: String,
    pub shape: ShapeKind,
    pub style: LineStyle,
    pub start: Position,
    pub end: Position,
    pub done: bool,
    pub ended_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StampItem {
    pub item_id: String,
    pub stamp_id: String,
    pub center: Position,
    /// キャンバス幅・高さに対する正規化表示サイズ。
    pub width_n: f64,
    pub height_n: f64,
    pub opacity: f64,
    pub done: bool,
    pub ended_at: Option<f64>,
}

/// 描画順を保つ単一の履歴。v1 の Stroke は互換 snapshot 用にも残す。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CanvasItem {
    Stroke {
        #[serde(flatten)]
        stroke: Stroke,
    },
    Shape {
        #[serde(flatten)]
        shape: ShapeItem,
    },
    Stamp {
        #[serde(flatten)]
        stamp: StampItem,
    },
}

impl CanvasItem {
    pub fn item_id(&self) -> &str {
        match self {
            Self::Stroke { stroke } => &stroke.stroke_id,
            Self::Shape { shape } => &shape.item_id,
            Self::Stamp { stamp } => &stamp.item_id,
        }
    }

    pub fn is_done(&self) -> bool {
        match self {
            Self::Stroke { stroke } => stroke.done,
            Self::Shape { shape } => shape.done,
            Self::Stamp { stamp } => stamp.done,
        }
    }

    pub fn point_count(&self) -> usize {
        match self {
            Self::Stroke { stroke } => stroke.pts.len(),
            Self::Shape { .. } | Self::Stamp { .. } => 0,
        }
    }
}

/// Win32 入力層 → ローカル WebSocket ハブ → OBS overlay。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PainterMessage {
    #[serde(rename_all = "camelCase")]
    StrokeBegin {
        stroke_id: String,
        brush: Brush,
    },
    #[serde(rename_all = "camelCase")]
    StrokePoints {
        stroke_id: String,
        pts: Vec<Point>,
    },
    #[serde(rename_all = "camelCase")]
    StrokeEnd {
        stroke_id: String,
        ended_at: f64,
    },
    #[serde(rename_all = "camelCase")]
    StrokeCancel {
        stroke_id: String,
    },
    #[serde(rename_all = "camelCase")]
    ShapeBegin {
        shape: ShapeItem,
    },
    #[serde(rename_all = "camelCase")]
    ShapeUpdate {
        item_id: String,
        end: Position,
    },
    #[serde(rename_all = "camelCase")]
    ShapeEnd {
        item_id: String,
        ended_at: f64,
    },
    #[serde(rename_all = "camelCase")]
    ShapeCancel {
        item_id: String,
    },
    #[serde(rename_all = "camelCase")]
    StampAdd {
        stamp: StampItem,
    },
    #[serde(rename_all = "camelCase")]
    StampMovePreview {
        item_id: String,
        center: Position,
    },
    #[serde(rename_all = "camelCase")]
    StampMove {
        item_id: String,
        center: Position,
    },
    Undo {},
    Redo {
        item: CanvasItem,
    },
    Clear {},
}

/// ハブが確定したrevisionを内部描画イベントへ付与した、overlay向け増分イベント。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayEvent<'a> {
    pub rev: u64,
    #[serde(flatten)]
    pub event: &'a PainterMessage,
}

/// ローカルハブ → OBS overlay の接続管理メッセージ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OverlayControlMessage {
    #[serde(rename_all = "camelCase")]
    Snapshot {
        protocol_version: u32,
        rev: u64,
        fade_after_ms: Option<f64>,
        /// 描画順を保つ完全な描画履歴。
        items: Vec<CanvasItem>,
    },
    Pong {
        t: f64,
    },
}

/// OBS overlay → ローカルハブ。未知の type は前方互換のため無視する。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OverlayClientMessage {
    Ping {
        t: f64,
    },
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pen() -> Brush {
        Brush {
            tool: Tool::Pen,
            color: "#ff4d6d".into(),
            opacity: 1.0,
            width_n: 0.005,
            pressure_width: true,
        }
    }

    #[test]
    fn stroke_begin_json_matches_protocol() {
        let msg = PainterMessage::StrokeBegin {
            stroke_id: "s1".into(),
            brush: pen(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "stroke_begin",
                "strokeId": "s1",
                "brush": {
                    "tool": "pen",
                    "color": "#ff4d6d",
                    "opacity": 1.0,
                    "widthN": 0.005,
                    "pressureWidth": true,
                },
            })
        );
    }

    #[test]
    fn stroke_points_serializes_as_arrays() {
        let msg = PainterMessage::StrokePoints {
            stroke_id: "s1".into(),
            pts: vec![(0.1, 0.2, 0.5, 0.0), (0.15, 0.25, 0.6, 16.0)],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "stroke_points",
                "strokeId": "s1",
                "pts": [[0.1, 0.2, 0.5, 0.0], [0.15, 0.25, 0.6, 16.0]],
            })
        );
    }

    #[test]
    fn stroke_end_includes_timestamp() {
        let msg = PainterMessage::StrokeEnd {
            stroke_id: "s1".into(),
            ended_at: 1234.0,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "stroke_end",
                "strokeId": "s1",
                "endedAt": 1234.0,
            })
        );
        let back: PainterMessage = serde_json::from_value(json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn overlay_message_roundtrips_snapshot() {
        let msg: OverlayControlMessage = serde_json::from_str(
            r##"{"type":"snapshot","protocolVersion":5,"rev":3,"fadeAfterMs":null,"items":[{"kind":"stroke","strokeId":"s1","brush":{"tool":"pen","color":"#ff4d6d","opacity":1,"widthN":0.005,"pressureWidth":true},"pts":[[0.1,0.2,0.5,0]],"done":true,"endedAt":123}]}"##,
        )
        .unwrap();
        match msg {
            OverlayControlMessage::Snapshot {
                protocol_version,
                rev,
                items,
                ..
            } => {
                assert_eq!(protocol_version, PROTOCOL_VERSION);
                assert_eq!(rev, 3);
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].item_id(), "s1");
            }
            OverlayControlMessage::Pong { .. } => panic!("unexpected pong"),
        }
    }

    #[test]
    fn overlay_event_flattens_revision_into_the_paint_event() {
        let message = PainterMessage::StrokeEnd {
            stroke_id: "s1".into(),
            ended_at: 1234.0,
        };
        let json = serde_json::to_value(OverlayEvent {
            rev: 7,
            event: &message,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "stroke_end",
                "strokeId": "s1",
                "endedAt": 1234.0,
                "rev": 7,
            })
        );
    }

    #[test]
    fn stamp_move_events_identify_the_item_and_destination() {
        for (message, message_type) in [
            (
                PainterMessage::StampMovePreview {
                    item_id: "stamp-item-1".into(),
                    center: (0.75, 0.25),
                },
                "stamp_move_preview",
            ),
            (
                PainterMessage::StampMove {
                    item_id: "stamp-item-1".into(),
                    center: (0.75, 0.25),
                },
                "stamp_move",
            ),
        ] {
            let json = serde_json::to_value(&message).unwrap();
            assert_eq!(
                json,
                serde_json::json!({
                    "type": message_type,
                    "itemId": "stamp-item-1",
                    "center": [0.75, 0.25],
                })
            );
            assert_eq!(
                serde_json::from_value::<PainterMessage>(json).unwrap(),
                message
            );
        }
    }

    #[test]
    fn canvas_items_are_internally_tagged_and_flattened() {
        let item = CanvasItem::Shape {
            shape: ShapeItem {
                item_id: "shape-1".into(),
                shape: ShapeKind::Arrow,
                style: LineStyle {
                    color: "#ffffff".into(),
                    opacity: 0.8,
                    width_n: 0.01,
                },
                start: (0.1, 0.2),
                end: (0.8, 0.7),
                done: true,
                ended_at: Some(42.0),
            },
        };
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["kind"], "shape");
        assert_eq!(json["itemId"], "shape-1");
        assert_eq!(json["shape"], "arrow");
        assert!(json.get("shape_item").is_none());
        assert_eq!(serde_json::from_value::<CanvasItem>(json).unwrap(), item);
    }

    #[test]
    fn overlay_client_message_parses_ping_and_unknown() {
        let ping: OverlayClientMessage =
            serde_json::from_str(r#"{"type":"ping","t":123}"#).unwrap();
        assert_eq!(ping, OverlayClientMessage::Ping { t: 123.0 });
        let unknown: OverlayClientMessage =
            serde_json::from_str(r#"{"type":"someday_new_message"}"#).unwrap();
        assert_eq!(unknown, OverlayClientMessage::Unknown);
    }
}
