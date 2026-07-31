//! オーバーレイウィンドウ・入力・アプリ統合 (docs/painter.md)。
//!
//! - WS_EX_NOREDIRECTIONBITMAP + WS_EX_TOPMOST + WS_EX_NOACTIVATE + WS_EX_TOOLWINDOW
//! - F9 (グローバルホットキー) でパススルー ⇔ 描画モードを切替 (WS_EX_TRANSPARENT)
//! - WM_POINTER* で入力を受け、CanvasEngine → local web hub + ローカルエコー描画
//! - 20ms タイマで stroke_points をバッチ送信

use anyhow::{anyhow, Context, Result};
use tracing::{info, warn};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT, VK_F9,
};
use windows::Win32::UI::Input::Pointer::EnableMouseInPointer;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, KillTimer, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassW,
    SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    TranslateMessage, GWLP_USERDATA, GWL_EXSTYLE, HWND_TOPMOST, IDC_CROSS, LWA_ALPHA, MSG,
    POINTER_MESSAGE_FLAG_CANCELED, POINTER_MESSAGE_FLAG_FIRSTBUTTON,
    POINTER_MESSAGE_FLAG_SECONDBUTTON, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SW_HIDE, SW_SHOWNOACTIVATE, WM_APP, WM_CANCELMODE, WM_CAPTURECHANGED, WM_DESTROY,
    WM_DISPLAYCHANGE, WM_HOTKEY, WM_POINTERCAPTURECHANGED, WM_POINTERDOWN, WM_POINTERUP,
    WM_POINTERUPDATE, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::config;
use crate::engine::canvas_engine::CanvasEngine;
use crate::engine::content_rect::{content_rect, parse_aspect, Rect};
use crate::net::local_server::{self, LocalServerHandle};
use crate::net::obs::{self, ObsSettings, ProjectorView};
use crate::protocol::{Brush, LineStyle, ShapeKind, Tool};
use crate::win::menu::{self, DrawTool, MenuAction};
use crate::win::monitor::{self, Monitor};
use crate::win::projector;
use crate::win::radial_menu::{self, RadialMenu, RadialRelease};
use crate::win::render::Renderer;
use crate::win::settings;
use crate::win::tray::{self, TrayCommand, WM_TRAY};

const HOTKEY_TOGGLE: i32 = 1;
/// 現在はポインタ種別を区別しないため、マウス相当の一定入力として扱う。
const POINTER_PRESSURE: f64 = 1.0;
/// 20ms バッチ (50 msg/s)
const FLUSH_TIMER_ID: usize = 1;
const FLUSH_INTERVAL_MS: u32 = 20;
/// OBS プロジェクター検知のポーリング間隔 (docs/painter.md)
const PROJECTOR_TIMER_ID: usize = 2;
const PROJECTOR_INTERVAL_MS: u32 = 2000;
/// obs-websocket でプロジェクターを開いた後、表示を確認するまでの高速ポーリング
const PENDING_TIMER_ID: usize = 3;
const PENDING_INTERVAL_MS: u32 = 250;
const PENDING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const RENDERER_RECOVERY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
/// obs-websocket 要求スレッドからの結果通知 (wparam: 成功=1)
const WM_OBS_RESULT: u32 = WM_APP + 2;
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
    /// OBS プロジェクター表示への追従 (config.follow_projector)
    follow_projector: bool,
    /// プロジェクター検知結果。false の間はオーバーレイを隠し F9 も無効
    projector_visible: bool,
    /// obs-websocket 設定 (None = 連携無効)
    obs: Option<ObsSettings>,
    /// 描画モード終了時にプロジェクターを閉じるか
    close_projector: bool,
    /// F9 → プロジェクターを開いて描画モードに入る、の完了待ち (開始時刻)
    pending_draw: Option<std::time::Instant>,
    /// F9を確保できない場合もトレイ操作だけで継続する。
    hotkey_registered: bool,
    /// 現在のプロジェクターを自分が obs-websocket で開いたか
    /// (手動で開かれたものは F9 オフでも閉じない)
    projector_opened_by_us: bool,
    /// OBSプロジェクターを前面化し、その直上にオーバーレイを保つ。
    projector_z_order: projector::ZOrderGuard,
    /// 右ボタンを押している間のジェスチャーメニュー。
    radial_menu: Option<RadialMenu>,
}

