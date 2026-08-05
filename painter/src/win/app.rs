//! オーバーレイウィンドウ・入力・アプリ統合 (docs/painter.md)。
//!
//! - WS_EX_NOREDIRECTIONBITMAP + WS_EX_TOPMOST + WS_EX_NOACTIVATE + WS_EX_TOOLWINDOW
//! - 設定可能なグローバルホットキーでパススルー ⇔ 描画モードを切替
//!   (既定F9、WS_EX_TRANSPARENT)
//! - WM_POINTER* で入力を受け、CanvasEngine → local web hub + ローカルエコー描画
//! - 16ms タイマで描画差分を約60fpsにバッチ送信

use anyhow::{anyhow, Context, Result};
use tracing::{info, warn};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::Pointer::EnableMouseInPointer;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    GetWindowLongPtrW, KillTimer, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassW,
    SetCursor, SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    TranslateMessage, GWLP_USERDATA, GWL_EXSTYLE, HWND_TOPMOST, IDC_ARROW, IDC_CROSS, IDC_SIZEALL,
    IDC_SIZENESW, IDC_SIZENWSE, LWA_ALPHA, MSG, POINTER_MESSAGE_FLAG_CANCELED,
    POINTER_MESSAGE_FLAG_FIRSTBUTTON, POINTER_MESSAGE_FLAG_SECONDBUTTON, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOWNOACTIVATE, WM_APP,
    WM_CANCELMODE, WM_CAPTURECHANGED, WM_DESTROY, WM_DISPLAYCHANGE, WM_HOTKEY, WM_PAINT,
    WM_POINTERCAPTURECHANGED, WM_POINTERDOWN, WM_POINTERUP, WM_POINTERUPDATE, WM_SETCURSOR,
    WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::config;
use crate::engine::canvas_engine::CanvasEngine;
use crate::engine::content_rect::{content_rect, parse_aspect, Rect};
use crate::engine::item_transform::{
    apply_item_transform, selection_handle_at, TransformCorner, TransformHandle,
    TransformInteraction,
};
use crate::net::local_server::{self, LocalServerHandle};
use crate::net::obs::{self, ObsSettings, ProjectorView};
use crate::net::obs_request::{
    PollDisposition, ProjectorRequestTracker, RequestGeneration, WorkerDisposition,
};
use crate::protocol::{Brush, CanvasItem, CanvasLayer, LineStyle, PainterMessage, ShapeKind, Tool};
use crate::win::hotkey::{self, ChangeCommand, HotkeyManager};
use crate::win::menu::{self, DrawTool, MenuAction};
use crate::win::monitor::{self, Monitor};
use crate::win::pointer;
use crate::win::projector;
use crate::win::radial_menu::{self, RadialLayerEntry, RadialMenu, RadialRelease};
use crate::win::render::Renderer;
use crate::win::settings;
use crate::win::tray::{self, TrayCommand, WM_TRAY};

/// 動画フレームに合わせた約60fpsのバッチ送信。
const FLUSH_TIMER_ID: usize = 1;
const FLUSH_INTERVAL_MS: u32 = 16;
/// OBS プロジェクター検知のポーリング間隔 (docs/painter.md)
const PROJECTOR_TIMER_ID: usize = 2;
const PROJECTOR_INTERVAL_MS: u32 = 2000;
/// obs-websocket でプロジェクターを開いた後、表示を確認するまでの高速ポーリング
const PENDING_TIMER_ID: usize = 3;
const PENDING_INTERVAL_MS: u32 = 250;
const RENDERER_RECOVERY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
/// obs-websocket workerがchannelへ結果を追加したことだけを通知する。payloadは持たない。
/// WM_APP+3 はhotkey transactionが使用するため、既存の+2を維持する。
const WM_OBS_RESULT: u32 = WM_APP + 2;

struct ObsWorkerResult {
    generation: RequestGeneration,
    outcome: std::result::Result<(), String>,
}

struct ItemDrag {
    pointer_id: u32,
    interaction: TransformInteraction,
}

struct ItemSelection {
    item: CanvasItem,
    drag: Option<ItemDrag>,
}

/// 高ポーリングレート入力から同じフレームへの描画要求を1件へ集約する純状態。
#[derive(Default)]
struct FrameGate {
    pending: bool,
}

impl FrameGate {
    /// 新しく低優先度の paint を予約すべき場合だけ true。
    fn request(&mut self) -> bool {
        if self.pending {
            return false;
        }
        self.pending = true;
        true
    }

    fn take(&mut self) -> bool {
        std::mem::take(&mut self.pending)
    }

    fn cancel(&mut self) {
        self.pending = false;
    }
}

/// 設定ポリシーとエンジンの全消去可否から、確認画面が必要かだけを決める。
/// 空キャンバスや操作中は不要な確認を出さない。
fn clear_confirmation_required(confirm_before_clear: bool, can_clear: bool) -> bool {
    confirm_before_clear && can_clear
}

/// Deleteはオーバーレイがキーボードfocusを持たないためglobal hotkeyで受ける。
/// StreamPainter自身のUIや入力操作の最中に背後のキャンバスを消さない。
fn layer_clear_hotkey_allowed(
    draw_mode: bool,
    radial_menu_open: bool,
    foreground_ui_active: bool,
    engine_active: bool,
    item_drag_active: bool,
) -> bool {
    draw_mode && !radial_menu_open && !foreground_ui_active && !engine_active && !item_drag_active
}

struct App {
    engine: CanvasEngine,
    web: LocalServerHandle,
    overlay_hwnd: HWND,
    renderer: Option<Renderer>,
    content_local: Rect,
    last_renderer_recovery: Option<std::time::Instant>,
    tool: DrawTool,
    stamps: Vec<crate::config::StampConfig>,
    color: String,
    width_n: f64,
    /// content rect (スクリーン座標)。入力の正規化に使う
    content_screen: Rect,
    configured_screen: usize,
    canvas_aspect: (f64, f64),
    monitor: Monitor,
    draw_mode: bool,
    local_echo: bool,
    confirm_before_clear: bool,
    /// OBS プロジェクター表示への追従 (config.follow_projector)
    follow_projector: bool,
    /// プロジェクター検知結果。false の間はオーバーレイを隠し切替も無効
    projector_visible: bool,
    /// obs-websocket 設定 (None = 連携無効)
    obs: Option<ObsSettings>,
    /// 描画モード終了時にプロジェクターを閉じるか
    close_projector: bool,
    /// 世代IDとworker/UI共通deadlineを持つprojector要求状態。
    obs_requests: ProjectorRequestTracker,
    /// receiverをsenderより先にdropし、終了中のworker送信をdisconnectさせる。
    obs_result_rx: std::sync::mpsc::Receiver<ObsWorkerResult>,
    obs_result_tx: std::sync::mpsc::Sender<ObsWorkerResult>,
    obs_worker_wake_enabled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// 登録競合時もトレイ操作だけで継続し、設定変更はtransactionalに反映する。
    hotkey: HotkeyManager,
    /// 現在のプロジェクターを自分が obs-websocket で開いたか
    /// (手動で開かれたものは描画モード終了時にも閉じない)
    projector_opened_by_us: bool,
    /// OBSプロジェクターを前面化し、その直上にオーバーレイを保つ。
    projector_z_order: projector::ZOrderGuard,
    /// 右ボタンを押している間のジェスチャーメニュー。
    radial_menu: Option<RadialMenu>,
    /// 描画モードの間だけ予約する、現在レイヤー消去用Delete hotkey。
    layer_clear_hotkey_registered: bool,
    /// 選択中shape/stampより前をcacheし、対象以降を履歴順にフレーム合成する。
    item_selection: Option<ItemSelection>,
    frame_gate: FrameGate,
}

