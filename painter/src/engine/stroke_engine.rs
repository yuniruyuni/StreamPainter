//! 入力点 → ストローク/プロトコルメッセージへの変換と全状態の保持 (docs/painter.md)。
//! Win32 に依存しない純ロジック。状態はローカルハブと同じ規則でトリムする。
//!
//! ストローク一覧はローカルエコー描画用に Arc<Mutex<..>> で保持する。
//! 書き込みは UI スレッドのみ (ロックは常に短時間)。

use std::sync::{Arc, Mutex};

use crate::protocol::{
    Brush, PainterMessage, Point, Stroke, MAX_POINTS_PER_MESSAGE, MAX_STROKES, MAX_STROKE_POINTS,
    MAX_TOTAL_POINTS,
};

/// 間引き閾値: 距離 (正規化) と筆圧変化の両方が小さい点は捨てる
const MIN_DISTANCE: f64 = 0.0005;
const MIN_PRESSURE_DELTA: f64 = 0.05;

pub type SharedStrokes = Arc<Mutex<Vec<Stroke>>>;

struct ActiveStroke {
    stroke_id: String,
    started_at: f64, // epoch ms
    pending: Vec<Point>,
    last: Option<Point>,
}

pub struct StrokeEngine {
    strokes: SharedStrokes,
    active: Option<ActiveStroke>,
    total_points: usize,
}

impl StrokeEngine {
    pub fn new() -> Self {
        Self {
            strokes: Arc::new(Mutex::new(Vec::new())),
            active: None,
            total_points: 0,
        }
    }

    /// Win32 レンダラーと共有するストローク一覧のハンドル
    pub fn shared_strokes(&self) -> SharedStrokes {
        Arc::clone(&self.strokes)
    }

    pub fn is_drawing(&self) -> bool {
        self.active.is_some()
    }

    /// ペンダウン。stroke_begin を返す
    pub fn begin(
        &mut self,
        brush: Brush,
        u: f64,
        v: f64,
        p: f64,
        now_ms: f64,
    ) -> Vec<PainterMessage> {
        if self.active.is_some() {
            return Vec::new();
        }
        let stroke_id = uuid::Uuid::now_v7().to_string();
        let first: Point = (round5(u), round5(v), round2(p), 0.0);

        self.strokes.lock().unwrap().push(Stroke {
            stroke_id: stroke_id.clone(),
            brush: brush.clone(),
            pts: vec![first],
            done: false,
            ended_at: None,
        });
        self.total_points += 1;
        self.trim();

        self.active = Some(ActiveStroke {
            stroke_id: stroke_id.clone(),
            started_at: now_ms,
            pending: vec![first],
            last: Some(first),
        });
        vec![PainterMessage::StrokeBegin { stroke_id, brush }]
    }

    /// ポインタ移動。総点数上限に達したら強制確定のメッセージ列を返す
    pub fn move_to(&mut self, u: f64, v: f64, p: f64, now_ms: f64) -> Vec<PainterMessage> {
        let Some(active) = self.active.as_mut() else {
            return Vec::new();
        };
        let dt = (now_ms - active.started_at).max(0.0);
        let pt: Point = (round5(u), round5(v), round2(p), dt);

        // 間引き
        if let Some(last) = active.last {
            let dist = ((pt.0 - last.0).powi(2) + (pt.1 - last.1).powi(2)).sqrt();
            if dist < MIN_DISTANCE && (pt.2 - last.2).abs() < MIN_PRESSURE_DELTA {
                return Vec::new();
            }
        }
        active.last = Some(pt);
        active.pending.push(pt);

        let count = {
            let mut strokes = self.strokes.lock().unwrap();
            let stroke = strokes
                .iter_mut()
                .find(|s| s.stroke_id == active.stroke_id)
                .expect("active stroke must exist");
            stroke.pts.push(pt);
            stroke.pts.len()
        };

        self.total_points += 1;
        self.trim();
        // ローカルハブと同じ条件で強制確定する
        if count >= MAX_STROKE_POINTS {
            return self.end(now_ms);
        }
        Vec::new()
    }