pub fn run() -> Result<()> {
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
        follow_projector: config.follow_projector,
        projector_visible: false,
        obs,
        close_projector: config.close_projector,
        pending_draw: None,
        hotkey_registered: false,
        projector_opened_by_us: false,
        projector_z_order: projector::ZOrderGuard::default(),
        radial_menu: None,
    });

    // 起動時に透明フレームを 1 回描き、D2D シェーダコンパイル・swapchain 初回
    // Present などの一時コストをここで消化する (初回 F9 の体感遅延対策)
    {
        let t = std::time::Instant::now();
        let renderer = app.renderer.as_mut().expect("renderer was initialized");
        renderer.rebuild_baked(&[])?;
        renderer.draw_frame(&[], false, None)?;
        info!("renderer warmup: {:?}", t.elapsed());
    }

    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(app) as isize);
        let app_ref = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App);
        app_ref.hotkey_registered =
            match RegisterHotKey(Some(hwnd), HOTKEY_TOGGLE, MOD_NOREPEAT, VK_F9.0 as u32) {
                Ok(()) => true,
                Err(error) => {
                    warn!(
                        "F9 global hotkey is unavailable ({error}); \
                         use the task tray to toggle draw mode"
                    );
                    false
                }
            };
        tray::add(hwnd, app_ref.hotkey_registered)?;
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
    }
    info!("ready — 描画モードはF9またはタスクトレイから切り替えられます");

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