pub fn run() -> Result<()> {
    match crate::win::autostart::SystemAutostart::current()
        .and_then(|autostart| autostart.inspect())
    {
        Ok(crate::win::autostart::RegistrationStatus::NeedsRepair(problem)) => {
            warn!(
                "Windows auto-start registration needs repair: {}; open settings to repair or disable it",
                problem.description()
            );
        }
        Err(error) => warn!("failed to inspect Windows auto-start registration: {error:#}"),
        _ => {}
    }

    let config = config::load()?;
    if let Err(error) = config::cleanup_unregistered_stamps(&config) {
        warn!("failed to clean unregistered stamps: {error:#}");
    }

    unsafe {
        // マニフェストでも宣言しているが、古い Windows への保険として実行時にも設定する
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // マウスもポインタ系メッセージに統一する
        EnableMouseInPointer(true).context("EnableMouseInPointer")?;
    }

    let monitors = monitor::enumerate();
    let (screen_index, mon) = match select_monitor(&monitors, config.screen) {
        Some((screen_index, monitor)) => {
            if screen_index != config.screen {
                warn!(
                    "screen index {} が見つかりません (モニタ数: {}) — プライマリを使用します",
                    config.screen,
                    monitors.len()
                );
            }
            (screen_index, monitor)
        }
        None => {
            warn!(
                "screen index {} が見つかりません (モニタ数: 0)",
                config.screen
            );
            return Err(anyhow!("利用可能なモニターが見つかりません"));
        }
    };
    info!(
        "monitor {}: {}x{} at ({},{})",
        screen_index, mon.width, mon.height, mon.x, mon.y
    );

    let (aw, ah) =
        parse_aspect(&config.canvas_aspect).ok_or_else(|| anyhow!("canvas_aspect が不正です"))?;
    // ウィンドウローカル座標での content rect
    let content_local = content_rect(mon.width as f64, mon.height as f64, aw, ah);
    let content_screen = Rect {
        x: content_local.x + mon.x as f64,
        y: content_local.y + mon.y as f64,
        ..content_local
    };

    let obs = if config.obs_control {
        Some(ObsSettings {
            url: config.obs_websocket_url.clone(),
            password: config.obs_websocket_password.clone(),
            view: if config.projector_view == "preview" {
                ProjectorView::Preview
            } else {
                ProjectorView::Program
            },
        })
    } else {
        None
    };

    let engine = CanvasEngine::new();
    let web = local_server::spawn(
        config.local_server_port,
        &config.stamps,
        engine.shared_items(),
        engine.shared_layers(),
    )?;
    debug_assert_eq!(web.overlay_url(), config.overlay_url());

    let hwnd = create_overlay_window(mon.x, mon.y, mon.width, mon.height)?;
    let renderer = Renderer::new(
        hwnd,
        mon.width as u32,
        mon.height as u32,
        content_local,
        &config.stamps,
    )?;

    let mut hotkey = HotkeyManager::new(hwnd);
    let startup_hotkey_error = hotkey.register_initial(&config.hotkey).err();
    let (obs_result_tx, obs_result_rx) = std::sync::mpsc::channel();
    let obs_worker_wake_enabled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));

    let mut app = Box::new(App {
        engine,
        web,
        overlay_hwnd: hwnd,
        renderer: Some(renderer),
        content_local,
        last_renderer_recovery: None,
        tool: DrawTool::Pen,
        stamps: config.stamps.clone(),
        color: config.brush.color.clone(),
        width_n: config.brush.width_n,
        content_screen,
        configured_screen: config.screen,
        canvas_aspect: (aw, ah),
        monitor: mon,
        draw_mode: false,
        local_echo: config.local_echo,
        confirm_before_clear: config.confirm_before_clear,
        follow_projector: config.follow_projector,
        projector_visible: false,
        obs,
        close_projector: config.close_projector,
        obs_requests: ProjectorRequestTracker::default(),
        obs_result_rx,
        obs_result_tx,
        obs_worker_wake_enabled,
        hotkey,
        projector_opened_by_us: false,
        projector_z_order: projector::ZOrderGuard::default(),
        radial_menu: None,
        layer_clear_hotkey_registered: false,
        item_selection: None,
        frame_gate: FrameGate::default(),
    });

    // 起動時に透明フレームを 1 回描き、D2D シェーダコンパイル・swapchain 初回
    // Present などの一時コストをここで消化する (初回切替の体感遅延対策)
    {
        let t = std::time::Instant::now();
        let renderer = app.renderer.as_mut().expect("renderer was initialized");
        let layers = [CanvasLayer::default()];
        renderer.rebuild_layered_baked(&layers, &[])?;
        renderer.draw_frame(&layers, &[], false, None, None)?;
        info!("renderer warmup: {:?}", t.elapsed());
    }

    let active_hotkey_name = unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(app) as isize);
        let app_ref = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App);
        tray::add(hwnd, app_ref.hotkey.active_display_name())?;
        // 初期状態はパススルー
        set_transparent(hwnd, true);
        SetTimer(Some(hwnd), PROJECTOR_TIMER_ID, PROJECTOR_INTERVAL_MS, None);
        // 追従モードでは初回検知が終わるまで隠しておく (poll_projector が表示する)。
        // 追従しない場合も、OBSプロジェクターがあればZ-orderだけは同期する。
        if !app_ref.follow_projector {
            app_ref.projector_visible = true;
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        app_ref.poll_projector(hwnd);
        app_ref
            .hotkey
            .active_display_name()
            .unwrap_or("tray only")
            .to_owned()
    };
    if let Some(error) = startup_hotkey_error {
        warn!("global hotkey is unavailable; using tray fallback: {error:#}");
        crate::win::message_box_warning(
            hwnd,
            &format!(
                "描画モード切替ホットキーを登録できませんでした。\n\n{error:#}\n\nStreamPainterは継続して動作します。タスクトレイの「描画モード切替」を使用するか、設定から別のキーを指定してください。"
            ),
        );
    }
    info!("ready — draw mode hotkey: {active_hotkey_name}");

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            if settings::handle_dialog_message(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

fn create_overlay_window(x: i32, y: i32, width: i32, height: i32) -> Result<HWND> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let class_name = w!("stream-painter-overlay");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: hinstance.into(),
            lpszClassName: class_name,
            // クラスカーソル未設定だと起動直後のビジーカーソルが
            // このウィンドウ上で更新されず残り続ける。描画モードでは十字を出す
            hCursor: LoadCursorW(None, IDC_CROSS)?,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(anyhow!("RegisterClassW failed"));
        }
        // WS_EX_LAYERED はクリックスルー (WS_EX_TRANSPARENT) を機能させるために必須。
        // レイヤードでないウィンドウの TRANSPARENT はヒットテストを素通しにしない。
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP
                | WS_EX_LAYERED
                | WS_EX_TOPMOST
                | WS_EX_NOACTIVATE
                | WS_EX_TOOLWINDOW,
            class_name,
            w!("StreamPainter"),
            WS_POPUP,
            x,
            y,
            width,
            height,
            None,
            None,
            Some(hinstance.into()),
            None,
        )?;
        // レイヤードウィンドウは属性を設定するまで表示されない (DComp の内容には alpha は影響しない)
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)?;
        Ok(hwnd)
    }
}

/// WS_EX_TRANSPARENT の付け外し (パススルー ⇔ 描画)
fn set_transparent(hwnd: HWND, transparent: bool) {
    unsafe {
        let mut ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if transparent {
            ex |= WS_EX_TRANSPARENT.0 as isize;
        } else {
            ex &= !(WS_EX_TRANSPARENT.0 as isize);
        }
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex);
        // topmost を再主張しつつスタイル変更を反映
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as f64
}

fn pointer_id(wparam: WPARAM) -> u32 {
    (wparam.0 & 0xffff) as u32
}

