//! DirectComposition + Direct2D による透明オーバーレイ描画 (docs/painter.md)。
//!
//! - swapchain (premultiplied alpha) を DComp visual に載せ、WS_EX_NOREDIRECTIONBITMAP
//!   のウィンドウへ GPU 合成する
//! - 確定ストロークは baked ビットマップに焼き込み、フレームでは
//!   baked + 描画中ストロークのみを描く (client の layers.ts と同じ構造)
//! - 幾何は engine::geometry (docs/protocol.md) に従う

use std::collections::HashMap;

use anyhow::{Context, Result};
use tracing::warn;
use windows::core::{w, Interface};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED,
    D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D1_PIXEL_FORMAT,
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1Brush, ID2D1DeviceContext, ID2D1Factory1,
    ID2D1PathGeometry, ID2D1SolidColorBrush, ID2D1StrokeStyle1, D2D1_ARC_SEGMENT,
    D2D1_ARC_SIZE_SMALL, D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_NONE,
    D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_CAP_STYLE_ROUND,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_LINE_JOIN_ROUND, D2D1_PRIMITIVE_BLEND_COPY,
    D2D1_PRIMITIVE_BLEND_SOURCE_OVER, D2D1_QUADRATIC_BEZIER_SEGMENT, D2D1_ROUNDED_RECT,
    D2D1_STROKE_STYLE_PROPERTIES1, D2D1_SWEEP_DIRECTION_CLOCKWISE,
    D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain1,
    DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows_numerics::Vector2;

use crate::config::{self, StampConfig};
use crate::engine::content_rect::Rect;
use crate::engine::geometry::{dot, full_segments, Segment};
use crate::protocol::{
    Brush, CanvasItem, LineStyle, ShapeItem, ShapeKind, StampItem, Stroke, Tool,
};
use crate::win::menu::{DrawTool, COLORS};
use crate::win::radial_menu::{
    self, RadialCommand, RadialMenu, RadialSelection, COLOR_COUNT, STAMPS_PER_RING,
    STAMP_TOOL_INDEX, TOOL_COUNT,
};

pub struct Renderer {
    factory: ID2D1Factory1,
    dc: ID2D1DeviceContext,
    swapchain: IDXGISwapChain1,
    target: ID2D1Bitmap1,
    baked: ID2D1Bitmap1,
    stamp_bitmaps: HashMap<String, ID2D1Bitmap1>,
    stroke_style: ID2D1StrokeStyle1,
    radial_text: IDWriteTextFormat,
    /// content rect (ウィンドウローカル座標)。正規化座標をこの矩形に展開する
    content: Rect,
    // DComp オブジェクトは drop されると合成が消えるため保持し続ける
    _dcomp_device: IDCompositionDevice,
    _dcomp_target: IDCompositionTarget,
    _dcomp_visual: IDCompositionVisual,
}

impl Renderer {
    pub fn new(
        hwnd: HWND,
        width: u32,
        height: u32,
        content: Rect,
        stamps: &[StampConfig],
    ) -> Result<Self> {
        unsafe {
            // D3D11 デバイス (BGRA サポートは D2D 連携に必須)
            let mut d3d_device: Option<ID3D11Device> = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d_device),
                None,
                None,
            )
            .context("D3D11CreateDevice")?;
            let d3d_device = d3d_device.context("no d3d device")?;
            let dxgi_device: IDXGIDevice = d3d_device.cast()?;

