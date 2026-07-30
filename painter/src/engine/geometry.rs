//! ストローク幾何 (docs/protocol.md)。
//! client/src/Overlay/renderer/geometry.ts と同一の数式であること —
//! 両実装の見た目一致は本モジュールのテストと client 側テストの対で担保する。

use crate::protocol::{Brush, Point};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

/// 中点法二次ベジェの 1 セグメント (from → ctrl → to の quadratic curve)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub from: Vec2,
    pub ctrl: Vec2,
    pub to: Vec2,
    pub width: f64,
}

/// 筆圧 → 線幅
pub fn stroke_width(brush: &Brush, pressure: f64, canvas_height: f64) -> f64 {
    let base = brush.width_n * canvas_height;
    if brush.pressure_width {
        base * (0.35 + 0.65 * pressure)
    } else {
        base
    }
}

fn pos(pt: &Point, w: f64, h: f64) -> Vec2 {
    Vec2 {
        x: pt.0 * w,
        y: pt.1 * h,
    }
}

fn mid(a: Vec2, b: Vec2) -> Vec2 {
    Vec2 {
        x: (a.x + b.x) / 2.0,
        y: (a.y + b.y) / 2.0,
    }
}

/// n 点のうち形が確定しているセグメント数 (j = 1..=n-2)
#[allow(dead_code)] // M3 の増分描画最適化で使用予定 (docs/roadmap.md)
pub fn stable_segment_count(point_count: usize) -> usize {
    point_count.saturating_sub(2)
}

/// from_segment (1-origin) 以降の確定済みセグメント
pub fn stable_segments(
    pts: &[Point],
    canvas_w: f64,
    canvas_h: f64,
    brush: &Brush,
    from_segment: usize,
) -> Vec<Segment> {
    let mut segments = Vec::new();
    if pts.len() < 3 {
        return segments;
    }
    for j in from_segment.max(1)..=pts.len() - 2 {
        let prev = pos(&pts[j - 1], canvas_w, canvas_h);
        let curr = pos(&pts[j], canvas_w, canvas_h);
        let next = pos(&pts[j + 1], canvas_w, canvas_h);
        segments.push(Segment {
            from: if j == 1 { prev } else { mid(prev, curr) },
            ctrl: curr,
            to: mid(curr, next),
            width: stroke_width(brush, pts[j].2, canvas_h),
        });
    }
    segments
}

/// 末尾セグメント (ストローク確定時のみ描画)
pub fn tail_segment(pts: &[Point], canvas_w: f64, canvas_h: f64, brush: &Brush) -> Option<Segment> {
    let n = pts.len();
    if n < 2 {
        return None;
    }
    let last = pos(&pts[n - 1], canvas_w, canvas_h);
    let prev = pos(&pts[n - 2], canvas_w, canvas_h);
    Some(Segment {
        from: if n == 2 { prev } else { mid(prev, last) },
        ctrl: last,
        to: last,
        width: stroke_width(brush, pts[n - 1].2, canvas_h),
    })
}

/// 1 点ストローク: round cap の点 (中心, 半径)
pub fn dot(pts: &[Point], canvas_w: f64, canvas_h: f64, brush: &Brush) -> Option<(Vec2, f64)> {
    if pts.len() != 1 {
        return None;
    }
    Some((
        pos(&pts[0], canvas_w, canvas_h),
        stroke_width(brush, pts[0].2, canvas_h) / 2.0,
    ))
}

/// ストローク全体のセグメント列 (確定描画用)
pub fn full_segments(pts: &[Point], canvas_w: f64, canvas_h: f64, brush: &Brush) -> Vec<Segment> {
    let mut segments = stable_segments(pts, canvas_w, canvas_h, brush, 1);
    if let Some(tail) = tail_segment(pts, canvas_w, canvas_h, brush) {
        segments.push(tail);
    }
    segments
}

#[cfg(test)]
mod tests {
    //! client/src/Overlay/renderer/geometry.test.ts と同じ期待値のテスト。
    //! 双方が docs/protocol.md の同じ数式を実装していることを保証する。
    use super::*;
    use crate::protocol::Tool;

    fn brush() -> Brush {
        Brush {
            tool: Tool::Pen,
            color: "#ff4d6d".into(),
            opacity: 1.0,
            width_n: 0.01,
            pressure_width: true,
        }
    }

    fn pts() -> Vec<Point> {
        vec![
            (0.0, 0.0, 0.5, 0.0),
            (0.1, 0.0, 0.5, 16.0),
            (0.2, 0.0, 0.5, 32.0),
            (0.3, 0.0, 0.5, 48.0),
        ]
    }

    #[test]
    fn width_reflects_pressure() {
        assert!((stroke_width(&brush(), 1.0, 1000.0) - 10.0).abs() < 1e-9);
        assert!((stroke_width(&brush(), 0.0, 1000.0) - 3.5).abs() < 1e-9);
        assert!((stroke_width(&brush(), 0.5, 1000.0) - 6.75).abs() < 1e-9);
    }

    #[test]
    fn stable_segment_geometry_matches_client() {
        let segs = stable_segments(&pts(), 1000.0, 1000.0, &brush(), 1);
        assert_eq!(segs.len(), 2);
        // 最初のセグメントは P0 から
        assert_eq!(segs[0].from, Vec2 { x: 0.0, y: 0.0 });
        assert_eq!(segs[0].ctrl, Vec2 { x: 100.0, y: 0.0 });
        assert_eq!(segs[0].to, Vec2 { x: 150.0, y: 0.0 });
        // 以降は中点から連続
        assert_eq!(segs[1].from, Vec2 { x: 150.0, y: 0.0 });
        assert_eq!(segs[1].to, Vec2 { x: 250.0, y: 0.0 });
    }

    #[test]
    fn incremental_equals_full() {
        let all = stable_segments(&pts(), 1000.0, 1000.0, &brush(), 1);
        let first = stable_segments(&pts()[..3], 1000.0, 1000.0, &brush(), 1);
        let rest = stable_segments(&pts(), 1000.0, 1000.0, &brush(), first.len() + 1);
        let joined: Vec<_> = first.into_iter().chain(rest).collect();
        assert_eq!(joined, all);
    }

    #[test]
    fn tail_and_dot() {
        let tail = tail_segment(&pts(), 1000.0, 1000.0, &brush()).unwrap();
        assert_eq!(tail.from, Vec2 { x: 250.0, y: 0.0 });
        assert_eq!(tail.to, Vec2 { x: 300.0, y: 0.0 });

        let two = tail_segment(&pts()[..2], 1000.0, 1000.0, &brush()).unwrap();
        assert_eq!(two.from, Vec2 { x: 0.0, y: 0.0 });
        assert_eq!(two.to, Vec2 { x: 100.0, y: 0.0 });

        assert!(tail_segment(&pts()[..1], 1000.0, 1000.0, &brush()).is_none());
        let (center, radius) = dot(&pts()[..1], 1000.0, 1000.0, &brush()).unwrap();
        assert_eq!(center, Vec2 { x: 0.0, y: 0.0 });
        assert!((radius - 6.75 / 2.0).abs() < 1e-9);
    }

    #[test]
    fn full_is_stable_plus_tail() {
        assert_eq!(full_segments(&pts(), 1000.0, 1000.0, &brush()).len(), 3);
    }
}