    /// バッチ送信 (16ms タイマから呼ぶ)。溜まった点を stroke_points にして返す
    pub fn flush(&mut self) -> Vec<PainterMessage> {
        let Some(active) = self.active.as_mut() else {
            return Vec::new();
        };
        if active.pending.is_empty() {
            return Vec::new();
        }
        let pts = std::mem::take(&mut active.pending);
        pts.chunks(MAX_POINTS_PER_MESSAGE)
            .map(|chunk| PainterMessage::StrokePoints {
                stroke_id: active.stroke_id.clone(),
                pts: chunk.to_vec(),
            })
            .collect()
    }

    /// ペンアップ。残バッファの flush + stroke_end を返す
    pub fn end(&mut self, now_ms: f64) -> Vec<PainterMessage> {
        let mut messages = self.flush();
        let Some(active) = self.active.take() else {
            return messages;
        };
        {
            let mut strokes = self.strokes.lock().unwrap();
            if let Some(stroke) = strokes.iter_mut().find(|s| s.stroke_id == active.stroke_id) {
                stroke.done = true;
                stroke.ended_at = Some(now_ms);
            }
        }
        messages.push(PainterMessage::StrokeEnd {
            stroke_id: active.stroke_id,
            ended_at: now_ms,
        });
        messages
    }

    /// 描画中ストロークの破棄 (モード切替時など)
    pub fn cancel(&mut self) -> Vec<PainterMessage> {
        let Some(active) = self.active.take() else {
            return Vec::new();
        };
        let mut strokes = self.strokes.lock().unwrap();
        if let Some(index) = strokes
            .iter()
            .position(|stroke| stroke.stroke_id == active.stroke_id)
        {
            self.total_points = self.total_points.saturating_sub(strokes[index].pts.len());
            strokes.remove(index);
        }
        vec![PainterMessage::StrokeCancel {
            stroke_id: active.stroke_id,
        }]
    }

    /// 最後の確定ストロークを削除。削除できたときのみ undo を返す
    pub fn undo(&mut self) -> Vec<PainterMessage> {
        let mut strokes = self.strokes.lock().unwrap();
        let Some(index) = strokes.iter().rposition(|s| s.done) else {
            return Vec::new();
        };
        let removed = strokes.remove(index);
        self.total_points = self.total_points.saturating_sub(removed.pts.len());
        vec![PainterMessage::Undo {}]
    }

    pub fn clear(&mut self) -> Vec<PainterMessage> {
        let mut strokes = self.strokes.lock().unwrap();
        if strokes.is_empty() {
            return Vec::new();
        }
        strokes.clear();
        self.total_points = 0;
        drop(strokes);
        // 描画中ストロークも消えるため active を破棄する (メッセージ不要 — clear が全消しする)
        self.active = None;
        vec![PainterMessage::Clear {}]
    }

    /// ローカルハブと同じトリム規則。古い確定ストロークから捨てる
    fn trim(&mut self) {
        let mut strokes = self.strokes.lock().unwrap();
        while strokes.len() > MAX_STROKES {
            let Some(index) = strokes.iter().position(|s| s.done) else {
                break;
            };
            let removed = strokes.remove(index);
            self.total_points = self.total_points.saturating_sub(removed.pts.len());
        }
        while self.total_points > MAX_TOTAL_POINTS {
            let Some(index) = strokes.iter().position(|s| s.done) else {
                break;
            };
            let removed = strokes.remove(index);
            self.total_points = self.total_points.saturating_sub(removed.pts.len());
        }
    }
}

fn round5(v: f64) -> f64 {
    (v * 1e5).round() / 1e5
}