            // Composition 用 swapchain (premultiplied alpha)
            let dxgi_factory: IDXGIFactory2 = CreateDXGIFactory1()?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                ..Default::default()
            };
            let swapchain = dxgi_factory.CreateSwapChainForComposition(&d3d_device, &desc, None)?;

            // D2D デバイスコンテキスト
            let factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2d_device = factory.CreateDevice(&dxgi_device)?;
            let dc = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            // 透明swapchain上ではClearTypeではなくアルファ対応のグレースケールを使う。
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);

            let bitmap_props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: core::mem::ManuallyDrop::new(None),
            };
            let surface: IDXGISurface = swapchain.GetBuffer(0)?;
            let target = dc.CreateBitmapFromDxgiSurface(&surface, Some(&bitmap_props))?;

            // 確定ストロークの焼き込み先
            let baked_props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET,
                colorContext: core::mem::ManuallyDrop::new(None),
            };
            let baked = dc.CreateBitmap(D2D_SIZE_U { width, height }, None, 0, &baked_props)?;
            let stamp_bitmaps = load_stamp_bitmaps(&dc, stamps);

            // round cap / round join (docs/protocol.md)
            let stroke_style = factory.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES1 {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    dashCap: D2D1_CAP_STYLE_ROUND,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    miterLimit: 10.0,
                    ..Default::default()
                },
                None,
            )?;

            let dwrite_factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let radial_scale = radial_menu::scale_for_surface(width, height);
            let radial_text = dwrite_factory.CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                13.0 * radial_scale,
                w!("ja-JP"),
            )?;
            radial_text.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            radial_text.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;

            // DComp: swapchain を visual としてウィンドウに合成
            let dcomp_device: IDCompositionDevice = DCompositionCreateDevice(&dxgi_device)?;
            let dcomp_target = dcomp_device.CreateTargetForHwnd(hwnd, true)?;
            let dcomp_visual = dcomp_device.CreateVisual()?;
            dcomp_visual.SetContent(&swapchain)?;
            dcomp_target.SetRoot(&dcomp_visual)?;
            dcomp_device.Commit()?;

            Ok(Self {
                factory,
                dc,
                swapchain,
                target,
                baked,
                stamp_bitmaps,
                stroke_style,
                radial_text,
                content,
                _dcomp_device: dcomp_device,
                _dcomp_target: dcomp_target,
                _dcomp_visual: dcomp_visual,
            })
        }
    }

    /// 確定 CanvasItem 一覧から baked を再構築する。
    pub fn rebuild_baked(&mut self, items: &[CanvasItem]) -> Result<()> {
        self.rebuild_baked_excluding(items, None)
    }

    /// 選択中スタンプだけをフレーム側へ分離して baked を再構築する。
    pub fn rebuild_baked_excluding(
        &mut self,
        items: &[CanvasItem],
        excluded_item_id: Option<&str>,
    ) -> Result<()> {
        unsafe {
            self.dc.SetTarget(&self.baked);
            self.dc.BeginDraw();
            self.dc.Clear(Some(&transparent()));
            for item in items
                .iter()
                .filter(|item| item.is_done() && excluded_item_id != Some(item.item_id()))
            {
                self.draw_item(item)?;
            }
            self.dc.EndDraw(None, None)?;
        }
        Ok(())
    }

    /// 新しく確定した1項目だけをbakedへ追記する。
    pub fn bake_item(&mut self, item: &CanvasItem) -> Result<()> {
        unsafe {
            self.dc.SetTarget(&self.baked);
            self.dc.BeginDraw();
        }
        let draw_result = self.draw_item(item);
        let end_result = unsafe { self.dc.EndDraw(None, None) };
        draw_result?;
        end_result?;
        Ok(())
    }

    /// 空フレームを提示してオーバーレイ表示を消す (パススルー復帰時)。
    /// baked ビットマップは保持したままなので、次の描画モードで再表示される
    pub fn clear_frame(&mut self) -> Result<()> {
        unsafe {
            self.dc.SetTarget(&self.target);
            self.dc.BeginDraw();
            self.dc.Clear(Some(&transparent()));
            self.dc.EndDraw(None, None)?;
            self.swapchain.Present(1, Default::default()).ok()?;
        }
        Ok(())
    }

    /// 1 フレーム描画: baked + 描画中項目 + 描画UI。
    pub fn draw_frame(
        &mut self,
        items: &[CanvasItem],
        draw_mode: bool,
        selected_stamp: Option<&StampItem>,
        radial: Option<(&RadialMenu, &DrawTool, &str, &[StampConfig])>,
    ) -> Result<()> {
        unsafe {
            self.dc.SetTarget(&self.target);
            self.dc.BeginDraw();
            self.dc.Clear(Some(&transparent()));
            self.dc.DrawBitmap(
                &self.baked,
                None,
                1.0,
                D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                None,
                None,
            );
            for item in items.iter().filter(|item| !item.is_done()) {
                self.draw_item(item)?;
            }
            if let Some(stamp) = selected_stamp {
                self.draw_stamp(stamp)?;
                self.draw_stamp_selection(stamp)?;
            }
            if draw_mode {
                self.draw_mode_border()?;
            }
            if let Some((menu, tool, color, stamps)) = radial {
                self.draw_radial_menu(menu, tool, color, stamps)?;
            }
            self.dc.EndDraw(None, None)?;
            self.swapchain.Present(1, Default::default()).ok()?;
        }
        Ok(())
    }

    fn draw_item(&self, item: &CanvasItem) -> Result<()> {
        match item {
            CanvasItem::Stroke { stroke } => self.draw_stroke(stroke),
            CanvasItem::Shape { shape } => self.draw_shape(shape),
            CanvasItem::Stamp { stamp } => self.draw_stamp(stamp),
        }
    }

    fn draw_stroke(&self, stroke: &Stroke) -> Result<()> {
        // eraser は COPY ブレンドで透明色を書き込み、既存ピクセルを消す。
        // marker (半透明) はアルファ直描きのため自己交差部がわずかに濃くなる
        // (overlay はストローク単位レイヤー合成)。厳密な一致は M3 で対応
        let eraser = stroke.brush.tool == Tool::Eraser;
        let brush = if eraser {
            unsafe {
                self.dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_COPY);
                self.dc.CreateSolidColorBrush(&transparent(), None)?
            }
        } else {
            self.solid_brush(&stroke.brush)?
        };
        let result = self.draw_stroke_shape(stroke, &brush);
        if eraser {
            unsafe {
                self.dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_SOURCE_OVER);
            }
        }
        result
    }

    fn draw_stroke_shape(&self, stroke: &Stroke, brush: &ID2D1SolidColorBrush) -> Result<()> {
        if let Some((center, radius)) = dot(
            &stroke.pts,
            self.content.width,
            self.content.height,
            &stroke.brush,
        ) {
            unsafe {
                self.dc.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: self.to_local(center.x, center.y),
                        radiusX: radius as f32,
                        radiusY: radius as f32,
                    },
                    brush,
                );
            }
            return Ok(());
        }

        let segments = full_segments(
            &stroke.pts,
            self.content.width,
            self.content.height,
            &stroke.brush,
        );
        for segment in &segments {
            self.draw_segment(segment, brush)?;
        }
        Ok(())
    }

    fn draw_segment(&self, segment: &Segment, brush: &ID2D1SolidColorBrush) -> Result<()> {
        unsafe {
            let geometry = self.factory.CreatePathGeometry()?;
            let sink = geometry.Open()?;
            sink.BeginFigure(
                self.to_local(segment.from.x, segment.from.y),
                D2D1_FIGURE_BEGIN_HOLLOW,
            );
            sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT {
                point1: self.to_local(segment.ctrl.x, segment.ctrl.y),
                point2: self.to_local(segment.to.x, segment.to.y),
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close()?;
            self.dc
                .DrawGeometry(&geometry, brush, segment.width as f32, &self.stroke_style);
        }
        Ok(())
    }

    fn draw_shape(&self, shape: &ShapeItem) -> Result<()> {
        let brush = self.line_brush(&shape.style)?;
        let width = (shape.style.width_n * self.content.height) as f32;
        let start = self.normalized_to_local(shape.start);
        let end = self.normalized_to_local(shape.end);

        unsafe {
            match shape.shape {
                ShapeKind::Line => {
                    self.dc
                        .DrawLine(start, end, &brush, width, &self.stroke_style);
                }
                ShapeKind::Arrow => {
                    self.dc
                        .DrawLine(start, end, &brush, width, &self.stroke_style);
                    let dx = f64::from(end.X - start.X);
                    let dy = f64::from(end.Y - start.Y);
                    let length = dx.hypot(dy);
                    if length > 0.0 {
                        let angle = dy.atan2(dx);
                        let head_length = (length * 0.4)
                            .min((f64::from(width) * 4.0).max(self.content.height * 0.02));
                        let spread = std::f64::consts::PI / 6.0;
                        for head_angle in [angle - spread, angle + spread] {
                            let point = Vector2 {
                                X: end.X - (head_length * head_angle.cos()) as f32,
                                Y: end.Y - (head_length * head_angle.sin()) as f32,
                            };
                            self.dc
                                .DrawLine(end, point, &brush, width, &self.stroke_style);
                        }
                    }
                }
                ShapeKind::Rectangle => {
                    let rect = D2D_RECT_F {
                        left: start.X.min(end.X),
                        top: start.Y.min(end.Y),
                        right: start.X.max(end.X),
                        bottom: start.Y.max(end.Y),
                    };
                    self.dc
                        .DrawRectangle(&rect, &brush, width, &self.stroke_style);
                }
                ShapeKind::Ellipse => {
                    self.dc.DrawEllipse(
                        &D2D1_ELLIPSE {
                            point: Vector2 {
                                X: (start.X + end.X) / 2.0,
                                Y: (start.Y + end.Y) / 2.0,
                            },
                            radiusX: (end.X - start.X).abs() / 2.0,
                            radiusY: (end.Y - start.Y).abs() / 2.0,
                        },
                        &brush,
                        width,
                        &self.stroke_style,
                    );
                }
            }
        }
        Ok(())
    }

    fn draw_stamp(&self, stamp: &StampItem) -> Result<()> {
        let Some(bitmap) = self.stamp_bitmaps.get(&stamp.stamp_id) else {
            return Ok(());
        };
        let destination = self.stamp_rect(stamp);
        unsafe {
            self.dc.DrawBitmap(
                bitmap,
                Some(&destination),
                stamp.opacity as f32,
                D2D1_INTERPOLATION_MODE_LINEAR,
                None,
                None,
            );
        }
        Ok(())
    }

    fn stamp_rect(&self, stamp: &StampItem) -> D2D_RECT_F {
        let center = self.normalized_to_local(stamp.center);
        let width = (stamp.width_n * self.content.width) as f32;
        let height = (stamp.height_n * self.content.height) as f32;
        D2D_RECT_F {
            left: center.X - width / 2.0,
            top: center.Y - height / 2.0,
            right: center.X + width / 2.0,
            bottom: center.Y + height / 2.0,
        }
    }

    fn draw_stamp_selection(&self, stamp: &StampItem) -> Result<()> {
        let rect = self.stamp_rect(stamp);
        let shadow = unsafe {
            self.dc.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 0.015,
                    g: 0.025,
                    b: 0.04,
                    a: 0.95,
                },
                None,
            )?
        };
        let accent = unsafe {
            self.dc.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 0.12,
                    g: 0.72,
                    b: 0.98,
                    a: 1.0,
                },
                None,
            )?
        };
        unsafe {
            self.dc
                .DrawRectangle(&rect, &shadow, 5.0, &self.stroke_style);
            self.dc
                .DrawRectangle(&rect, &accent, 2.0, &self.stroke_style);
        }

        let handle = (self.content.height as f32 * 0.007).clamp(5.0, 9.0);
        for (x, y) in [
            (rect.left, rect.top),
            (rect.right, rect.top),
            (rect.right, rect.bottom),
            (rect.left, rect.bottom),
        ] {
            let outer = D2D_RECT_F {
                left: x - handle,
                top: y - handle,
                right: x + handle,
                bottom: y + handle,
            };
            let inner = D2D_RECT_F {
                left: x - handle + 2.0,
                top: y - handle + 2.0,
                right: x + handle - 2.0,
                bottom: y + handle - 2.0,
            };
            unsafe {
                self.dc.FillRectangle(&outer, &shadow);
                self.dc.FillRectangle(&inner, &accent);
            }
        }
        Ok(())
    }

    fn draw_radial_menu(
        &self,
        menu: &RadialMenu,
        current_tool: &DrawTool,
        current_color: &str,
        stamps: &[StampConfig],
    ) -> Result<()> {
        debug_assert_eq!(menu.stamp_count(), stamps.len());
        let center = menu.center_local();
        let outline = unsafe {
            self.dc.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 0.02,
                    g: 0.03,
                    b: 0.05,
                    a: 0.9,
                },
                None,
            )?
        };
        let text = unsafe {
            self.dc.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
                None,
            )?
        };

        for index in 0..TOOL_COUNT {
            let selection = if index == STAMP_TOOL_INDEX {
                RadialSelection::StampCategory
            } else {
                RadialSelection::Tool(index)
            };
            let enabled = index != STAMP_TOOL_INDEX || !stamps.is_empty();
            let highlighted = enabled
                && (menu.highlighted() == Some(selection)
                    || (index == STAMP_TOOL_INDEX && menu.stamp_mode()));
            let current = if index == STAMP_TOOL_INDEX {
                matches!(current_tool, DrawTool::Stamp(_))
            } else {
                radial_menu::tool_at(index)
                    .as_ref()
                    .is_some_and(|tool| tool == current_tool)
            };
            let fill_color = if highlighted {
                D2D1_COLOR_F {
                    r: 0.12,
                    g: 0.52,
                    b: 0.76,
                    a: 0.98,
                }
            } else if current {
                D2D1_COLOR_F {
                    r: 0.08,
                    g: 0.26,
                    b: 0.36,
                    a: 0.96,
                }
            } else if !enabled {
                D2D1_COLOR_F {
                    r: 0.035,
                    g: 0.04,
                    b: 0.05,
                    a: 0.78,
                }
            } else {
                D2D1_COLOR_F {
                    r: 0.055,
                    g: 0.065,
                    b: 0.085,
                    a: 0.94,
                }
            };
            let fill = unsafe { self.dc.CreateSolidColorBrush(&fill_color, None)? };
            let (start, end) = radial_menu::sector_angles(index, TOOL_COUNT);
            let geometry = self.annular_wedge(
                center,
                menu.tool_inner_radius(),
                menu.tool_outer_radius(),
                start,
                end,
            )?;
            unsafe {
                self.dc.FillGeometry(&geometry, &fill, None::<&ID2D1Brush>);
                self.dc.DrawGeometry(
                    &geometry,
                    &outline,
                    if highlighted { 3.0 } else { 1.5 },
                    &self.stroke_style,
                );
            }

            if let Some(label) = radial_menu::tool_label(index) {
                let angle = radial_menu::sector_center_angle(index, TOOL_COUNT);
                let radius = (menu.tool_inner_radius() + menu.tool_outer_radius()) / 2.0;
                let position = (
                    center.0 + radius * angle.cos(),
                    center.1 + radius * angle.sin(),
                );
                self.draw_centered_text(
                    label,
                    position,
                    72.0 * menu.scale(),
                    32.0 * menu.scale(),
                    &text,
                );
            }
        }

        if menu.stamp_mode() {
            self.draw_radial_stamp_rings(menu, current_tool, stamps, &outline, &text)?;
        } else {
            self.draw_radial_color_ring(menu, current_color, &outline)?;
            self.draw_radial_commands(menu, &text)?;
        }

        let center_radius = menu.tool_inner_radius() - 5.0 * menu.scale();
        let center_highlighted = menu.highlighted() == Some(RadialSelection::StandardMenu);
        let center_color = if center_highlighted {
            D2D1_COLOR_F {
                r: 0.1,
                g: 0.38,
                b: 0.56,
                a: 0.98,
            }
        } else {
            D2D1_COLOR_F {
                r: 0.055,
                g: 0.065,
                b: 0.085,
                a: 0.96,
            }
        };
        let center_fill = unsafe { self.dc.CreateSolidColorBrush(&center_color, None)? };
        unsafe {
            self.dc.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 {
                        X: center.0,
                        Y: center.1,
                    },
                    radiusX: center_radius,
                    radiusY: center_radius,
                },
                &center_fill,
            );
            self.dc.DrawEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 {
                        X: center.0,
                        Y: center.1,
                    },
                    radiusX: center_radius,
                    radiusY: center_radius,
                },
                &outline,
                if center_highlighted { 3.0 } else { 1.5 },
                &self.stroke_style,
            );
        }
        let center_label = match menu.highlighted() {
            Some(RadialSelection::StandardMenu) => "標準\nメニュー",
            Some(RadialSelection::Tool(index)) => radial_menu::tool_label(index).unwrap_or(""),
            Some(RadialSelection::StampCategory) if stamps.is_empty() => "スタンプ\n未登録",
            Some(RadialSelection::StampCategory) => "スタンプ\n外へ",
            Some(RadialSelection::Stamp(index)) => {
                stamps.get(index).map_or("", |stamp| stamp.name.as_str())
            }
            Some(RadialSelection::Color(index)) => COLORS.get(index).map_or("", |(name, _)| name),
            Some(RadialSelection::Command(RadialCommand::Undo)) => "元に\n戻す",
            Some(RadialSelection::Command(RadialCommand::Redo)) => "やり\n直す",
            Some(RadialSelection::Command(RadialCommand::Clear)) => "全消去",
            None if menu.stamp_mode() => "スタンプ\n選択",
            None => "標準\nメニュー",
        };
        self.draw_centered_text(
            center_label,
            center,
            center_radius * 1.8,
            center_radius * 1.8,
            &text,
        );
        Ok(())
    }

    fn draw_radial_color_ring(
        &self,
        menu: &RadialMenu,
        current_color: &str,
        outline: &ID2D1SolidColorBrush,
    ) -> Result<()> {
        let center = menu.center_local();
        for (index, (_, hex)) in COLORS.iter().enumerate() {
            let selection = RadialSelection::Color(index);
            let highlighted = menu.highlighted() == Some(selection);
            let (r, g, b) = parse_color(hex);
            let fill = unsafe {
                self.dc
                    .CreateSolidColorBrush(&D2D1_COLOR_F { r, g, b, a: 0.96 }, None)?
            };
            let (start, end) = radial_menu::sector_angles(index, COLOR_COUNT);
            let geometry = self.annular_wedge(
                center,
                menu.color_inner_radius(),
                menu.color_outer_radius(),
                start,
                end,
            )?;
            unsafe {
                self.dc.FillGeometry(&geometry, &fill, None::<&ID2D1Brush>);
                self.dc
                    .DrawGeometry(&geometry, outline, 1.5, &self.stroke_style);
            }

            let current = hex.eq_ignore_ascii_case(current_color);
            self.draw_radial_indicator(&geometry, highlighted, current)?;
        }
        Ok(())
    }

    fn draw_radial_stamp_rings(
        &self,
        menu: &RadialMenu,
        current_tool: &DrawTool,
        stamps: &[StampConfig],
        outline: &ID2D1SolidColorBrush,
        text: &ID2D1SolidColorBrush,
    ) -> Result<()> {
        let center = menu.center_local();
        for ring in 0..menu.stamp_ring_count() {
            let Some((inner, outer)) = menu.stamp_ring_radii(ring) else {
                continue;
            };
            for slot in 0..menu.stamp_ring_item_count(ring) {
                let index = ring * STAMPS_PER_RING + slot;
                let Some(stamp) = stamps.get(index) else {
                    continue;
                };
                let selection = RadialSelection::Stamp(index);
                let highlighted = menu.highlighted() == Some(selection);
                let current = match current_tool {
                    DrawTool::Stamp(id) => id == &stamp.id,
                    _ => false,
                };
                let fill_color = if highlighted {
                    D2D1_COLOR_F {
                        r: 0.12,
                        g: 0.52,
                        b: 0.76,
                        a: 0.98,
                    }
                } else if current {
                    D2D1_COLOR_F {
                        r: 0.08,
                        g: 0.26,
                        b: 0.36,
                        a: 0.96,
                    }
                } else {
                    D2D1_COLOR_F {
                        r: 0.055,
                        g: 0.065,
                        b: 0.085,
                        a: 0.94,
                    }
                };
                let fill = unsafe { self.dc.CreateSolidColorBrush(&fill_color, None)? };
                let (start, end) = radial_menu::sector_angles(slot, STAMPS_PER_RING);
                let geometry = self.annular_wedge(center, inner, outer, start, end)?;
                unsafe {
                    self.dc.FillGeometry(&geometry, &fill, None::<&ID2D1Brush>);
                    self.dc
                        .DrawGeometry(&geometry, outline, 1.5, &self.stroke_style);
                }

                let angle = radial_menu::sector_center_angle(slot, STAMPS_PER_RING);
                let radius = (inner + outer) / 2.0;
                let position = (
                    center.0 + radius * angle.cos(),
                    center.1 + radius * angle.sin(),
                );
                if let Some(bitmap) = self.stamp_bitmaps.get(&stamp.id) {
                    let max_size = 42.0 * menu.scale();
                    let aspect = stamp.width_px as f32 / stamp.height_px as f32;
                    let (width, height) = if aspect >= 1.0 {
                        (max_size, max_size / aspect)
                    } else {
                        (max_size * aspect, max_size)
                    };
                    let destination = D2D_RECT_F {
                        left: position.0 - width / 2.0,
                        top: position.1 - height / 2.0,
                        right: position.0 + width / 2.0,
                        bottom: position.1 + height / 2.0,
                    };
                    unsafe {
                        self.dc.DrawBitmap(
                            bitmap,
                            Some(&destination),
                            1.0,
                            D2D1_INTERPOLATION_MODE_LINEAR,
                            None,
                            None,
                        );
                    }
                } else {
                    self.draw_centered_text(
                        &stamp.name,
                        position,
                        68.0 * menu.scale(),
                        36.0 * menu.scale(),
                        text,
                    );
                }
                self.draw_radial_indicator(&geometry, highlighted, current)?;
            }
        }
        Ok(())
    }

    fn draw_radial_commands(&self, menu: &RadialMenu, text: &ID2D1SolidColorBrush) -> Result<()> {
        let muted_text = unsafe {
            self.dc.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 0.55,
                    g: 0.58,
                    b: 0.64,
                    a: 0.72,
                },
                None,
            )?
        };
        for (command, rect) in menu.command_buttons() {
            let enabled = menu.command_enabled(command);
            let highlighted =
                enabled && menu.highlighted() == Some(RadialSelection::Command(command));
            let fill_color = match (command, highlighted, enabled) {
                (RadialCommand::Undo, true, _) => D2D1_COLOR_F {
                    r: 0.1,
                    g: 0.45,
                    b: 0.7,
                    a: 0.98,
                },
                (RadialCommand::Redo, true, _) => D2D1_COLOR_F {
                    r: 0.08,
                    g: 0.52,
                    b: 0.4,
                    a: 0.98,
                },
                (RadialCommand::Clear, true, _) => D2D1_COLOR_F {
                    r: 0.72,
                    g: 0.12,
                    b: 0.17,
                    a: 0.98,
                },
                (RadialCommand::Clear, false, true) => D2D1_COLOR_F {
                    r: 0.2,
                    g: 0.055,
                    b: 0.075,
                    a: 0.96,
                },
                (_, false, true) => D2D1_COLOR_F {
                    r: 0.055,
                    g: 0.065,
                    b: 0.085,
                    a: 0.96,
                },
                (_, false, false) => D2D1_COLOR_F {
                    r: 0.035,
                    g: 0.04,
                    b: 0.05,
                    a: 0.78,
                },
            };
            let accent_color = match (command, enabled) {
                (_, false) => D2D1_COLOR_F {
                    r: 0.16,
                    g: 0.18,
                    b: 0.22,
                    a: 0.8,
                },
                (RadialCommand::Undo, true) => D2D1_COLOR_F {
                    r: 0.23,
                    g: 0.7,
                    b: 0.93,
                    a: 1.0,
                },
                (RadialCommand::Redo, true) => D2D1_COLOR_F {
                    r: 0.3,
                    g: 0.82,
                    b: 0.62,
                    a: 1.0,
                },
                (RadialCommand::Clear, true) => D2D1_COLOR_F {
                    r: 1.0,
                    g: 0.3,
                    b: 0.38,
                    a: 1.0,
                },
            };
            let fill = unsafe { self.dc.CreateSolidColorBrush(&fill_color, None)? };
            let accent = unsafe { self.dc.CreateSolidColorBrush(&accent_color, None)? };
            let rounded = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: rect.left,
                    top: rect.top,
                    right: rect.right,
                    bottom: rect.bottom,
                },
                radiusX: 10.0 * menu.scale(),
                radiusY: 10.0 * menu.scale(),
            };
            unsafe {
                self.dc.FillRoundedRectangle(&rounded, &fill);
                self.dc.DrawRoundedRectangle(
                    &rounded,
                    &accent,
                    if highlighted { 3.0 } else { 1.5 },
                    &self.stroke_style,
                );
            }
            self.draw_centered_text(
                radial_menu::command_label(command),
                rect.center(),
                rect.width() - 8.0 * menu.scale(),
                rect.height(),
                if enabled { text } else { &muted_text },
            );
        }
        Ok(())
    }

    fn draw_radial_indicator(
        &self,
        geometry: &ID2D1PathGeometry,
        highlighted: bool,
        current: bool,
    ) -> Result<()> {
        if !highlighted && !current {
            return Ok(());
        }
        let indicator_color = if highlighted {
            D2D1_COLOR_F {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            }
        } else {
            D2D1_COLOR_F {
                r: 0.23,
                g: 0.7,
                b: 0.93,
                a: 1.0,
            }
        };
        let indicator = unsafe { self.dc.CreateSolidColorBrush(&indicator_color, None)? };
        unsafe {
            self.dc.DrawGeometry(
                geometry,
                &indicator,
                if highlighted { 5.0 } else { 3.0 },
                &self.stroke_style,
            );
        }
        Ok(())
    }

    fn annular_wedge(
        &self,
        center: (f32, f32),
        inner: f32,
        outer: f32,
        start: f32,
        end: f32,
    ) -> Result<ID2D1PathGeometry> {
        let point = |radius: f32, angle: f32| Vector2 {
            X: center.0 + radius * angle.cos(),
            Y: center.1 + radius * angle.sin(),
        };
        unsafe {
            let geometry = self.factory.CreatePathGeometry()?;
            let sink = geometry.Open()?;
            sink.BeginFigure(point(outer, start), D2D1_FIGURE_BEGIN_FILLED);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: point(outer, end),
                size: D2D_SIZE_F {
                    width: outer,
                    height: outer,
                },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: D2D1_ARC_SIZE_SMALL,
            });
            sink.AddLine(point(inner, end));
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: point(inner, start),
                size: D2D_SIZE_F {
                    width: inner,
                    height: inner,
                },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
                arcSize: D2D1_ARC_SIZE_SMALL,
            });
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;
            Ok(geometry.into())
        }
    }

    fn draw_centered_text(
        &self,
        text: &str,
        center: (f32, f32),
        width: f32,
        height: f32,
        brush: &ID2D1SolidColorBrush,
    ) {
        let text = text.encode_utf16().collect::<Vec<_>>();
        let rect = D2D_RECT_F {
            left: center.0 - width / 2.0,
            top: center.1 - height / 2.0,
            right: center.0 + width / 2.0,
            bottom: center.1 + height / 2.0,
        };
        unsafe {
            self.dc.DrawText(
                &text,
                &self.radial_text,
                &rect,
                brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    /// 描画モード中の視覚インジケータ: content rect の枠線
    fn draw_mode_border(&self) -> Result<()> {
        let color = D2D1_COLOR_F {
            r: 0.23,
            g: 0.68,
            b: 0.93, // sky
            a: 0.9,
        };
        unsafe {
            let brush = self.dc.CreateSolidColorBrush(&color, None)?;
            let rect = D2D_RECT_F {
                left: self.content.x as f32 + 1.5,
                top: self.content.y as f32 + 1.5,
                right: (self.content.x + self.content.width) as f32 - 1.5,
                bottom: (self.content.y + self.content.height) as f32 - 1.5,
            };
            self.dc
                .DrawRectangle(&rect, &brush, 3.0, &self.stroke_style);
        }
        Ok(())
    }

    fn solid_brush(&self, brush: &Brush) -> Result<ID2D1SolidColorBrush> {
        let (r, g, b) = parse_color(&brush.color);
        let color = D2D1_COLOR_F {
            r,
            g,
            b,
            a: brush.opacity as f32,
        };
        unsafe { Ok(self.dc.CreateSolidColorBrush(&color, None)?) }
    }

    fn line_brush(&self, style: &LineStyle) -> Result<ID2D1SolidColorBrush> {
        let (r, g, b) = parse_color(&style.color);
        let color = D2D1_COLOR_F {
            r,
            g,
            b,
            a: style.opacity as f32,
        };
        unsafe { Ok(self.dc.CreateSolidColorBrush(&color, None)?) }
    }

    fn normalized_to_local(&self, position: (f64, f64)) -> Vector2 {
        self.to_local(
            position.0 * self.content.width,
            position.1 * self.content.height,
        )
    }

    /// geometry (content rect 基準の px) → ウィンドウローカル座標
    fn to_local(&self, x: f64, y: f64) -> Vector2 {
        Vector2 {
            X: (self.content.x + x) as f32,
            Y: (self.content.y + y) as f32,
        }
    }
}

fn load_stamp_bitmaps(
    dc: &ID2D1DeviceContext,
    stamps: &[StampConfig],
) -> HashMap<String, ID2D1Bitmap1> {
    stamps
        .iter()
        .filter_map(|stamp| match load_stamp_bitmap(dc, stamp) {
            Ok(bitmap) => Some((stamp.id.clone(), bitmap)),
            Err(error) => {
                warn!("failed to load stamp {}: {error:#}", stamp.name);
                None
            }
        })
        .collect()
}

fn load_stamp_bitmap(dc: &ID2D1DeviceContext, stamp: &StampConfig) -> Result<ID2D1Bitmap1> {
    let path = config::stamp_path(&stamp.id)?;
    let decoded = config::decode_stamp_png(&path)?.into_rgba8();
    let (width, height) = decoded.dimensions();
    if width != stamp.width_px || height != stamp.height_px {
        anyhow::bail!(
            "image dimensions changed (config {}x{}, file {}x{})",
            stamp.width_px,
            stamp.height_px,
            width,
            height
        );
    }

    // D2D の BGRA premultiplied 形式へ変換する。
    let mut pixels = decoded.into_raw();
    for pixel in pixels.chunks_exact_mut(4) {
        let red = pixel[0];
        let green = pixel[1];
        let blue = pixel[2];
        let alpha = pixel[3];
        let premultiply = |channel: u8| ((u16::from(channel) * u16::from(alpha) + 127) / 255) as u8;
        pixel[0] = premultiply(blue);
        pixel[1] = premultiply(green);
        pixel[2] = premultiply(red);
        pixel[3] = alpha;
    }
    let properties = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
        colorContext: core::mem::ManuallyDrop::new(None),
    };
    unsafe {
        Ok(dc.CreateBitmap(
            D2D_SIZE_U { width, height },
            Some(pixels.as_ptr().cast()),
            width * 4,
            &properties,
        )?)
    }
}

fn transparent() -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    }
}

fn parse_color(hex: &str) -> (f32, f32, f32) {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return (1.0, 0.3, 0.43); // 既定色にフォールバック
    }
    let parse = |s: &str| u8::from_str_radix(s, 16).unwrap_or(0) as f32 / 255.0;
    (parse(&hex[0..2]), parse(&hex[2..4]), parse(&hex[4..6]))
}
