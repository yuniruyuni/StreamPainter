//! 入力点 → CanvasItem / プロトコルメッセージへの変換と全状態の保持。
//! Win32 に依存しない純ロジック。状態はローカルハブと同じ規則でトリムする。
//!
//! 描画履歴はローカルエコー描画用に Arc<Mutex<..>> で保持する。
//! 書き込みは UI スレッドのみ (ロックは常に短時間)。

use std::sync::{Arc, Mutex};

use crate::protocol::{
    Brush, CanvasItem, LineStyle, PainterMessage, Point, ShapeItem, ShapeKind, StampItem, Stroke,
    MAX_ITEMS, MAX_POINTS_PER_MESSAGE, MAX_STROKE_POINTS, MAX_TOTAL_POINTS,
};

/// 間引き閾値: 距離 (正規化) と筆圧変化の両方が小さい点は捨てる
const MIN_DISTANCE: f64 = 0.0005;
const MIN_PRESSURE_DELTA: f64 = 0.05;

pub type SharedItems = Arc<Mutex<Vec<CanvasItem>>>;

struct ActiveStroke {
    stroke_id: String,
    started_at: f64, // epoch ms
    pending: Vec<Point>,
    last: Option<Point>,
}

struct ActiveShape {
    item_id: String,
    pending_end: Option<(f64, f64)>,
}

struct ActiveStampMove {
    item_id: String,
    origin: (f64, f64),
    pending_center: Option<(f64, f64)>,
    sent_any: bool,
}

enum ActiveItem {
    Stroke(ActiveStroke),
    Shape(ActiveShape),
    StampMove(ActiveStampMove),
}

/// 確定済み項目の追加とスタンプ移動を、ユーザー操作の順番で戻す。
/// Add は項目本体を items 側に保持しているため ID だけを持つ。
enum UndoAction {
    Add {
        item_id: String,
    },
    MoveStamp {
        item_id: String,
        from: (f64, f64),
        to: (f64, f64),
    },
}

/// Add の項目本体は Undo 時に items からこちらへ移す。
enum RedoAction {
    Add {
        item: CanvasItem,
    },
    MoveStamp {
        item_id: String,
        from: (f64, f64),
        to: (f64, f64),
    },
}

impl UndoAction {
    fn item_id(&self) -> &str {
        match self {
            Self::Add { item_id } | Self::MoveStamp { item_id, .. } => item_id,
        }
    }
}

impl RedoAction {
    fn item_id(&self) -> &str {
        match self {
            Self::Add { item } => item.item_id(),
            Self::MoveStamp { item_id, .. } => item_id,
        }
    }
}

pub struct CanvasEngine {
    items: SharedItems,
    active: Option<ActiveItem>,
    undo_actions: Vec<UndoAction>,
    redo_actions: Vec<RedoAction>,
    total_points: usize,
    rebuild_required: bool,
}

impl CanvasEngine {
    pub fn new() -> Self {
        Self {
            items: Arc::new(Mutex::new(Vec::new())),
            active: None,
            undo_actions: Vec::new(),
            redo_actions: Vec::new(),
            total_points: 0,
            rebuild_required: false,
        }
    }

    /// Win32 レンダラーと共有する、描画順を保った履歴のハンドル。
    pub fn shared_items(&self) -> SharedItems {
        Arc::clone(&self.items)
    }

    pub fn is_drawing(&self) -> bool {
        self.active.is_some()
    }

    /// 上限トリムで baked 履歴から項目が消えたかを一度だけ通知する。
    pub fn take_rebuild_required(&mut self) -> bool {
        std::mem::take(&mut self.rebuild_required)
    }

    /// フリーハンドのペンダウン。stroke_begin を返す。
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
        self.redo_actions.clear();
        let stroke_id = uuid::Uuid::now_v7().to_string();
        let first: Point = (round5(u), round5(v), round2(p), 0.0);
        let stroke = Stroke {
            stroke_id: stroke_id.clone(),
            brush: brush.clone(),
            pts: vec![first],
            done: false,
            ended_at: None,
        };

        self.items
            .lock()
            .unwrap()
            .push(CanvasItem::Stroke { stroke });
        self.total_points += 1;
        self.trim();

