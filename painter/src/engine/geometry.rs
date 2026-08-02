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

const MIN_PRESSURE_SCALE: f64 = 0.05;
const MAX_TILT_SCALE: f64 = 4.0;

/// 筆圧・傾き → 線幅。
///
/// 不正な値は、筆圧は従来幅(1)、傾きは直立(0)へfallbackする。Windows入力と
/// Browser Sourceの双方が同じ式を使い、markerだけが最初のtilt対応brushとなる。
pub fn stroke_width(
    brush: &Brush,
    pressure: f64,
    tilt_x: f64,
    tilt_y: f64,
    canvas_height: f64,
) -> f64 {
    let base = brush.width_n * canvas_height;
    let pressure_scale = if brush.pressure_width {
        let pressure = finite_or(pressure, 1.0).clamp(0.0, 1.0);
        let minimum = finite_or(brush.pressure_min, 1.0).clamp(MIN_PRESSURE_SCALE, 1.0);
        minimum + (1.0 - minimum) * pressure
    } else {
        1.0
    };
    let tilt_scale = if brush.tilt_width {
        let x = finite_or(tilt_x, 0.0).clamp(-1.0, 1.0);
        let y = finite_or(tilt_y, 0.0).clamp(-1.0, 1.0);
        let magnitude = x.hypot(y).min(1.0);
        let maximum = finite_or(brush.tilt_max_scale, 1.0).clamp(1.0, MAX_TILT_SCALE);
        1.0 + (maximum - 1.0) * magnitude
    } else {
        1.0
    };
    base * pressure_scale * tilt_scale
}

fn finite_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        fallback
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
#[allow(dead_code)] // protocol/client conformance とcursor計測テストでも共有する
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
            width: stroke_width(brush, pts[j].2, pts[j].4, pts[j].5, canvas_h),
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
        width: stroke_width(brush, pts[n - 1].2, pts[n - 1].4, pts[n - 1].5, canvas_h),
    })
}

/// 1 点ストローク: round cap の点 (中心, 半径)
pub fn dot(pts: &[Point], canvas_w: f64, canvas_h: f64, brush: &Brush) -> Option<(Vec2, f64)> {
    if pts.len() != 1 {
        return None;
    }
    Some((
        pos(&pts[0], canvas_w, canvas_h),
        stroke_width(brush, pts[0].2, pts[0].4, pts[0].5, canvas_h) / 2.0,
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
            pressure_min: 0.2,
            tilt_width: false,
            tilt_max_scale: 1.0,
        }
    }

    fn pts() -> Vec<Point> {
        vec![
            (0.0, 0.0, 0.5, 0.0, 0.0, 0.0),
            (0.1, 0.0, 0.5, 16.0, 0.0, 0.0),
            (0.2, 0.0, 0.5, 32.0, 0.0, 0.0),
            (0.3, 0.0, 0.5, 48.0, 0.0, 0.0),
        ]
    }

    #[test]
    fn width_reflects_pressure() {
        assert!((stroke_width(&brush(), 1.0, 0.0, 0.0, 1000.0) - 10.0).abs() < 1e-9);
        assert!((stroke_width(&brush(), 0.0, 0.0, 0.0, 1000.0) - 2.0).abs() < 1e-9);
        assert!((stroke_width(&brush(), 0.5, 0.0, 0.0, 1000.0) - 6.0).abs() < 1e-9);
    }

    #[test]
    fn marker_tilt_uses_normalized_magnitude() {
        let marker = Brush {
            tool: Tool::Marker,
            tilt_width: true,
            tilt_max_scale: 1.75,
            ..brush()
        };
        assert!((stroke_width(&marker, 1.0, 0.0, 0.0, 1000.0) - 10.0).abs() < 1e-9);
        assert!((stroke_width(&marker, 1.0, 0.6, 0.8, 1000.0) - 17.5).abs() < 1e-9);
        // Invalid protocol data cannot create negative, infinite, or unbounded widths.
        assert!((stroke_width(&marker, f64::NAN, 10.0, f64::NAN, 1000.0) - 17.5).abs() < 1e-9);
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
        assert!((radius - 6.0 / 2.0).abs() < 1e-9);
    }

    #[test]
    fn full_is_stable_plus_tail() {
        assert_eq!(full_segments(&pts(), 1000.0, 1000.0, &brush()).len(), 3);
    }

    fn long_stroke(point_count: usize) -> Vec<Point> {
        (0..point_count)
            .map(|index| {
                let u = index as f64 / (point_count - 1) as f64;
                let v = ((index * 37) % 997) as f64 / 996.0;
                let pressure = (index % 101) as f64 / 100.0;
                (
                    u,
                    v,
                    pressure,
                    index as f64 * 0.25,
                    (index % 91) as f64 / 90.0,
                    -((index % 46) as f64 / 45.0),
                )
            })
            .collect()
    }

    #[test]
    fn ten_thousand_point_incremental_geometry_matches_full_geometry() {
        let source = long_stroke(10_000);
        let mut received = Vec::with_capacity(source.len());
        let mut incremental = Vec::with_capacity(stable_segment_count(source.len()));
        let mut next_segment = 1;

        for point in &source {
            received.push(*point);
            let new_segments = stable_segments(&received, 3840.0, 2160.0, &brush(), next_segment);
            // 1点ずつ届く通常更新では、既存9,999点の有無にかかわらず新規仕事は
            // 最大1segment。Rendererもこのcursorをそのまま保持する。
            assert!(new_segments.len() <= 1);
            next_segment += new_segments.len();
            incremental.extend(new_segments);
        }

        assert_eq!(
            incremental,
            stable_segments(&source, 3840.0, 2160.0, &brush(), 1)
        );
        assert_eq!(next_segment, stable_segment_count(source.len()) + 1);
    }

    #[test]
    fn ten_thousand_point_last_update_meets_the_native_frame_budget() {
        const SAMPLES: usize = 2_000;
        const TARGET_FRAME_MS: f64 = 1_000.0 / 60.0;
        let source = long_stroke(10_000);
        let next_segment = stable_segment_count(source.len() - 1) + 1;

        let started = std::time::Instant::now();
        let mut produced = 0;
        for _ in 0..SAMPLES {
            produced += std::hint::black_box(stable_segments(
                std::hint::black_box(&source),
                3840.0,
                2160.0,
                std::hint::black_box(&brush()),
                std::hint::black_box(next_segment),
            ))
            .len();
        }
        let elapsed = started.elapsed();
        let average_ms = elapsed.as_secs_f64() * 1_000.0 / SAMPLES as f64;
        eprintln!(
            "10,000-point incremental geometry: {average_ms:.6} ms/update ({SAMPLES} samples)"
        );

        assert_eq!(produced, SAMPLES);
        assert!(
            average_ms < TARGET_FRAME_MS,
            "incremental geometry {average_ms:.3} ms exceeded the 60fps frame budget"
        );
    }
}
