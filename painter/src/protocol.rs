//! ローカル WebSocket プロトコルの serde 型 (docs/protocol.md)。
//! TypeScript 側の `client/src/protocol.ts` と JSON 表現を揃える。
//! フィールドは camelCase、type タグは snake_case、点は [u, v, p, dt] の配列。

use serde::{Deserialize, Serialize};

/// [u, v, p, dt] — 正規化座標・筆圧 0..1・stroke_begin からの相対 ms
pub type Point = (f64, f64, f64, f64);

/// painter / local hub / overlay が同じ値で適用する上限。
pub const MAX_STROKES: usize = 500;
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
    Undo {},
    Clear {},
}

/// ローカルハブ → OBS overlay の接続管理メッセージ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OverlayControlMessage {
    #[serde(rename_all = "camelCase")]
    Snapshot {
        rev: u64,
        fade_after_ms: Option<f64>,
        strokes: Vec<Stroke>,
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
            r##"{"type":"snapshot","rev":3,"fadeAfterMs":null,"strokes":[{"strokeId":"s1","brush":{"tool":"pen","color":"#ff4d6d","opacity":1,"widthN":0.005,"pressureWidth":true},"pts":[[0.1,0.2,0.5,0]],"done":true,"endedAt":123}]}"##,
        )
        .unwrap();
        match msg {
            OverlayControlMessage::Snapshot { rev, strokes, .. } => {
                assert_eq!(rev, 3);
                assert_eq!(strokes.len(), 1);
                assert_eq!(strokes[0].stroke_id, "s1");
            }
            OverlayControlMessage::Pong { .. } => panic!("unexpected pong"),
        }
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
