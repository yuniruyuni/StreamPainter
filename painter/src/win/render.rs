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
use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN,
    D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1DeviceContext, ID2D1Factory1, ID2D1SolidColorBrush,
    ID2D1StrokeStyle1, D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_NONE,
    D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_CAP_STYLE_ROUND,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_INTERPOLATION_MODE_LINEAR, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_LINE_JOIN_ROUND,
    D2D1_PRIMITIVE_BLEND_COPY, D2D1_PRIMITIVE_BLEND_SOURCE_OVER, D2D1_QUADRATIC_BEZIER_SEGMENT,
    D2D1_STROKE_STYLE_PROPERTIES1,
};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
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

pub struct Renderer {
    factory: ID2D1Factory1,
    dc: ID2D1DeviceContext,
    swapchain: IDXGISwapChain1,
    target: ID2D1Bitmap1,
    baked: ID2D1Bitmap1,
    stamp_bitmaps: HashMap<String, ID2D1Bitmap1>,
    stroke_style: ID2D1StrokeStyle1,
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
                content,
                _dcomp_device: dcomp_device,
                _dcomp_target: dcomp_target,
                _dcomp_visual: dcomp_visual,
            })
        }
    }

    /// 確定 CanvasItem 一覧から baked を再構築する。
    pub fn rebuild_baked(&mut self, items: &[CanvasItem]) -> Result<()> {
        unsafe {
            self.dc.SetTarget(&self.baked);
            self.dc.BeginDraw();
            self.dc.Clear(Some(&transparent()));
            for item in items.iter().filter(|item| item.is_done()) {
                self.draw_item(item)?;
            }
            self.dc.EndDraw(None, None)?;
        }
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

    /// 1 フレーム描画: baked + 描画中項目 (+ 描画モードの枠表示)
    pub fn draw_frame(&mut self, items: &[CanvasItem], draw_mode: bool) -> Result<()> {
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
            if draw_mode {
                self.draw_mode_border()?;
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
        let center = self.normalized_to_local(stamp.center);
        let width = (stamp.width_n * self.content.width) as f32;
        let height = (stamp.height_n * self.content.height) as f32;
        let destination = D2D_RECT_F {
            left: center.X - width / 2.0,
            top: center.Y - height / 2.0,
            right: center.X + width / 2.0,
            bottom: center.Y + height / 2.0,
        };
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