        self.active = Some(ActiveItem::Stroke(ActiveStroke {
            stroke_id: stroke_id.clone(),
            started_at: now_ms,
            pending: vec![first],
            last: Some(first),
        }));
        vec![PainterMessage::StrokeBegin { stroke_id, brush }]
    }

    /// 図形のドラッグ開始。
    pub fn begin_shape(
        &mut self,
        shape_kind: ShapeKind,
        style: LineStyle,
        u: f64,
        v: f64,
    ) -> Vec<PainterMessage> {
        if self.active.is_some() {
            return Vec::new();
        }
        self.redo_actions.clear();
        let item_id = uuid::Uuid::now_v7().to_string();
        let position = (round5(u), round5(v));
        let shape = ShapeItem {
            item_id: item_id.clone(),
            shape: shape_kind,
            style,
            start: position,
            end: position,
            done: false,
            ended_at: None,
        };
        self.items.lock().unwrap().push(CanvasItem::Shape {
            shape: shape.clone(),
        });
        self.trim();
        self.active = Some(ActiveItem::Shape(ActiveShape {
            item_id,
            // shape_begin に初期終点が含まれるため、最初の flush では再送しない。
            pending_end: None,
        }));
        vec![PainterMessage::ShapeBegin { shape }]
    }

    /// 登録済みスタンプを 1 個、確定済みアイテムとして追加する。
    pub fn add_stamp(
        &mut self,
        stamp_id: String,
        center: (f64, f64),
        width_n: f64,
        height_n: f64,
        opacity: f64,
        now_ms: f64,
    ) -> Vec<PainterMessage> {
        if self.active.is_some() {
            return Vec::new();
        }
        self.redo_actions.clear();
        let stamp = StampItem {
            item_id: uuid::Uuid::now_v7().to_string(),
            stamp_id,
            center: (round5(center.0), round5(center.1)),
            width_n,
            height_n,
            opacity,
            done: true,
            ended_at: Some(now_ms),
        };
        let item_id = stamp.item_id.clone();
        self.items.lock().unwrap().push(CanvasItem::Stamp {
            stamp: stamp.clone(),
        });
        self.undo_actions.push(UndoAction::Add { item_id });
        self.trim();
        vec![PainterMessage::StampAdd { stamp }]
    }

    /// 指定位置に重なる最前面の確定済みスタンプを返す。
    pub fn stamp_at(&self, u: f64, v: f64) -> Option<StampItem> {
        if !u.is_finite() || !v.is_finite() {
            return None;
        }
        self.items
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|item| match item {
                CanvasItem::Stamp { stamp }
                    if stamp.done
                        && (u - stamp.center.0).abs() <= stamp.width_n / 2.0
                        && (v - stamp.center.1).abs() <= stamp.height_n / 2.0 =>
                {
                    Some(stamp.clone())
                }
                _ => None,
            })
    }

    /// ID で確定済みスタンプを取得する。選択プレビューの確定後同期に使う。
    pub fn stamp_by_id(&self, item_id: &str) -> Option<StampItem> {
        self.items
            .lock()
            .unwrap()
            .iter()
            .find_map(|item| match item {
                CanvasItem::Stamp { stamp } if stamp.done && stamp.item_id == item_id => {
                    Some(stamp.clone())
                }
                _ => None,
            })
    }

    /// スタンプのドラッグ移動を開始する。Redo履歴は実際の移動確定まで保持する。
    pub fn begin_stamp_move(&mut self, item_id: &str) -> bool {
        if self.active.is_some() {
            return false;
        }
        let Some(origin) = self.stamp_by_id(item_id).map(|stamp| stamp.center) else {
            return false;
        };
        self.active = Some(ActiveItem::StampMove(ActiveStampMove {
            item_id: item_id.to_owned(),
            origin,
            pending_center: None,
            sent_any: false,
        }));
        true
    }

    /// ドラッグ中のローカル状態を更新し、次のflushで最新位置だけをOBSへ送る。
    pub fn preview_stamp_move(&mut self, item_id: &str, center: (f64, f64)) -> bool {
        let Some(center) = normalized_stamp_center(center) else {
            return false;
        };
        let Some(ActiveItem::StampMove(active)) = self.active.as_mut() else {
            return false;
        };
        if active.item_id != item_id {
            return false;
        }

        let mut items = self.items.lock().unwrap();
        let Some(stamp) = items.iter_mut().find_map(|item| match item {
            CanvasItem::Stamp { stamp } if stamp.done && stamp.item_id == item_id => Some(stamp),
            _ => None,
        }) else {
            return false;
        };
        if stamp.center == center {
            return false;
        }
        stamp.center = center;
        active.pending_center = Some(center);
        true
    }

    /// ポインタ移動。ストロークでは点を追加し、図形では終点を更新する。
    /// ストロークの点数上限に達した場合のみ、ここから強制確定メッセージを返す。
    pub fn move_to(&mut self, u: f64, v: f64, p: f64, now_ms: f64) -> Vec<PainterMessage> {
        let Some(active) = self.active.as_mut() else {
            return Vec::new();
        };

        match active {
            ActiveItem::StampMove(_) => Vec::new(),
            ActiveItem::Shape(active) => {
                let end = (round5(u), round5(v));
                active.pending_end = Some(end);
                let mut items = self.items.lock().unwrap();
                if let Some(CanvasItem::Shape { shape }) = items
                    .iter_mut()
                    .find(|item| item.item_id() == active.item_id)
                {
                    shape.end = end;
                }
                Vec::new()
            }
            ActiveItem::Stroke(active) => {
                let dt = (now_ms - active.started_at).max(0.0);
                let pt: Point = (round5(u), round5(v), round2(p), dt);

                if let Some(last) = active.last {
                    let dist = ((pt.0 - last.0).powi(2) + (pt.1 - last.1).powi(2)).sqrt();
                    if dist < MIN_DISTANCE && (pt.2 - last.2).abs() < MIN_PRESSURE_DELTA {
                        return Vec::new();
                    }
                }
                active.last = Some(pt);
                active.pending.push(pt);

                let count = {
                    let mut items = self.items.lock().unwrap();
                    let stroke = items
                        .iter_mut()
                        .find_map(|item| match item {
                            CanvasItem::Stroke { stroke }
                                if stroke.stroke_id == active.stroke_id =>
                            {
                                Some(stroke)
                            }
                            _ => None,
                        })
                        .expect("active stroke must exist");
                    stroke.pts.push(pt);
                    stroke.pts.len()
                };

                self.total_points += 1;
                self.trim();
                if count >= MAX_STROKE_POINTS {
                    return self.end(now_ms);
                }
                Vec::new()
            }
        }
    }

    /// UIタイマから呼ぶバッチ送信。図形とスタンプ移動は最新座標だけを送る。
    pub fn flush(&mut self) -> Vec<PainterMessage> {
        let Some(active) = self.active.as_mut() else {
            return Vec::new();
        };
        match active {
            ActiveItem::Shape(active) => active
                .pending_end
                .take()
                .map(|end| PainterMessage::ShapeUpdate {
                    item_id: active.item_id.clone(),
                    end,
                })
                .into_iter()
                .collect(),
            ActiveItem::Stroke(active) => {
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
            ActiveItem::StampMove(active) => active
                .pending_center
                .take()
                .map(|center| {
                    active.sent_any = true;
                    PainterMessage::StampMovePreview {
                        item_id: active.item_id.clone(),
                        center,
                    }
                })
                .into_iter()
                .collect(),
        }
    }

    /// ポインタアップ。残バッファを flush し、アクティブ項目を確定する。
    pub fn end(&mut self, now_ms: f64) -> Vec<PainterMessage> {
        // スタンプは中間previewをflushせず、確定イベントだけを即時送る。
        // それ以外の描画は従来どおり残バッファを確定前に送る。
        let stamp_move = matches!(self.active.as_ref(), Some(ActiveItem::StampMove(_)));
        let mut messages = if stamp_move { Vec::new() } else { self.flush() };
        let Some(active) = self.active.take() else {
            return messages;
        };
        let mut items = self.items.lock().unwrap();
        let history_action = match active {
            ActiveItem::Stroke(active) => {
                let committed = if let Some(CanvasItem::Stroke { stroke }) = items
                    .iter_mut()
                    .find(|item| item.item_id() == active.stroke_id)
                {
                    stroke.done = true;
                    stroke.ended_at = Some(now_ms);
                    true
                } else {
                    false
                };
                let item_id = active.stroke_id;
                messages.push(PainterMessage::StrokeEnd {
                    stroke_id: item_id.clone(),
                    ended_at: now_ms,
                });
                committed.then_some(UndoAction::Add { item_id })
            }
            ActiveItem::Shape(active) => {
                let committed = if let Some(CanvasItem::Shape { shape }) = items
                    .iter_mut()
                    .find(|item| item.item_id() == active.item_id)
                {
                    shape.done = true;
                    shape.ended_at = Some(now_ms);
                    true
                } else {
                    false
                };
                let item_id = active.item_id;
                messages.push(PainterMessage::ShapeEnd {
                    item_id: item_id.clone(),
                    ended_at: now_ms,
                });
                committed.then_some(UndoAction::Add { item_id })
            }
            ActiveItem::StampMove(active) => {
                let center = items.iter().find_map(|item| match item {
                    CanvasItem::Stamp { stamp }
                        if stamp.done && stamp.item_id == active.item_id =>
                    {
                        Some(stamp.center)
                    }
                    _ => None,
                });
                let Some(center) = center else {
                    return messages;
                };
                if center != active.origin || active.sent_any {
                    messages.push(PainterMessage::StampMove {
                        item_id: active.item_id.clone(),
                        center,
                    });
                }
                (center != active.origin).then_some(UndoAction::MoveStamp {
                    item_id: active.item_id,
                    from: active.origin,
                    to: center,
                })
            }
        };
        drop(items);
        if let Some(action) = history_action {
            self.redo_actions.clear();
            self.undo_actions.push(action);
        }
        messages
    }

    /// 描画中項目の破棄 (モード切替時など)。スタンプ移動は元位置へ戻す。
    pub fn cancel(&mut self) -> Vec<PainterMessage> {
        let Some(active) = self.active.take() else {
            return Vec::new();
        };
        let (item_id, message) = match active {
            ActiveItem::Stroke(active) => {
                let id = active.stroke_id;
                (id.clone(), PainterMessage::StrokeCancel { stroke_id: id })
            }
            ActiveItem::Shape(active) => {
                let id = active.item_id;
                (id.clone(), PainterMessage::ShapeCancel { item_id: id })
            }
            ActiveItem::StampMove(active) => {
                let mut items = self.items.lock().unwrap();
                let Some(stamp) = items.iter_mut().find_map(|item| match item {
                    CanvasItem::Stamp { stamp }
                        if stamp.done && stamp.item_id == active.item_id =>
                    {
                        Some(stamp)
                    }
                    _ => None,
                }) else {
                    return Vec::new();
                };
                let moved = stamp.center != active.origin;
                stamp.center = active.origin;
                if moved || active.sent_any {
                    return vec![PainterMessage::StampMove {
                        item_id: active.item_id,
                        center: active.origin,
                    }];
                }
                return Vec::new();
            }
        };
        let mut items = self.items.lock().unwrap();
        if let Some(index) = items.iter().position(|item| item.item_id() == item_id) {
            self.total_points = self.total_points.saturating_sub(items[index].point_count());
            items.remove(index);
        }
        vec![message]
    }

    /// 最後の確定操作（項目追加またはスタンプ移動）を戻す。
    pub fn undo(&mut self) -> Vec<PainterMessage> {
        while let Some(action) = self.undo_actions.pop() {
            match action {
                UndoAction::Add { item_id } => {
                    let removed = {
                        let mut items = self.items.lock().unwrap();
                        let Some(index) = items
                            .iter()
                            .position(|item| item.is_done() && item.item_id() == item_id)
                        else {
                            continue;
                        };
                        items.remove(index)
                    };
                    self.total_points = self.total_points.saturating_sub(removed.point_count());
                    self.redo_actions.push(RedoAction::Add { item: removed });
                    return vec![PainterMessage::Undo {}];
                }
                UndoAction::MoveStamp { item_id, from, to } => {
                    {
                        let mut items = self.items.lock().unwrap();
                        let Some(stamp) = items.iter_mut().find_map(|item| match item {
                            CanvasItem::Stamp { stamp }
                                if stamp.done && stamp.item_id == item_id =>
                            {
                                Some(stamp)
                            }
                            _ => None,
                        }) else {
                            continue;
                        };
                        stamp.center = from;
                    }
                    self.redo_actions.push(RedoAction::MoveStamp {
                        item_id: item_id.clone(),
                        from,
                        to,
                    });
                    return vec![PainterMessage::StampMove {
                        item_id,
                        center: from,
                    }];
                }
            }
        }
        Vec::new()
    }

    /// 最後にUndoした操作をやり直す。
    pub fn redo(&mut self) -> Vec<PainterMessage> {
        if self.active.is_some() {
            return Vec::new();
        }
        while let Some(action) = self.redo_actions.pop() {
            match action {
                RedoAction::Add { item } => {
                    if self
                        .items
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|existing| existing.item_id() == item.item_id())
                    {
                        continue;
                    }
                    let item_id = item.item_id().to_owned();
                    self.total_points += item.point_count();
                    self.items.lock().unwrap().push(item.clone());
                    self.undo_actions.push(UndoAction::Add { item_id });
                    self.trim();
                    return vec![PainterMessage::Redo { item }];
                }
                RedoAction::MoveStamp { item_id, from, to } => {
                    {
                        let mut items = self.items.lock().unwrap();
                        let Some(stamp) = items.iter_mut().find_map(|item| match item {
                            CanvasItem::Stamp { stamp }
                                if stamp.done && stamp.item_id == item_id =>
                            {
                                Some(stamp)
                            }
                            _ => None,
                        }) else {
                            continue;
                        };
                        stamp.center = to;
                    }
                    self.undo_actions.push(UndoAction::MoveStamp {
                        item_id: item_id.clone(),
                        from,
                        to,
                    });
                    return vec![PainterMessage::StampMove {
                        item_id,
                        center: to,
                    }];
                }
            }
        }
        Vec::new()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_actions.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_actions.is_empty() && self.active.is_none()
    }

    pub fn clear(&mut self) -> Vec<PainterMessage> {
        let mut items = self.items.lock().unwrap();
        self.undo_actions.clear();
        self.redo_actions.clear();
        if items.is_empty() {
            return Vec::new();
        }
        items.clear();
        self.total_points = 0;
        drop(items);
        self.active = None;
        vec![PainterMessage::Clear {}]
    }

    /// ローカルハブと同じトリム規則。古い確定項目から捨てる。
    fn trim(&mut self) {
        let mut items = self.items.lock().unwrap();
        let mut removed_any = false;
        let mut removed_ids = Vec::new();
        while items.len() > MAX_ITEMS {
            let Some(index) = items.iter().position(CanvasItem::is_done) else {
                break;
            };
            let removed = items.remove(index);
            self.total_points = self.total_points.saturating_sub(removed.point_count());
            removed_ids.push(removed.item_id().to_owned());
            removed_any = true;
        }
        while self.total_points > MAX_TOTAL_POINTS {
            let Some(index) = items.iter().position(CanvasItem::is_done) else {
                break;
            };
            let removed = items.remove(index);
            self.total_points = self.total_points.saturating_sub(removed.point_count());
            removed_ids.push(removed.item_id().to_owned());
            removed_any = true;
        }
        drop(items);
        if !removed_ids.is_empty() {
            self.undo_actions
                .retain(|action| !removed_ids.iter().any(|id| id == action.item_id()));
            self.redo_actions
                .retain(|action| !removed_ids.iter().any(|id| id == action.item_id()));
        }
        self.rebuild_required |= removed_any;
    }
}