impl App {
    /// 現在のツール・色から Brush を組み立てる (テストページと同じマッピング)
    fn current_brush(&self) -> Option<Brush> {
        match &self.tool {
            DrawTool::Pen => Some(Brush {
                tool: Tool::Pen,
                color: self.color.clone(),
                opacity: 1.0,
                width_n: self.width_n,
                pressure_width: false,
            }),
            DrawTool::Marker => Some(Brush {
                tool: Tool::Marker,
                color: self.color.clone(),
                opacity: 0.5,
                width_n: self.width_n * 3.0,
                pressure_width: false,
            }),
            DrawTool::Eraser => Some(Brush {
                tool: Tool::Eraser,
                color: "#000000".into(),
                opacity: 1.0,
                width_n: self.width_n * 3.0,
                pressure_width: false,
            }),
            DrawTool::Line
            | DrawTool::Arrow
            | DrawTool::Rectangle
            | DrawTool::Ellipse
            | DrawTool::Stamp(_) => None,
        }
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

    /// popup が閉じた後に選択結果を反映する。true はアプリ終了要求。
    fn apply_menu_action(&mut self, hwnd: HWND, action: MenuAction) -> bool {
        match action {
            MenuAction::SelectTool(tool) => {
                info!("tool: {tool:?}");
                self.tool = tool;
            }
            MenuAction::SelectColor(color) => {
                self.color = color.to_string();
                // 色を選んだ = 描く意図なので、消しゴム中ならペンに戻す
                if matches!(&self.tool, DrawTool::Eraser | DrawTool::Stamp(_)) {
                    self.tool = DrawTool::Pen;
                }
            }
            MenuAction::Undo => {
                let msgs = self.engine.undo();
                if !msgs.is_empty() {
                    self.web.send_all(msgs);
                    self.rebuild();
                    self.render();
                }
            }
            MenuAction::Redo => {
                let msgs = self.engine.redo();
                if !msgs.is_empty() {
                    self.web.send_all(msgs);
                    self.rebuild();
                    self.render();
                }
            }
            MenuAction::Clear => {
                if !crate::win::confirm(hwnd, "すべての描画を消去しますか？") {
                    return false;
                }
                let msgs = self.engine.clear();
                if !msgs.is_empty() {
                    self.web.send_all(msgs);
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
    fn render(&mut self) {
        if !self.draw_mode {
            return;
        }
        let items = self.engine.shared_items();
        let items = items.lock().unwrap();
        let visible = if self.local_echo { &items[..] } else { &[] };
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
            .and_then(|renderer| renderer.draw_frame(visible, self.draw_mode, radial));
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
        let result = self
            .renderer
            .as_mut()
            .ok_or_else(|| anyhow!("renderer is unavailable"))
            .and_then(|renderer| renderer.rebuild_baked(&items));
        drop(items);
        if let Err(error) = result {
            self.recover_renderer("rebuild_baked", error);
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
        let result = self
            .renderer
            .as_mut()
            .ok_or_else(|| anyhow!("renderer is unavailable"))
            .and_then(|renderer| renderer.bake_item(item));
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
        let mut renderer = Renderer::new(
            self.overlay_hwnd,
            self.monitor.width as u32,
            self.monitor.height as u32,
            self.content_local,
            &self.stamps,
        )?;
        if self.local_echo {
            renderer.rebuild_baked(&items)?;
        } else {
            renderer.rebuild_baked(&[])?;
        }
        if self.draw_mode {
            let visible = if self.local_echo { &items[..] } else { &[] };
            let radial = self.radial_menu.as_ref().map(|menu| {
                (
                    menu,
                    &self.tool,
                    self.color.as_str(),
                    self.stamps.as_slice(),
                )
            });
            renderer.draw_frame(visible, true, radial)?;
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
        self.radial_menu = None;

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

    /// F9 / トレイからの切替。プロジェクター未表示なら obs-websocket で開く
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
        if self.pending_draw.is_some() {
            return;
        }
        let Some(obs) = self.obs.clone() else { return };
        self.pending_draw = Some(std::time::Instant::now());
        info!("opening OBS projector via obs-websocket...");

        let mon = self.monitor;
        let hwnd_raw = hwnd.0 as isize;
        std::thread::spawn(move || {
            let result = obs::open_projector(&obs, mon.x, mon.y, mon.width, mon.height);
            if let Err(e) = &result {
                warn!("open projector failed: {e:#}");
            }
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(hwnd_raw as *mut core::ffi::c_void)),
                    WM_OBS_RESULT,
                    WPARAM(result.is_ok() as usize),
                    LPARAM(0),
                );
            }
        });
        unsafe {
            SetTimer(Some(hwnd), PENDING_TIMER_ID, PENDING_INTERVAL_MS, None);
        }
    }

    fn on_obs_result(&mut self, hwnd: HWND, ok: bool) {
        if !ok {
            self.cancel_pending(hwnd, "obs-websocket request failed");
            return;
        }
        self.poll_projector(hwnd);
        self.try_finish_pending(hwnd);
    }

    fn on_pending_timer(&mut self, hwnd: HWND) {
        self.poll_projector(hwnd);
        self.try_finish_pending(hwnd);
        if let Some(started) = self.pending_draw {
            if started.elapsed() > PENDING_TIMEOUT {
                self.cancel_pending(hwnd, "projector did not appear in time");
            }
        }
    }

    /// プロジェクターの表示が確認できたら描画モードへ入る
    fn try_finish_pending(&mut self, hwnd: HWND) {
        if self.pending_draw.is_some() && self.projector_visible {
            self.pending_draw = None;
            self.projector_opened_by_us = true;
            unsafe {
                let _ = KillTimer(Some(hwnd), PENDING_TIMER_ID);
            }
            self.set_draw_mode(hwnd, true);
        }
    }

    fn cancel_pending(&mut self, hwnd: HWND, reason: &str) {
        if self.pending_draw.take().is_some() {
            warn!("pending draw canceled: {reason}");
            unsafe {
                let _ = KillTimer(Some(hwnd), PENDING_TIMER_ID);
            }
        }
    }

    fn set_draw_mode(&mut self, hwnd: HWND, on: bool) {
        if self.draw_mode == on {
            return;
        }
        let t = std::time::Instant::now();
        self.draw_mode = on;
        if !self.draw_mode {
            self.radial_menu = None;
            // 描画中に切り替えた場合はストロークを破棄する
            let msgs = self.engine.cancel();
            if !msgs.is_empty() {
                self.web.send_all(msgs);
                unsafe {
                    let _ = KillTimer(Some(hwnd), FLUSH_TIMER_ID);
                }
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

    fn begin_radial_menu(&mut self, pointer_id: u32, lparam: LPARAM) {
        if !self.draw_mode || self.engine.is_drawing() || self.radial_menu.is_some() {
            return;
        }
        let screen = pointer_screen(lparam);
        let local = (
            (screen.0 - f64::from(self.monitor.x)) as f32,
            (screen.1 - f64::from(self.monitor.y)) as f32,
        );
        let scale =
            radial_menu::scale_for_surface(self.monitor.width as u32, self.monitor.height as u32);
        self.radial_menu = Some(RadialMenu::new(
            pointer_id,
            screen,
            local,
            (self.monitor.width as u32, self.monitor.height as u32),
            scale,
            self.stamps.len(),
            (self.engine.can_undo(), self.engine.can_redo()),
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
            self.render();
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
        if self
            .radial_menu
            .as_mut()
            .is_some_and(|menu| menu.set_history_availability(can_undo, can_redo))
        {
            self.render();
        }
    }

    fn on_pointer_down(&mut self, hwnd: HWND, lparam: LPARAM) {
        if !self.draw_mode || self.engine.is_drawing() {
            return;
        }
        let x = (lparam.0 & 0xffff) as i16 as f64;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f64;
        // 黒帯への誤描画防止
        if !self.content_screen.contains(x, y) {
            return;
        }
        let (u, v) = self.content_screen.normalize(x, y);
        let msgs = match self.tool.clone() {
            DrawTool::Pen | DrawTool::Marker | DrawTool::Eraser => {
                let Some(brush) = self.current_brush() else {
                    return;
                };
                self.engine.begin(brush, u, v, POINTER_PRESSURE, now_ms())
            }
            DrawTool::Line => {
                self.engine
                    .begin_shape(ShapeKind::Line, self.current_line_style(), u, v)
            }
            DrawTool::Arrow => {
                self.engine
                    .begin_shape(ShapeKind::Arrow, self.current_line_style(), u, v)
            }
            DrawTool::Rectangle => {
                self.engine
                    .begin_shape(ShapeKind::Rectangle, self.current_line_style(), u, v)
            }
            DrawTool::Ellipse => {
                self.engine
                    .begin_shape(ShapeKind::Ellipse, self.current_line_style(), u, v)
            }
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
        self.render();
    }

    fn on_pointer_update(&mut self, hwnd: HWND, lparam: LPARAM) {
        if !self.engine.is_drawing() {
            return;
        }
        let (u, v) = self.pointer_uv(lparam);
        let msgs = self.engine.move_to(u, v, POINTER_PRESSURE, now_ms());
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
        self.render();
    }

    fn on_pointer_up(&mut self, hwnd: HWND) {
        if !self.engine.is_drawing() {
            return;
        }
        let msgs = self.engine.end(now_ms());
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
        (!app.engine.is_drawing()).then(|| {
            (
                app.tool.clone(),
                app.color.clone(),
                app.stamps.clone(),
                app.engine.can_undo(),
                app.engine.can_redo(),
            )
        })
    };
    let Some((tool, color, stamps, can_undo, can_redo)) = menu_input else {
        return;
    };
    let action = menu::show(hwnd, &tool, &color, &stamps, can_undo, can_redo);
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
        WM_HOTKEY if wparam.0 as i32 == HOTKEY_TOGGLE => {
            // popup menu の内部ループへ届いた hotkey は、メニューを閉じずに背後の
            // overlay 状態だけ変えることになるため無視する。
            if !projector::foreground_ui_active() {
                unsafe { &mut *app_ptr }.toggle_mode(hwnd);
            }
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
                unsafe { &mut *app_ptr }.on_pointer_down(hwnd, lparam);
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
            } else if unsafe { &mut *app_ptr }.update_radial_menu(id, lparam) {
                LRESULT(0)
            } else if unsafe { &*app_ptr }.engine.is_drawing() {
                unsafe { &mut *app_ptr }.on_pointer_update(hwnd, lparam);
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
            } else if unsafe { &*app_ptr }.engine.is_drawing() {
                unsafe { &mut *app_ptr }.on_pointer_up(hwnd);
                LRESULT(0)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_POINTERCAPTURECHANGED | WM_CAPTURECHANGED => {
            if unsafe { &mut *app_ptr }.cancel_radial_interaction() {
                LRESULT(0)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_CANCELMODE => {
            if unsafe { &mut *app_ptr }.dismiss_radial_menu() {
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
                unsafe { &*app_ptr }.hotkey_registered,
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
                Some(TrayCommand::Settings) => {
                    unsafe { &mut *app_ptr }.set_draw_mode(hwnd, false);
                    if let Err(error) = settings::open(hwnd) {
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
            unsafe { &mut *app_ptr }.on_obs_result(hwnd, wparam.0 != 0);
            LRESULT(0)
        }
        WM_DESTROY => {
            tray::remove(hwnd);
            if unsafe { &*app_ptr }.hotkey_registered {
                unsafe {
                    let _ = UnregisterHotKey(Some(hwnd), HOTKEY_TOGGLE);
                }
            }
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
    use super::*;

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
    fn monitor_selection_falls_back_to_the_first_monitor() {
        let monitors = [test_monitor(0, true), test_monitor(1920, false)];
        assert_eq!(select_monitor(&monitors, 1), Some((1, monitors[1])));
        assert_eq!(select_monitor(&monitors, 9), Some((0, monitors[0])));
        assert_eq!(select_monitor(&[], 0), None);
    }
}
