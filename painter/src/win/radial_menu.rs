//! 右ホールドと固定クリックの両方で使うラジアルメニューの状態・配置・ヒットテスト。

use std::f64::consts::{PI, TAU};

use crate::protocol::MAX_LAYERS;
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
const PIN_ANCHOR_TOLERANCE: f32 = 8.0;
const COMMAND_TOTAL_WIDTH: f32 = COMMAND_WIDTH * 3.0 + COMMAND_GAP * 2.0;
const LAYOUT_EPSILON: f32 = 0.01;
const LAYER_PANEL_WIDTH: f32 = 184.0;
const LAYER_PANEL_GAP: f32 = 14.0;
const LAYER_HEADER_HEIGHT: f32 = 40.0;
const LAYER_ROW_HEIGHT: f32 = 34.0;
const LAYER_ACTION_SIZE: f32 = 30.0;

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
    Layer(usize),
    LayerAdd,
    LayerDelete,
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
    /// 初回右ホールドの開始位置。移動量とpin判定だけに使う。
    anchor_screen: (f64, f64),
    /// 補正後の描画・hit-test中心に対応するscreen座標。
    center_screen: (f64, f64),
    center_local: (f32, f32),
    surface_size: (f32, f32),
    scale: f32,
    stamp_count: usize,
    stamp_mode: bool,
    can_undo: bool,
    can_redo: bool,
    can_clear: bool,
    layers: Vec<RadialLayerEntry>,
    active_layer_id: String,
    panel_on_right: bool,
    highlighted: Option<RadialSelection>,
    max_distance: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadialLayerEntry {
    pub layer_id: String,
    pub name: String,
    pub item_count: usize,
}