fn normalized_stamp_center(center: (f64, f64)) -> Option<(f64, f64)> {
    (center.0.is_finite() && center.1.is_finite()).then(|| {
        (
            round5(center.0.clamp(0.0, 1.0)),
            round5(center.1.clamp(0.0, 1.0)),
        )
    })
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

    fn line_style() -> LineStyle {
        LineStyle {
            color: "#ffffff".into(),
            opacity: 1.0,
            width_n: 0.005,
        }
    }

    fn drain_types(msgs: &[PainterMessage]) -> Vec<&'static str> {
        msgs.iter()
            .map(|m| match m {
                PainterMessage::StrokeBegin { .. } => "begin",
                PainterMessage::StrokePoints { .. } => "points",
                PainterMessage::StrokeEnd { .. } => "end",
                PainterMessage::StrokeCancel { .. } => "cancel",
                PainterMessage::ShapeBegin { .. } => "shape_begin",
                PainterMessage::ShapeUpdate { .. } => "shape_update",
                PainterMessage::ShapeEnd { .. } => "shape_end",
                PainterMessage::ShapeCancel { .. } => "shape_cancel",
                PainterMessage::StampAdd { .. } => "stamp_add",
                PainterMessage::StampMovePreview { .. } => "stamp_move_preview",
                PainterMessage::StampMove { .. } => "stamp_move",
                PainterMessage::Undo {} => "undo",
                PainterMessage::Redo { .. } => "redo",
                PainterMessage::Clear {} => "clear",
            })
            .collect()
    }

    fn first_stroke(items: &[CanvasItem]) -> &Stroke {
        match &items[0] {
            CanvasItem::Stroke { stroke } => stroke,
            _ => panic!("expected stroke"),
        }
    }

    #[test]
    fn begin_move_flush_end_lifecycle() {
        let mut engine = CanvasEngine::new();
        let msgs = engine.begin(brush(), 0.1, 0.1, 0.5, 1000.0);
        assert_eq!(drain_types(&msgs), ["begin"]);

        engine.move_to(0.2, 0.2, 0.5, 1016.0);
        let flushed = engine.flush();
        assert_eq!(drain_types(&flushed), ["points"]);
        if let PainterMessage::StrokePoints { pts, .. } = &flushed[0] {
            assert_eq!(pts.len(), 2);
            assert_eq!(pts[1].3, 16.0);
        }

        let ended = engine.end(1100.0);
        assert_eq!(drain_types(&ended), ["end"]);
        let items = engine.shared_items();
        let items = items.lock().unwrap();
        let stroke = first_stroke(&items);
        assert!(stroke.done);
        assert_eq!(stroke.ended_at, Some(1100.0));
    }

    #[test]
    fn shape_updates_are_coalesced_and_committed() {
        let mut engine = CanvasEngine::new();
        assert_eq!(
            drain_types(&engine.begin_shape(ShapeKind::Arrow, line_style(), 0.1, 0.2)),
            ["shape_begin"]
        );
        engine.move_to(0.4, 0.5, 0.5, 10.0);
        engine.move_to(0.8, 0.7, 0.5, 20.0);
        let flushed = engine.flush();
        assert_eq!(drain_types(&flushed), ["shape_update"]);
        assert!(engine.flush().is_empty());
        assert_eq!(drain_types(&engine.end(30.0)), ["shape_end"]);
        let items = engine.shared_items();
        let items = items.lock().unwrap();
        match &items[0] {
            CanvasItem::Shape { shape } => {
                assert_eq!(shape.end, (0.8, 0.7));
                assert!(shape.done);
            }
            _ => panic!("expected shape"),
        }
    }

    #[test]
    fn stamp_and_shape_participate_in_undo_order() {
        let mut engine = CanvasEngine::new();
        engine.begin_shape(ShapeKind::Rectangle, line_style(), 0.1, 0.1);
        engine.end(10.0);
        assert_eq!(
            drain_types(&engine.add_stamp("stamp-1".into(), (0.5, 0.5), 0.1, 0.2, 1.0, 20.0)),
            ["stamp_add"]
        );
        assert_eq!(engine.shared_items().lock().unwrap().len(), 2);
        assert_eq!(drain_types(&engine.undo()), ["undo"]);
        let items = engine.shared_items();
        let items = items.lock().unwrap();
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0], CanvasItem::Shape { .. }));
    }

    #[test]
    fn stamp_hit_test_prefers_the_topmost_item() {
        let mut engine = CanvasEngine::new();
        engine.add_stamp("lower".into(), (0.5, 0.5), 0.4, 0.4, 1.0, 10.0);
        engine.add_stamp("upper".into(), (0.5, 0.5), 0.2, 0.2, 1.0, 20.0);

        assert_eq!(engine.stamp_at(0.5, 0.5).unwrap().stamp_id, "upper");
        assert_eq!(engine.stamp_at(0.68, 0.5).unwrap().stamp_id, "lower");
        assert!(engine.stamp_at(0.9, 0.9).is_none());
    }

    #[test]
    fn live_stamp_move_coalesces_previews_and_commits_one_history_action() {
        let mut engine = CanvasEngine::new();
        engine.add_stamp("stamp-1".into(), (0.2, 0.3), 0.1, 0.1, 1.0, 10.0);
        let item_id = engine.stamp_at(0.2, 0.3).unwrap().item_id;

        assert!(engine.begin_stamp_move(&item_id));
        assert!(engine.preview_stamp_move(&item_id, (0.4, 0.5)));
        assert!(engine.preview_stamp_move(&item_id, (0.6, 0.7)));
        let preview = engine.flush();
        assert_eq!(drain_types(&preview), ["stamp_move_preview"]);
        assert!(matches!(
            &preview[0],
            PainterMessage::StampMovePreview {
                center: (0.6, 0.7),
                ..
            }
        ));
        assert!(engine.flush().is_empty());

        assert!(engine.preview_stamp_move(&item_id, (0.75, 0.6)));
        assert_eq!(drain_types(&engine.end(20.0)), ["stamp_move"]);
        assert_eq!(engine.stamp_by_id(&item_id).unwrap().center, (0.75, 0.6));
        assert_eq!(drain_types(&engine.undo()), ["stamp_move"]);
        assert_eq!(engine.stamp_by_id(&item_id).unwrap().center, (0.2, 0.3));
        assert_eq!(drain_types(&engine.redo()), ["stamp_move"]);
        assert_eq!(engine.stamp_by_id(&item_id).unwrap().center, (0.75, 0.6));
    }

    #[test]
    fn canceled_live_stamp_move_restores_browser_position_and_redo_history() {
        let mut engine = CanvasEngine::new();
        engine.add_stamp("one".into(), (0.2, 0.3), 0.1, 0.1, 1.0, 10.0);
        let first_id = engine.stamp_at(0.2, 0.3).unwrap().item_id;
        engine.add_stamp("two".into(), (0.7, 0.6), 0.1, 0.1, 1.0, 20.0);
        engine.undo();
        assert!(engine.can_redo());

        assert!(engine.begin_stamp_move(&first_id));
        assert!(engine.preview_stamp_move(&first_id, (0.8, 0.8)));
        assert_eq!(drain_types(&engine.flush()), ["stamp_move_preview"]);
        let canceled = engine.cancel();
        assert_eq!(drain_types(&canceled), ["stamp_move"]);
        assert!(matches!(
            &canceled[0],
            PainterMessage::StampMove {
                center: (0.2, 0.3),
                ..
            }
        ));
        assert_eq!(engine.stamp_by_id(&first_id).unwrap().center, (0.2, 0.3));
        assert!(engine.can_redo());
    }

    #[test]
    fn selecting_without_moving_preserves_redo_history() {
        let mut engine = CanvasEngine::new();
        engine.add_stamp("one".into(), (0.2, 0.3), 0.1, 0.1, 1.0, 10.0);
        engine.add_stamp("two".into(), (0.7, 0.6), 0.1, 0.1, 1.0, 20.0);
        engine.undo();
        let first_id = engine.stamp_at(0.2, 0.3).unwrap().item_id;

        assert!(engine.begin_stamp_move(&first_id));
        assert!(engine.end(30.0).is_empty());
        assert!(engine.can_redo());
    }

    #[test]
    fn returning_to_origin_after_preview_finalizes_browser_without_history() {
        let mut engine = CanvasEngine::new();
        engine.add_stamp("one".into(), (0.2, 0.3), 0.1, 0.1, 1.0, 10.0);
        let item_id = engine.stamp_at(0.2, 0.3).unwrap().item_id;

        assert!(engine.begin_stamp_move(&item_id));
        assert!(engine.preview_stamp_move(&item_id, (0.7, 0.6)));
        assert_eq!(drain_types(&engine.flush()), ["stamp_move_preview"]);
        assert!(engine.preview_stamp_move(&item_id, (0.2, 0.3)));

        let committed = engine.end(30.0);
        assert_eq!(drain_types(&committed), ["stamp_move"]);
        assert!(matches!(
            &committed[0],
            PainterMessage::StampMove {
                center: (0.2, 0.3),
                ..
            }
        ));
        assert_eq!(drain_types(&engine.undo()), ["undo"]);
        assert!(engine.stamp_by_id(&item_id).is_none());
    }

    #[test]
    fn thinning_drops_close_points() {
        let mut engine = CanvasEngine::new();
        engine.begin(brush(), 0.1, 0.1, 0.5, 0.0);
        engine.move_to(0.10001, 0.1, 0.5, 8.0);
        engine.move_to(0.2, 0.1, 0.5, 16.0);
        let flushed = engine.flush();
        if let PainterMessage::StrokePoints { pts, .. } = &flushed[0] {
            assert_eq!(pts.len(), 2);
        } else {
            panic!("expected points");
        }
    }

    #[test]
    fn flush_chunks_large_batches() {
        let mut engine = CanvasEngine::new();
        engine.begin(brush(), 0.0, 0.0, 0.5, 0.0);
        for i in 1..=600 {
            engine.move_to(i as f64 * 0.001, 0.0, 0.5, i as f64);
        }
        assert_eq!(engine.flush().len(), 2);
    }

    #[test]
    fn force_end_at_point_cap() {
        let mut engine = CanvasEngine::new();
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
        let mut engine = CanvasEngine::new();
        engine.begin(brush(), 0.1, 0.1, 0.5, 0.0);
        engine.end(10.0);
        engine.begin(brush(), 0.2, 0.2, 0.5, 20.0);

        assert_eq!(drain_types(&engine.undo()), ["undo"]);
        assert_eq!(engine.shared_items().lock().unwrap().len(), 1);
        assert!(engine.undo().is_empty());
    }

    #[test]
    fn redo_restores_undone_items_in_order() {
        let mut engine = CanvasEngine::new();
        engine.add_stamp("one".into(), (0.1, 0.1), 0.1, 0.1, 1.0, 10.0);
        engine.add_stamp("two".into(), (0.2, 0.2), 0.1, 0.1, 1.0, 20.0);

        assert!(engine.can_undo());
        assert_eq!(drain_types(&engine.undo()), ["undo"]);
        assert_eq!(drain_types(&engine.undo()), ["undo"]);
        assert!(engine.can_redo());
        assert_eq!(drain_types(&engine.redo()), ["redo"]);
        assert_eq!(drain_types(&engine.redo()), ["redo"]);
        assert!(!engine.can_redo());

        let items = engine.shared_items();
        let items = items.lock().unwrap();
        let stamp_ids: Vec<_> = items
            .iter()
            .filter_map(|item| match item {
                CanvasItem::Stamp { stamp } => Some(stamp.stamp_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(stamp_ids, ["one", "two"]);
    }

    #[test]
    fn new_item_discards_redo_history() {
        let mut engine = CanvasEngine::new();
        engine.add_stamp("one".into(), (0.1, 0.1), 0.1, 0.1, 1.0, 10.0);
        engine.undo();
        assert!(engine.can_redo());

        engine.add_stamp("replacement".into(), (0.2, 0.2), 0.1, 0.1, 1.0, 20.0);
        assert!(!engine.can_redo());
        assert!(engine.redo().is_empty());
    }

    #[test]
    fn cancel_discards_active() {
        let mut engine = CanvasEngine::new();
        engine.begin(brush(), 0.1, 0.1, 0.5, 0.0);
        assert_eq!(drain_types(&engine.cancel()), ["cancel"]);
        assert!(engine.shared_items().lock().unwrap().is_empty());
    }

    #[test]
    fn trim_keeps_item_cap() {
        let mut engine = CanvasEngine::new();
        for i in 0..MAX_ITEMS + 10 {
            engine.begin(brush(), 0.1, 0.1, 0.5, i as f64);
            engine.end(i as f64 + 1.0);
            engine.flush();
        }
        assert_eq!(engine.shared_items().lock().unwrap().len(), MAX_ITEMS);
        assert!(engine.take_rebuild_required());
        assert!(!engine.take_rebuild_required());
    }
}