fn pointer_flags(wparam: WPARAM) -> u32 {
    ((wparam.0 >> 16) & 0xffff) as u32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrawingCancellation {
    Pointer(u32),
    AnyPointer,
}

/// Win32メッセージを通常描画セッションのキャンセル対象へ変換する。
/// pointer固有メッセージはowner IDを照合し、汎用capture lossだけは現在のownerを対象にする。
fn drawing_cancellation(message: u32, wparam: WPARAM) -> Option<DrawingCancellation> {
    match message {
        WM_POINTERUPDATE | WM_POINTERUP
            if pointer_flags(wparam) & POINTER_MESSAGE_FLAG_CANCELED != 0 =>
        {
            Some(DrawingCancellation::Pointer(pointer_id(wparam)))
        }
        WM_POINTERCAPTURECHANGED => Some(DrawingCancellation::Pointer(pointer_id(wparam))),
        WM_CAPTURECHANGED | WM_CANCELMODE => Some(DrawingCancellation::AnyPointer),
        _ => None,
    }
}

fn pointer_screen(lparam: LPARAM) -> (f64, f64) {
    (
        (lparam.0 & 0xffff) as i16 as f64,
        ((lparam.0 >> 16) & 0xffff) as i16 as f64,
    )
}

fn select_monitor(monitors: &[Monitor], configured: usize) -> Option<(usize, Monitor)> {
    monitors
        .get(configured)
        .copied()
        .map(|monitor| (configured, monitor))
        .or_else(|| monitors.first().copied().map(|monitor| (0, monitor)))
}

/// Freehand tool dynamics are serialized with each stroke so Direct2D and the
/// Browser Source never need hidden platform-specific tuning.
fn brush_for_tool(tool: &DrawTool, color: &str, width_n: f64) -> Option<Brush> {
    match tool {
        DrawTool::Pen => Some(Brush {
            tool: Tool::Pen,
            color: color.to_owned(),
            opacity: 1.0,
            width_n,
            pressure_width: true,
            pressure_min: 0.2,
            tilt_width: false,
            tilt_max_scale: 1.0,
        }),
        DrawTool::Marker => Some(Brush {
            tool: Tool::Marker,
            color: color.to_owned(),
            opacity: 0.5,
            width_n: width_n * 3.0,
            // Marker pressure is deliberately gentler than the pen. Tilt
            // magnitude broadens the round highlighter up to 1.75x.
            pressure_width: true,
            pressure_min: 0.65,
            tilt_width: true,
            tilt_max_scale: 1.75,
        }),
        DrawTool::Eraser => Some(Brush {
            tool: Tool::Eraser,
            color: "#000000".into(),
            opacity: 1.0,
            width_n: width_n * 3.0,
            // A fixed eraser remains predictable and exactly matches the old
            // mouse/touch behavior even when a pen reports contact pressure.
            pressure_width: false,
            pressure_min: 1.0,
            tilt_width: false,
            tilt_max_scale: 1.0,
        }),
        DrawTool::Select
        | DrawTool::Line
        | DrawTool::Arrow
        | DrawTool::Rectangle
        | DrawTool::Ellipse
        | DrawTool::Stamp(_) => None,
    }
}

impl App {
    /// 現在のツール・色から Brush を組み立てる (テストページと同じマッピング)
    fn current_brush(&self) -> Option<Brush> {
        brush_for_tool(&self.tool, &self.color, self.width_n)
    }

    fn current_line_style(&self) -> LineStyle {
        LineStyle {
            color: self.color.clone(),
            opacity: 1.0,
            width_n: self.width_n,
        }
    }

    /// オーバーレイが操作可能な状態か (プロジェクター追従が無効なら常に可)
    fn overlay_enabled(&self) -> bool {
        !self.follow_projector || self.projector_visible
    }

    fn item_drag_active(&self) -> bool {
        self.item_selection
            .as_ref()
            .is_some_and(|selection| selection.drag.is_some())
    }

    fn item_drag_owns(&self, pointer_id: u32) -> bool {
        self.item_selection
            .as_ref()
            .and_then(|selection| selection.drag.as_ref())
            .is_some_and(|drag| drag.pointer_id == pointer_id)
    }

    fn update_selection_cursor(&self) -> bool {
        if !self.draw_mode || self.tool != DrawTool::Select || self.radial_menu.is_some() {
            return false;
        }
        let mut point = POINT::default();
        if unsafe { GetCursorPos(&mut point) }.is_err() {
            return false;
        }
        let pointer = self
            .content_screen
            .normalize(f64::from(point.x), f64::from(point.y));
        let aspect = self.content_screen.width / self.content_screen.height;
        let radius = (9.0 / self.content_screen.height).max(0.006);
        let handle = self
            .item_selection
            .as_ref()
            .and_then(|selection| {
                selection
                    .drag
                    .as_ref()
                    .map(|drag| drag.interaction.handle())
                    .or_else(|| selection_handle_at(&selection.item, pointer, aspect, radius))
            })
            .or_else(|| {
                self.engine
                    .transformable_at(pointer.0, pointer.1, aspect, radius)
                    .map(|_| TransformHandle::Move)
            });
        let cursor_name = match handle {
            Some(TransformHandle::Move) => IDC_SIZEALL,
            Some(TransformHandle::Scale(
                TransformCorner::NorthWest | TransformCorner::SouthEast,
            )) => IDC_SIZENWSE,
            Some(TransformHandle::Scale(
                TransformCorner::NorthEast | TransformCorner::SouthWest,
            )) => IDC_SIZENESW,
            Some(TransformHandle::Rotate) => IDC_CROSS,
            None => IDC_ARROW,
        };
        let Ok(cursor) = (unsafe { LoadCursorW(None, cursor_name) }) else {
            return false;
        };
        unsafe {
            SetCursor(Some(cursor));
        }
        true
    }

    /// OBSプロジェクターを検出し、表示追従とZ-orderを同期する。
    fn poll_projector(&mut self, hwnd: HWND) {
        // TrackPopupMenu や設定画面が独自のメッセージループを回している間に
        // overlay を Topmost 帯の先頭へ移すと、StreamPainter 自身の UI を覆う。
        // UI が閉じた直後または次回 timer で改めて同期する。
        if projector::foreground_ui_active() {
            return;
        }

        let projector = projector::find_projector(&self.monitor, hwnd);
        let visible = projector.is_some();

        if self.follow_projector && visible != self.projector_visible {
            self.projector_visible = visible;
            if visible {
                info!("OBS projector detected — overlay enabled");
                unsafe {
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                }
            } else {
                info!("OBS projector closed — overlay disabled");
                self.projector_opened_by_us = false;
                if self.draw_mode {
                    // 描画モードのまま消えた場合はパススルーへ戻す
                    self.set_draw_mode(hwnd, false);
                }
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
        }

        if let Err(error) = self.projector_z_order.enforce(projector, hwnd) {
            warn!("OBS projector Z-order sync failed: {error}");
        }
    }

    /// 複数項目やレイヤーを復元した場合は、増分eventより完全snapshotを優先する。
    fn dispatch_engine_change(&mut self, messages: Vec<PainterMessage>) -> bool {
        let full_sync = self.engine.take_full_sync_required();
        let changed = full_sync || !messages.is_empty();
        if full_sync {
            self.web.send_snapshot();
        } else if changed {
            self.web.send_all(messages);
        }
        changed
    }

    fn set_layer_clear_hotkey_enabled(&mut self, hwnd: HWND, enabled: bool) {
        if enabled {
            if self.layer_clear_hotkey_registered {
                return;
            }
            match hotkey::register_layer_clear(hwnd) {
                Ok(()) => self.layer_clear_hotkey_registered = true,
                Err(error) => warn!(
                    "active-layer Delete hotkey is unavailable; use the layer menu instead: {error:#}"
                ),
            }
        } else if self.layer_clear_hotkey_registered {
            hotkey::unregister_layer_clear(hwnd);
            self.layer_clear_hotkey_registered = false;
        }
    }

    fn can_handle_layer_clear_hotkey(&self) -> bool {
        layer_clear_hotkey_allowed(
            self.draw_mode,
            self.radial_menu.is_some(),
            projector::foreground_ui_active(),
            self.engine.is_drawing(),
            self.item_drag_active(),
        ) && self.engine.can_clear_layer(self.engine.active_layer_id())
    }

    /// popup が閉じた後に選択結果を反映する。true はアプリ終了要求。
    fn apply_menu_action(&mut self, hwnd: HWND, action: MenuAction) -> bool {
        match action {
            MenuAction::SelectTool(tool) => {
                info!("tool: {tool:?}");
                let deselected = tool != DrawTool::Select && self.item_selection.take().is_some();
                self.tool = tool;
                if deselected {
                    self.rebuild();
                    self.render();
                }
            }
            MenuAction::SelectColor(color) => {
                self.color = color.to_string();
                // 色を選んだ = 描く意図なので、非着色ツール中ならペンに戻す
                if matches!(
                    &self.tool,
                    DrawTool::Select | DrawTool::Eraser | DrawTool::Stamp(_)
                ) {
                    let deselected = self.item_selection.take().is_some();
                    self.tool = DrawTool::Pen;
                    if deselected {
                        self.rebuild();
                        self.render();
                    }
                }
            }
            MenuAction::SelectLayer(layer_id) => {
                let deselected = self.item_selection.take().is_some();
                if self.engine.select_layer(&layer_id) {
                    info!("active layer: {layer_id}");
                }
                if deselected {
                    self.rebuild();
                }
                self.render();
            }
            MenuAction::AddLayer => {
                let deselected = self.item_selection.take().is_some();
                let messages = self.engine.add_layer();
                let changed = self.dispatch_engine_change(messages);
                if deselected || changed {
                    self.rebuild();
                    self.render();
                }
            }
            MenuAction::ClearLayer(layer_id) => {
                if !self.engine.can_clear_layer(&layer_id) {
                    return false;
                }
                let deselected = self.item_selection.take().is_some();
                let messages = self.engine.clear_layer(&layer_id);
                let changed = self.dispatch_engine_change(messages);
                if deselected || changed {
                    self.rebuild();
                    self.render();
                }
            }
            MenuAction::DeleteLayer(layer_id) => {
                let deselected = self.item_selection.take().is_some();
                let messages = self.engine.delete_layer(&layer_id);
                let changed = self.dispatch_engine_change(messages);
                if deselected || changed {
                    self.rebuild();
                    self.render();
                }
            }
            MenuAction::Undo => {
                let deselected = self.item_selection.take().is_some();
                let messages = self.engine.undo();
                let changed = self.dispatch_engine_change(messages);
                if deselected || changed {
                    self.rebuild();
                    self.render();
                }
            }
            MenuAction::Redo => {
                let deselected = self.item_selection.take().is_some();
                let messages = self.engine.redo();
                let changed = self.dispatch_engine_change(messages);
                if deselected || changed {
                    self.rebuild();
                    self.render();
                }
            }
            MenuAction::Clear => {
                let can_clear = self.engine.can_clear();
                if !can_clear {
                    return false;
                }
                if clear_confirmation_required(self.confirm_before_clear, can_clear)
                    && !crate::win::confirm(hwnd, "すべての描画を消去しますか？")
                {
                    return false;
                }
                let deselected = self.item_selection.take().is_some();
                let messages = self.engine.clear();
                let changed = self.dispatch_engine_change(messages);
                if deselected || changed {
                    self.rebuild();
                    self.render();
                }
            }
            MenuAction::Exit => return true,
        }
        false
    }

    // ローカルエコーは描画モード中のみ表示する。パススルー中は overlay
    // (プロジェクター内のブラウザソース) 側の表示だけが見える
    fn request_render(&mut self) {
        if !self.draw_mode || !self.frame_gate.request() {
            return;
        }
        // WM_PAINT は queued input/posted messages を処理した後にのみ生成され、同じ
        // invalid region への複数要求も1件へ統合される。WM_APP をPostするとinputより
        // 先に取り出され得て 1 update = 1 Present になるため、frame予約には使わない。
        if !unsafe { InvalidateRect(Some(self.overlay_hwnd), None, false) }.as_bool() {
            warn!("failed to invalidate native frame");
            // invalidation失敗時もローカルエコーを止めない。
            self.frame_gate.cancel();
            self.render();
        }
    }

    fn on_frame_request(&mut self) {
        if self.frame_gate.take() {
            self.render();
        }
    }

    fn render(&mut self) {
        // pointer up / menu 操作などの即時描画は予約済みframeを包含する。
        self.frame_gate.cancel();
        if !self.draw_mode {
            return;
        }
        let items = self.engine.shared_items();
        let items = items.lock().unwrap();
        let layers = self.engine.shared_layers();
        let layers = layers.lock().unwrap();
        let visible = if self.local_echo { &items[..] } else { &[] };
        let selected_item = self
            .item_selection
            .as_ref()
            .map(|selection| &selection.item);
        let radial = self.radial_menu.as_ref().map(|menu| {
            (
                menu,
                &self.tool,
                self.color.as_str(),
                self.stamps.as_slice(),
            )
        });
        let result = self
            .renderer
            .as_mut()
            .ok_or_else(|| anyhow!("renderer is unavailable"))
            .and_then(|renderer| {
                renderer.draw_frame(&layers, visible, self.draw_mode, selected_item, radial)
            });
        drop(layers);
        drop(items);
        if let Err(error) = result {
            self.recover_renderer("draw_frame", error);
        }
    }

    fn rebuild(&mut self) {
        if !self.local_echo {
            return;
        }
        let items = self.engine.shared_items();
        let items = items.lock().unwrap();
        let layers = self.engine.shared_layers();
        let layers = layers.lock().unwrap();
        let result = self
            .renderer
            .as_mut()
            .ok_or_else(|| anyhow!("renderer is unavailable"))
            .and_then(|renderer| renderer.rebuild_layered_baked(&layers, &items));
        drop(layers);
        drop(items);
        if let Err(error) = result {
            self.recover_renderer("rebuild_baked", error);
        }
    }

    /// 選択開始では対象レイヤーだけをprefix cacheへ切り替える。完成cacheを持つ
    /// 他レイヤーは再生しないため、大きな別レイヤーがdrag frameへ影響しない。
    fn prepare_item_transform(&mut self, item_id: &str) {
        if !self.local_echo {
            return;
        }
        let items = self.engine.shared_items();
        let items = items.lock().unwrap();
        let layers = self.engine.shared_layers();
        let layers = layers.lock().unwrap();
        let result = self
            .renderer
            .as_mut()
            .ok_or_else(|| anyhow!("renderer is unavailable"))
            .and_then(|renderer| renderer.prepare_layer_transform(&layers, &items, item_id))
            .map(|_| ());
        drop(layers);
        drop(items);
        if let Err(error) = result {
            self.recover_renderer("prepare_item_transform", error);
        }
    }

    fn bake_last_done(&mut self) {
        if !self.local_echo {
            return;
        }
        let items = self.engine.shared_items();
        let items = items.lock().unwrap();
        let Some(item) = items.iter().rfind(|item| item.is_done()) else {
            return;
        };
        let layers = self.engine.shared_layers();
        let layers = layers.lock().unwrap();
        let result = self
            .renderer
            .as_mut()
            .ok_or_else(|| anyhow!("renderer is unavailable"))
            .and_then(|renderer| renderer.bake_layer_item(&layers, item));
        drop(layers);
        drop(items);
        if let Err(error) = result {
            self.recover_renderer("bake_item", error);
        }
    }

    /// GPUデバイス喪失などの描画失敗時に、完全履歴から描画資源を作り直す。
    fn recover_renderer(&mut self, operation: &str, error: anyhow::Error) {
        let now = std::time::Instant::now();
        if self
            .last_renderer_recovery
            .is_some_and(|last| now.duration_since(last) < RENDERER_RECOVERY_INTERVAL)
        {
            return;
        }
        self.last_renderer_recovery = Some(now);
        warn!("{operation}: {error:#}; recreating graphics resources");

        // 同じHWNDに複数のDirectComposition targetを作らないよう先に破棄する。
        self.renderer.take();
        match self.create_renderer_from_history() {
            Ok(renderer) => {
                self.renderer = Some(renderer);
                info!("graphics resources recovered");
            }
            Err(recovery_error) => {
                warn!("graphics resource recovery failed: {recovery_error:#}");
            }
        }
    }

    fn create_renderer_from_history(&self) -> anyhow::Result<Renderer> {
        let items = {
            let shared = self.engine.shared_items();
            let snapshot = shared.lock().unwrap().clone();
            snapshot
        };
        let layers = self.engine.layers();
        let mut renderer = Renderer::new(
            self.overlay_hwnd,
            self.monitor.width as u32,
            self.monitor.height as u32,
            self.content_local,
            &self.stamps,
        )?;
        if self.local_echo {
            renderer.rebuild_layered_baked(&layers, &items)?;
            if let Some(selected) = self.item_selection.as_ref() {
                let _ =
                    renderer.prepare_layer_transform(&layers, &items, selected.item.item_id())?;
            }
        } else {
            renderer.rebuild_layered_baked(&layers, &[])?;
        }
        if self.draw_mode {
            let visible = if self.local_echo { &items[..] } else { &[] };
            let selected_item = self
                .item_selection
                .as_ref()
                .map(|selection| &selection.item);
            let radial = self.radial_menu.as_ref().map(|menu| {
                (
                    menu,
                    &self.tool,
                    self.color.as_str(),
                    self.stamps.as_slice(),
                )
            });
            renderer.draw_frame(&layers, visible, true, selected_item, radial)?;
        } else {
            renderer.clear_frame()?;
        }
        Ok(renderer)
    }

    /// 解像度変更・モニタ増減に合わせてウィンドウ、座標変換、GPU資源を更新する。
    fn on_display_change(&mut self, hwnd: HWND) {
        let monitors = monitor::enumerate();
        let Some((actual_index, next_monitor)) = select_monitor(&monitors, self.configured_screen)
        else {
            warn!("display configuration changed; no monitor is available");
            self.cancel_obs_request(hwnd, "display configuration became unavailable");
            self.set_draw_mode(hwnd, false);
            self.projector_visible = false;
            let _ = self.projector_z_order.enforce(None, hwnd);
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            return;
        };

        if next_monitor == self.monitor {
            self.poll_projector(hwnd);
            return;
        }
        self.cancel_obs_request(hwnd, "target display changed");
        self.radial_menu = None;
        self.cancel_item_drag(hwnd);

        let (aspect_width, aspect_height) = self.canvas_aspect;
        let content_local = content_rect(
            next_monitor.width as f64,
            next_monitor.height as f64,
            aspect_width,
            aspect_height,
        );
        let content_screen = Rect {
            x: content_local.x + next_monitor.x as f64,
            y: content_local.y + next_monitor.y as f64,
            ..content_local
        };
        let positioned = unsafe {
            SetWindowPos(
                hwnd,
                None,
                next_monitor.x,
                next_monitor.y,
                next_monitor.width,
                next_monitor.height,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED,
            )
        };
        if let Err(error) = positioned {
            warn!("failed to resize overlay after display change: {error}");
            return;
        }

        info!(
            "display changed; monitor {} is now {}x{} at ({},{})",
            actual_index, next_monitor.width, next_monitor.height, next_monitor.x, next_monitor.y
        );
        self.monitor = next_monitor;
        self.content_local = content_local;
        self.content_screen = content_screen;
        self.projector_visible = false;
        self.projector_opened_by_us = false;
        self.renderer.take();
        self.last_renderer_recovery = None;
        match self.create_renderer_from_history() {
            Ok(renderer) => {
                self.renderer = Some(renderer);
                info!("graphics resources resized for the new display");
            }
            Err(error) => {
                self.last_renderer_recovery = Some(std::time::Instant::now());
                warn!("failed to recreate graphics resources after display change: {error:#}");
            }
        }
        self.poll_projector(hwnd);
    }

    fn handle_hotkey_change(&mut self, hwnd: HWND, request: &mut hotkey::ChangeRequest) {
        request.handled = true;
        let result = match &request.command {
            ChangeCommand::Prepare(config) => self.hotkey.prepare(config),
            ChangeCommand::Commit => self.hotkey.commit(),
            ChangeCommand::Rollback => self.hotkey.rollback(),
        };
        request.error = result.err().map(|error| format!("{error:#}"));
        if let Err(error) = tray::update_hotkey(hwnd, self.hotkey.active_display_name()) {
            warn!("failed to update tray hotkey label: {error:#}");
        }
    }

    /// hotkey / トレイからの切替。プロジェクター未表示なら obs-websocket で開く
    fn toggle_mode(&mut self, hwnd: HWND) {
        if settings::is_open() {
            return;
        }
        if self.draw_mode {
            self.set_draw_mode(hwnd, false);
            if self.close_projector
                && self.projector_opened_by_us
                && projector::close_projector(&self.monitor, hwnd)
            {
                info!("projector close requested");
                self.projector_opened_by_us = false;
            }
            return;
        }
        if self.overlay_enabled() {
            self.set_draw_mode(hwnd, true);
            return;
        }
        if self.obs.is_some() {
            self.request_projector(hwnd);
            return;
        }
        info!("toggle ignored: OBS projector is not visible");
    }

    /// obs-websocket でプロジェクターを開き、表示確認後に描画モードへ入る
    fn request_projector(&mut self, hwnd: HWND) {
        let Some(obs) = self.obs.clone() else { return };
        let Some(request) = self.obs_requests.begin(std::time::Instant::now()) else {
            return;
        };
        info!(
            "opening OBS projector via obs-websocket (generation={})...",
            request.generation
        );
        if unsafe { SetTimer(Some(hwnd), PENDING_TIMER_ID, PENDING_INTERVAL_MS, None) } == 0 {
            let _ = self.obs_requests.cancel();
            warn!(
                "could not start OBS projector deadline timer (generation={})",
                request.generation
            );
            return;
        }

        let mon = self.monitor;
        let result_tx = self.obs_result_tx.clone();
        let wake_enabled = std::sync::Arc::clone(&self.obs_worker_wake_enabled);
        // HWNDそのものはthread間で渡さず、wake message送信用のcopyable handle値だけを持つ。
        // 結果payloadはchannel側に置くため、messageにpointerは含めない。
        let hwnd_raw = hwnd.0 as isize;
        std::thread::spawn(move || {
            let outcome =
                obs::open_projector(&obs, mon.x, mon.y, mon.width, mon.height, request.deadline)
                    .map_err(|error| format!("{error:#}"));
            let delivered = result_tx
                .send(ObsWorkerResult {
                    generation: request.generation,
                    outcome,
                })
                .is_ok();
            if delivered && wake_enabled.load(std::sync::atomic::Ordering::Acquire) {
                // App終了後はreceiverがdropされるためsendが失敗し、stale HWNDへpostしない。
                // send成功とDestroyのraceでpostされてもpayloadなしのprivate messageなので安全。
                let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_OBS_RESULT, WPARAM(0), LPARAM(0));
                }
            }
        });
    }

    fn on_obs_results(&mut self, hwnd: HWND) {
        loop {
            let result = match self.obs_result_rx.try_recv() {
                Ok(result) => result,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            };
            self.on_obs_worker_result(hwnd, result);
        }
    }

    fn on_obs_worker_result(&mut self, hwnd: HWND, result: ObsWorkerResult) {
        let succeeded = result.outcome.is_ok();
        match self.obs_requests.worker_finished(
            std::time::Instant::now(),
            result.generation,
            succeeded,
        ) {
            WorkerDisposition::Ignored => {
                info!(
                    "ignored stale OBS projector result (generation={}, success={succeeded})",
                    result.generation
                );
            }
            WorkerDisposition::AwaitingProjector => {
                info!(
                    "OBS projector request succeeded (generation={}); waiting for window",
                    result.generation
                );
                self.poll_projector(hwnd);
                self.apply_pending_poll(hwnd);
            }
            WorkerDisposition::Failed(generation) => {
                self.stop_pending_timer(hwnd);
                warn!(
                    "OBS projector request failed (generation={generation}): {}",
                    result
                        .outcome
                        .expect_err("failed disposition requires a worker error")
                );
            }
            WorkerDisposition::TimedOut(generation) => {
                self.stop_pending_timer(hwnd);
                warn!(
                    "OBS projector request timed out (generation={generation}); late worker result ignored"
                );
            }
        }
    }

    fn on_pending_timer(&mut self, hwnd: HWND) {
        // wake messageが配送されなかった場合も、deadline timerをchannelのfallback drainにする。
        self.on_obs_results(hwnd);
        self.poll_projector(hwnd);
        self.apply_pending_poll(hwnd);
    }

    fn apply_pending_poll(&mut self, hwnd: HWND) {
        match self
            .obs_requests
            .poll(std::time::Instant::now(), self.projector_visible)
        {
            PollDisposition::Idle | PollDisposition::Waiting => {}
            PollDisposition::Ready(generation) => {
                info!("OBS projector became visible (generation={generation})");
                self.projector_opened_by_us = true;
                self.stop_pending_timer(hwnd);
                self.set_draw_mode(hwnd, true);
            }
            PollDisposition::TimedOut(generation) => {
                self.stop_pending_timer(hwnd);
                warn!(
                    "OBS projector request timed out before the window appeared (generation={generation})"
                );
            }
        }
    }

    fn stop_pending_timer(&self, hwnd: HWND) {
        unsafe {
            let _ = KillTimer(Some(hwnd), PENDING_TIMER_ID);
        }
    }

    fn cancel_obs_request(&mut self, hwnd: HWND, reason: &str) {
        if let Some(generation) = self.obs_requests.cancel() {
            info!("canceled OBS projector request (generation={generation}, reason={reason})");
            self.stop_pending_timer(hwnd);
        }
    }

    fn set_draw_mode(&mut self, hwnd: HWND, on: bool) {
        if self.draw_mode == on {
            return;
        }
        let t = std::time::Instant::now();
        self.draw_mode = on;
        self.set_layer_clear_hotkey_enabled(hwnd, self.draw_mode);
        if !self.draw_mode {
            self.frame_gate.cancel();
            self.radial_menu = None;
            let deselected = self.item_selection.take().is_some();
            hotkey::unregister_transform_escape(hwnd);
            // 描画中に切り替えた場合はストロークを破棄する
            let had_active_input = self.engine.is_drawing();
            let msgs = self.engine.cancel();
            if !msgs.is_empty() {
                self.web.send_all(msgs);
            }
            if had_active_input {
                unsafe {
                    let _ = KillTimer(Some(hwnd), FLUSH_TIMER_ID);
                }
            }
            if deselected {
                self.rebuild();
            }
        }
        set_transparent(hwnd, !self.draw_mode);
        let after_style = t.elapsed();
        if self.draw_mode {
            self.render();
        } else {
            let result = self
                .renderer
                .as_mut()
                .ok_or_else(|| anyhow!("renderer is unavailable"))
                .and_then(Renderer::clear_frame);
            if let Err(error) = result {
                self.recover_renderer("clear_frame", error);
            }
        }
        info!(
            "draw mode: {} (style: {:?}, total: {:?})",
            self.draw_mode,
            after_style,
            t.elapsed()
        );
    }

    /// lparam のスクリーン座標を正規化座標へ
    fn pointer_uv(&self, lparam: LPARAM) -> (f64, f64) {
        let (x, y) = pointer_screen(lparam);
        self.content_screen.normalize(x, y)
    }

    fn begin_item_drag(&mut self, hwnd: HWND, pointer_id: u32, u: f64, v: f64) {
        let aspect = self.content_screen.width / self.content_screen.height;
        let handle_radius = (9.0 / self.content_screen.height).max(0.006);
        let selected_hit = self.item_selection.as_ref().and_then(|selection| {
            selection_handle_at(&selection.item, (u, v), aspect, handle_radius)
                .map(|handle| (selection.item.clone(), handle))
        });
        let topmost = self.engine.transformable_at(u, v, aspect, handle_radius);
        let hit = match selected_hit {
            // 選択枠の明示handleはitem本体より外にもあるため最優先する。
            Some((item, handle @ (TransformHandle::Scale(_) | TransformHandle::Rotate))) => {
                Some((item, handle))
            }
            Some((selected, TransformHandle::Move)) => match topmost {
                Some(item) if item.item_id() != selected.item_id() => {
                    Some((item, TransformHandle::Move))
                }
                _ => Some((selected, TransformHandle::Move)),
            },
            None => topmost.map(|item| (item, TransformHandle::Move)),
        };

        let Some((item, handle)) = hit else {
            if self.item_selection.take().is_some() {
                self.rebuild();
                self.render();
            }
            return;
        };
        let Some(interaction) = TransformInteraction::begin(&item, handle, (u, v), aspect) else {
            return;
        };
        let selected_item_id = item.item_id().to_owned();
        if !self.engine.begin_item_transform(item.item_id(), aspect) {
            return;
        }
        let changed_selection = self
            .item_selection
            .as_ref()
            .is_none_or(|selection| selection.item.item_id() != item.item_id());
        self.item_selection = Some(ItemSelection {
            item,
            drag: Some(ItemDrag {
                pointer_id,
                interaction,
            }),
        });
        if changed_selection {
            self.prepare_item_transform(&selected_item_id);
        }
        if let Err(error) = hotkey::register_transform_escape(hwnd) {
            warn!("failed to register transform Escape hotkey: {error:#}");
        }
        unsafe {
            SetTimer(Some(hwnd), FLUSH_TIMER_ID, FLUSH_INTERVAL_MS, None);
        }
        self.render();
    }

    /// 選択transformを更新した場合は true。対象ポインタのメッセージを消費する。
    fn update_item_drag(&mut self, pointer_id: u32, lparam: LPARAM) -> bool {
        let pointer = self.pointer_uv(lparam);
        let pending = {
            let Some(selection) = self.item_selection.as_ref() else {
                return false;
            };
            let Some(drag) = selection
                .drag
                .as_ref()
                .filter(|drag| drag.pointer_id == pointer_id)
            else {
                return false;
            };
            drag.interaction
                .update(pointer)
                .map(|transform| (selection.item.item_id().to_owned(), transform))
        };
        if let Some((item_id, transform)) = pending {
            if let Some(applied) = self.engine.preview_item_transform(&item_id, transform) {
                if let Some(selection) = self.item_selection.as_mut() {
                    let _ = apply_item_transform(&mut selection.item, applied);
                }
                self.request_render();
            }
        }
        let _ = self.update_selection_cursor();
        true
    }

    fn finish_item_drag(&mut self, hwnd: HWND, pointer_id: u32, lparam: LPARAM) -> bool {
        if !self.update_item_drag(pointer_id, lparam) {
            return false;
        }
        let item_id = {
            let Some(selection) = self.item_selection.as_mut() else {
                return false;
            };
            let Some(drag) = selection.drag.take() else {
                return false;
            };
            debug_assert_eq!(drag.pointer_id, pointer_id);
            selection.item.item_id().to_owned()
        };

        let messages = self.engine.end_item_transform(now_ms());
        if !messages.is_empty() {
            self.web.send_all(messages);
        }
        hotkey::unregister_transform_escape(hwnd);
        unsafe {
            let _ = KillTimer(Some(hwnd), FLUSH_TIMER_ID);
        }
        if let Some(item) = self.engine.transformable_by_id(&item_id) {
            if let Some(selection) = self.item_selection.as_mut() {
                selection.item = item;
            }
        } else {
            self.item_selection = None;
            self.rebuild();
        }
        self.render();
        true
    }

    fn cancel_item_drag(&mut self, hwnd: HWND) -> bool {
        let item_id = {
            let Some(selection) = self.item_selection.as_mut() else {
                return false;
            };
            if selection.drag.take().is_none() {
                return false;
            }
            selection.item.item_id().to_owned()
        };
        let messages = self.engine.cancel();
        if !messages.is_empty() {
            self.web.send_all(messages);
        }
        if let Some(item) = self.engine.transformable_by_id(&item_id) {
            if let Some(selection) = self.item_selection.as_mut() {
                selection.item = item;
            }
        }
        hotkey::unregister_transform_escape(hwnd);
        unsafe {
            let _ = KillTimer(Some(hwnd), FLUSH_TIMER_ID);
        }
        self.render();
        true
    }

    /// 通常のStroke / Shapeをキャンセルし、ローカルとBrowser Sourceを同じ状態へ戻す。
    fn cancel_drawing_for_message(&mut self, hwnd: HWND, message: u32, wparam: WPARAM) -> bool {
        let Some(cancellation) = drawing_cancellation(message, wparam) else {
            return false;
        };
        let messages = match cancellation {
            DrawingCancellation::Pointer(pointer_id) => self.engine.cancel_pointer(pointer_id),
            DrawingCancellation::AnyPointer if self.engine.has_pointer_session() => {
                self.engine.cancel()
            }
            DrawingCancellation::AnyPointer => return false,
        };
        if messages.is_empty() {
            return false;
        }
        self.web.send_all(messages);
        unsafe {
            let _ = KillTimer(Some(hwnd), FLUSH_TIMER_ID);
        }
        if self.engine.take_rebuild_required() {
            self.rebuild();
        }
        self.render();
        true
    }

    fn begin_radial_menu(&mut self, pointer_id: u32, lparam: LPARAM) {
        if !self.draw_mode
            || self.engine.is_drawing()
            || self.item_drag_active()
            || self.radial_menu.is_some()
        {
            return;
        }
        let screen = pointer_screen(lparam);
        let local = (
            (screen.0 - f64::from(self.monitor.x)) as f32,
            (screen.1 - f64::from(self.monitor.y)) as f32,
        );
        let scale = radial_menu::scale_for_menu(
            self.monitor.width as u32,
            self.monitor.height as u32,
            self.stamps.len(),
        );
        let layers = self.radial_layer_entries();
        self.radial_menu = Some(RadialMenu::new_with_layers(
            pointer_id,
            screen,
            local,
            (self.monitor.width as u32, self.monitor.height as u32),
            scale,
            self.stamps.len(),
            (
                self.engine.can_undo(),
                self.engine.can_redo(),
                self.engine.can_clear(),
            ),
            layers,
            self.engine.active_layer_id().to_owned(),
        ));
        self.render();
    }

    fn begin_radial_click(&mut self, pointer_id: u32, lparam: LPARAM) -> bool {
        let changed = {
            let Some(menu) = self.radial_menu.as_mut() else {
                return false;
            };
            if !menu.begin_click(pointer_id) {
                return false;
            }
            menu.update(pointer_screen(lparam))
        };
        if changed {
            self.render();
        }
        true
    }

    fn update_radial_menu(&mut self, pointer_id: u32, lparam: LPARAM) -> bool {
        let Some(menu) = self.radial_menu.as_mut() else {
            return false;
        };
        if !menu.accepts_update_from(pointer_id) {
            return false;
        }
        if menu.update(pointer_screen(lparam)) {
            self.request_render();
        }
        true
    }

    fn release_radial_menu(&mut self, pointer_id: u32, lparam: LPARAM) -> Option<RadialRelease> {
        let release = {
            let menu = self.radial_menu.as_mut()?;
            if !menu.owns_pointer(pointer_id) {
                return None;
            }
            menu.release(pointer_screen(lparam))
        };
        if !release.keeps_menu_open() {
            self.radial_menu.take();
        }
        self.render();
        Some(release)
    }

    fn cancel_radial_interaction(&mut self) -> bool {
        let keep_pinned = {
            let Some(menu) = self.radial_menu.as_mut() else {
                return false;
            };
            if !menu.has_active_pointer() {
                return false;
            }
            menu.cancel_active_pointer();
            menu.is_pinned()
        };
        if !keep_pinned {
            self.radial_menu.take();
        }
        self.render();
        true
    }

    fn dismiss_radial_menu(&mut self) -> bool {
        if self.radial_menu.take().is_none() {
            return false;
        }
        self.render();
        true
    }

    fn radial_menu_owns(&self, pointer_id: u32) -> bool {
        self.radial_menu
            .as_ref()
            .is_some_and(|menu| menu.owns_pointer(pointer_id))
    }

    fn radial_menu_is_pinned(&self) -> bool {
        self.radial_menu.as_ref().is_some_and(RadialMenu::is_pinned)
    }

    fn sync_radial_history(&mut self) {
        let can_undo = self.engine.can_undo();
        let can_redo = self.engine.can_redo();
        let can_clear = self.engine.can_clear();
        let layers = self.radial_layer_entries();
        let active_layer_id = self.engine.active_layer_id().to_owned();
        let changed = self.radial_menu.as_mut().is_some_and(|menu| {
            let commands = menu.set_command_availability(can_undo, can_redo, can_clear);
            let layers = menu.set_layers(layers, active_layer_id);
            commands || layers
        });
        if changed {
            self.render();
        }
    }

    fn radial_layer_entries(&self) -> Vec<RadialLayerEntry> {
        let layers = self.engine.layers();
        let items = self.engine.shared_items();
        let items = items.lock().unwrap();
        layers
            .into_iter()
            .map(|layer| RadialLayerEntry {
                item_count: items
                    .iter()
                    .filter(|item| item.layer_id() == layer.layer_id)
                    .count(),
                layer_id: layer.layer_id,
                name: layer.name,
            })
            .collect()
    }

    fn on_pointer_down(&mut self, hwnd: HWND, pointer_id: u32, lparam: LPARAM) {
        if !self.draw_mode || self.engine.is_drawing() || self.item_drag_active() {
            return;
        }
        let x = (lparam.0 & 0xffff) as i16 as f64;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f64;
        let (u, v) = self.content_screen.normalize(x, y);
        if self.tool == DrawTool::Select {
            self.begin_item_drag(hwnd, pointer_id, u, v);
            return;
        }
        // 黒帯への誤描画防止（選択handleだけはcontent外へ出る場合があるため除外）。
        if !self.content_screen.contains(x, y) {
            return;
        }
        let canvas_aspect = self.content_screen.width / self.content_screen.height;
        let msgs = match self.tool.clone() {
            DrawTool::Select => unreachable!("select is handled before drawing"),
            DrawTool::Pen | DrawTool::Marker | DrawTool::Eraser => {
                let Some(brush) = self.current_brush() else {
                    return;
                };
                let dynamics = pointer::sample(pointer_id).dynamics;
                self.engine
                    .begin_with_dynamics(pointer_id, brush, u, v, dynamics, now_ms())
            }
            DrawTool::Line => self.engine.begin_shape(
                pointer_id,
                ShapeKind::Line,
                self.current_line_style(),
                u,
                v,
                canvas_aspect,
            ),
            DrawTool::Arrow => self.engine.begin_shape(
                pointer_id,
                ShapeKind::Arrow,
                self.current_line_style(),
                u,
                v,
                canvas_aspect,
            ),
            DrawTool::Rectangle => self.engine.begin_shape(
                pointer_id,
                ShapeKind::Rectangle,
                self.current_line_style(),
                u,
                v,
                canvas_aspect,
            ),
            DrawTool::Ellipse => self.engine.begin_shape(
                pointer_id,
                ShapeKind::Ellipse,
                self.current_line_style(),
                u,
                v,
                canvas_aspect,
            ),
            DrawTool::Stamp(stamp_id) => {
                let Some(stamp) = self
                    .stamps
                    .iter()
                    .find(|stamp| stamp.id == stamp_id)
                    .cloned()
                else {
                    warn!("selected stamp is no longer registered: {stamp_id}");
                    return;
                };
                let aspect = f64::from(stamp.width_px) / f64::from(stamp.height_px);
                let width_n = stamp.default_height_n * aspect * self.content_screen.height
                    / self.content_screen.width;
                let msgs = self.engine.add_stamp(
                    stamp.id,
                    (u, v),
                    width_n,
                    stamp.default_height_n,
                    stamp.opacity,
                    now_ms(),
                );
                self.web.send_all(msgs);
                if self.engine.take_rebuild_required() {
                    self.rebuild();
                } else {
                    self.bake_last_done();
                }
                self.render();
                return;
            }
        };
        self.web.send_all(msgs);
        if self.engine.take_rebuild_required() {
            self.rebuild();
        }
        unsafe {
            SetTimer(Some(hwnd), FLUSH_TIMER_ID, FLUSH_INTERVAL_MS, None);
        }
        // active scratch と cursor を入力開始時に確立する。以降の update はframe集約。
        self.render();
    }

    fn on_pointer_update(&mut self, hwnd: HWND, pointer_id: u32, lparam: LPARAM) {
        if self.update_item_drag(pointer_id, lparam) {
            return;
        }
        if self.item_drag_active() {
            return;
        }
        if !self.engine.owns_pointer(pointer_id) {
            return;
        }
        let (u, v) = self.pointer_uv(lparam);
        let dynamics = pointer::sample(pointer_id).dynamics;
        let msgs = self
            .engine
            .move_to_with_dynamics(pointer_id, u, v, dynamics, now_ms());
        let trimmed = self.engine.take_rebuild_required();
        if !msgs.is_empty() {
            // 総点数上限による強制確定
            self.web.send_all(msgs);
            unsafe {
                let _ = KillTimer(Some(hwnd), FLUSH_TIMER_ID);
            }
            if trimmed {
                self.rebuild();
            } else {
                self.bake_last_done();
            }
        } else if trimmed {
            self.rebuild();
        }
        self.request_render();
    }

    fn on_pointer_up(&mut self, hwnd: HWND, pointer_id: u32, lparam: LPARAM) {
        if self.finish_item_drag(hwnd, pointer_id, lparam) {
            return;
        }
        if self.item_drag_active() {
            return;
        }
        if !self.engine.owns_pointer(pointer_id) {
            return;
        }
        let msgs = self.engine.end(pointer_id, now_ms());
        self.web.send_all(msgs);
        unsafe {
            let _ = KillTimer(Some(hwnd), FLUSH_TIMER_ID);
        }
        if self.engine.take_rebuild_required() {
            self.rebuild();
        } else {
            self.bake_last_done();
        }
        self.render();
    }

    fn on_flush_timer(&mut self) {
        let msgs = self.engine.flush();
        self.web.send_all(msgs);
    }
}

