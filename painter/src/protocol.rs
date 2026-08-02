//! ローカル WebSocket プロトコルの serde 型 (docs/protocol.md)。
//! TypeScript 側の `client/src/protocol.ts` と JSON 表現を揃える。
//! フィールドは camelCase、type タグは snake_case、点は
//! [u, v, pressure, dt, tilt_x, tilt_y] の配列。

use serde::{Deserialize, Serialize};

/// [u, v, pressure, dt, tilt_x, tilt_y]
///
/// 座標・筆圧は0..1、tiltはWindowsの±90°を-1..1へ正規化した値。
/// dtはstroke_beginからの相対ms。v5の4要素prefixを保ったままtiltを末尾へ追加する。
pub type Point = (f64, f64, f64, f64, f64, f64);

/// painter / local hub / overlay が同じ値で適用する上限。
pub const PROTOCOL_VERSION: u32 = 7;
/// v6は筆圧・傾き対応済みで、transform field/eventだけが存在しない。
pub const MIN_COMPATIBLE_PROTOCOL_VERSION: u32 = 6;
const _: () = assert!(MIN_COMPATIBLE_PROTOCOL_VERSION <= PROTOCOL_VERSION);
pub const MAX_ITEMS: usize = 500;
pub const MAX_TOTAL_POINTS: usize = 200_000;
pub const MAX_STROKE_POINTS: usize = 10_000;
pub const MAX_POINTS_PER_MESSAGE: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// pressure=0のときの基準幅に対する倍率。0.05..1。
    pub pressure_min: f64,
    /// tilt magnitudeを線幅へ反映するか。
    pub tilt_width: bool,
    /// tilt magnitude=1のときの最大倍率。1..4。
    pub tilt_max_scale: f64,
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

/// shape / stampで共有する永続transform。
///
/// `width_n`はcontent幅、`height_n`はcontent高さに対する比率、`rotation`はcanvas上の
/// 時計回りradian。shapeの旧`start`/`end`はv6 snapshotのfallbackとして残す。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemTransform {
    pub center: Position,
    pub width_n: f64,
    pub height_n: f64,
    pub rotation: f64,
}

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
    /// v6 snapshotでは存在しない。Noneの場合はstart/endから回転なしのgeometryを復元する。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<ItemTransform>,
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
    /// v6 snapshotでは存在せず、serde defaultの0 radianとして移行する。
    #[serde(default)]
    pub rotation: f64,
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

