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
    ID2D1PathGeometry, ID2D1SolidColorBrush, ID2D1StrokeStyle1, D2D1_ANTIALIAS_MODE_ALIASED,
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL, D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
    D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
    D2D1_CAP_STYLE_ROUND, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_LINE_JOIN_ROUND, D2D1_PRIMITIVE_BLEND_COPY,
    D2D1_PRIMITIVE_BLEND_SOURCE_OVER, D2D1_QUADRATIC_BEZIER_SEGMENT, D2D1_ROUNDED_RECT,
    D2D1_STROKE_STYLE_PROPERTIES1, D2D1_SWEEP_DIRECTION_CLOCKWISE,
    D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE};
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
use windows_numerics::{Matrix3x2, Vector2};

use crate::config::{self, StampConfig};
use crate::engine::content_rect::Rect;
use crate::engine::geometry::{dot, full_segments, stable_segments, tail_segment, Segment};
use crate::engine::item_transform::{
    item_transform, selection_half_extents, ROTATE_HANDLE_OFFSET_N,
};
use crate::protocol::{
    Brush, CanvasItem, LineStyle, ShapeItem, ShapeKind, StampItem, Stroke, Tool,
};
use crate::win::menu::{DrawTool, COLORS};
use crate::win::radial_menu::{
    self, RadialCommand, RadialMenu, RadialSelection, COLOR_COUNT, STAMPS_PER_RING,
    STAMP_TOOL_INDEX, TOOL_COUNT,
};

/// `BeginDraw` と `EndDraw` を必ず対にする。
///
/// 描画コール側が失敗しても `EndDraw` は実行するため、次フレームへ描画状態を
/// 持ち越さない。`EndDraw` 自体の device-loss エラーも呼び出し元へ返す。
fn draw_transaction<T>(dc: &ID2D1DeviceContext, draw: impl FnOnce() -> Result<T>) -> Result<T> {
    unsafe {
        dc.BeginDraw();
    }
    let draw_result = draw();
    let end_result = unsafe { dc.EndDraw(None, None) }.context("finish Direct2D draw");
    match (draw_result, end_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(draw_error), Err(end_error)) => {
            Err(draw_error.context(format!("Direct2D EndDraw also failed: {end_error:#}")))
        }
    }
}

/// Direct2D の push/pop スタックを、早期 return を含めて必ず釣り合わせる。
#[must_use = "the guard must stay alive for the clipped drawing scope"]
struct AxisAlignedClipGuard {
    dc: ID2D1DeviceContext,
}

impl AxisAlignedClipGuard {
    fn push(dc: &ID2D1DeviceContext, rect: D2D_RECT_F) -> Self {
        unsafe {
            // Browser の canvas 境界と同じ、ぼかしのない矩形クリップにする。
            dc.PushAxisAlignedClip(&rect, D2D1_ANTIALIAS_MODE_ALIASED);
        }
        Self { dc: dc.clone() }
    }
}

impl Drop for AxisAlignedClipGuard {
    fn drop(&mut self) {
        unsafe {
            self.dc.PopAxisAlignedClip();
        }
    }
}

/// eraser の COPY blend を早期 return 後も SOURCE_OVER へ戻す。
#[must_use = "the guard must stay alive for the COPY drawing scope"]
struct CopyBlendGuard {
    dc: ID2D1DeviceContext,
}

impl CopyBlendGuard {
    fn set(dc: &ID2D1DeviceContext) -> Self {
        unsafe {
            dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_COPY);
        }
        Self { dc: dc.clone() }
    }
}

impl Drop for CopyBlendGuard {
    fn drop(&mut self) {
        unsafe {
            self.dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_SOURCE_OVER);
        }
    }
}

fn content_clip_rect(content: Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: content.x as f32,
        top: content.y as f32,
        right: (content.x + content.width) as f32,
        bottom: (content.y + content.height) as f32,
    }
}

fn with_content_clip<T>(
    dc: &ID2D1DeviceContext,
    content: Rect,
    draw: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let _clip = AxisAlignedClipGuard::push(dc, content_clip_rect(content));
    draw()
}

/// 描画中ストロークの GPU scratch にどこまで確定セグメントを書いたかを保持する。
///
/// `next_segment` は Browser Source の `ActiveEntry.nextSegment` と同じ 1-origin の
/// cursor で、点列全体を再走査せず未描画部分だけを `active` bitmap へ追記する。
#[derive(Debug)]
struct ActiveStrokeState {
    stroke_id: String,
    brush: Brush,
    next_segment: usize,
}