fn apply_menu_result(hwnd: HWND, app_ptr: *mut App, action: Option<MenuAction>) {
    let current = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut App;
    if current != app_ptr {
        return;
    }
    let exit =
        action.is_some_and(|action| unsafe { (&mut *app_ptr).apply_menu_action(hwnd, action) });
    if exit {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    } else {
        let app = unsafe { &mut *app_ptr };
        app.sync_radial_history();
        // popupが消えた場合と固定メニューが残る場合の双方でZ-orderを再同期する。
        app.poll_projector(hwnd);
    }
}

/// TrackPopupMenu の内部ループ中は App の参照を保持しない。
fn show_legacy_menu(hwnd: HWND, app_ptr: *mut App) {
    let menu_input = {
        let app = unsafe { &*app_ptr };
        (!app.engine.is_drawing() && !app.item_drag_active()).then(|| {
            (
                app.tool.clone(),
                app.color.clone(),
                app.stamps.clone(),
                app.engine.can_undo(),
                app.engine.can_redo(),
                app.engine.can_clear(),
                app.confirm_before_clear,
                app.engine.layers(),
                app.radial_layer_entries()
                    .into_iter()
                    .map(|layer| layer.item_count)
                    .collect::<Vec<_>>(),
                app.engine.active_layer_id().to_owned(),
            )
        })
    };
    let Some((
        tool,
        color,
        stamps,
        can_undo,
        can_redo,
        can_clear,
        confirm_before_clear,
        layers,
        layer_counts,
        active_layer_id,
    )) = menu_input
    else {
        return;
    };
    let action = menu::show(
        hwnd,
        &tool,
        &color,
        &stamps,
        menu::LayerMenuState {
            layers: &layers,
            item_counts: &layer_counts,
            active_layer_id: &active_layer_id,
        },
        menu::CommandMenuState {
            can_undo,
            can_redo,
            can_clear,
            confirm_before_clear,
        },
    );
    apply_menu_result(hwnd, app_ptr, action);
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let app_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut App;
    if app_ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }

    match msg {
        WM_HOTKEY if wparam.0 as i32 == hotkey::LAYER_CLEAR_ID => {
            let action = {
                let app = unsafe { &*app_ptr };
                app.can_handle_layer_clear_hotkey()
                    .then(|| MenuAction::ClearLayer(app.engine.active_layer_id().to_owned()))
            };
            if action.is_some() {
                apply_menu_result(hwnd, app_ptr, action);
            }
            LRESULT(0)
        }
        WM_HOTKEY if wparam.0 as i32 == hotkey::TRANSFORM_ESCAPE_ID => {
            unsafe { &mut *app_ptr }.cancel_item_drag(hwnd);
            LRESULT(0)
        }
        WM_HOTKEY if unsafe { &*app_ptr }.hotkey.handles_message(wparam.0 as i32) => {
            // popup menu の内部ループへ届いた hotkey は、メニューを閉じずに背後の
            // overlay 状態だけ変えることになるため無視する。
            if !projector::foreground_ui_active() {
                unsafe { &mut *app_ptr }.toggle_mode(hwnd);
            }
            LRESULT(0)
        }
        WM_SETCURSOR if unsafe { &*app_ptr }.update_selection_cursor() => LRESULT(1),
        hotkey::WM_HOTKEY_CHANGE => {
            hotkey::with_change_request(|request| {
                unsafe { &mut *app_ptr }.handle_hotkey_change(hwnd, request);
            });
            LRESULT(0)
        }
        // パススルー中は「処理済み」にせず DefWindowProc に流す (握りつぶすと
        // 下のウィンドウへ届かない)。描画モード中のみ自分で処理する
        WM_POINTERDOWN => {
            if !unsafe { &*app_ptr }.draw_mode {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            // wparam の HIWORD にボタン種別フラグが入る (POINTER_MESSAGE_FLAG_*)
            let flags = pointer_flags(wparam);
            let id = pointer_id(wparam);
            if unsafe { &*app_ptr }.radial_menu_is_pinned() {
                if flags & (POINTER_MESSAGE_FLAG_FIRSTBUTTON | POINTER_MESSAGE_FLAG_SECONDBUTTON)
                    != 0
                {
                    unsafe { &mut *app_ptr }.begin_radial_click(id, lparam);
                }
            } else if flags & POINTER_MESSAGE_FLAG_SECONDBUTTON != 0 {
                unsafe { &mut *app_ptr }.begin_radial_menu(id, lparam);
            } else if flags & POINTER_MESSAGE_FLAG_FIRSTBUTTON != 0 {
                unsafe { &mut *app_ptr }.on_pointer_down(hwnd, id, lparam);
            }
            LRESULT(0)
        }
        WM_POINTERUPDATE => {
            let id = pointer_id(wparam);
            if pointer_flags(wparam) & POINTER_MESSAGE_FLAG_CANCELED != 0
                && unsafe { &*app_ptr }.radial_menu_owns(id)
            {
                unsafe { &mut *app_ptr }.cancel_radial_interaction();
                LRESULT(0)
            } else if pointer_flags(wparam) & POINTER_MESSAGE_FLAG_CANCELED != 0
                && unsafe { &*app_ptr }.item_drag_owns(id)
            {
                unsafe { &mut *app_ptr }.cancel_item_drag(hwnd);
                LRESULT(0)
            } else if unsafe { &mut *app_ptr }.cancel_drawing_for_message(hwnd, msg, wparam)
                || unsafe { &mut *app_ptr }.update_radial_menu(id, lparam)
            {
                LRESULT(0)
            } else if unsafe { &*app_ptr }.engine.owns_pointer(id)
                || unsafe { &*app_ptr }.item_drag_owns(id)
            {
                unsafe { &mut *app_ptr }.on_pointer_update(hwnd, id, lparam);
                LRESULT(0)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_POINTERUP => {
            let id = pointer_id(wparam);
            if pointer_flags(wparam) & POINTER_MESSAGE_FLAG_CANCELED != 0
                && unsafe { &*app_ptr }.radial_menu_owns(id)
            {
                unsafe { &mut *app_ptr }.cancel_radial_interaction();
                LRESULT(0)
            } else if pointer_flags(wparam) & POINTER_MESSAGE_FLAG_CANCELED != 0
                && unsafe { &*app_ptr }.item_drag_owns(id)
            {
                unsafe { &mut *app_ptr }.cancel_item_drag(hwnd);
                LRESULT(0)
            } else if unsafe { &mut *app_ptr }.cancel_drawing_for_message(hwnd, msg, wparam) {
                LRESULT(0)
            } else if let Some(release) = unsafe { &mut *app_ptr }.release_radial_menu(id, lparam) {
                match release {
                    RadialRelease::Action { action, .. } => {
                        apply_menu_result(hwnd, app_ptr, Some(action));
                    }
                    RadialRelease::Stamp(index) => {
                        let action = unsafe { &*app_ptr }
                            .stamps
                            .get(index)
                            .map(|stamp| MenuAction::SelectTool(DrawTool::Stamp(stamp.id.clone())));
                        apply_menu_result(hwnd, app_ptr, action);
                    }
                    RadialRelease::LegacyMenu => show_legacy_menu(hwnd, app_ptr),
                    RadialRelease::Pin | RadialRelease::StayOpen => {
                        unsafe { &mut *app_ptr }.poll_projector(hwnd);
                    }
                    RadialRelease::Cancel => unsafe { &mut *app_ptr }.poll_projector(hwnd),
                }
                LRESULT(0)
            } else if unsafe { &*app_ptr }.engine.owns_pointer(id)
                || unsafe { &*app_ptr }.item_drag_owns(id)
            {
                unsafe { &mut *app_ptr }.on_pointer_up(hwnd, id, lparam);
                LRESULT(0)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_POINTERCAPTURECHANGED => {
            let id = pointer_id(wparam);
            if (unsafe { &*app_ptr }.radial_menu_owns(id)
                && unsafe { &mut *app_ptr }.cancel_radial_interaction())
                || (unsafe { &*app_ptr }.item_drag_owns(id)
                    && unsafe { &mut *app_ptr }.cancel_item_drag(hwnd))
                || unsafe { &mut *app_ptr }.cancel_drawing_for_message(hwnd, msg, wparam)
            {
                LRESULT(0)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_CAPTURECHANGED => {
            if unsafe { &mut *app_ptr }.cancel_radial_interaction()
                || unsafe { &mut *app_ptr }.cancel_item_drag(hwnd)
                || unsafe { &mut *app_ptr }.cancel_drawing_for_message(hwnd, msg, wparam)
            {
                LRESULT(0)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_CANCELMODE => {
            if unsafe { &mut *app_ptr }.dismiss_radial_menu()
                || unsafe { &mut *app_ptr }.cancel_item_drag(hwnd)
                || unsafe { &mut *app_ptr }.cancel_drawing_for_message(hwnd, msg, wparam)
            {
                LRESULT(0)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_TRAY => {
            // tray popup も内部メッセージループを持つため、先に結果だけ取得する。
            let command = tray::on_message(
                hwnd,
                (lparam.0 & 0xffff) as u32,
                unsafe { &*app_ptr }.hotkey.active_display_name(),
                unsafe { &*app_ptr }.web.diagnostics().snapshot(),
            );
            let current = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut App;
            if current != app_ptr {
                return LRESULT(0);
            }
            match command {
                Some(TrayCommand::ToggleMode) => {
                    unsafe { &mut *app_ptr }.toggle_mode(hwnd);
                    unsafe { &mut *app_ptr }.poll_projector(hwnd);
                }
                Some(TrayCommand::CopyOverlayUrl) => {
                    let overlay_url = unsafe { &*app_ptr }.web.overlay_url().to_owned();
                    if let Err(error) = crate::win::clipboard::copy_text(hwnd, &overlay_url) {
                        warn!("copy overlay URL: {error:#}");
                        crate::win::message_box(&format!(
                            "OBS Browser Source URLをコピーできません:\n{error:#}"
                        ));
                    }
                    unsafe { &mut *app_ptr }.poll_projector(hwnd);
                }
                Some(TrayCommand::Settings) => {
                    let app = unsafe { &mut *app_ptr };
                    app.cancel_obs_request(hwnd, "settings opened");
                    app.set_draw_mode(hwnd, false);
                    let diagnostics = unsafe { &*app_ptr }.web.diagnostics();
                    if let Err(error) = settings::open(hwnd, Some(diagnostics)) {
                        warn!("settings: {error:#}");
                        crate::win::message_box(&format!("設定画面を開けません:\n{error:#}"));
                    }
                    unsafe { &mut *app_ptr }.poll_projector(hwnd);
                }
                Some(TrayCommand::Logs) => {
                    unsafe { &mut *app_ptr }.set_draw_mode(hwnd, false);
                    let result = crate::win::logging::log_directory()
                        .ok_or_else(|| anyhow!("ログフォルダーを取得できません"))
                        .and_then(|path| crate::win::open_path(hwnd, &path));
                    if let Err(error) = result {
                        warn!("logs: {error:#}");
                        crate::win::message_box(&format!("ログフォルダーを開けません:\n{error:#}"));
                    }
                    unsafe { &mut *app_ptr }.poll_projector(hwnd);
                }
                Some(TrayCommand::Licenses) => {
                    let licenses_url = {
                        let app = unsafe { &mut *app_ptr };
                        app.set_draw_mode(hwnd, false);
                        app.web.licenses_url().to_owned()
                    };
                    if let Err(error) = crate::win::open_url(hwnd, &licenses_url) {
                        warn!("licenses: {error:#}");
                        crate::win::message_box(&format!(
                            "第三者ライセンスを開けません:\n{error:#}"
                        ));
                    }
                    unsafe { &mut *app_ptr }.poll_projector(hwnd);
                }
                Some(TrayCommand::Exit) => unsafe {
                    let _ = DestroyWindow(hwnd);
                },
                None => unsafe { &mut *app_ptr }.poll_projector(hwnd),
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == FLUSH_TIMER_ID => {
            unsafe { &mut *app_ptr }.on_flush_timer();
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == PROJECTOR_TIMER_ID => {
            unsafe { &mut *app_ptr }.poll_projector(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == PENDING_TIMER_ID => {
            unsafe { &mut *app_ptr }.on_pending_timer(hwnd);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            unsafe { &mut *app_ptr }.on_display_change(hwnd);
            LRESULT(0)
        }
        WM_OBS_RESULT => {
            unsafe { &mut *app_ptr }.on_obs_results(hwnd);
            LRESULT(0)
        }
        WM_PAINT => {
            // BeginPaint/EndPaintでupdate regionを必ずvalidateする。即時renderが予約を
            // 包含済みならFrameGate::takeはfalseとなり、このpaintはno-opになる。
            let mut paint = PAINTSTRUCT::default();
            let _ = unsafe { BeginPaint(hwnd, &mut paint) };
            unsafe { &mut *app_ptr }.on_frame_request();
            let _ = unsafe { EndPaint(hwnd, &paint) };
            LRESULT(0)
        }
        WM_DESTROY => {
            tray::remove(hwnd);
            let app = unsafe { &mut *app_ptr };
            app.set_layer_clear_hotkey_enabled(hwnd, false);
            app.obs_worker_wake_enabled
                .store(false, std::sync::atomic::Ordering::Release);
            if let Some(generation) = app.obs_requests.cancel() {
                info!("canceled OBS projector request on exit (generation={generation})");
            }
            app.stop_pending_timer(hwnd);
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(app_ptr));
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, PM_REMOVE};

    const WM_FRAME_TEST_UPDATE: u32 = WM_APP + 0x500;
    static FRAME_TEST_UPDATES: AtomicUsize = AtomicUsize::new(0);
    static FRAME_TEST_PAINTS: AtomicUsize = AtomicUsize::new(0);
    static FRAME_TEST_UPDATES_AT_FIRST_PAINT: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "system" fn frame_test_window_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_FRAME_TEST_UPDATE => {
                FRAME_TEST_UPDATES.fetch_add(1, Ordering::SeqCst);
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                LRESULT(0)
            }
            WM_PAINT => {
                if FRAME_TEST_PAINTS.fetch_add(1, Ordering::SeqCst) == 0 {
                    FRAME_TEST_UPDATES_AT_FIRST_PAINT
                        .store(FRAME_TEST_UPDATES.load(Ordering::SeqCst), Ordering::SeqCst);
                }
                let mut paint = PAINTSTRUCT::default();
                let _ = unsafe { BeginPaint(hwnd, &mut paint) };
                let _ = unsafe { EndPaint(hwnd, &paint) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    fn pointer_wparam(pointer_id: u32, flags: u32) -> WPARAM {
        WPARAM((pointer_id as usize) | ((flags as usize) << 16))
    }

    fn test_monitor(x: i32, primary: bool) -> Monitor {
        Monitor {
            x,
            y: 0,
            width: 1920,
            height: 1080,
            primary,
        }
    }

    #[test]
    fn layer_clear_hotkey_is_only_allowed_while_canvas_input_is_idle() {
        assert!(layer_clear_hotkey_allowed(true, false, false, false, false));
        assert!(!layer_clear_hotkey_allowed(
            false, false, false, false, false
        ));
        assert!(!layer_clear_hotkey_allowed(true, true, false, false, false));
        assert!(!layer_clear_hotkey_allowed(true, false, true, false, false));
        assert!(!layer_clear_hotkey_allowed(true, false, false, true, false));
        assert!(!layer_clear_hotkey_allowed(true, false, false, false, true));
    }

    #[test]
    fn monitor_selection_falls_back_to_the_first_monitor() {
        let monitors = [test_monitor(0, true), test_monitor(1920, false)];
        assert_eq!(select_monitor(&monitors, 1), Some((1, monitors[1])));
        assert_eq!(select_monitor(&monitors, 9), Some((0, monitors[0])));
        assert_eq!(select_monitor(&[], 0), None);
    }

    #[test]
    fn freehand_tools_serialize_their_pointer_dynamics_tuning() {
        let pen = brush_for_tool(&DrawTool::Pen, "#123456", 0.01).unwrap();
        assert_eq!(pen.tool, Tool::Pen);
        assert!(pen.pressure_width);
        assert_eq!(pen.pressure_min, 0.2);
        assert!(!pen.tilt_width);

        let marker = brush_for_tool(&DrawTool::Marker, "#123456", 0.01).unwrap();
        assert_eq!(marker.tool, Tool::Marker);
        assert!(marker.pressure_width);
        assert_eq!(marker.pressure_min, 0.65);
        assert!(marker.tilt_width);
        assert_eq!(marker.tilt_max_scale, 1.75);

        let eraser = brush_for_tool(&DrawTool::Eraser, "#123456", 0.01).unwrap();
        assert_eq!(eraser.tool, Tool::Eraser);
        assert!(!eraser.pressure_width);
        assert!(!eraser.tilt_width);
        assert_eq!(eraser.pressure_min, 1.0);
        assert_eq!(eraser.tilt_max_scale, 1.0);
    }

    #[test]
    fn frame_gate_coalesces_pointer_updates_until_the_frame_is_taken() {
        let mut gate = FrameGate::default();
        assert!(gate.request());
        for _ in 0..1_000 {
            assert!(!gate.request());
        }
        assert!(gate.take());
        assert!(!gate.take());
        assert!(gate.request());
        gate.cancel();
        assert!(!gate.take());
    }

    #[test]
    fn clear_confirmation_policy_requires_both_setting_and_content() {
        assert!(clear_confirmation_required(true, true));
        assert!(!clear_confirmation_required(true, false));
        assert!(!clear_confirmation_required(false, true));
        assert!(!clear_confirmation_required(false, false));
    }

    #[test]
    fn low_priority_paint_runs_after_queued_updates_and_coalesces_invalidations() -> Result<()> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class_name = w!("stream-painter-frame-coalescing-test");
            let class = WNDCLASSW {
                lpfnWndProc: Some(frame_test_window_proc),
                hInstance: instance.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&class), 0);
            let hwnd = CreateWindowExW(
                Default::default(),
                class_name,
                w!("StreamPainter frame coalescing test"),
                WS_POPUP,
                -10_000,
                -10_000,
                64,
                64,
                None,
                None,
                Some(instance.into()),
                None,
            )?;

            // 非表示windowにはWM_PAINTが生成されないため、画面外で表示し初期paintを
            // drainしてから今回のframe予約を計測する。
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let mut message = MSG::default();
            while PeekMessageW(&mut message, Some(hwnd), 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }

            // 表示直後の初期paintは今回のframe予約とは無関係なので除外する。
            FRAME_TEST_UPDATES.store(0, Ordering::SeqCst);
            FRAME_TEST_PAINTS.store(0, Ordering::SeqCst);
            FRAME_TEST_UPDATES_AT_FIRST_PAINT.store(0, Ordering::SeqCst);

            for _ in 0..1_000 {
                PostMessageW(Some(hwnd), WM_FRAME_TEST_UPDATE, WPARAM(0), LPARAM(0))?;
            }

            while PeekMessageW(&mut message, Some(hwnd), 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            let _ = DestroyWindow(hwnd);
        }

        assert_eq!(FRAME_TEST_UPDATES.load(Ordering::SeqCst), 1_000);
        assert_eq!(
            FRAME_TEST_UPDATES_AT_FIRST_PAINT.load(Ordering::SeqCst),
            1_000
        );
        assert_eq!(FRAME_TEST_PAINTS.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn win32_cancel_messages_preserve_pointer_scope() {
        let owner = 42;
        let canceled = pointer_wparam(owner, POINTER_MESSAGE_FLAG_CANCELED);
        let ordinary = pointer_wparam(owner, 0);

        assert_eq!(
            drawing_cancellation(WM_POINTERUPDATE, canceled),
            Some(DrawingCancellation::Pointer(owner))
        );
        assert_eq!(
            drawing_cancellation(WM_POINTERUP, canceled),
            Some(DrawingCancellation::Pointer(owner))
        );
        assert_eq!(drawing_cancellation(WM_POINTERUPDATE, ordinary), None);
        assert_eq!(drawing_cancellation(WM_POINTERUP, ordinary), None);
        assert_eq!(
            drawing_cancellation(WM_POINTERCAPTURECHANGED, ordinary),
            Some(DrawingCancellation::Pointer(owner))
        );
        assert_eq!(
            drawing_cancellation(WM_CAPTURECHANGED, ordinary),
            Some(DrawingCancellation::AnyPointer)
        );
        assert_eq!(
            drawing_cancellation(WM_CANCELMODE, ordinary),
            Some(DrawingCancellation::AnyPointer)
        );
    }
}