// enum と canonical fixture の例を同じ宣言から作る。variant を追加したのに
// Rust → TypeScript conformance fixture を追加し忘れるとコンパイルできない。
macro_rules! define_painter_messages {
    (
        $(
            $(#[$variant_meta:meta])*
            $variant:ident {
                $(
                    $(#[$field_meta:meta])*
                    $field:ident: $field_type:ty
                ),* $(,)?
            } => $fixture:expr
        ),+ $(,)?
    ) => {
        /// Win32 入力層 → ローカル WebSocket ハブ → OBS overlay。
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        pub enum PainterMessage {
            $(
                $(#[$variant_meta])*
                $variant {
                    $(
                        $(#[$field_meta])*
                        $field: $field_type,
                    )*
                },
            )+
        }

        #[cfg(test)]
        pub(crate) fn canonical_painter_messages() -> Vec<PainterMessage> {
            vec![$($fixture),+]
        }
    };
}

define_painter_messages! {
    #[serde(rename_all = "camelCase")]
    StrokeBegin {
        stroke_id: String,
        brush: Brush,
    } => PainterMessage::StrokeBegin {
        stroke_id: "fixture-stroke-begin".into(),
        brush: fixture_brush(Tool::Marker),
    },
    #[serde(rename_all = "camelCase")]
    StrokePoints {
        stroke_id: String,
        /// source canvas 上で `pts[0]` が占める点番号。
        ///
        /// snapshot 復旧との重複を避けるためのプロセス内 metadata であり、
        /// WebSocket protocol には含めない。
        #[serde(skip)]
        offset: usize,
        pts: Vec<Point>,
    } => PainterMessage::StrokePoints {
        stroke_id: "fixture-stroke-points".into(),
        offset: 1,
        pts: vec![
            (0.2, 0.3, 0.6, 16.0, 0.25, -0.5),
            (0.4, 0.5, 0.7, 32.0, 0.5, -0.25),
        ],
    },
    #[serde(rename_all = "camelCase")]
    StrokeEnd {
        stroke_id: String,
        ended_at: f64,
    } => PainterMessage::StrokeEnd {
        stroke_id: "fixture-stroke-end".into(),
        ended_at: 1_700_000_000_123.0,
    },
    #[serde(rename_all = "camelCase")]
    StrokeCancel {
        stroke_id: String,
    } => PainterMessage::StrokeCancel {
        stroke_id: "fixture-stroke-cancel".into(),
    },
    #[serde(rename_all = "camelCase")]
    ShapeBegin {
        shape: ShapeItem,
    } => PainterMessage::ShapeBegin {
        shape: fixture_shape("fixture-shape-begin", ShapeKind::Arrow, false),
    },
    #[serde(rename_all = "camelCase")]
    ShapeUpdate {
        item_id: String,
        end: Position,
    } => PainterMessage::ShapeUpdate {
        item_id: "fixture-shape-update".into(),
        end: (0.8, 0.7),
    },
    #[serde(rename_all = "camelCase")]
    ShapeEnd {
        item_id: String,
        ended_at: f64,
        /// v6 eventには存在しない。v7で確定shapeのsize/rotationを永続化する。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transform: Option<ItemTransform>,
    } => PainterMessage::ShapeEnd {
        item_id: "fixture-shape-end".into(),
        ended_at: 1_700_000_000_456.0,
        transform: Some(fixture_transform()),
    },
    #[serde(rename_all = "camelCase")]
    ShapeCancel {
        item_id: String,
    } => PainterMessage::ShapeCancel {
        item_id: "fixture-shape-cancel".into(),
    },
    #[serde(rename_all = "camelCase")]
    StampAdd {
        stamp: StampItem,
    } => PainterMessage::StampAdd {
        stamp: fixture_stamp("fixture-stamp-add", false),
    },
    #[serde(rename_all = "camelCase")]
    StampMovePreview {
        item_id: String,
        center: Position,
    } => PainterMessage::StampMovePreview {
        item_id: "fixture-stamp-preview".into(),
        center: (0.65, 0.45),
    },
    #[serde(rename_all = "camelCase")]
    StampMove {
        item_id: String,
        center: Position,
    } => PainterMessage::StampMove {
        item_id: "fixture-stamp-move".into(),
        center: (0.75, 0.6),
    },
    #[serde(rename_all = "camelCase")]
    ItemTransformPreview {
        item_id: String,
        transform: ItemTransform,
    } => PainterMessage::ItemTransformPreview {
        item_id: "fixture-transform-preview".into(),
        transform: fixture_transform(),
    },
    #[serde(rename_all = "camelCase")]
    ItemTransformCommit {
        item_id: String,
        transform: ItemTransform,
    } => PainterMessage::ItemTransformCommit {
        item_id: "fixture-transform-commit".into(),
        transform: fixture_transform(),
    },
    Undo {} => PainterMessage::Undo {},
    Redo {
        item: CanvasItem,
    } => PainterMessage::Redo {
        item: CanvasItem::Stamp {
            stamp: fixture_stamp("fixture-redo", true),
        },
    },
    Clear {} => PainterMessage::Clear {},
}

#[cfg(test)]
fn fixture_brush(tool: Tool) -> Brush {
    Brush {
        tool,
        color: "#12abef".into(),
        opacity: 0.75,
        width_n: 0.0125,
        pressure_width: true,
        pressure_min: 0.2,
        tilt_width: tool == Tool::Marker,
        tilt_max_scale: if tool == Tool::Marker { 1.75 } else { 1.0 },
    }
}

#[cfg(test)]
fn fixture_shape(item_id: &str, shape: ShapeKind, done: bool) -> ShapeItem {
    ShapeItem {
        item_id: item_id.into(),
        shape,
        style: LineStyle {
            color: "#fedcba".into(),
            opacity: 0.625,
            width_n: 0.01,
        },
        start: (0.1, 0.2),
        end: (0.3, 0.4),
        transform: done.then_some(fixture_transform()),
        done,
        ended_at: done.then_some(1_700_000_000_000.0),
    }
}

#[cfg(test)]
fn fixture_stamp(item_id: &str, done: bool) -> StampItem {
    StampItem {
        item_id: item_id.into(),
        stamp_id: "fixture-stamp".into(),
        center: (0.25, 0.35),
        width_n: 0.125,
        height_n: 0.225,
        rotation: 0.375,
        opacity: 0.875,
        done,
        ended_at: Some(1_700_000_000_789.0),
    }
}

#[cfg(test)]
fn fixture_transform() -> ItemTransform {
    ItemTransform {
        center: (0.45, 0.55),
        width_n: 0.2,
        height_n: 0.15,
        rotation: 0.375,
    }
}

/// ハブが確定したrevisionを内部描画イベントへ付与した、overlay向け増分イベント。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayEvent<'a> {
    pub rev: u64,
    #[serde(flatten)]
    pub event: &'a PainterMessage,
}

macro_rules! define_overlay_control_messages {
    (
        $(
            $(#[$variant_meta:meta])*
            $variant:ident {
                $(
                    $(#[$field_meta:meta])*
                    $field:ident: $field_type:ty
                ),* $(,)?
            } => $fixture:expr
        ),+ $(,)?
    ) => {
        /// ローカルハブ → OBS overlay の接続管理メッセージ。
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        pub enum OverlayControlMessage {
            $(
                $(#[$variant_meta])*
                $variant {
                    $(
                        $(#[$field_meta])*
                        $field: $field_type,
                    )*
                },
            )+
        }

        #[cfg(test)]
        pub(crate) fn canonical_overlay_control_messages() -> Vec<OverlayControlMessage> {
            vec![$($fixture),+]
        }
    };
}

define_overlay_control_messages! {
    #[serde(rename_all = "camelCase")]
    Snapshot {
        protocol_version: u32,
        rev: u64,
        fade_after_ms: Option<f64>,
        /// 描画順を保つ完全な描画履歴。
        items: Vec<CanvasItem>,
    } => OverlayControlMessage::Snapshot {
        protocol_version: PROTOCOL_VERSION,
        rev: 40,
        fade_after_ms: Some(2_500.0),
        items: fixture_canvas_items(),
    },
    Pong {
        t: f64,
    } => OverlayControlMessage::Pong {
        t: 1_700_000_001_000.0,
    },
}

#[cfg(test)]
fn fixture_canvas_items() -> Vec<CanvasItem> {
    vec![
        CanvasItem::Stroke {
            stroke: Stroke {
                stroke_id: "fixture-snapshot-stroke".into(),
                brush: fixture_brush(Tool::Pen),
                pts: vec![(0.1, 0.2, 0.5, 0.0, 0.25, -0.5)],
                done: true,
                ended_at: Some(1_700_000_000_100.0),
            },
        },
        CanvasItem::Shape {
            shape: fixture_shape("fixture-snapshot-shape", ShapeKind::Ellipse, true),
        },
        CanvasItem::Stamp {
            stamp: fixture_stamp("fixture-snapshot-stamp", true),
        },
    ]
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
            pressure_min: 0.2,
            tilt_width: false,
            tilt_max_scale: 1.0,
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
                    "pressureMin": 0.2,
                    "tiltWidth": false,
                    "tiltMaxScale": 1.0,
                },
            })
        );
    }

    #[test]
    fn stroke_points_serializes_as_arrays() {
        let msg = PainterMessage::StrokePoints {
            stroke_id: "s1".into(),
            offset: 0,
            pts: vec![
                (0.1, 0.2, 0.5, 0.0, 0.0, 0.0),
                (0.15, 0.25, 0.6, 16.0, 0.25, -0.5),
            ],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "stroke_points",
                "strokeId": "s1",
                "pts": [
                    [0.1, 0.2, 0.5, 0.0, 0.0, 0.0],
                    [0.15, 0.25, 0.6, 16.0, 0.25, -0.5]
                ],
            })
        );
        assert_eq!(serde_json::from_value::<PainterMessage>(json).unwrap(), msg);
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
            r##"{"type":"snapshot","protocolVersion":6,"rev":3,"fadeAfterMs":null,"items":[{"kind":"stroke","strokeId":"s1","brush":{"tool":"pen","color":"#ff4d6d","opacity":1,"widthN":0.005,"pressureWidth":true,"pressureMin":0.2,"tiltWidth":false,"tiltMaxScale":1},"pts":[[0.1,0.2,0.5,0,0,0]],"done":true,"endedAt":123}]}"##,
        )
        .unwrap();
        match msg {
            OverlayControlMessage::Snapshot {
                protocol_version,
                rev,
                items,
                ..
            } => {
                assert_eq!(protocol_version, MIN_COMPATIBLE_PROTOCOL_VERSION);
                assert_eq!(rev, 3);
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].item_id(), "s1");
            }
            OverlayControlMessage::Pong { .. } => panic!("unexpected pong"),
        }
    }

    #[test]
    fn v6_shape_stamp_and_shape_end_migrate_without_transform_fields() {
        let snapshot: OverlayControlMessage = serde_json::from_str(
            r##"{"type":"snapshot","protocolVersion":6,"rev":3,"fadeAfterMs":null,"items":[{"kind":"stroke","strokeId":"stroke-1","brush":{"tool":"marker","color":"#fff","opacity":0.5,"widthN":0.01,"pressureWidth":true,"pressureMin":0.65,"tiltWidth":true,"tiltMaxScale":1.75},"pts":[[0.1,0.2,0.5,0,0.25,-0.5]],"done":true,"endedAt":0},{"kind":"shape","itemId":"shape-1","shape":"rectangle","style":{"color":"#fff","opacity":1,"widthN":0.01},"start":[0.1,0.2],"end":[0.4,0.5],"done":true,"endedAt":1},{"kind":"stamp","itemId":"stamp-1","stampId":"asset","center":[0.5,0.5],"widthN":0.1,"heightN":0.2,"opacity":1,"done":true,"endedAt":2}]}"##,
        )
        .unwrap();
        let OverlayControlMessage::Snapshot {
            protocol_version,
            items,
            ..
        } = snapshot
        else {
            panic!("expected snapshot");
        };
        assert_eq!(protocol_version, MIN_COMPATIBLE_PROTOCOL_VERSION);
        assert!(matches!(
            &items[1],
            CanvasItem::Shape { shape } if shape.transform.is_none()
        ));
        assert!(matches!(
            &items[2],
            CanvasItem::Stamp { stamp } if stamp.rotation == 0.0
        ));

        let end: PainterMessage =
            serde_json::from_str(r##"{"type":"shape_end","itemId":"shape-1","endedAt":3}"##)
                .unwrap();
        assert!(matches!(
            end,
            PainterMessage::ShapeEnd {
                transform: None,
                ..
            }
        ));
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
                transform: None,
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