pub struct Renderer {
    factory: ID2D1Factory1,
    dc: ID2D1DeviceContext,
    swapchain: IDXGISwapChain1,
    target: ID2D1Bitmap1,
    baked: ID2D1Bitmap1,
    /// 描画中ストローク専用の再利用 bitmap。pen/marker は透明 scratch、eraser は
    /// stroke 開始時点の baked 複製として使う。
    active: ID2D1Bitmap1,
    active_stroke: Option<ActiveStrokeState>,
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
        Self::new_with_driver(
            hwnd,
            width,
            height,
            content,
            stamps,
            D3D_DRIVER_TYPE_HARDWARE,
        )
    }

    fn new_with_driver(
        hwnd: HWND,
        width: u32,
        height: u32,
        content: Rect,
        stamps: &[StampConfig],
        driver_type: D3D_DRIVER_TYPE,
    ) -> Result<Self> {
        unsafe {
            // D3D11 デバイス (BGRA サポートは D2D 連携に必須)
            let mut d3d_device: Option<ID3D11Device> = None;
            D3D11CreateDevice(
                None,
                driver_type,
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

            // 確定履歴と描画中ストロークの再利用 scratch。
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
            let active = dc.CreateBitmap(D2D_SIZE_U { width, height }, None, 0, &baked_props)?;
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
            let radial_scale = radial_menu::scale_for_menu(width, height, stamps.len());
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
                active,
                active_stroke: None,
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
        self.rebuild_baked_prefix(items, None)
    }

    /// transform対象より前の確定履歴だけをbakedへcacheする。対象とsuffixは
    /// frame target上へ元の順序で再合成し、後続eraserの意味を維持する。
    pub fn rebuild_baked_prefix(
        &mut self,
        items: &[CanvasItem],
        transformed_item_id: Option<&str>,
    ) -> Result<()> {
        // rebuild/device recovery 後は現在の点列から scratch を再同期する。
        self.active_stroke = None;
        self.clear_baked_bitmap()?;

        let prefix_end = transformed_item_id
            .and_then(|item_id| items.iter().position(|item| item.item_id() == item_id))
            .unwrap_or(items.len());
        let visible = items[..prefix_end]
            .iter()
            .filter(|item| item.is_done())
            .collect::<Vec<_>>();
        let baked = self.baked.clone();
        self.append_composited_items(&baked, &visible)
    }

    fn append_composited_items(
        &self,
        target: &ID2D1Bitmap1,
        visible: &[&CanvasItem],
    ) -> Result<()> {
        let mut direct_start = 0;
        for (index, item) in visible.iter().enumerate() {
            let CanvasItem::Stroke { stroke } = item else {
                continue;
            };
            if !stroke_uses_opacity_scratch(stroke) {
                continue;
            }
            self.append_bitmap_items(target, &visible[direct_start..index])?;
            self.composite_translucent_stroke_to(target, stroke)?;
            direct_start = index + 1;
        }
        self.append_bitmap_items(target, &visible[direct_start..])
    }

    fn clear_baked_bitmap(&self) -> Result<()> {
        unsafe {
            self.dc.SetTarget(&self.baked);
        }
        let dc = self.dc.clone();
        draw_transaction(&dc, || {
            unsafe {
                self.dc.Clear(Some(&transparent()));
            }
            Ok(())
        })
    }

    fn append_bitmap_items(&self, target: &ID2D1Bitmap1, items: &[&CanvasItem]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        unsafe {
            self.dc.SetTarget(target);
        }
        let dc = self.dc.clone();
        draw_transaction(&dc, || {
            with_content_clip(&self.dc, self.content, || {
                for item in items {
                    self.draw_item(item)?;
                }
                Ok(())
            })
        })
    }

    /// 新しく確定した1項目だけをbakedへ追記する。
    pub fn bake_item(&mut self, item: &CanvasItem) -> Result<()> {
        if let CanvasItem::Stroke { stroke } = item {
            if self
                .active_stroke
                .as_ref()
                .is_some_and(|active| active.stroke_id == stroke.stroke_id)
            {
                return self.bake_active_stroke(stroke);
            }
            if stroke_uses_opacity_scratch(stroke) {
                self.active_stroke = None;
                return self.composite_translucent_stroke(stroke);
            }
        }
        unsafe {
            self.dc.SetTarget(&self.baked);
        }
        let dc = self.dc.clone();
        draw_transaction(&dc, || {
            with_content_clip(&self.dc, self.content, || self.draw_item(item))
        })
    }

    /// Browser Sourceと同じく、半透明strokeは不透明scratchへ全体を描いてから
    /// opacityを1回だけ掛ける。rebuild後もactive確定時と同じ見た目を保つ。
    fn composite_translucent_stroke(&self, stroke: &Stroke) -> Result<()> {
        self.composite_translucent_stroke_to(&self.baked, stroke)
    }

    fn composite_translucent_stroke_to(
        &self,
        target: &ID2D1Bitmap1,
        stroke: &Stroke,
    ) -> Result<()> {
        unsafe {
            self.dc.SetTarget(&self.active);
        }
        let dc = self.dc.clone();
        draw_transaction(&dc, || {
            unsafe {
                self.dc.Clear(Some(&transparent()));
            }
            with_content_clip(&self.dc, self.content, || {
                let brush = self.opaque_brush(&stroke.brush)?;
                self.draw_stroke_shape(stroke, &brush)
            })
        })?;

        unsafe {
            self.dc.SetTarget(target);
        }
        let dc = self.dc.clone();
        draw_transaction(&dc, || {
            with_content_clip(&self.dc, self.content, || {
                unsafe {
                    self.dc.DrawBitmap(
                        &self.active,
                        None,
                        stroke.brush.opacity as f32,
                        D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                        None,
                        None,
                    );
                }
                Ok(())
            })
        })
    }

    /// 空フレームを提示してオーバーレイ表示を消す (パススルー復帰時)。
    /// baked ビットマップは保持したままなので、次の描画モードで再表示される
    pub fn clear_frame(&mut self) -> Result<()> {
        self.active_stroke = None;
        unsafe {
            self.dc.SetTarget(&self.target);
        }
        let dc = self.dc.clone();
        draw_transaction(&dc, || {
            unsafe {
                self.dc.Clear(Some(&transparent()));
            }
            Ok(())
        })?;
        unsafe {
            self.swapchain.Present(1, Default::default()).ok()?;
        }
        Ok(())
    }

    /// 1 フレーム描画: baked + 描画中項目 + 描画UI。
    pub fn draw_frame(
        &mut self,
        items: &[CanvasItem],
        draw_mode: bool,
        selected_item: Option<&CanvasItem>,
        radial: Option<(&RadialMenu, &DrawTool, &str, &[StampConfig])>,
    ) -> Result<()> {
        self.sync_active_stroke(items)?;
        if let Some(item) = selected_item {
            if self.draw_transform_history_preview(items, item)? {
                unsafe {
                    self.dc.SetTarget(&self.target);
                }
                let dc = self.dc.clone();
                draw_transaction(&dc, || {
                    // 選択枠・枠・ラジアルメニューは履歴合成後の操作UIとして描く。
                    self.draw_item_selection(item)?;
                    if draw_mode {
                        self.draw_mode_border()?;
                    }
                    if let Some((menu, tool, color, stamps)) = radial {
                        self.draw_radial_menu(menu, tool, color, stamps)?;
                    }
                    Ok(())
                })?;
                unsafe {
                    self.swapchain.Present(1, Default::default()).ok()?;
                }
                return Ok(());
            }
        }

        unsafe {
            self.dc.SetTarget(&self.target);
        }
        let dc = self.dc.clone();
        draw_transaction(&dc, || {
            unsafe {
                self.dc.Clear(Some(&transparent()));
            }
            with_content_clip(&self.dc, self.content, || {
                // Browser overlay の canvas と同じく、コンテンツだけを canvas 境界で切る。
                self.draw_cached_canvas_layers();
                for item in items.iter().filter(|item| !item.is_done()) {
                    // Stroke は active bitmap へ増分描画済み。Shape の終点だけは
                    // 最新値からフレーム単位で描き直す。
                    if !matches!(item, CanvasItem::Stroke { .. }) {
                        self.draw_item(item)?;
                    }
                }
                if let Some(item) = selected_item {
                    self.draw_item(item)?;
                }
                Ok(())
            })?;
            // 選択枠・枠・ラジアルメニューは操作 UI なので、端でも欠けないよう
            // content clip の後に描く。
            if let Some(item) = selected_item {
                self.draw_item_selection(item)?;
            }
            if draw_mode {
                self.draw_mode_border()?;
            }
            if let Some((menu, tool, color, stamps)) = radial {
                self.draw_radial_menu(menu, tool, color, stamps)?;
            }
            Ok(())
        })?;
        unsafe {
            self.swapchain.Present(1, Default::default()).ok()?;
        }
        Ok(())
    }

    /// cached prefixをtargetへ転写し、transform対象と後続履歴を元の順序で再合成する。
    /// targetより後のeraserや半透明strokeも、commit後の完全rebuildと同じ意味になる。
    fn draw_transform_history_preview(
        &mut self,
        items: &[CanvasItem],
        selected_item: &CanvasItem,
    ) -> Result<bool> {
        let Some(transformed_index) = items
            .iter()
            .position(|item| item.is_done() && item.item_id() == selected_item.item_id())
        else {
            return Ok(false);
        };
        // Select toolは描画sessionと排他的。違反時は通常frameへfallbackする。
        if self.active_stroke.is_some() {
            return Ok(false);
        }

        let target = self.target.clone();
        unsafe {
            self.dc.SetTarget(&target);
        }
        let dc = self.dc.clone();
        draw_transaction(&dc, || {
            unsafe {
                self.dc.Clear(Some(&transparent()));
            }
            with_content_clip(&self.dc, self.content, || {
                unsafe {
                    self.dc.DrawBitmap(
                        &self.baked,
                        None,
                        1.0,
                        D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                        None,
                        None,
                    );
                }
                Ok(())
            })
        })?;

        let suffix = items[transformed_index..]
            .iter()
            .filter(|item| item.is_done())
            .map(|item| {
                if item.item_id() == selected_item.item_id() {
                    selected_item
                } else {
                    item
                }
            })
            .collect::<Vec<_>>();
        self.append_composited_items(&target, &suffix)?;
        Ok(true)
    }

    /// 最新の active stroke と GPU scratch を同期する。1-origin の cursor より前の
    /// 点には触れないため、通常の pointer update は新規点数にだけ比例する。
    fn sync_active_stroke(&mut self, items: &[CanvasItem]) -> Result<()> {
        let active = items.iter().find_map(|item| match item {
            CanvasItem::Stroke { stroke } if !stroke.done => Some(stroke),
            _ => None,
        });
        let Some(stroke) = active else {
            self.active_stroke = None;
            return Ok(());
        };

        let needs_reset = self.active_stroke.as_ref().is_none_or(|cached| {
            cached.stroke_id != stroke.stroke_id || cached.brush != stroke.brush
        });
        if needs_reset {
            self.begin_active_stroke(stroke)?;
        }
        self.append_active_stroke(stroke)
    }

    fn begin_active_stroke(&mut self, stroke: &Stroke) -> Result<()> {
        self.active_stroke = None;
        unsafe {
            self.dc.SetTarget(&self.active);
        }
        let dc = self.dc.clone();
        draw_transaction(&dc, || {
            unsafe {
                self.dc.Clear(Some(&transparent()));
                // Eraser preview は確定 baked を壊さない。開始時の複製へ増分消去し、
                // cancel 時は state を捨てるだけで元表示へ戻せる。
                if stroke.brush.tool == Tool::Eraser {
                    self.dc.DrawBitmap(
                        &self.baked,
                        None,
                        1.0,
                        D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                        None,
                        None,
                    );
                }
            }
            Ok(())
        })?;
        self.active_stroke = Some(ActiveStrokeState {
            stroke_id: stroke.stroke_id.clone(),
            brush: stroke.brush.clone(),
            next_segment: 1,
        });
        Ok(())
    }

    fn append_active_stroke(&mut self, stroke: &Stroke) -> Result<()> {
        let from_segment = self
            .active_stroke
            .as_ref()
            .filter(|active| active.stroke_id == stroke.stroke_id)
            .map_or(1, |active| active.next_segment);
        let segments = stable_segments(
            &stroke.pts,
            self.content.width,
            self.content.height,
            &stroke.brush,
            from_segment,
        );
        if segments.is_empty() {
            return Ok(());
        }

        unsafe {
            self.dc.SetTarget(&self.active);
        }
        let dc = self.dc.clone();
        let result = draw_transaction(&dc, || {
            with_content_clip(&self.dc, self.content, || {
                self.draw_active_segments(&stroke.brush, &segments)
            })
        });
        if let Err(error) = result {
            // 部分描画済みbitmapを同じcursorで再利用しない。次回はclearして再同期する。
            self.active_stroke = None;
            return Err(error);
        }
        if let Some(active) = self.active_stroke.as_mut() {
            active.next_segment += segments.len();
        }
        Ok(())
    }

    fn draw_active_segments(&self, brush: &Brush, segments: &[Segment]) -> Result<()> {
        let eraser = brush.tool == Tool::Eraser;
        let _copy = eraser.then(|| CopyBlendGuard::set(&self.dc));
        let color = if eraser {
            unsafe { self.dc.CreateSolidColorBrush(&transparent(), None)? }
        } else {
            self.opaque_brush(brush)?
        };
        for segment in segments {
            self.draw_segment(segment, &color)?;
        }
        Ok(())
    }

    /// 確定時は未描画の stable segment と tail/dot だけを scratch へ足し、全点から
    /// geometry を作り直さず baked へ1回合成する。
    fn bake_active_stroke(&mut self, stroke: &Stroke) -> Result<()> {
        let result = self.bake_active_stroke_inner(stroke);
        // 成否にかかわらず、このscratchへtailを重ねることはできない。失敗時は
        // App側のdevice recoveryが完全履歴からbakedを再構築する。
        self.active_stroke = None;
        result
    }

    fn bake_active_stroke_inner(&mut self, stroke: &Stroke) -> Result<()> {
        self.append_active_stroke(stroke)?;
        unsafe {
            self.dc.SetTarget(&self.active);
        }
        let dc = self.dc.clone();
        draw_transaction(&dc, || {
            with_content_clip(&self.dc, self.content, || {
                let eraser = stroke.brush.tool == Tool::Eraser;
                let _copy = eraser.then(|| CopyBlendGuard::set(&self.dc));
                let color = if eraser {
                    unsafe { self.dc.CreateSolidColorBrush(&transparent(), None)? }
                } else {
                    self.opaque_brush(&stroke.brush)?
                };
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
                            &color,
                        );
                    }
                } else if let Some(tail) = tail_segment(
                    &stroke.pts,
                    self.content.width,
                    self.content.height,
                    &stroke.brush,
                ) {
                    self.draw_segment(&tail, &color)?;
                }
                Ok(())
            })
        })?;

        unsafe {
            self.dc.SetTarget(&self.baked);
        }
        let dc = self.dc.clone();
        draw_transaction(&dc, || {
            with_content_clip(&self.dc, self.content, || {
                let eraser = stroke.brush.tool == Tool::Eraser;
                let _copy = eraser.then(|| CopyBlendGuard::set(&self.dc));
                unsafe {
                    self.dc.DrawBitmap(
                        &self.active,
                        None,
                        if eraser {
                            1.0
                        } else {
                            stroke.brush.opacity as f32
                        },
                        D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                        None,
                        None,
                    );
                }
                Ok(())
            })
        })?;
        Ok(())
    }

    fn draw_cached_canvas_layers(&self) {
        let eraser = self
            .active_stroke
            .as_ref()
            .is_some_and(|active| active.brush.tool == Tool::Eraser);
        unsafe {
            // Eraser scratch は baked の複製を含むので二重描画しない。
            if !eraser {
                self.dc.DrawBitmap(
                    &self.baked,
                    None,
                    1.0,
                    D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                    None,
                    None,
                );
            }
            if let Some(active) = &self.active_stroke {
                self.dc.DrawBitmap(
                    &self.active,
                    None,
                    if eraser {
                        1.0
                    } else {
                        active.brush.opacity as f32
                    },
                    D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                    None,
                    None,
                );
            }
        }
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
        // 半透明strokeは呼び出し側で不透明scratchへ描いてから一括合成する。
        let eraser = stroke.brush.tool == Tool::Eraser;
        let _copy = eraser.then(|| CopyBlendGuard::set(&self.dc));
        let brush = if eraser {
            unsafe { self.dc.CreateSolidColorBrush(&transparent(), None)? }
        } else {
            self.solid_brush(&stroke.brush)?
        };
        self.draw_stroke_shape(stroke, &brush)
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
        if let Some(transform) = shape.transform {
            let center = self.normalized_to_local(transform.center);
            return self.with_rotation(transform.rotation, center, || {
                self.draw_transformed_shape(shape, transform)
            });
        }
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

    fn draw_transformed_shape(
        &self,
        shape: &ShapeItem,
        transform: crate::protocol::ItemTransform,
    ) -> Result<()> {
        let brush = self.line_brush(&shape.style)?;
        let line_width = (shape.style.width_n * self.content.height) as f32;
        let center = self.normalized_to_local(transform.center);
        let width = (transform.width_n * self.content.width) as f32;
        let height = (transform.height_n * self.content.height) as f32;
        let start = Vector2 {
            X: center.X - width / 2.0,
            Y: center.Y,
        };
        let end = Vector2 {
            X: center.X + width / 2.0,
            Y: center.Y,
        };
        unsafe {
            match shape.shape {
                ShapeKind::Line => {
                    self.dc
                        .DrawLine(start, end, &brush, line_width, &self.stroke_style);
                }
                ShapeKind::Arrow => {
                    self.dc
                        .DrawLine(start, end, &brush, line_width, &self.stroke_style);
                    let head_length = (f64::from(width) * 0.4)
                        .min((f64::from(line_width) * 4.0).max(self.content.height * 0.02));
                    let spread = std::f64::consts::PI / 6.0;
                    for head_angle in [-spread, spread] {
                        let point = Vector2 {
                            X: end.X - (head_length * head_angle.cos()) as f32,
                            Y: end.Y - (head_length * head_angle.sin()) as f32,
                        };
                        self.dc
                            .DrawLine(end, point, &brush, line_width, &self.stroke_style);
                    }
                }
                ShapeKind::Rectangle => {
                    self.dc.DrawRectangle(
                        &D2D_RECT_F {
                            left: center.X - width / 2.0,
                            top: center.Y - height / 2.0,
                            right: center.X + width / 2.0,
                            bottom: center.Y + height / 2.0,
                        },
                        &brush,
                        line_width,
                        &self.stroke_style,
                    );
                }
                ShapeKind::Ellipse => {
                    self.dc.DrawEllipse(
                        &D2D1_ELLIPSE {
                            point: center,
                            radiusX: width / 2.0,
                            radiusY: height / 2.0,
                        },
                        &brush,
                        line_width,
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
        let center = self.normalized_to_local(stamp.center);
        self.with_rotation(stamp.rotation, center, || {
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
        })
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

    fn draw_item_selection(&self, item: &CanvasItem) -> Result<()> {
        let aspect = self.content.width / self.content.height;
        let Some(transform) = item_transform(item, aspect) else {
            return Ok(());
        };
        let center = self.normalized_to_local(transform.center);
        let (half_width_n, half_height_n) = selection_half_extents(transform, item, aspect);
        let half_width = (half_width_n * self.content.height) as f32;
        let half_height = (half_height_n * self.content.height) as f32;
        let rect = D2D_RECT_F {
            left: center.X - half_width,
            top: center.Y - half_height,
            right: center.X + half_width,
            bottom: center.Y + half_height,
        };
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
        self.with_rotation(transform.rotation, center, || {
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
            let rotate_y = rect.top - (ROTATE_HANDLE_OFFSET_N * self.content.height) as f32;
            unsafe {
                self.dc.DrawLine(
                    Vector2 {
                        X: center.X,
                        Y: rect.top,
                    },
                    Vector2 {
                        X: center.X,
                        Y: rotate_y,
                    },
                    &shadow,
                    5.0,
                    &self.stroke_style,
                );
                self.dc.DrawLine(
                    Vector2 {
                        X: center.X,
                        Y: rect.top,
                    },
                    Vector2 {
                        X: center.X,
                        Y: rotate_y,
                    },
                    &accent,
                    2.0,
                    &self.stroke_style,
                );
                self.dc.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: Vector2 {
                            X: center.X,
                            Y: rotate_y,
                        },
                        radiusX: handle,
                        radiusY: handle,
                    },
                    &shadow,
                );
                self.dc.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: Vector2 {
                            X: center.X,
                            Y: rotate_y,
                        },
                        radiusX: handle - 2.0,
                        radiusY: handle - 2.0,
                    },
                    &accent,
                );
            }
            Ok(())
        })
    }

    fn with_rotation<T>(
        &self,
        rotation_radians: f64,
        center: Vector2,
        draw: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let mut previous = Matrix3x2::identity();
        unsafe {
            self.dc.GetTransform(&mut previous);
            self.dc.SetTransform(&Matrix3x2::rotation_around(
                rotation_radians.to_degrees() as f32,
                center,
            ));
        }
        let result = draw();
        unsafe {
            self.dc.SetTransform(&previous);
        }
        result
    }

    fn draw_radial_menu(
        &self,
        menu: &RadialMenu,
        current_tool: &DrawTool,
        current_color: &str,
        stamps: &[StampConfig],
    ) -> Result<()> {
        debug_assert_eq!(menu.stamp_count(), stamps.len());
        debug_assert!(menu.layout_within_surface());
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

    /// Browser Source の stroke scratch と同じく、stroke opacity は bitmap 合成時に
    /// 一度だけ掛けるため、segment 自体は不透明で描く。
    fn opaque_brush(&self, brush: &Brush) -> Result<ID2D1SolidColorBrush> {
        let (r, g, b) = parse_color(&brush.color);
        let color = D2D1_COLOR_F { r, g, b, a: 1.0 };
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

fn stroke_uses_opacity_scratch(stroke: &Stroke) -> bool {
    stroke.brush.tool != Tool::Eraser && stroke.brush.opacity < 1.0
}

fn parse_color(hex: &str) -> (f32, f32, f32) {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return (1.0, 0.3, 0.43); // 既定色にフォールバック
    }
    let parse = |s: &str| u8::from_str_radix(s, 16).unwrap_or(0) as f32 / 255.0;
    (parse(&hex[0..2]), parse(&hex[2..4]), parse(&hex[4..6]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Direct2D::{
        ID2D1Image, D2D1_BITMAP_OPTIONS_CPU_READ, D2D1_MAP_OPTIONS_READ,
    };
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
    use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_POPUP};

    const TEST_SIZE: D2D_SIZE_U = D2D_SIZE_U {
        width: 64,
        height: 48,
    };

    struct Pixels {
        bytes: Vec<u8>,
        pitch: usize,
    }

    struct TestWindow(HWND);

    impl Drop for TestWindow {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }

    impl Pixels {
        fn bgra(&self, x: u32, y: u32) -> [u8; 4] {
            let offset = y as usize * self.pitch + x as usize * 4;
            self.bytes[offset..offset + 4].try_into().unwrap()
        }

        fn visible_pixel_count(&self) -> usize {
            (0..TEST_SIZE.height)
                .flat_map(|y| (0..TEST_SIZE.width).map(move |x| (x, y)))
                .filter(|&(x, y)| self.bgra(x, y)[3] != 0)
                .count()
        }
    }

    fn test_context() -> Result<(ID2D1DeviceContext, ID2D1Bitmap1)> {
        unsafe {
            let mut d3d_device: Option<ID3D11Device> = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d_device),
                None,
                None,
            )?;
            let d3d_device = d3d_device.context("no WARP D3D device")?;
            let dxgi_device: IDXGIDevice = d3d_device.cast()?;
            let factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2d_device = factory.CreateDevice(&dxgi_device)?;
            let dc = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            let target = dc.CreateBitmap(
                TEST_SIZE,
                None,
                0,
                &D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET,
                    colorContext: core::mem::ManuallyDrop::new(None),
                },
            )?;
            dc.SetTarget(&target);
            Ok((dc, target))
        }
    }

    fn test_renderer() -> Result<(Renderer, TestWindow)> {
        unsafe {
            // STATIC はWindows組み込みclassなので、並列テストでもclass登録を共有しない。
            let hwnd = CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                w!("StreamPainter renderer test"),
                WS_POPUP,
                0,
                0,
                TEST_SIZE.width as i32,
                TEST_SIZE.height as i32,
                None,
                None,
                None,
                None,
            )?;
            let renderer = Renderer::new_with_driver(
                hwnd,
                TEST_SIZE.width,
                TEST_SIZE.height,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: TEST_SIZE.width as f64,
                    height: TEST_SIZE.height as f64,
                },
                &[],
                D3D_DRIVER_TYPE_WARP,
            )?;
            Ok((renderer, TestWindow(hwnd)))
        }
    }

    fn test_stroke(id: &str, tool: Tool, opacity: f64, pts: Vec<crate::protocol::Point>) -> Stroke {
        Stroke {
            stroke_id: id.into(),
            brush: Brush {
                tool,
                color: "#ff0000".into(),
                opacity,
                width_n: 0.2,
                pressure_width: false,
                pressure_min: 1.0,
                tilt_width: false,
                tilt_max_scale: 1.0,
            },
            pts,
            done: false,
            ended_at: None,
        }
    }

    fn stroke_item(stroke: &Stroke) -> CanvasItem {
        CanvasItem::Stroke {
            stroke: stroke.clone(),
        }
    }

    fn transformed_line_item(id: &str, color: &str, center_y: f64) -> CanvasItem {
        CanvasItem::Shape {
            shape: ShapeItem {
                item_id: id.into(),
                shape: ShapeKind::Line,
                style: LineStyle {
                    color: color.into(),
                    opacity: 1.0,
                    width_n: 0.12,
                },
                start: (0.1, center_y),
                end: (0.9, center_y),
                transform: Some(crate::protocol::ItemTransform {
                    center: (0.5, center_y),
                    width_n: 0.8,
                    height_n: 0.0,
                    rotation: 0.0,
                }),
                done: true,
                ended_at: Some(1.0),
            },
        }
    }

    fn fill(dc: &ID2D1DeviceContext, rect: D2D_RECT_F, color: D2D1_COLOR_F) -> Result<()> {
        unsafe {
            let brush = dc.CreateSolidColorBrush(&color, None)?;
            dc.FillRectangle(&rect, &brush);
        }
        Ok(())
    }

    fn read_pixels(dc: &ID2D1DeviceContext, target: &ID2D1Bitmap1) -> Result<Pixels> {
        unsafe {
            dc.SetTarget(None::<&ID2D1Image>);
            let readback = dc.CreateBitmap(
                TEST_SIZE,
                None,
                0,
                &D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                    colorContext: core::mem::ManuallyDrop::new(None),
                },
            )?;
            readback.CopyFromBitmap(None, target, None)?;
            let mapped = readback.Map(D2D1_MAP_OPTIONS_READ)?;
            let length = mapped.pitch as usize * TEST_SIZE.height as usize;
            let bytes = core::slice::from_raw_parts(mapped.bits, length).to_vec();
            readback.Unmap()?;
            Ok(Pixels {
                bytes,
                pitch: mapped.pitch as usize,
            })
        }
    }

    fn full_surface() -> D2D_RECT_F {
        D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: TEST_SIZE.width as f32,
            bottom: TEST_SIZE.height as f32,
        }
    }

    fn red() -> D2D1_COLOR_F {
        D2D1_COLOR_F {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        }
    }

    fn blue() -> D2D1_COLOR_F {
        D2D1_COLOR_F {
            r: 0.0,
            g: 0.0,
            b: 1.0,
            a: 1.0,
        }
    }

    #[test]
    fn native_transform_preview_keeps_suffix_eraser_and_stroke_history_order() -> Result<()> {
        let (mut renderer, _window) = test_renderer()?;
        let prefix = transformed_line_item("prefix", "#0000ff", 0.35);
        let selected = transformed_line_item("selected", "#00ff00", 0.5);
        let mut eraser = test_stroke(
            "later-eraser",
            Tool::Eraser,
            1.0,
            vec![
                (0.5, 0.2, 1.0, 0.0, 0.0, 0.0),
                (0.5, 0.8, 1.0, 1.0, 0.0, 0.0),
            ],
        );
        eraser.done = true;
        eraser.ended_at = Some(2.0);
        let mut later_pen = test_stroke(
            "later-pen",
            Tool::Pen,
            1.0,
            vec![(0.5, 0.5, 1.0, 0.0, 0.0, 0.0)],
        );
        later_pen.done = true;
        later_pen.ended_at = Some(3.0);
        let items = vec![
            prefix,
            selected.clone(),
            stroke_item(&eraser),
            stroke_item(&later_pen),
        ];

        renderer.rebuild_baked_prefix(&items, Some(selected.item_id()))?;
        assert!(renderer.draw_transform_history_preview(&items, &selected)?);
        let preview_pixel = read_pixels(&renderer.dc, &renderer.target)?.bgra(32, 24);

        renderer.rebuild_baked(&items)?;
        let committed_pixel = read_pixels(&renderer.dc, &renderer.baked)?.bgra(32, 24);
        assert_eq!(preview_pixel, committed_pixel);
        assert!(preview_pixel[2] > 220, "later red pen must remain visible");
        assert!(
            preview_pixel[1] < 32,
            "selected green line must not move on top"
        );
        Ok(())
    }

    #[test]
    fn native_active_scratch_is_incremental_rebuildable_and_cancel_safe() -> Result<()> {
        let (mut renderer, _window) = test_renderer()?;
        renderer.rebuild_baked(&[])?;

        let mut marker = test_stroke(
            "marker",
            Tool::Marker,
            0.5,
            vec![
                (0.1, 0.5, 1.0, 0.0, 0.0, 0.0),
                (0.35, 0.5, 1.0, 1.0, 0.0, 0.0),
                (0.65, 0.5, 1.0, 2.0, 0.0, 0.0),
            ],
        );
        renderer.sync_active_stroke(&[stroke_item(&marker)])?;
        assert_eq!(
            renderer
                .active_stroke
                .as_ref()
                .map(|active| active.next_segment),
            Some(2)
        );

        // 同じ点列を再同期しても確定segmentを再描画しない。
        renderer.sync_active_stroke(&[stroke_item(&marker)])?;
        assert_eq!(
            renderer
                .active_stroke
                .as_ref()
                .map(|active| active.next_segment),
            Some(2)
        );
        marker.pts.push((0.9, 0.5, 1.0, 3.0, 0.0, 0.0));
        renderer.sync_active_stroke(&[stroke_item(&marker)])?;
        assert_eq!(
            renderer
                .active_stroke
                .as_ref()
                .map(|active| active.next_segment),
            Some(3)
        );
        let active_pixels = read_pixels(&renderer.dc, &renderer.active)?;
        assert_eq!(active_pixels.bgra(32, 24), [0, 0, 255, 255]);

        // baked rebuild / device再生成相当ではcursorを捨て、最新点列から再同期できる。
        renderer.rebuild_baked(&[])?;
        assert!(renderer.active_stroke.is_none());
        renderer.sync_active_stroke(&[stroke_item(&marker)])?;
        assert_eq!(
            renderer
                .active_stroke
                .as_ref()
                .map(|active| active.next_segment),
            Some(3)
        );

        marker.done = true;
        marker.ended_at = Some(4.0);
        renderer.bake_item(&stroke_item(&marker))?;
        assert!(renderer.active_stroke.is_none());
        let baked_marker = read_pixels(&renderer.dc, &renderer.baked)?.bgra(32, 24);
        assert_eq!(&baked_marker[..2], &[0, 0]);
        assert!((120..=136).contains(&baked_marker[2]));
        assert!((120..=136).contains(&baked_marker[3]));
        renderer.rebuild_baked(&[stroke_item(&marker)])?;
        assert_eq!(
            read_pixels(&renderer.dc, &renderer.baked)?.bgra(32, 24),
            baked_marker
        );

        let mut eraser = test_stroke(
            "eraser",
            Tool::Eraser,
            1.0,
            vec![
                (0.5, 0.1, 1.0, 0.0, 0.0, 0.0),
                (0.5, 0.5, 1.0, 1.0, 0.0, 0.0),
                (0.5, 0.9, 1.0, 2.0, 0.0, 0.0),
            ],
        );
        renderer.sync_active_stroke(&[stroke_item(&eraser)])?;
        assert_eq!(
            read_pixels(&renderer.dc, &renderer.active)?.bgra(32, 24),
            [0, 0, 0, 0]
        );
        // preview/cancelは確定bakedを破壊しない。
        assert_eq!(
            read_pixels(&renderer.dc, &renderer.baked)?.bgra(32, 24),
            baked_marker
        );
        renderer.sync_active_stroke(&[])?;
        assert!(renderer.active_stroke.is_none());
        assert_eq!(
            read_pixels(&renderer.dc, &renderer.baked)?.bgra(32, 24),
            baked_marker
        );

        renderer.sync_active_stroke(&[stroke_item(&eraser)])?;
        eraser.done = true;
        eraser.ended_at = Some(5.0);
        renderer.bake_item(&stroke_item(&eraser))?;
        assert_eq!(
            read_pixels(&renderer.dc, &renderer.baked)?.bgra(32, 24),
            [0, 0, 0, 0]
        );
        Ok(())
    }

    #[test]
    fn ten_thousand_point_native_scratch_measures_only_the_new_segment() -> Result<()> {
        let (mut renderer, _window) = test_renderer()?;
        renderer.rebuild_baked(&[])?;
        let mut stroke = test_stroke(
            "long-stroke",
            Tool::Pen,
            1.0,
            (0..9_999)
                .map(|index| {
                    (
                        index as f64 / 9_999.0,
                        0.5,
                        1.0,
                        index as f64 * 0.25,
                        0.0,
                        0.0,
                    )
                })
                .collect(),
        );
        renderer.sync_active_stroke(&[stroke_item(&stroke)])?;
        assert_eq!(
            renderer
                .active_stroke
                .as_ref()
                .map(|active| active.next_segment),
            Some(9_998)
        );

        stroke.pts.push((1.0, 0.5, 1.0, 2_499.75, 0.0, 0.0));
        let started = std::time::Instant::now();
        renderer.sync_active_stroke(&[stroke_item(&stroke)])?;
        let elapsed = started.elapsed();
        eprintln!(
            "10,000-point native Direct2D scratch: {:.6} ms for the final segment",
            elapsed.as_secs_f64() * 1_000.0
        );
        assert_eq!(
            renderer
                .active_stroke
                .as_ref()
                .map(|active| active.next_segment),
            Some(9_999)
        );
        // Wall-clock tests can be descheduled on shared CI. The averaged pure geometry test
        // enforces 16.67ms; this only guards a catastrophic native regression.
        assert!(elapsed < std::time::Duration::from_millis(250));
        Ok(())
    }

    #[test]
    fn native_warp_renderer_reflects_pressure_and_tilt_widths() -> Result<()> {
        let (mut renderer, _window) = test_renderer()?;
        let points = |pressure, tilt_x, tilt_y| {
            vec![
                (0.15, 0.5, pressure, 0.0, tilt_x, tilt_y),
                (0.5, 0.5, pressure, 1.0, tilt_x, tilt_y),
                (0.85, 0.5, pressure, 2.0, tilt_x, tilt_y),
            ]
        };

        let mut pen = test_stroke("pen-low", Tool::Pen, 1.0, points(0.0, 0.0, 0.0));
        pen.done = true;
        pen.brush.width_n = 0.18;
        pen.brush.pressure_width = true;
        pen.brush.pressure_min = 0.2;
        renderer.rebuild_baked(&[stroke_item(&pen)])?;
        let low_pressure = read_pixels(&renderer.dc, &renderer.baked)?.visible_pixel_count();

        pen.stroke_id = "pen-high".into();
        pen.pts = points(1.0, 0.0, 0.0);
        renderer.rebuild_baked(&[stroke_item(&pen)])?;
        let high_pressure = read_pixels(&renderer.dc, &renderer.baked)?.visible_pixel_count();
        assert!(
            high_pressure > low_pressure * 2,
            "pressure coverage did not grow: low={low_pressure}, high={high_pressure}"
        );

        let mut marker = test_stroke("marker-upright", Tool::Marker, 1.0, points(1.0, 0.0, 0.0));
        marker.done = true;
        marker.brush.width_n = 0.12;
        marker.brush.tilt_width = true;
        marker.brush.tilt_max_scale = 1.75;
        renderer.rebuild_baked(&[stroke_item(&marker)])?;
        let upright = read_pixels(&renderer.dc, &renderer.baked)?.visible_pixel_count();

        marker.stroke_id = "marker-tilted".into();
        marker.pts = points(1.0, 0.6, 0.8);
        renderer.rebuild_baked(&[stroke_item(&marker)])?;
        let tilted = read_pixels(&renderer.dc, &renderer.baked)?.visible_pixel_count();
        assert!(
            tilted * 10 > upright * 13,
            "tilt coverage did not grow: upright={upright}, tilted={tilted}"
        );
        Ok(())
    }

    #[test]
    fn content_is_clipped_but_ui_can_draw_in_letterbox_and_pillarbox_bars() -> Result<()> {
        let cases = [
            (
                "letterbox",
                Rect {
                    x: 0.0,
                    y: 8.0,
                    width: 64.0,
                    height: 32.0,
                },
                (32, 24),
                (32, 2),
                D2D_RECT_F {
                    left: 28.0,
                    top: 43.0,
                    right: 36.0,
                    bottom: 47.0,
                },
                (32, 45),
            ),
            (
                "pillarbox",
                Rect {
                    x: 8.0,
                    y: 0.0,
                    width: 48.0,
                    height: 48.0,
                },
                (32, 24),
                (2, 24),
                D2D_RECT_F {
                    left: 59.0,
                    top: 20.0,
                    right: 63.0,
                    bottom: 28.0,
                },
                (61, 24),
            ),
        ];

        for (name, content, content_point, untouched_bar, ui_rect, ui_point) in cases {
            let (dc, target) = test_context()?;
            draw_transaction(&dc, || {
                unsafe {
                    dc.Clear(Some(&transparent()));
                }
                with_content_clip(&dc, content, || {
                    // stroke/shape/stamp/baked は同じスコープを通るので、面全体を塗って
                    // 各プリミティブが境界を跨いだ場合の可視範囲を pixel で検証する。
                    fill(&dc, full_surface(), red())
                })?;
                // 枠・ラジアルメニューに相当する UI はクリップ外へ描画できる。
                fill(&dc, ui_rect, blue())
            })?;

            let pixels = read_pixels(&dc, &target)?;
            assert_eq!(
                pixels.bgra(content_point.0, content_point.1),
                [0, 0, 255, 255],
                "{name}"
            );
            assert_eq!(
                pixels.bgra(untouched_bar.0, untouched_bar.1),
                [0, 0, 0, 0],
                "{name}"
            );
            assert_eq!(
                pixels.bgra(ui_point.0, ui_point.1),
                [255, 0, 0, 255],
                "{name}"
            );
        }
        Ok(())
    }

    #[test]
    fn clip_and_draw_transaction_are_balanced_after_early_error() -> Result<()> {
        let (dc, target) = test_context()?;
        let content = Rect {
            x: 8.0,
            y: 0.0,
            width: 48.0,
            height: 48.0,
        };
        let result = draw_transaction(&dc, || -> Result<()> {
            unsafe {
                dc.Clear(Some(&transparent()));
            }
            with_content_clip(&dc, content, || {
                fill(&dc, full_surface(), red())?;
                anyhow::bail!("synthetic item draw failure")
            })
        });
        assert!(result.is_err());

        // 早期 return 後も次セッションを開始でき、かつ古い clip は残っていない。
        draw_transaction(&dc, || {
            fill(
                &dc,
                D2D_RECT_F {
                    left: 0.0,
                    top: 20.0,
                    right: 4.0,
                    bottom: 28.0,
                },
                blue(),
            )
        })?;
        let pixels = read_pixels(&dc, &target)?;
        assert_eq!(pixels.bgra(2, 24), [255, 0, 0, 255]);
        assert_eq!(pixels.bgra(6, 24), [0, 0, 0, 0]);
        assert_eq!(pixels.bgra(32, 24), [0, 0, 255, 255]);
        Ok(())
    }
}
