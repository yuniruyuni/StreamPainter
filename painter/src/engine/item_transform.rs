//! Shape / stampの選択、hit-test、move / scale / rotateに共通する純geometry。
//!
//! 計算はcontent高さを1とする表示座標で行う。xだけcanvas aspectを掛けるため、
//! 16:9 canvasでもpointerと同じ見た目の角度・距離になる。

#![cfg_attr(not(windows), allow(dead_code))]

use std::f64::consts::PI;

use crate::protocol::{CanvasItem, ItemTransform, Position, ShapeItem, ShapeKind};

/// boxの各辺、line / arrowの長さに適用する最小表示寸法（content高さ比）。
pub const MIN_ITEM_SIZE_N: f64 = 0.02;
/// line / arrowの選択枠だけに与える最小高さ。item geometry自体は高さ0のまま。
pub const LINE_SELECTION_HEIGHT_N: f64 = 0.024;
/// 選択枠上辺からrotation handleまでの表示距離（content高さ比）。
pub const ROTATE_HANDLE_OFFSET_N: f64 = 0.035;
/// pointer の量子化や丸めだけで履歴を作らないための表示座標上の dead zone。
const TRANSFORM_DRAG_EPSILON_N: f64 = 1e-5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformCorner {
    NorthWest,
    NorthEast,
    SouthEast,
    SouthWest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformHandle {
    Move,
    Scale(TransformCorner),
    Rotate,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TransformGeometry {
    Line { arrow: bool, stroke_radius_n: f64 },
    Box { stroke_radius_n: f64 },
    Ellipse { stroke_radius_n: f64 },
}

/// 1 pointer drag分の純状態。scaleは常に中心基準・縦横比固定。
#[derive(Debug, Clone, Copy)]
pub struct TransformInteraction {
    handle: TransformHandle,
    origin: ItemTransform,
    pointer_origin: Position,
    aspect: f64,
    geometry: TransformGeometry,
    origin_angle: f64,
    origin_distance: f64,
}

impl TransformInteraction {
    pub fn begin(
        item: &CanvasItem,
        handle: TransformHandle,
        pointer: Position,
        aspect: f64,
    ) -> Option<Self> {
        let origin = item_transform(item, aspect)?;
        if !valid_position(pointer) || !valid_aspect(aspect) {
            return None;
        }
        let relative = display_delta(origin.center, pointer, aspect);
        let origin_distance = relative.0.hypot(relative.1);
        let origin_angle = relative.1.atan2(relative.0);
        if matches!(handle, TransformHandle::Scale(_) | TransformHandle::Rotate)
            && origin_distance <= f64::EPSILON
        {
            return None;
        }
        Some(Self {
            handle,
            origin,
            pointer_origin: pointer,
            aspect,
            geometry: item_geometry(item)?,
            origin_angle,
            origin_distance,
        })
    }

    pub fn handle(&self) -> TransformHandle {
        self.handle
    }

    pub fn update(&self, pointer: Position) -> Option<ItemTransform> {
        if !valid_position(pointer) {
            return None;
        }
        if display_distance(
            display_delta(self.pointer_origin, pointer, self.aspect),
            (0.0, 0.0),
        ) <= TRANSFORM_DRAG_EPSILON_N
        {
            // clickだけならnormalize/roundingを通さず、永続値をbyte-for-byte維持する。
            return Some(self.origin);
        }
        let mut desired = self.origin;
        match self.handle {
            TransformHandle::Move => {
                desired.center.0 += pointer.0 - self.pointer_origin.0;
                desired.center.1 += pointer.1 - self.pointer_origin.1;
            }
            TransformHandle::Scale(_) => {
                let delta = display_delta(self.origin.center, pointer, self.aspect);
                let factor = (delta.0.hypot(delta.1) / self.origin_distance).max(
                    minimum_dimension_scale(self.origin, self.geometry, self.aspect),
                );
                desired.width_n *= factor;
                desired.height_n *= factor;
            }
            TransformHandle::Rotate => {
                let delta = display_delta(self.origin.center, pointer, self.aspect);
                let angle = delta.1.atan2(delta.0);
                desired.rotation += normalize_angle(angle - self.origin_angle);
            }
        }
        normalize_for_geometry(desired, self.geometry, self.aspect)
    }
}

/// v6 shapeのstart/endから、表示座標上で同じgeometryになるtransformを得る。
pub fn shape_transform_from_legacy(shape: &ShapeItem, aspect: f64) -> Option<ItemTransform> {
    if !valid_aspect(aspect) || !valid_position(shape.start) || !valid_position(shape.end) {
        return None;
    }
    let center = (
        (shape.start.0 + shape.end.0) / 2.0,
        (shape.start.1 + shape.end.1) / 2.0,
    );
    let dx = shape.end.0 - shape.start.0;
    let dy = shape.end.1 - shape.start.1;
    let transform = match shape.shape {
        ShapeKind::Line | ShapeKind::Arrow => {
            let display_length = (dx * aspect).hypot(dy);
            ItemTransform {
                center,
                width_n: display_length / aspect,
                height_n: 0.0,
                rotation: dy.atan2(dx * aspect),
            }
        }
        ShapeKind::Rectangle | ShapeKind::Ellipse => ItemTransform {
            center,
            width_n: dx.abs(),
            height_n: dy.abs(),
            rotation: 0.0,
        },
    };
    finite_transform(transform).then_some(ItemTransform {
        center: (round6(transform.center.0), round6(transform.center.1)),
        width_n: round6(transform.width_n),
        height_n: round6(transform.height_n),
        rotation: round6(normalize_angle(transform.rotation)),
    })
}

pub fn item_transform(item: &CanvasItem, aspect: f64) -> Option<ItemTransform> {
    match item {
        CanvasItem::Shape { shape } => shape
            .transform
            .filter(|transform| finite_transform(*transform))
            .or_else(|| shape_transform_from_legacy(shape, aspect)),
        CanvasItem::Stamp { stamp } => {
            let transform = ItemTransform {
                center: stamp.center,
                width_n: stamp.width_n,
                height_n: stamp.height_n,
                rotation: stamp.rotation,
            };
            finite_transform(transform).then_some(transform)
        }
        CanvasItem::Stroke { .. } => None,
    }
}

/// authoritative transformをitemへ反映する。shapeのstart/endはv6 fallbackとして保持する。
pub fn apply_item_transform(item: &mut CanvasItem, transform: ItemTransform) -> bool {
    match item {
        CanvasItem::Shape { shape } => {
            shape.transform = Some(transform);
            true
        }
        CanvasItem::Stamp { stamp } => {
            stamp.center = transform.center;
            stamp.width_n = transform.width_n;
            stamp.height_n = transform.height_n;
            stamp.rotation = transform.rotation;
            true
        }
        CanvasItem::Stroke { .. } => false,
    }
}

pub fn normalize_item_transform(
    item: &CanvasItem,
    transform: ItemTransform,
    aspect: f64,
) -> Option<ItemTransform> {
    normalize_for_geometry(transform, item_geometry(item)?, aspect)
}

/// topmost探索に使う、回転を考慮したshape / stamp hit-test。
pub fn item_hit_test(item: &CanvasItem, point: Position, aspect: f64, tolerance_n: f64) -> bool {
    if !item.is_done() || !valid_position(point) || !valid_aspect(aspect) {
        return false;
    }
    let Some(transform) = item_transform(item, aspect) else {
        return false;
    };
    let (x, y) = point_in_local_display(transform, point, aspect);
    let tolerance = tolerance_n.max(0.0);
    match item_geometry(item) {
        Some(geometry @ TransformGeometry::Line { arrow, .. }) => {
            let half_width = transform.width_n.abs() * aspect / 2.0;
            let radius = geometry_stroke_radius(geometry) + tolerance;
            let mut segments = vec![((-half_width, 0.0), (half_width, 0.0))];
            if arrow {
                let head_length = arrow_head_length(transform, geometry, aspect);
                let spread = PI / 6.0;
                for angle in [-spread, spread] {
                    segments.push((
                        (half_width, 0.0),
                        (
                            half_width - head_length * angle.cos(),
                            -head_length * angle.sin(),
                        ),
                    ));
                }
            }
            segments
                .into_iter()
                .any(|(start, end)| point_segment_distance((x, y), start, end) <= radius)
        }
        Some(geometry @ TransformGeometry::Box { .. }) => {
            let half_width = transform.width_n.abs() * aspect / 2.0;
            let half_height = transform.height_n.abs() / 2.0;
            let padding = geometry_stroke_radius(geometry) + tolerance;
            x.abs() <= half_width + padding && y.abs() <= half_height + padding
        }
        Some(geometry @ TransformGeometry::Ellipse { .. }) => {
            let padding = geometry_stroke_radius(geometry) + tolerance;
            let rx = transform.width_n.abs() * aspect / 2.0 + padding;
            let ry = transform.height_n.abs() / 2.0 + padding;
            rx > 0.0 && ry > 0.0 && (x / rx).powi(2) + (y / ry).powi(2) <= 1.0
        }
        None => false,
    }
}

/// selected itemのhandleを優先して調べ、最後にbody moveを返す。
pub fn selection_handle_at(
    item: &CanvasItem,
    point: Position,
    aspect: f64,
    handle_radius_n: f64,
) -> Option<TransformHandle> {
    if !item.is_done() || !valid_position(point) || !valid_aspect(aspect) {
        return None;
    }
    let transform = item_transform(item, aspect)?;
    let radius = handle_radius_n.max(0.0);
    let (half_width, half_height) = selection_half_extents(transform, item, aspect);
    let local = point_in_local_display(transform, point, aspect);
    let rotate = (0.0, -half_height - ROTATE_HANDLE_OFFSET_N);
    if display_distance(local, rotate) <= radius {
        return Some(TransformHandle::Rotate);
    }
    for (corner, position) in [
        (TransformCorner::NorthWest, (-half_width, -half_height)),
        (TransformCorner::NorthEast, (half_width, -half_height)),
        (TransformCorner::SouthEast, (half_width, half_height)),
        (TransformCorner::SouthWest, (-half_width, half_height)),
    ] {
        if display_distance(local, position) <= radius {
            return Some(TransformHandle::Scale(corner));
        }
    }
    item_hit_test(item, point, aspect, radius).then_some(TransformHandle::Move)
}

/// selection UI用の半幅・半高（content高さ=1の表示座標）。
pub fn selection_half_extents(
    transform: ItemTransform,
    item: &CanvasItem,
    aspect: f64,
) -> (f64, f64) {
    let Some(geometry) = item_geometry(item) else {
        return (0.0, 0.0);
    };
    let (half_width, half_height) = visual_half_extents(transform, geometry, aspect);
    if matches!(geometry, TransformGeometry::Line { .. }) {
        (half_width, half_height.max(LINE_SELECTION_HEIGHT_N / 2.0))
    } else {
        (half_width, half_height)
    }
}

fn item_geometry(item: &CanvasItem) -> Option<TransformGeometry> {
    match item {
        CanvasItem::Shape { shape } => match shape.shape {
            ShapeKind::Line | ShapeKind::Arrow => Some(TransformGeometry::Line {
                arrow: shape.shape == ShapeKind::Arrow,
                stroke_radius_n: finite_stroke_radius(shape.style.width_n),
            }),
            ShapeKind::Rectangle => Some(TransformGeometry::Box {
                stroke_radius_n: finite_stroke_radius(shape.style.width_n),
            }),
            ShapeKind::Ellipse => Some(TransformGeometry::Ellipse {
                stroke_radius_n: finite_stroke_radius(shape.style.width_n),
            }),
        },
        CanvasItem::Stamp { .. } => Some(TransformGeometry::Box {
            stroke_radius_n: 0.0,
        }),
        CanvasItem::Stroke { .. } => None,
    }
}

fn normalize_for_geometry(
    mut transform: ItemTransform,
    geometry: TransformGeometry,
    aspect: f64,
) -> Option<ItemTransform> {
    if !finite_transform(transform) || !valid_aspect(aspect) {
        return None;
    }
    transform.rotation = normalize_angle(transform.rotation);
    transform.width_n = transform.width_n.abs();
    transform.height_n = transform.height_n.abs();
    match geometry {
        TransformGeometry::Line { .. } => {
            transform.width_n = transform.width_n.max((MIN_ITEM_SIZE_N + 1e-6) / aspect);
            transform.height_n = 0.0;
        }
        TransformGeometry::Box { .. } | TransformGeometry::Ellipse { .. } => {
            ensure_box_minimum(&mut transform, aspect);
        }
    }

    // stroke幅・矢印headを含む回転後のvisible inkがcontentを越える場合だけ、
    // 設定済みminimumまでの範囲で縦横比を保って縮小する。
    const FIT_LIMIT: f64 = 0.5 - 4e-6;
    let extents = rotated_visual_extents(transform, geometry, aspect);
    if extents.0 > FIT_LIMIT || extents.1 > FIT_LIMIT {
        let minimum_scale = minimum_dimension_scale(transform, geometry, aspect).min(1.0);
        let mut minimum = transform;
        minimum.width_n *= minimum_scale;
        minimum.height_n *= minimum_scale;
        let minimum_extents = rotated_visual_extents(minimum, geometry, aspect);
        if minimum_extents.0 <= FIT_LIMIT && minimum_extents.1 <= FIT_LIMIT {
            let mut low = minimum_scale;
            let mut high = 1.0;
            for _ in 0..48 {
                let middle = (low + high) / 2.0;
                let mut candidate = transform;
                candidate.width_n *= middle;
                candidate.height_n *= middle;
                let candidate_extents = rotated_visual_extents(candidate, geometry, aspect);
                if candidate_extents.0 <= FIT_LIMIT && candidate_extents.1 <= FIT_LIMIT {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            transform.width_n *= low;
            transform.height_n *= low;
        } else {
            // 極端に太い外部snapshotでも0x0にはせず、minimum geometryを回復可能に保つ。
            transform = minimum;
        }
    }

    transform.width_n = round6(transform.width_n);
    transform.height_n = round6(transform.height_n);
    transform.rotation = round6(transform.rotation);
    let (extent_x, extent_y) = rotated_visual_extents(transform, geometry, aspect);
    transform.center.0 = clamp_axis_rounded(transform.center.0, extent_x);
    transform.center.1 = clamp_axis_rounded(transform.center.1, extent_y);
    Some(transform)
}

fn rotated_visual_extents(
    transform: ItemTransform,
    geometry: TransformGeometry,
    aspect: f64,
) -> (f64, f64) {
    let cosine = transform.rotation.cos().abs();
    let sine = transform.rotation.sin().abs();
    let (half_width_px, half_height_px) = visual_half_extents(transform, geometry, aspect);
    let extent_x_px = cosine * half_width_px + sine * half_height_px;
    let extent_y = sine * half_width_px + cosine * half_height_px;
    (extent_x_px / aspect, extent_y)
}

fn visual_half_extents(
    transform: ItemTransform,
    geometry: TransformGeometry,
    aspect: f64,
) -> (f64, f64) {
    let half_width = transform.width_n.abs() * aspect / 2.0;
    let half_height = transform.height_n.abs() / 2.0;
    let radius = geometry_stroke_radius(geometry);
    match geometry {
        TransformGeometry::Line { arrow: false, .. } => (half_width + radius, radius),
        TransformGeometry::Line { arrow: true, .. } => (
            half_width + radius,
            (arrow_head_length(transform, geometry, aspect) * (PI / 6.0).sin() + radius)
                .max(radius),
        ),
        TransformGeometry::Box { .. } | TransformGeometry::Ellipse { .. } => {
            (half_width + radius, half_height + radius)
        }
    }
}

fn arrow_head_length(transform: ItemTransform, geometry: TransformGeometry, aspect: f64) -> f64 {
    let length = transform.width_n.abs() * aspect;
    let line_width = geometry_stroke_radius(geometry) * 2.0;
    (length * 0.4).min((line_width * 4.0).max(0.02))
}

fn geometry_stroke_radius(geometry: TransformGeometry) -> f64 {
    match geometry {
        TransformGeometry::Line {
            stroke_radius_n, ..
        }
        | TransformGeometry::Box { stroke_radius_n }
        | TransformGeometry::Ellipse { stroke_radius_n } => stroke_radius_n,
    }
}

fn finite_stroke_radius(width_n: f64) -> f64 {
    if width_n.is_finite() {
        width_n.abs() / 2.0
    } else {
        0.0
    }
}

fn minimum_dimension_scale(
    transform: ItemTransform,
    geometry: TransformGeometry,
    aspect: f64,
) -> f64 {
    let target = MIN_ITEM_SIZE_N + 1e-6;
    match geometry {
        TransformGeometry::Line { .. } => {
            target / (transform.width_n.abs() * aspect).max(f64::EPSILON)
        }
        TransformGeometry::Box { .. } | TransformGeometry::Ellipse { .. } => (target
            / (transform.width_n.abs() * aspect).max(f64::EPSILON))
        .max(target / transform.height_n.abs().max(f64::EPSILON)),
    }
}

fn ensure_box_minimum(transform: &mut ItemTransform, aspect: f64) {
    let target = MIN_ITEM_SIZE_N + 1e-6;
    let width_display = transform.width_n * aspect;
    match (
        width_display > f64::EPSILON,
        transform.height_n > f64::EPSILON,
    ) {
        (true, true) => {
            let scale = (target / width_display)
                .max(target / transform.height_n)
                .max(1.0);
            transform.width_n *= scale;
            transform.height_n *= scale;
        }
        (true, false) => {
            transform.width_n = transform.width_n.max(target / aspect);
            transform.height_n = target;
        }
        (false, true) => {
            transform.width_n = target / aspect;
            transform.height_n = transform.height_n.max(target);
        }
        (false, false) => {
            transform.width_n = target / aspect;
            transform.height_n = target;
        }
    }
}

fn point_segment_distance(point: (f64, f64), start: (f64, f64), end: (f64, f64)) -> f64 {
    let delta = (end.0 - start.0, end.1 - start.1);
    let length_squared = delta.0 * delta.0 + delta.1 * delta.1;
    if length_squared <= f64::EPSILON {
        return display_distance(point, start);
    }
    let projection = (((point.0 - start.0) * delta.0 + (point.1 - start.1) * delta.1)
        / length_squared)
        .clamp(0.0, 1.0);
    display_distance(
        point,
        (
            start.0 + projection * delta.0,
            start.1 + projection * delta.1,
        ),
    )
}

fn clamp_axis_rounded(value: f64, extent: f64) -> f64 {
    if extent >= 0.5 {
        return 0.5;
    }
    let lower = ceil6(extent);
    let upper = floor6(1.0 - extent);
    round6(value).clamp(lower, upper)
}

fn point_in_local_display(transform: ItemTransform, point: Position, aspect: f64) -> (f64, f64) {
    let (dx, dy) = display_delta(transform.center, point, aspect);
    let cosine = transform.rotation.cos();
    let sine = transform.rotation.sin();
    (cosine * dx + sine * dy, -sine * dx + cosine * dy)
}

fn display_delta(from: Position, to: Position, aspect: f64) -> (f64, f64) {
    ((to.0 - from.0) * aspect, to.1 - from.1)
}

fn display_distance(left: (f64, f64), right: (f64, f64)) -> f64 {
    (left.0 - right.0).hypot(left.1 - right.1)
}

fn normalize_angle(angle: f64) -> f64 {
    (angle + PI).rem_euclid(2.0 * PI) - PI
}

fn finite_transform(transform: ItemTransform) -> bool {
    valid_position(transform.center)
        && transform.width_n.is_finite()
        && transform.height_n.is_finite()
        && transform.rotation.is_finite()
}

fn valid_position(position: Position) -> bool {
    position.0.is_finite() && position.1.is_finite()
}

fn valid_aspect(aspect: f64) -> bool {
    aspect.is_finite() && aspect > 0.0
}

fn round6(value: f64) -> f64 {
    (value * 1e6).round() / 1e6
}

fn ceil6(value: f64) -> f64 {
    (value * 1e6).ceil() / 1e6
}

fn floor6(value: f64) -> f64 {
    (value * 1e6).floor() / 1e6
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{LineStyle, StampItem};

    const ASPECT: f64 = 16.0 / 9.0;

    fn shape(kind: ShapeKind, start: Position, end: Position) -> CanvasItem {
        CanvasItem::Shape {
            shape: ShapeItem {
                item_id: format!("{kind:?}"),
                layer_id: crate::protocol::DEFAULT_LAYER_ID.into(),
                shape: kind,
                style: LineStyle {
                    color: "#fff".into(),
                    opacity: 1.0,
                    width_n: 0.01,
                },
                start,
                end,
                transform: None,
                done: true,
                ended_at: Some(1.0),
            },
        }
    }

    fn stamp(transform: ItemTransform) -> CanvasItem {
        CanvasItem::Stamp {
            stamp: StampItem {
                item_id: "stamp".into(),
                layer_id: crate::protocol::DEFAULT_LAYER_ID.into(),
                stamp_id: "asset".into(),
                center: transform.center,
                width_n: transform.width_n,
                height_n: transform.height_n,
                rotation: transform.rotation,
                opacity: 1.0,
                done: true,
                ended_at: Some(1.0),
            },
        }
    }

    fn local_display_position(
        transform: ItemTransform,
        local: (f64, f64),
        aspect: f64,
    ) -> Position {
        let cosine = transform.rotation.cos();
        let sine = transform.rotation.sin();
        (
            transform.center.0 + (cosine * local.0 - sine * local.1) / aspect,
            transform.center.1 + sine * local.0 + cosine * local.1,
        )
    }

    #[test]
    fn legacy_line_transform_preserves_display_angle_and_length() {
        let item = shape(ShapeKind::Arrow, (0.1, 0.8), (0.7, 0.2));
        let transform = item_transform(&item, ASPECT).unwrap();
        assert!((transform.center.0 - 0.4).abs() < 1e-9);
        assert!((transform.center.1 - 0.5).abs() < 1e-9);
        assert!((transform.rotation - (-0.6_f64).atan2(0.6 * ASPECT)).abs() < 1e-6);
        assert!(transform.width_n > 0.6);
        assert_eq!(transform.height_n, 0.0);
    }

    #[test]
    fn rotated_item_hit_test_uses_visual_canvas_coordinates() {
        let item = stamp(ItemTransform {
            center: (0.5, 0.5),
            width_n: 0.3,
            height_n: 0.1,
            rotation: PI / 2.0,
        });
        assert!(item_hit_test(&item, (0.5, 0.7), ASPECT, 0.0));
        assert!(!item_hit_test(&item, (0.68, 0.5), ASPECT, 0.0));
    }

    #[test]
    fn selection_distinguishes_rotate_scale_and_move_handles() {
        let item = stamp(ItemTransform {
            center: (0.5, 0.5),
            width_n: 0.2,
            height_n: 0.2,
            rotation: 0.0,
        });
        let transform = item_transform(&item, ASPECT).unwrap();
        let (half_width, half_height) = selection_half_extents(transform, &item, ASPECT);
        let to_position = |local: (f64, f64)| {
            (
                transform.center.0 + local.0 / ASPECT,
                transform.center.1 + local.1,
            )
        };
        assert_eq!(
            selection_handle_at(
                &item,
                to_position((0.0, -half_height - ROTATE_HANDLE_OFFSET_N)),
                ASPECT,
                0.01,
            ),
            Some(TransformHandle::Rotate)
        );
        assert_eq!(
            selection_handle_at(&item, to_position((half_width, half_height)), ASPECT, 0.01,),
            Some(TransformHandle::Scale(TransformCorner::SouthEast))
        );
        assert_eq!(
            selection_handle_at(&item, transform.center, ASPECT, 0.01),
            Some(TransformHandle::Move)
        );
    }

    #[test]
    fn move_clamps_the_rotated_bounds_inside_content() {
        let item = stamp(ItemTransform {
            center: (0.5, 0.5),
            width_n: 0.3,
            height_n: 0.2,
            rotation: PI / 4.0,
        });
        let interaction =
            TransformInteraction::begin(&item, TransformHandle::Move, (0.5, 0.5), ASPECT).unwrap();
        let moved = interaction.update((2.0, -1.0)).unwrap();
        let geometry = item_geometry(&item).unwrap();
        let (extent_x, extent_y) = rotated_visual_extents(moved, geometry, ASPECT);
        assert!(moved.center.0 <= 1.0 - extent_x + 1e-6);
        assert!(moved.center.1 >= extent_y - 1e-6);
    }

    #[test]
    fn scale_is_aspect_locked_and_enforces_minimum_size() {
        let item = stamp(ItemTransform {
            center: (0.5, 0.5),
            width_n: 0.2,
            height_n: 0.1,
            rotation: 0.0,
        });
        let interaction = TransformInteraction::begin(
            &item,
            TransformHandle::Scale(TransformCorner::SouthEast),
            (0.6, 0.55),
            ASPECT,
        )
        .unwrap();
        let scaled = interaction.update((0.500001, 0.500001)).unwrap();
        assert!(scaled.width_n * ASPECT >= MIN_ITEM_SIZE_N - 1e-6);
        assert!(scaled.height_n >= MIN_ITEM_SIZE_N - 1e-6);
        assert!((scaled.width_n / scaled.height_n - 2.0).abs() < 1e-4);
    }

    #[test]
    fn rotate_normalizes_angle_and_fits_item_without_changing_aspect() {
        let item = stamp(ItemTransform {
            center: (0.5, 0.5),
            width_n: 0.8,
            height_n: 0.2,
            rotation: 0.0,
        });
        let interaction =
            TransformInteraction::begin(&item, TransformHandle::Rotate, (0.5, 0.2), ASPECT)
                .unwrap();
        let rotated = interaction.update((0.8, 0.5)).unwrap();
        let geometry = item_geometry(&item).unwrap();
        let (extent_x, extent_y) = rotated_visual_extents(rotated, geometry, ASPECT);
        assert!(rotated.rotation >= -PI && rotated.rotation < PI);
        assert!(extent_x <= 0.5 + 1e-9);
        assert!(extent_y <= 0.5 + 1e-9);
        assert!((rotated.width_n / rotated.height_n - 4.0).abs() < 1e-4);
    }

    #[test]
    fn scale_handle_at_exact_center_keeps_a_recoverable_minimum_item() {
        let item = stamp(ItemTransform {
            center: (0.5, 0.5),
            width_n: 0.2,
            height_n: 0.1,
            rotation: 0.0,
        });
        let interaction = TransformInteraction::begin(
            &item,
            TransformHandle::Scale(TransformCorner::SouthEast),
            (0.6, 0.55),
            ASPECT,
        )
        .unwrap();
        let scaled = interaction.update((0.5, 0.5)).unwrap();
        assert!(scaled.width_n * ASPECT >= MIN_ITEM_SIZE_N);
        assert!(scaled.height_n >= MIN_ITEM_SIZE_N);
        assert!(scaled.width_n > 0.0 && scaled.height_n > 0.0);
        assert!(selection_handle_at(&stamp(scaled), scaled.center, ASPECT, 0.01).is_some());
    }

    #[test]
    fn click_and_sub_quantization_motion_return_the_exact_origin() {
        let origin = ItemTransform {
            center: (0.4567894, 0.5432106),
            width_n: 0.1234564,
            height_n: 0.2345674,
            rotation: 0.3456784,
        };
        let item = stamp(origin);
        let interaction =
            TransformInteraction::begin(&item, TransformHandle::Move, (0.5, 0.5), ASPECT).unwrap();
        assert_eq!(interaction.update((0.5, 0.5)), Some(origin));
        assert_eq!(interaction.update((0.5 + 1e-7, 0.5 - 1e-7)), Some(origin));
    }

    #[test]
    fn malformed_zero_box_normalizes_to_recoverable_minimum_dimensions() {
        let item = stamp(ItemTransform {
            center: (0.5, 0.5),
            width_n: 0.2,
            height_n: 0.1,
            rotation: 0.0,
        });
        let normalized = normalize_item_transform(
            &item,
            ItemTransform {
                center: (0.5, 0.5),
                width_n: 0.0,
                height_n: 0.0,
                rotation: 0.0,
            },
            ASPECT,
        )
        .unwrap();
        assert!(normalized.width_n * ASPECT >= MIN_ITEM_SIZE_N);
        assert!(normalized.height_n >= MIN_ITEM_SIZE_N);
    }

    #[test]
    fn arrowhead_visible_ink_is_hittable_and_clamped_inside_content() {
        let mut item = shape(ShapeKind::Arrow, (0.2, 0.5), (0.8, 0.5));
        let requested = ItemTransform {
            center: (0.99, 0.01),
            width_n: 0.35,
            height_n: 0.0,
            rotation: PI / 4.0,
        };
        let normalized = normalize_item_transform(&item, requested, ASPECT).unwrap();
        assert!(apply_item_transform(&mut item, normalized));
        let geometry = item_geometry(&item).unwrap();
        let (extent_x, extent_y) = rotated_visual_extents(normalized, geometry, ASPECT);
        assert!(normalized.center.0 >= extent_x - 1e-6);
        assert!(normalized.center.0 <= 1.0 - extent_x + 1e-6);
        assert!(normalized.center.1 >= extent_y - 1e-6);
        assert!(normalized.center.1 <= 1.0 - extent_y + 1e-6);

        let half_width = normalized.width_n * ASPECT / 2.0;
        let head_length = arrow_head_length(normalized, geometry, ASPECT);
        let wing = (
            half_width - head_length * (PI / 6.0).cos(),
            head_length * (PI / 6.0).sin(),
        );
        let wing_point = local_display_position(normalized, wing, ASPECT);
        assert!(item_hit_test(&item, wing_point, ASPECT, 0.0));

        let outside = local_display_position(
            normalized,
            (wing.0, wing.1 + geometry_stroke_radius(geometry) + 0.03),
            ASPECT,
        );
        assert!(!item_hit_test(&item, outside, ASPECT, 0.0));
    }
}