fn round2(v: f64) -> f64 {
    (v * 1e2).round() / 1e2
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Tool;

    fn brush() -> Brush {
        Brush {
            tool: Tool::Pen,
            color: "#ff4d6d".into(),
            opacity: 1.0,
            width_n: 0.005,
            pressure_width: true,
        }
    }

    fn drain_types(msgs: &[PainterMessage]) -> Vec<&'static str> {
        msgs.iter()
            .map(|m| match m {
                PainterMessage::StrokeBegin { .. } => "begin",
                PainterMessage::StrokePoints { .. } => "points",
                PainterMessage::StrokeEnd { .. } => "end",
                PainterMessage::StrokeCancel { .. } => "cancel",
                PainterMessage::Undo {} => "undo",
                PainterMessage::Clear {} => "clear",
            })
            .collect()
    }

    #[test]
    fn begin_move_flush_end_lifecycle() {
        let mut engine = StrokeEngine::new();
        let msgs = engine.begin(brush(), 0.1, 0.1, 0.5, 1000.0);
        assert_eq!(drain_types(&msgs), ["begin"]);

        engine.move_to(0.2, 0.2, 0.5, 1016.0);
        let flushed = engine.flush();
        assert_eq!(drain_types(&flushed), ["points"]);
        if let PainterMessage::StrokePoints { pts, .. } = &flushed[0] {
            // begin の初期点 + 移動点
            assert_eq!(pts.len(), 2);
            assert_eq!(pts[1].3, 16.0); // dt は begin からの相対 ms
        }

        let ended = engine.end(1100.0);
        assert_eq!(drain_types(&ended), ["end"]);
        let strokes = engine.shared_strokes();
        let strokes = strokes.lock().unwrap();
        assert_eq!(strokes.len(), 1);
        assert!(strokes[0].done);
        assert_eq!(strokes[0].ended_at, Some(1100.0));
    }

    #[test]
    fn thinning_drops_close_points() {
        let mut engine = StrokeEngine::new();
        engine.begin(brush(), 0.1, 0.1, 0.5, 0.0);
        engine.move_to(0.10001, 0.1, 0.5, 8.0); // 距離も筆圧差も閾値未満
        engine.move_to(0.2, 0.1, 0.5, 16.0);
        let flushed = engine.flush();
        if let PainterMessage::StrokePoints { pts, .. } = &flushed[0] {
            assert_eq!(pts.len(), 2); // 初期点 + 有効な移動点のみ
        } else {
            panic!("expected points");
        }
    }

    #[test]
    fn flush_chunks_large_batches() {
        let mut engine = StrokeEngine::new();
        engine.begin(brush(), 0.0, 0.0, 0.5, 0.0);
        for i in 1..=600 {
            engine.move_to(i as f64 * 0.001, 0.0, 0.5, i as f64);
        }
        let flushed = engine.flush();
        assert_eq!(flushed.len(), 2); // 512 + 残り
    }

    #[test]
    fn force_end_at_point_cap() {
        let mut engine = StrokeEngine::new();
        engine.begin(brush(), 0.0, 0.0, 0.5, 0.0);
        let mut ended = false;
        for i in 1..MAX_STROKE_POINTS + 10 {
            let msgs = engine.move_to(
                (i % 1000) as f64 * 0.001,
                (i / 1000) as f64 * 0.01,
                0.5,
                i as f64,
            );
            if drain_types(&msgs).contains(&"end") {
                ended = true;
                break;
            }
        }
        assert!(ended);
        assert!(!engine.is_drawing());
    }

    #[test]
    fn undo_removes_last_done_only() {
        let mut engine = StrokeEngine::new();
        engine.begin(brush(), 0.1, 0.1, 0.5, 0.0);
        engine.end(10.0);
        engine.begin(brush(), 0.2, 0.2, 0.5, 20.0); // 描画中

        assert_eq!(drain_types(&engine.undo()), ["undo"]);
        let strokes = engine.shared_strokes();
        assert_eq!(strokes.lock().unwrap().len(), 1); // 描画中は残る
        assert!(engine.undo().is_empty()); // 確定ストロークなし → no-op
    }

    #[test]
    fn cancel_discards_active() {
        let mut engine = StrokeEngine::new();
        engine.begin(brush(), 0.1, 0.1, 0.5, 0.0);
        assert_eq!(drain_types(&engine.cancel()), ["cancel"]);
        let strokes = engine.shared_strokes();
        assert!(strokes.lock().unwrap().is_empty());
    }

    #[test]
    fn trim_keeps_stroke_cap() {
        let mut engine = StrokeEngine::new();
        for i in 0..MAX_STROKES + 10 {
            engine.begin(brush(), 0.1, 0.1, 0.5, i as f64);
            engine.end(i as f64 + 1.0);
            engine.flush();
        }
        let strokes = engine.shared_strokes();
        assert_eq!(strokes.lock().unwrap().len(), MAX_STROKES);
    }
}