impl RadialMenu {
    #[cfg(test)]
    pub fn new(
        pointer_id: u32,
        anchor_screen: (f64, f64),
        anchor_local: (f32, f32),
        surface_size: (u32, u32),
        scale: f32,
        stamp_count: usize,
        command_available: (bool, bool, bool),
    ) -> Self {
        Self::new_with_layers(
            pointer_id,
            anchor_screen,
            anchor_local,
            surface_size,
            scale,
            stamp_count,
            command_available,
            vec![RadialLayerEntry {
                layer_id: "default".into(),
                name: "レイヤー 1".into(),
                item_count: 0,
            }],
            "default".into(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_layers(
        pointer_id: u32,
        anchor_screen: (f64, f64),
        anchor_local: (f32, f32),
        surface_size: (u32, u32),
        scale: f32,
        stamp_count: usize,
        command_available: (bool, bool, bool),
        layers: Vec<RadialLayerEntry>,
        active_layer_id: String,
    ) -> Self {
        let surface_size = (surface_size.0 as f32, surface_size.1 as f32);
        let scale = fit_scale_for_layer_layout(surface_size, scale, stamp_count);
        let outer_radius = outer_radius_for_stamp_count(stamp_count, scale);
        let margin = VIEWPORT_MARGIN * scale;
        let panel_width = LAYER_PANEL_WIDTH * scale;
        let panel_gap = LAYER_PANEL_GAP * scale;
        let panel_extent = outer_radius + panel_gap + panel_width;
        let right_space = surface_size.0 - anchor_local.0 - margin;
        let left_space = anchor_local.0 - margin;
        let panel_on_right = right_space >= panel_extent || right_space >= left_space;
        let (left_extent, right_extent) = if panel_on_right {
            (outer_radius, panel_extent)
        } else {
            (panel_extent, outer_radius)
        };
        let vertical_extent = outer_radius.max(layer_panel_height(MAX_LAYERS, scale) / 2.0);
        let fitted_center_local = (
            clamp_asymmetric(
                anchor_local.0,
                left_extent,
                right_extent,
                surface_size.0,
                margin,
            ),
            clamp_center(anchor_local.1, vertical_extent, surface_size.1, margin),
        );
        let center_screen = (
            anchor_screen.0 + f64::from(fitted_center_local.0 - anchor_local.0),
            anchor_screen.1 + f64::from(fitted_center_local.1 - anchor_local.1),
        );
        Self {
            pointer_id: Some(pointer_id),
            pinned: false,
            anchor_screen,
            center_screen,
            center_local: fitted_center_local,
            surface_size,
            scale,
            stamp_count,
            stamp_mode: false,
            can_undo: command_available.0,
            can_redo: command_available.1,
            can_clear: command_available.2,
            layers,
            active_layer_id,
            panel_on_right,
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
        let gesture_dx = screen.0 - self.anchor_screen.0;
        let gesture_dy = screen.1 - self.anchor_screen.1;
        self.max_distance = self.max_distance.max(gesture_dx.hypot(gesture_dy));

        let dx = screen.0 - self.center_screen.0;
        let dy = screen.1 - self.center_screen.1;
        let distance = dx.hypot(dy);
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

        // 最初の右ホールドを動かさず離すか、補正後の中央へ戻した場合は固定表示へ移る。
        if !was_pinned
            && (self.max_distance < f64::from(PIN_ANCHOR_TOLERANCE * self.scale)
                || self.highlighted == Some(RadialSelection::StandardMenu))
        {
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
            let keep_open = was_pinned
                && matches!(
                    &action,
                    MenuAction::Undo
                        | MenuAction::Redo
                        | MenuAction::SelectLayer(_)
                        | MenuAction::AddLayer
                        | MenuAction::DeleteLayer(_)
                );
            return RadialRelease::Action { action, keep_open };
        }
        if was_pinned
            && matches!(
                self.highlighted,
                Some(
                    RadialSelection::StampCategory
                        | RadialSelection::Command(_)
                        | RadialSelection::Layer(_)
                        | RadialSelection::LayerAdd
                        | RadialSelection::LayerDelete
                )
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

    pub fn layers(&self) -> &[RadialLayerEntry] {
        &self.layers
    }

    pub fn active_layer_id(&self) -> &str {
        &self.active_layer_id
    }

    #[cfg(test)]
    pub fn panel_on_right(&self) -> bool {
        self.panel_on_right
    }

    pub fn layer_panel_rect(&self) -> RadialRect {
        let width = LAYER_PANEL_WIDTH * self.scale;
        let height = layer_panel_height(self.layers.len(), self.scale);
        let gap = LAYER_PANEL_GAP * self.scale;
        let (left, right) = if self.panel_on_right {
            let left = self.center_local.0 + self.outer_radius() + gap;
            (left, left + width)
        } else {
            let right = self.center_local.0 - self.outer_radius() - gap;
            (right - width, right)
        };
        RadialRect {
            left,
            top: self.center_local.1 - height / 2.0,
            right,
            bottom: self.center_local.1 + height / 2.0,
        }
    }

    pub fn layer_add_button(&self) -> RadialRect {
        let panel = self.layer_panel_rect();
        let size = LAYER_ACTION_SIZE * self.scale;
        RadialRect {
            left: panel.right - size - 5.0 * self.scale,
            top: panel.top + 5.0 * self.scale,
            right: panel.right - 5.0 * self.scale,
            bottom: panel.top + 5.0 * self.scale + size,
        }
    }

    /// 表示順（上レイヤーから）とmodel indexの対応。
    pub fn layer_rows(&self) -> Vec<(usize, RadialRect)> {
        let panel = self.layer_panel_rect();
        (0..self.layers.len())
            .map(|display_index| {
                let index = self.layers.len() - 1 - display_index;
                let top = panel.top
                    + LAYER_HEADER_HEIGHT * self.scale
                    + display_index as f32 * LAYER_ROW_HEIGHT * self.scale;
                (
                    index,
                    RadialRect {
                        left: panel.left,
                        top,
                        right: panel.right,
                        bottom: top + LAYER_ROW_HEIGHT * self.scale,
                    },
                )
            })
            .collect()
    }

    pub fn layer_delete_button(&self) -> Option<RadialRect> {
        let (_, row) = self
            .layer_rows()
            .into_iter()
            .find(|(index, _)| self.layers[*index].layer_id == self.active_layer_id)?;
        let inset = 2.0 * self.scale;
        let size = LAYER_ACTION_SIZE * self.scale;
        Some(RadialRect {
            left: row.right - size - inset,
            top: row.top + inset,
            right: row.right - inset,
            bottom: row.top + inset + size,
        })
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

    fn outer_radius(&self) -> f32 {
        self.stamp_ring_count()
            .checked_sub(1)
            .and_then(|ring| self.stamp_ring_radii(ring))
            .map_or(self.color_outer_radius(), |(_, outer)| outer)
    }

    /// 描画する全リングとcommand dockがsurface内にあり、相互に重ならない。
    pub fn layout_within_surface(&self) -> bool {
        let outer = self.outer_radius();
        let margin = VIEWPORT_MARGIN * self.scale;
        let minimum = margin - LAYOUT_EPSILON;
        let maximum_x = self.surface_size.0 - margin + LAYOUT_EPSILON;
        let maximum_y = self.surface_size.1 - margin + LAYOUT_EPSILON;
        let circle_fits = self.center_local.0 - outer >= minimum
            && self.center_local.0 + outer <= maximum_x
            && self.center_local.1 - outer >= minimum
            && self.center_local.1 + outer <= maximum_y;
        let color_outer = self.color_outer_radius();
        circle_fits
            && {
                let panel = self.layer_panel_rect();
                panel.left >= minimum
                    && panel.top >= minimum
                    && panel.right <= maximum_x
                    && panel.bottom <= maximum_y
            }
            && self.command_buttons().into_iter().all(|(_, rect)| {
                rect.left >= minimum
                    && rect.top >= minimum
                    && rect.right <= maximum_x
                    && rect.bottom <= maximum_y
                    && (rect.bottom <= self.center_local.1 - color_outer
                        || rect.top >= self.center_local.1 + color_outer)
            })
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
            RadialCommand::Undo => self.can_undo,
            RadialCommand::Redo => self.can_redo,
            RadialCommand::Clear => self.can_clear,
        }
    }

    pub fn set_command_availability(
        &mut self,
        can_undo: bool,
        can_redo: bool,
        can_clear: bool,
    ) -> bool {
        if self.can_undo == can_undo && self.can_redo == can_redo && self.can_clear == can_clear {
            return false;
        }
        self.can_undo = can_undo;
        self.can_redo = can_redo;
        self.can_clear = can_clear;
        true
    }

    pub fn set_layers(&mut self, layers: Vec<RadialLayerEntry>, active_layer_id: String) -> bool {
        if self.layers == layers && self.active_layer_id == active_layer_id {
            return false;
        }
        self.layers = layers;
        self.active_layer_id = active_layer_id;
        true
    }

    fn selection_at(&mut self, dx: f64, dy: f64, distance: f64) -> Option<RadialSelection> {
        let local = (
            self.center_local.0 + dx as f32,
            self.center_local.1 + dy as f32,
        );
        let add_button = self.layer_add_button();
        if add_button.contains(local) {
            return Some(RadialSelection::LayerAdd);
        }
        if self.layers.len() > 1
            && self
                .layer_delete_button()
                .is_some_and(|rect| rect.contains(local))
        {
            return Some(RadialSelection::LayerDelete);
        }
        for (index, rect) in self.layer_rows() {
            if rect.contains(local) {
                return Some(RadialSelection::Layer(index));
            }
        }
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
            RadialSelection::Layer(index) => self
                .layers
                .get(index)
                .map(|layer| MenuAction::SelectLayer(layer.layer_id.clone())),
            RadialSelection::LayerAdd if self.layers.len() < MAX_LAYERS => {
                Some(MenuAction::AddLayer)
            }
            RadialSelection::LayerDelete if self.layers.len() > 1 => {
                Some(MenuAction::DeleteLayer(self.active_layer_id.clone()))
            }
            RadialSelection::StandardMenu
            | RadialSelection::StampCategory
            | RadialSelection::Stamp(_)
            | RadialSelection::Command(_)
            | RadialSelection::LayerAdd
            | RadialSelection::LayerDelete => None,
        }
    }
}

pub fn scale_for_surface(width: u32, height: u32) -> f32 {
    (width.min(height) as f32 / 1080.0).clamp(0.85, 1.6)
}

/// 解像度・登録スタンプ数を含む実レイアウト用scale。
pub fn scale_for_menu(width: u32, height: u32, stamp_count: usize) -> f32 {
    fit_scale_for_layout(
        (width as f32, height as f32),
        scale_for_surface(width, height),
        stamp_count,
    )
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

fn clamp_asymmetric(value: f32, before: f32, after: f32, limit: f32, margin: f32) -> f32 {
    let min = margin + before;
    let max = limit - margin - after;
    if min <= max {
        value.clamp(min, max)
    } else {
        (limit + before - after) / 2.0
    }
}

fn layer_panel_height(layer_count: usize, scale: f32) -> f32 {
    (LAYER_HEADER_HEIGHT + layer_count as f32 * LAYER_ROW_HEIGHT) * scale
}

fn outer_radius_for_stamp_count(stamp_count: usize, scale: f32) -> f32 {
    let ring_count = stamp_count.div_ceil(STAMPS_PER_RING);
    if ring_count == 0 {
        return COLOR_OUTER_RADIUS * scale;
    }
    let ring_width = COLOR_OUTER_RADIUS - COLOR_INNER_RADIUS;
    let step = ring_width + STAMP_RING_GAP;
    (COLOR_INNER_RADIUS + (ring_count - 1) as f32 * step + ring_width) * scale
}

fn fit_scale_for_layout(surface_size: (f32, f32), requested_scale: f32, stamp_count: usize) -> f32 {
    let outer = outer_radius_for_stamp_count(stamp_count, 1.0);
    let radial_width = outer * 2.0;
    let radial_height = outer * 2.0;
    let color_and_dock_height = COLOR_OUTER_RADIUS * 2.0 + COMMAND_DOCK_GAP + COMMAND_HEIGHT;
    let required_width = radial_width.max(COMMAND_TOTAL_WIDTH) + VIEWPORT_MARGIN * 2.0;
    let required_height = radial_height.max(color_and_dock_height) + VIEWPORT_MARGIN * 2.0;
    let fit = (surface_size.0 / required_width).min(surface_size.1 / required_height);
    requested_scale.min(fit).max(f32::EPSILON)
}

fn fit_scale_for_layer_layout(
    surface_size: (f32, f32),
    requested_scale: f32,
    stamp_count: usize,
) -> f32 {
    let outer = outer_radius_for_stamp_count(stamp_count, 1.0);
    let required_width = outer * 2.0 + LAYER_PANEL_GAP + LAYER_PANEL_WIDTH + VIEWPORT_MARGIN * 2.0;
    let radial_height = outer * 2.0;
    let panel_height = layer_panel_height(MAX_LAYERS, 1.0);
    let color_and_dock_height = COLOR_OUTER_RADIUS * 2.0 + COMMAND_DOCK_GAP + COMMAND_HEIGHT;
    let required_height =
        radial_height.max(panel_height).max(color_and_dock_height) + VIEWPORT_MARGIN * 2.0;
    let fit = (surface_size.0 / required_width).min(surface_size.1 / required_height);
    requested_scale.min(fit).max(f32::EPSILON)
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

    fn menu_with_history(
        stamp_count: usize,
        can_undo: bool,
        can_redo: bool,
        can_clear: bool,
    ) -> RadialMenu {
        RadialMenu::new(
            1,
            (0.0, 0.0),
            (500.0, 500.0),
            (1000, 1000),
            1.0,
            stamp_count,
            (can_undo, can_redo, can_clear),
        )
    }

    fn menu(stamp_count: usize) -> RadialMenu {
        menu_with_history(stamp_count, false, false, false)
    }

    fn layer_entries(count: usize) -> Vec<RadialLayerEntry> {
        (0..count)
            .map(|index| RadialLayerEntry {
                layer_id: format!("layer-{index}"),
                name: format!("Layer {}", index + 1),
                item_count: index * 3,
            })
            .collect()
    }

    fn layer_menu(anchor_local: (f32, f32), count: usize, active: usize) -> RadialMenu {
        let anchor_screen = (
            10_000.0 + f64::from(anchor_local.0),
            -5_000.0 + f64::from(anchor_local.1),
        );
        RadialMenu::new_with_layers(
            1,
            anchor_screen,
            anchor_local,
            (1000, 800),
            1.0,
            0,
            (true, true, true),
            layer_entries(count),
            format!("layer-{active}"),
        )
    }

    fn pin_at_anchor(mut menu: RadialMenu) -> RadialMenu {
        let anchor = menu.anchor_screen;
        assert_eq!(menu.release(anchor), RadialRelease::Pin);
        menu
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

    fn screen_point(menu: &RadialMenu, offset: (f64, f64)) -> (f64, f64) {
        (
            menu.center_screen.0 + offset.0,
            menu.center_screen.1 + offset.1,
        )
    }

    fn screen_for_local(menu: &RadialMenu, local: (f32, f32)) -> (f64, f64) {
        (
            menu.center_screen.0 + f64::from(local.0 - menu.center_local.0),
            menu.center_screen.1 + f64::from(local.1 - menu.center_local.1),
        )
    }

    fn assert_screen_point_is_inside(menu: &RadialMenu, screen: (f64, f64)) {
        let local = (
            menu.center_local.0 + (screen.0 - menu.center_screen.0) as f32,
            menu.center_local.1 + (screen.1 - menu.center_screen.1) as f32,
        );
        assert!(
            local.0 >= 0.0
                && local.0 <= menu.surface_size.0
                && local.1 >= 0.0
                && local.1 <= menu.surface_size.1,
            "target {local:?} is outside {:?}",
            menu.surface_size
        );
    }

    fn viewport_anchors(width: u32, height: u32) -> [(f32, f32); 9] {
        let right = width.saturating_sub(1) as f32;
        let bottom = height.saturating_sub(1) as f32;
        let middle_x = right / 2.0;
        let middle_y = bottom / 2.0;
        [
            (0.0, 0.0),
            (middle_x, 0.0),
            (right, 0.0),
            (0.0, middle_y),
            (middle_x, middle_y),
            (right, middle_y),
            (0.0, bottom),
            (middle_x, bottom),
            (right, bottom),
        ]
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
            let mut menu = pin(menu_with_history(0, true, true, true));
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
    fn clear_availability_is_independent_from_undo_history() {
        let mut menu = menu_with_history(0, false, true, true);
        assert!(!menu.command_enabled(RadialCommand::Undo));
        assert!(menu.command_enabled(RadialCommand::Redo));
        assert!(menu.command_enabled(RadialCommand::Clear));

        assert!(menu.set_command_availability(true, false, false));
        assert!(menu.command_enabled(RadialCommand::Undo));
        assert!(!menu.command_enabled(RadialCommand::Redo));
        assert!(!menu.command_enabled(RadialCommand::Clear));
        assert!(!menu.set_command_availability(true, false, false));
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
            (true, true, true),
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
            let category = screen_point(&menu, point(STAMP_TOOL_INDEX, TOOL_COUNT, 72.0));
            menu.update(category);
            let (inner, outer) = menu.stamp_ring_radii(ring).unwrap();
            let target = screen_point(
                &menu,
                point(slot, STAMPS_PER_RING, f64::from((inner + outer) / 2.0)),
            );
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
    fn every_visible_item_stays_hittable_at_viewport_edges_and_corners() {
        for (width, height) in [(640, 480), (1920, 1080), (7680, 4320)] {
            for stamp_count in [0, 1, 8, 9, 32] {
                for anchor_local in viewport_anchors(width, height) {
                    let anchor_screen = (
                        10_000.0 + f64::from(anchor_local.0),
                        -5_000.0 + f64::from(anchor_local.1),
                    );
                    let mut menu = RadialMenu::new(
                        1,
                        anchor_screen,
                        anchor_local,
                        (width, height),
                        scale_for_surface(width, height),
                        stamp_count,
                        (true, true, true),
                    );
                    assert!(
                        menu.layout_within_surface(),
                        "invalid layout for {width}x{height}, {stamp_count} stamps, anchor {anchor_local:?}: {menu:?}"
                    );

                    // 動かさずに離すpinは、描画中心を補正した場合もanchor基準で維持する。
                    assert_eq!(menu.release(anchor_screen), RadialRelease::Pin);

                    let center = menu.center_screen;
                    assert_screen_point_is_inside(&menu, center);
                    menu.update(center);
                    assert_eq!(menu.highlighted(), Some(RadialSelection::StandardMenu));

                    let tool_radius =
                        f64::from((menu.tool_inner_radius() + menu.tool_outer_radius()) / 2.0);
                    for index in 0..DRAW_TOOL_COUNT {
                        let target = screen_point(&menu, point(index, TOOL_COUNT, tool_radius));
                        assert_screen_point_is_inside(&menu, target);
                        menu.update(target);
                        assert_eq!(menu.highlighted(), Some(RadialSelection::Tool(index)));
                    }

                    // centerへ戻すとstamp modeを解除し、色・command dockを検証できる。
                    menu.update(center);
                    let color_radius =
                        f64::from((menu.color_inner_radius() + menu.color_outer_radius()) / 2.0);
                    for index in 0..COLOR_COUNT {
                        let target = screen_point(&menu, point(index, COLOR_COUNT, color_radius));
                        assert_screen_point_is_inside(&menu, target);
                        menu.update(target);
                        assert_eq!(menu.highlighted(), Some(RadialSelection::Color(index)));
                    }

                    menu.update(center);
                    for (command, rect) in menu.command_buttons() {
                        let target = screen_for_local(&menu, rect.center());
                        assert_screen_point_is_inside(&menu, target);
                        menu.update(target);
                        assert_eq!(menu.highlighted(), Some(RadialSelection::Command(command)));
                    }

                    if stamp_count == 0 {
                        continue;
                    }
                    let category =
                        screen_point(&menu, point(STAMP_TOOL_INDEX, TOOL_COUNT, tool_radius));
                    assert_screen_point_is_inside(&menu, category);
                    menu.update(category);
                    assert_eq!(menu.highlighted(), Some(RadialSelection::StampCategory));
                    assert!(menu.stamp_mode());

                    for index in 0..stamp_count {
                        let ring = index / STAMPS_PER_RING;
                        let slot = index % STAMPS_PER_RING;
                        let (inner, outer) = menu.stamp_ring_radii(ring).unwrap();
                        let target = screen_point(
                            &menu,
                            point(slot, STAMPS_PER_RING, f64::from((inner + outer) / 2.0)),
                        );
                        assert_screen_point_is_inside(&menu, target);
                        menu.update(target);
                        assert_eq!(menu.highlighted(), Some(RadialSelection::Stamp(index)));
                    }
                }
            }
        }
    }

    #[test]
    fn adjusted_center_preserves_hold_pin_legacy_menu_and_pinned_clicks() {
        let anchor = (2500.0, -1200.0);
        let mut menu = RadialMenu::new(
            1,
            anchor,
            (0.0, 0.0),
            (640, 480),
            1.6,
            32,
            (true, true, true),
        );
        assert_ne!(menu.center_local(), (0.0, 0.0));
        assert!(menu.scale() < 1.6);
        assert!(menu.layout_within_surface());
        assert_eq!(menu.release(anchor), RadialRelease::Pin);

        assert!(menu.begin_click(2));
        assert_eq!(menu.release(menu.center_screen), RadialRelease::LegacyMenu);

        let mut menu = RadialMenu::new(
            1,
            anchor,
            (0.0, 0.0),
            (640, 480),
            1.6,
            32,
            (true, true, true),
        );
        assert_eq!(menu.release(anchor), RadialRelease::Pin);
        assert!(menu.begin_click(2));
        let target = screen_point(
            &menu,
            point(
                2,
                TOOL_COUNT,
                f64::from((menu.tool_inner_radius() + menu.tool_outer_radius()) / 2.0),
            ),
        );
        assert_eq!(
            menu.release(target),
            RadialRelease::Action {
                action: MenuAction::SelectTool(DrawTool::Marker),
                keep_open: false,
            }
        );
    }

    #[test]
    fn returning_to_the_adjusted_visual_center_pins_the_hold_menu() {
        let anchor = (0.0, 0.0);
        let mut menu = RadialMenu::new(
            1,
            anchor,
            (0.0, 0.0),
            (640, 480),
            scale_for_surface(640, 480),
            32,
            (false, false, false),
        );
        let tool = screen_point(
            &menu,
            point(
                1,
                TOOL_COUNT,
                f64::from((menu.tool_inner_radius() + menu.tool_outer_radius()) / 2.0),
            ),
        );
        menu.update(tool);
        assert_eq!(menu.release(menu.center_screen), RadialRelease::Pin);
    }

    #[test]
    fn viewport_fitting_accepts_different_requested_dpi_scales() {
        for (width, height) in [(640, 480), (1920, 1080), (7680, 4320)] {
            for requested_scale in [0.75, 1.0, 1.25, 1.5, 1.6, 2.0] {
                for anchor in [(0.0, 0.0), (width as f32 - 1.0, height as f32 - 1.0)] {
                    let menu = RadialMenu::new(
                        1,
                        (f64::from(anchor.0), f64::from(anchor.1)),
                        anchor,
                        (width, height),
                        requested_scale,
                        32,
                        (true, true, true),
                    );
                    assert!(menu.scale() <= requested_scale);
                    assert!(menu.layout_within_surface());
                }
            }
        }
    }

    #[test]
    fn layer_panel_prefers_right_and_falls_back_left_near_the_right_edge() {
        let right = layer_menu((300.0, 400.0), 3, 1);
        assert!(right.panel_on_right());
        assert!(right.layer_panel_rect().left > right.center_local().0);
        assert!(right.layout_within_surface());

        let left = layer_menu((999.0, 400.0), 3, 1);
        assert!(!left.panel_on_right());
        assert!(left.layer_panel_rect().right < left.center_local().0);
        assert!(left.layout_within_surface());
    }

    #[test]
    fn eight_layer_panel_with_four_stamp_rings_fits_640_by_480() {
        for anchor in viewport_anchors(640, 480) {
            let menu = RadialMenu::new_with_layers(
                1,
                (f64::from(anchor.0), f64::from(anchor.1)),
                anchor,
                (640, 480),
                1.6,
                32,
                (true, true, true),
                layer_entries(MAX_LAYERS),
                "layer-7".into(),
            );
            assert!(menu.layout_within_surface(), "anchor={anchor:?}: {menu:?}");
            assert_eq!(menu.layer_rows().len(), MAX_LAYERS);
        }
    }

    #[test]
    fn pinned_layer_rows_add_and_delete_are_directly_hittable() {
        let mut menu = pin_at_anchor(layer_menu((500.0, 400.0), 3, 1));
        assert_eq!(
            menu.layer_rows()
                .into_iter()
                .map(|(index, _)| index)
                .collect::<Vec<_>>(),
            vec![2, 1, 0]
        );
        for (index, row) in menu.layer_rows() {
            assert!(menu.begin_click(10 + index as u32));
            let target = screen_for_local(&menu, row.center());
            menu.update(target);
            assert_eq!(menu.highlighted(), Some(RadialSelection::Layer(index)));
            assert_eq!(
                menu.release(target),
                RadialRelease::Action {
                    action: MenuAction::SelectLayer(format!("layer-{index}")),
                    keep_open: true,
                }
            );
        }

        assert!(menu.begin_click(20));
        let add = screen_for_local(&menu, menu.layer_add_button().center());
        menu.update(add);
        assert_eq!(menu.highlighted(), Some(RadialSelection::LayerAdd));
        assert_eq!(
            menu.release(add),
            RadialRelease::Action {
                action: MenuAction::AddLayer,
                keep_open: true,
            }
        );

        assert!(menu.begin_click(21));
        let delete = screen_for_local(&menu, menu.layer_delete_button().unwrap().center());
        menu.update(delete);
        assert_eq!(menu.highlighted(), Some(RadialSelection::LayerDelete));
        assert_eq!(
            menu.release(delete),
            RadialRelease::Action {
                action: MenuAction::DeleteLayer("layer-1".into()),
                keep_open: true,
            },
            "confirmation No/Yes can keep the pinned layer panel open"
        );
    }

    #[test]
    fn layer_row_works_for_hold_and_pinned_left_or_right_clicks() {
        let mut hold = layer_menu((500.0, 400.0), 2, 0);
        let row = hold
            .layer_rows()
            .into_iter()
            .find(|(index, _)| *index == 1)
            .unwrap()
            .1;
        let target = screen_for_local(&hold, row.center());
        hold.update(target);
        assert_eq!(
            hold.release(target),
            RadialRelease::Action {
                action: MenuAction::SelectLayer("layer-1".into()),
                keep_open: false,
            }
        );

        for pointer_id in [2, 3] {
            let mut pinned = pin_at_anchor(layer_menu((500.0, 400.0), 2, 0));
            assert!(pinned.begin_click(pointer_id));
            let target = screen_for_local(&pinned, row.center());
            pinned.update(target);
            assert_eq!(
                pinned.release(target),
                RadialRelease::Action {
                    action: MenuAction::SelectLayer("layer-1".into()),
                    keep_open: true,
                }
            );
        }
    }

    #[test]
    fn layer_limits_disable_add_and_last_layer_delete() {
        let mut maximum = pin_at_anchor(layer_menu((500.0, 400.0), MAX_LAYERS, 7));
        assert!(maximum.begin_click(2));
        let add = screen_for_local(&maximum, maximum.layer_add_button().center());
        maximum.update(add);
        assert_eq!(maximum.highlighted(), Some(RadialSelection::LayerAdd));
        assert_eq!(maximum.release(add), RadialRelease::StayOpen);

        let mut last = pin_at_anchor(layer_menu((500.0, 400.0), 1, 0));
        assert!(last.begin_click(2));
        let delete = screen_for_local(&last, last.layer_delete_button().unwrap().center());
        last.update(delete);
        assert_eq!(last.highlighted(), Some(RadialSelection::Layer(0)));
        assert_eq!(
            last.release(delete),
            RadialRelease::Action {
                action: MenuAction::SelectLayer("layer-0".into()),
                keep_open: true,
            }
        );
    }

    #[test]
    fn set_layers_refreshes_active_layer_names_and_counts() {
        let mut menu = layer_menu((500.0, 400.0), 2, 0);
        let refreshed = vec![
            RadialLayerEntry {
                layer_id: "layer-0".into(),
                name: "Background".into(),
                item_count: 42,
            },
            RadialLayerEntry {
                layer_id: "layer-1".into(),
                name: "Foreground".into(),
                item_count: 7,
            },
            RadialLayerEntry {
                layer_id: "layer-2".into(),
                name: "Notes".into(),
                item_count: 1,
            },
        ];
        assert!(menu.set_layers(refreshed.clone(), "layer-2".into()));
        assert_eq!(menu.layers(), refreshed);
        assert_eq!(menu.active_layer_id(), "layer-2");
        assert!(!menu.set_layers(refreshed, "layer-2".into()));
    }

    #[test]
    fn surface_scale_is_bounded() {
        assert_eq!(scale_for_surface(640, 480), 0.85);
        assert_eq!(scale_for_surface(1920, 1080), 1.0);
        assert_eq!(scale_for_surface(7680, 4320), 1.6);
        assert!(scale_for_menu(640, 480, 32) < scale_for_surface(640, 480));
        assert_eq!(scale_for_menu(7680, 4320, 32), 1.6);
    }
}
