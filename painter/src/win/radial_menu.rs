//! 右ホールドと固定クリックの両方で使うラジアルメニューの状態・配置・ヒットテスト。

use std::f64::consts::{PI, TAU};

use crate::win::menu::{DrawTool, MenuAction, COLORS};

pub const DRAW_TOOL_COUNT: usize = 8;
pub const TOOL_COUNT: usize = DRAW_TOOL_COUNT + 1;
pub const STAMP_TOOL_INDEX: usize = DRAW_TOOL_COUNT;
pub const COLOR_COUNT: usize = COLORS.len();
pub const STAMPS_PER_RING: usize = 8;

const TOOL_INNER_RADIUS: f32 = 38.0;
const TOOL_OUTER_RADIUS: f32 = 104.0;
const COLOR_INNER_RADIUS: f32 = 116.0;
const COLOR_OUTER_RADIUS: f32 = 174.0;
const STAMP_RING_GAP: f32 = 7.0;
const COMMAND_WIDTH: f32 = 96.0;
const COMMAND_HEIGHT: f32 = 38.0;
const COMMAND_GAP: f32 = 8.0;
const COMMAND_DOCK_GAP: f32 = 14.0;
const VIEWPORT_MARGIN: f32 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadialCommand {
    Undo,
    Redo,
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadialRect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl RadialRect {
    pub fn center(self) -> (f32, f32) {
        (
            (self.left + self.right) / 2.0,
            (self.top + self.bottom) / 2.0,
        )
    }

    pub fn width(self) -> f32 {
        self.right - self.left
    }

    pub fn height(self) -> f32 {
        self.bottom - self.top
    }

    fn contains(self, point: (f32, f32)) -> bool {
        point.0 >= self.left
            && point.0 <= self.right
            && point.1 >= self.top
            && point.1 <= self.bottom
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadialSelection {
    StandardMenu,
    Tool(usize),
    StampCategory,
    Stamp(usize),
    Color(usize),
    Command(RadialCommand),
}

#[derive(Debug, PartialEq)]
pub enum RadialRelease {
    Action { action: MenuAction, keep_open: bool },
    Stamp(usize),
    Pin,
    LegacyMenu,
    StayOpen,
    Cancel,
}

impl RadialRelease {
    pub fn keeps_menu_open(&self) -> bool {
        matches!(
            self,
            Self::Pin
                | Self::StayOpen
                | Self::Action {
                    keep_open: true,
                    ..
                }
        )
    }
}

#[derive(Debug)]
pub struct RadialMenu {
    pointer_id: Option<u32>,
    pinned: bool,
    center_screen: (f64, f64),
    center_local: (f32, f32),
    surface_size: (f32, f32),
    scale: f32,
    stamp_count: usize,
    stamp_mode: bool,
    can_undo: bool,
    can_redo: bool,
    highlighted: Option<RadialSelection>,
    max_distance: f64,
}

impl RadialMenu {
    pub fn new(
        pointer_id: u32,
        center_screen: (f64, f64),
        center_local: (f32, f32),
        surface_size: (u32, u32),
        scale: f32,
        stamp_count: usize,
        history_available: (bool, bool),
    ) -> Self {
        Self {
            pointer_id: Some(pointer_id),
            pinned: false,
            center_screen,
            center_local,
            surface_size: (surface_size.0 as f32, surface_size.1 as f32),
            scale,
            stamp_count,
            stamp_mode: false,
            can_undo: history_available.0,
            can_redo: history_available.1,
            highlighted: Some(RadialSelection::StandardMenu),
            max_distance: 0.0,
        }
    }

    /// 固定表示中の左右クリックを選択操作として開始する。
    pub fn begin_click(&mut self, pointer_id: u32) -> bool {
        if !self.pinned || self.pointer_id.is_some() {
            return false;
        }
        self.pointer_id = Some(pointer_id);
        true
    }

    /// 選択候補が変わった場合は true。描画更新の抑制に使う。
    pub fn update(&mut self, screen: (f64, f64)) -> bool {
        let dx = screen.0 - self.center_screen.0;
        let dy = screen.1 - self.center_screen.1;
        let distance = dx.hypot(dy);
        self.max_distance = self.max_distance.max(distance);
        let next = self.selection_at(dx, dy, distance);
        if next == self.highlighted {
            return false;
        }
        self.highlighted = next;
        true
    }

    pub fn release(&mut self, screen: (f64, f64)) -> RadialRelease {
        let was_pinned = self.pinned;
        self.update(screen);
        self.pointer_id = None;

        // 最初の右ホールドを中央で離した場合だけ、選択せず固定表示へ移る。
        if !was_pinned && self.max_distance < f64::from(self.tool_inner_radius()) {
            self.pinned = true;
            self.stamp_mode = false;
            self.highlighted = Some(RadialSelection::StandardMenu);
            return RadialRelease::Pin;
        }

        if was_pinned && self.highlighted == Some(RadialSelection::StandardMenu) {
            return RadialRelease::LegacyMenu;
        }
        if let Some(RadialSelection::Stamp(index)) = self.highlighted {
            return RadialRelease::Stamp(index);
        }
        if let Some(action) = self
            .highlighted
            .and_then(|item| self.selection_action(item))
        {
            let keep_open = was_pinned && matches!(&action, MenuAction::Undo | MenuAction::Redo);
            return RadialRelease::Action { action, keep_open };
        }
        if was_pinned
            && matches!(
                self.highlighted,
                Some(RadialSelection::StampCategory | RadialSelection::Command(_))
            )
        {
            return RadialRelease::StayOpen;
        }
        RadialRelease::Cancel
    }

    pub fn center_local(&self) -> (f32, f32) {
        self.center_local
    }

    pub fn owns_pointer(&self, pointer_id: u32) -> bool {
        self.pointer_id == Some(pointer_id)
    }

    pub fn has_active_pointer(&self) -> bool {
        self.pointer_id.is_some()
    }

    pub fn cancel_active_pointer(&mut self) -> bool {
        self.pointer_id.take().is_some()
    }

    /// 固定中はボタンを押していないマウス移動もhoverとして受け取る。
    pub fn accepts_update_from(&self, pointer_id: u32) -> bool {
        self.pointer_id
            .map_or(self.pinned, |active| active == pointer_id)
    }

    pub fn is_pinned(&self) -> bool {
        self.pinned
    }

    pub fn highlighted(&self) -> Option<RadialSelection> {
        self.highlighted
    }

    pub fn stamp_mode(&self) -> bool {
        self.stamp_mode
    }

    pub fn stamp_count(&self) -> usize {
        self.stamp_count
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn tool_inner_radius(&self) -> f32 {
        TOOL_INNER_RADIUS * self.scale
    }

    pub fn tool_outer_radius(&self) -> f32 {
        TOOL_OUTER_RADIUS * self.scale
    }

    pub fn color_inner_radius(&self) -> f32 {
        COLOR_INNER_RADIUS * self.scale
    }

    pub fn color_outer_radius(&self) -> f32 {
        COLOR_OUTER_RADIUS * self.scale
    }

    pub fn stamp_ring_count(&self) -> usize {
        self.stamp_count.div_ceil(STAMPS_PER_RING)
    }

    pub fn stamp_ring_item_count(&self, ring: usize) -> usize {
        self.stamp_count
            .saturating_sub(ring * STAMPS_PER_RING)
            .min(STAMPS_PER_RING)
    }

    pub fn stamp_ring_radii(&self, ring: usize) -> Option<(f32, f32)> {
        if ring >= self.stamp_ring_count() {
            return None;
        }
        let step = (COLOR_OUTER_RADIUS - COLOR_INNER_RADIUS) + STAMP_RING_GAP;
        let inner = (COLOR_INNER_RADIUS + ring as f32 * step) * self.scale;
        Some((
            inner,
            inner + (COLOR_OUTER_RADIUS - COLOR_INNER_RADIUS) * self.scale,
        ))
    }

    /// 色リング表示中にだけ出す、円から独立した履歴・全消去ドック。
    pub fn command_buttons(&self) -> [(RadialCommand, RadialRect); 3] {
        let width = COMMAND_WIDTH * self.scale;
        let height = COMMAND_HEIGHT * self.scale;
        let gap = COMMAND_GAP * self.scale;
        let margin = VIEWPORT_MARGIN * self.scale;
        let total_width = width * 3.0 + gap * 2.0;
        let half_total = total_width / 2.0;
        let dock_center_x =
            clamp_center(self.center_local.0, half_total, self.surface_size.0, margin);

        let dock_gap = COMMAND_DOCK_GAP * self.scale;
        let below_top = self.center_local.1 + self.color_outer_radius() + dock_gap;
        let above_top = self.center_local.1 - self.color_outer_radius() - dock_gap - height;
        let below_fits = below_top + height + margin <= self.surface_size.1;
        let above_fits = above_top >= margin;
        let prefer_below = below_fits
            || (!above_fits && self.surface_size.1 - self.center_local.1 >= self.center_local.1);
        let desired_top = if prefer_below { below_top } else { above_top };
        let max_top = (self.surface_size.1 - margin - height).max(margin);
        let top = desired_top.clamp(margin, max_top);
        let left = dock_center_x - half_total;

        [
            RadialCommand::Undo,
            RadialCommand::Redo,
            RadialCommand::Clear,
        ]
        .map(|command| {
            let index = match command {
                RadialCommand::Undo => 0,
                RadialCommand::Redo => 1,
                RadialCommand::Clear => 2,
            };
            let item_left = left + index as f32 * (width + gap);
            (
                command,
                RadialRect {
                    left: item_left,
                    top,
                    right: item_left + width,
                    bottom: top + height,
                },
            )
        })
    }

    pub fn command_enabled(&self, command: RadialCommand) -> bool {
        match command {
            RadialCommand::Undo | RadialCommand::Clear => self.can_undo,
            RadialCommand::Redo => self.can_redo,
        }
    }

    pub fn set_history_availability(&mut self, can_undo: bool, can_redo: bool) -> bool {
        if self.can_undo == can_undo && self.can_redo == can_redo {
            return false;
        }
        self.can_undo = can_undo;
        self.can_redo = can_redo;
        true
    }

    fn selection_at(&mut self, dx: f64, dy: f64, distance: f64) -> Option<RadialSelection> {
        let local = (
            self.center_local.0 + dx as f32,
            self.center_local.1 + dy as f32,
        );
        if !self.stamp_mode {
            for (command, rect) in self.command_buttons() {
                if rect.contains(local) {
                    return Some(RadialSelection::Command(command));
                }
            }
        }

        if distance < f64::from(self.tool_inner_radius()) {
            self.stamp_mode = false;
            return Some(RadialSelection::StandardMenu);
        }

        if distance <= f64::from(self.tool_outer_radius()) {
            let index = sector_index(dx, dy, TOOL_COUNT);
            if index == STAMP_TOOL_INDEX {
                self.stamp_mode = self.stamp_count > 0;
                return Some(RadialSelection::StampCategory);
            }
            self.stamp_mode = false;
            return Some(RadialSelection::Tool(index));
        }

        if self.stamp_mode {
            return self.stamp_selection_at(dx, dy, distance);
        }
        if distance >= f64::from(self.color_inner_radius())
            && (!self.pinned || distance <= f64::from(self.color_outer_radius()))
        {
            return Some(RadialSelection::Color(sector_index(dx, dy, COLOR_COUNT)));
        }
        None
    }

    fn stamp_selection_at(&self, dx: f64, dy: f64, distance: f64) -> Option<RadialSelection> {
        let ring_count = self.stamp_ring_count();
        if ring_count == 0 || distance < f64::from(self.color_inner_radius()) {
            return None;
        }

        let ring_width = (COLOR_OUTER_RADIUS - COLOR_INNER_RADIUS) * self.scale;
        let step = ring_width + STAMP_RING_GAP * self.scale;
        let relative = distance as f32 - self.color_inner_radius();
        let raw_ring = (relative / step).floor() as usize;
        let ring = raw_ring.min(ring_count - 1);
        let (inner, outer) = self.stamp_ring_radii(ring)?;
        if distance < f64::from(inner)
            || (distance > f64::from(outer) && (self.pinned || ring < ring_count - 1))
        {
            return None;
        }

        let slot = sector_index(dx, dy, STAMPS_PER_RING);
        if slot >= self.stamp_ring_item_count(ring) {
            return None;
        }
        Some(RadialSelection::Stamp(ring * STAMPS_PER_RING + slot))
    }

    fn selection_action(&self, selection: RadialSelection) -> Option<MenuAction> {
        match selection {
            RadialSelection::Tool(index) => tool_at(index).map(MenuAction::SelectTool),
            RadialSelection::Color(index) => COLORS
                .get(index)
                .map(|(_, color)| MenuAction::SelectColor(color)),
            RadialSelection::Command(command) if self.command_enabled(command) => {
                Some(match command {
                    RadialCommand::Undo => MenuAction::Undo,
                    RadialCommand::Redo => MenuAction::Redo,
                    RadialCommand::Clear => MenuAction::Clear,
                })
            }
            RadialSelection::StandardMenu
            | RadialSelection::StampCategory
            | RadialSelection::Stamp(_)
            | RadialSelection::Command(_) => None,
        }
    }
}

pub fn scale_for_surface(width: u32, height: u32) -> f32 {
    (width.min(height) as f32 / 1080.0).clamp(0.85, 1.6)
}

pub fn sector_angles(index: usize, count: usize) -> (f32, f32) {
    let width = std::f32::consts::TAU / count as f32;
    let center = -std::f32::consts::FRAC_PI_2 + index as f32 * width;
    (center - width / 2.0, center + width / 2.0)
}

pub fn sector_center_angle(index: usize, count: usize) -> f32 {
    -std::f32::consts::FRAC_PI_2 + index as f32 * std::f32::consts::TAU / count as f32
}

pub fn tool_at(index: usize) -> Option<DrawTool> {
    match index {
        0 => Some(DrawTool::Select),
        1 => Some(DrawTool::Pen),
        2 => Some(DrawTool::Marker),
        3 => Some(DrawTool::Eraser),
        4 => Some(DrawTool::Line),
        5 => Some(DrawTool::Arrow),
        6 => Some(DrawTool::Rectangle),
        7 => Some(DrawTool::Ellipse),
        _ => None,
    }
}

pub fn tool_label(index: usize) -> Option<&'static str> {
    match index {
        0 => Some("選択"),
        1 => Some("ペン"),
        2 => Some("マーカー"),
        3 => Some("消しゴム"),
        4 => Some("直線"),
        5 => Some("矢印"),
        6 => Some("四角"),
        7 => Some("楕円"),
        STAMP_TOOL_INDEX => Some("スタンプ"),
        _ => None,
    }
}

pub fn command_label(command: RadialCommand) -> &'static str {
    match command {
        RadialCommand::Undo => "↶  元に戻す",
        RadialCommand::Redo => "↷  やり直す",
        RadialCommand::Clear => "全消去",
    }
}

fn clamp_center(value: f32, half_size: f32, limit: f32, margin: f32) -> f32 {
    let min = margin + half_size;
    let max = limit - margin - half_size;
    if min <= max {
        value.clamp(min, max)
    } else {
        limit / 2.0
    }
}

fn sector_index(dx: f64, dy: f64, count: usize) -> usize {
    let width = TAU / count as f64;
    let from_top = (dy.atan2(dx) + PI / 2.0 + width / 2.0).rem_euclid(TAU);
    (from_top / width).floor() as usize % count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(index: usize, count: usize, radius: f64) -> (f64, f64) {
        let angle = -PI / 2.0 + index as f64 * TAU / count as f64;
        (radius * angle.cos(), radius * angle.sin())
    }

    fn menu_with_history(stamp_count: usize, can_undo: bool, can_redo: bool) -> RadialMenu {
        RadialMenu::new(
            1,
            (0.0, 0.0),
            (500.0, 500.0),
            (1000, 1000),
            1.0,
            stamp_count,
            (can_undo, can_redo),
        )
    }

    fn menu(stamp_count: usize) -> RadialMenu {
        menu_with_history(stamp_count, false, false)
    }

    fn pin(mut menu: RadialMenu) -> RadialMenu {
        assert_eq!(menu.release((3.0, -2.0)), RadialRelease::Pin);
        assert!(menu.is_pinned());
        menu
    }

    fn command_point(menu: &RadialMenu, target: RadialCommand) -> (f64, f64) {
        let (_, rect) = menu
            .command_buttons()
            .into_iter()
            .find(|(command, _)| *command == target)
            .unwrap();
        let center = rect.center();
        (
            f64::from(center.0 - menu.center_local().0),
            f64::from(center.1 - menu.center_local().1),
        )
    }

    #[test]
    fn tool_ring_selects_each_tool_by_direction() {
        for index in 0..DRAW_TOOL_COUNT {
            let mut menu = menu(0);
            menu.update(point(index, TOOL_COUNT, 72.0));
            assert_eq!(menu.highlighted(), Some(RadialSelection::Tool(index)));
            assert_eq!(
                menu.release(point(index, TOOL_COUNT, 72.0)),
                RadialRelease::Action {
                    action: MenuAction::SelectTool(tool_at(index).unwrap()),
                    keep_open: false,
                }
            );
        }
    }

    #[test]
    fn outer_ring_selects_each_color_by_direction() {
        for (index, (_, color)) in COLORS.iter().enumerate() {
            let mut menu = menu(0);
            menu.update(point(index, COLOR_COUNT, 145.0));
            assert_eq!(menu.highlighted(), Some(RadialSelection::Color(index)));
            assert_eq!(
                menu.release(point(index, COLOR_COUNT, 145.0)),
                RadialRelease::Action {
                    action: MenuAction::SelectColor(color),
                    keep_open: false,
                }
            );
        }
    }

    #[test]
    fn center_release_pins_then_center_click_opens_the_legacy_menu() {
        let mut menu = pin(menu(0));
        assert!(menu.begin_click(2));
        assert_eq!(menu.release((3.0, -2.0)), RadialRelease::LegacyMenu);
    }

    #[test]
    fn pinned_menu_accepts_a_left_or_right_click_selection() {
        let mut menu = pin(menu(0));
        assert!(menu.begin_click(2));
        let target = point(2, TOOL_COUNT, 72.0);
        menu.update(target);
        assert_eq!(
            menu.release(target),
            RadialRelease::Action {
                action: MenuAction::SelectTool(DrawTool::Marker),
                keep_open: false,
            }
        );
    }

    #[test]
    fn undo_and_redo_stay_open_but_clear_closes() {
        for (command, action, keep_open) in [
            (RadialCommand::Undo, MenuAction::Undo, true),
            (RadialCommand::Redo, MenuAction::Redo, true),
            (RadialCommand::Clear, MenuAction::Clear, false),
        ] {
            let mut menu = pin(menu_with_history(0, true, true));
            let target = command_point(&menu, command);
            assert!(menu.begin_click(2));
            menu.update(target);
            assert_eq!(
                menu.release(target),
                RadialRelease::Action { action, keep_open }
            );
        }
    }

    #[test]
    fn disabled_history_command_keeps_the_pinned_menu_open() {
        let mut menu = pin(menu(0));
        let target = command_point(&menu, RadialCommand::Undo);
        assert!(menu.begin_click(2));
        menu.update(target);
        assert_eq!(
            menu.highlighted(),
            Some(RadialSelection::Command(RadialCommand::Undo))
        );
        assert_eq!(menu.release(target), RadialRelease::StayOpen);
    }

    #[test]
    fn command_dock_moves_above_the_ring_near_the_bottom_edge() {
        let menu = RadialMenu::new(
            1,
            (0.0, 0.0),
            (500.0, 960.0),
            (1000, 1000),
            1.0,
            0,
            (true, true),
        );
        for (_, rect) in menu.command_buttons() {
            assert!(rect.bottom < menu.center_local().1 - menu.color_outer_radius());
            assert!(rect.left >= 0.0 && rect.right <= 1000.0);
        }
    }

    #[test]
    fn releasing_in_the_gap_after_a_gesture_cancels() {
        assert_eq!(menu(0).release((110.0, 0.0)), RadialRelease::Cancel);
    }

    #[test]
    fn stamp_category_is_disabled_without_registered_stamps() {
        let mut menu = menu(0);
        let category = point(STAMP_TOOL_INDEX, TOOL_COUNT, 72.0);
        menu.update(category);
        assert_eq!(menu.highlighted(), Some(RadialSelection::StampCategory));
        assert!(!menu.stamp_mode());
        assert_eq!(menu.release(category), RadialRelease::Cancel);
    }

    #[test]
    fn stamp_category_switches_the_outer_rings_to_stamps() {
        let mut menu = menu(9);
        menu.update(point(STAMP_TOOL_INDEX, TOOL_COUNT, 72.0));
        assert!(menu.stamp_mode());
        assert_eq!(menu.stamp_ring_count(), 2);

        menu.update(point(3, STAMPS_PER_RING, 145.0));
        assert_eq!(menu.highlighted(), Some(RadialSelection::Stamp(3)));
        assert_eq!(
            menu.release(point(3, STAMPS_PER_RING, 145.0)),
            RadialRelease::Stamp(3)
        );
    }

    #[test]
    fn all_four_stamp_rings_are_selectable() {
        for index in 0..32 {
            let ring = index / STAMPS_PER_RING;
            let slot = index % STAMPS_PER_RING;
            let mut menu = menu(32);
            menu.update(point(STAMP_TOOL_INDEX, TOOL_COUNT, 72.0));
            let (inner, outer) = menu.stamp_ring_radii(ring).unwrap();
            let target = point(slot, STAMPS_PER_RING, f64::from((inner + outer) / 2.0));
            menu.update(target);
            assert_eq!(menu.highlighted(), Some(RadialSelection::Stamp(index)));
            assert_eq!(menu.release(target), RadialRelease::Stamp(index));
        }
    }

    #[test]
    fn returning_to_the_center_restores_color_selection() {
        let mut menu = menu(8);
        menu.update(point(STAMP_TOOL_INDEX, TOOL_COUNT, 72.0));
        assert!(menu.stamp_mode());
        menu.update((0.0, 0.0));
        assert!(!menu.stamp_mode());
        assert_eq!(menu.highlighted(), Some(RadialSelection::StandardMenu));
        menu.update(point(2, COLOR_COUNT, 145.0));
        assert_eq!(menu.highlighted(), Some(RadialSelection::Color(2)));
    }

    #[test]
    fn pinned_outer_ring_does_not_capture_clicks_far_outside() {
        let mut menu = pin(menu(0));
        menu.update((400.0, 0.0));
        assert_eq!(menu.highlighted(), None);
        assert!(menu.begin_click(2));
        assert_eq!(menu.release((400.0, 0.0)), RadialRelease::Cancel);
    }

    #[test]
    fn surface_scale_is_bounded() {
        assert_eq!(scale_for_surface(640, 480), 0.85);
        assert_eq!(scale_for_surface(1920, 1080), 1.0);
        assert_eq!(scale_for_surface(7680, 4320), 1.6);
    }
}
