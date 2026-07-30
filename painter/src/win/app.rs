//! オーバーレイウィンドウ・入力・アプリ統合 (docs/painter.md)。
//!
//! - WS_EX_NOREDIRECTIONBITMAP + WS_EX_TOPMOST + WS_EX_NOACTIVATE + WS_EX_TOOLWINDOW
//! - F9 (グローバルホットキー) でパススルー ⇔ 描画モードを切替 (WS_EX_TRANSPARENT)
//! - WM_POINTER* で入力を受け、StrokeEngine → local web hub + ローカルエコー描画
//! - 20ms タイマで stroke_points をバッチ送信

use anyhow::{anyhow, Context, Result};
use tracing::{info, warn};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, MOD_NOREPEAT, VK_F9};
use windows::Win32::UI::Input::Pointer::EnableMouseInPointer;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, KillTimer, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassW,
    SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    TranslateMessage, GWLP_USERDATA, GWL_EXSTYLE, HWND_TOPMOST, IDC_CROSS, LWA_ALPHA, MSG,
    POINTER_MESSAGE_FLAG_FIRSTBUTTON, POINTER_MESSAGE_FLAG_SECONDBUTTON, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, WM_APP, WM_DESTROY,
    WM_HOTKEY, WM_POINTERDOWN, WM_POINTERUP, WM_POINTERUPDATE, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::config;
use crate::engine::content_rect::{content_rect, parse_aspect, Rect};
use crate::engine::stroke_engine::StrokeEngine;
use crate::net::local_server::{self, LocalServerHandle};
use crate::net::obs::{self, ObsSettings, ProjectorView};
use crate::protocol::{Brush, Tool};
use crate::win::menu::{self, MenuAction};
use crate::win::monitor::{self, Monitor};
use crate::win::projector;
use crate::win::render::Renderer;
use crate::win::settings;
use crate::win::tray::{self, TrayCommand, WM_TRAY};

const HOTKEY_TOGGLE: i32 = 1;
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
/// obs-websocket 要求スレッドからの結果通知 (wparam: 成功=1)
const WM_OBS_RESULT: u32 = WM_APP + 2;
struct App {
    engine: StrokeEngine,
    web: LocalServerHandle,
    renderer: Renderer,
    tool: Tool,
    color: String,
    width_n: f64,
    /// content rect (スクリーン座標)。入力の正規化に使う
    content_screen: Rect,
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
    /// 現在のプロジェクターを自分が obs-websocket で開いたか
    /// (手動で開かれたものは F9 オフでも閉じない)
    projector_opened_by_us: bool,
}

pub fn run() -> Result<()> {
    let config = config::load()?;

    unsafe {
        // マニフェストでも宣言しているが、古い Windows への保険として実行時にも設定する
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // マウスもポインタ系メッセージに統一する
        EnableMouseInPointer(true).context("EnableMouseInPointer")?;
    }

    let monitors = monitor::enumerate();
    let (screen_index, mon) = match monitors.get(config.screen).copied() {
        Some(monitor) => (config.screen, monitor),
        None if !monitors.is_empty() => {
            warn!(
                "screen index {} が見つかりません (モニタ数: {}) — プライマリを使用します",
                config.screen,
                monitors.len()
            );
            (0, monitors[0])
        }
        None => return Err(anyhow!("利用可能なモニターが見つかりません")),
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

    let engine = StrokeEngine::new();
    let web = local_server::spawn(config.local_server_port)?;
    debug_assert_eq!(web.overlay_url(), config.overlay_url());

    let hwnd = create_overlay_window(mon.x, mon.y, mon.width, mon.height)?;
    let renderer = Renderer::new(hwnd, mon.width as u32, mon.height as u32, content_local)?;

    let mut app = Box::new(App {
        engine,
        web,
        renderer,
        tool: Tool::Pen,
        color: config.brush.color.clone(),
        width_n: config.brush.width_n,
        content_screen,
        monitor: mon,
        draw_mode: false,
        local_echo: config.local_echo,
        follow_projector: config.follow_projector,
        projector_visible: false,
        obs,
        close_projector: config.close_projector,
        pending_draw: None,
        projector_opened_by_us: false,
    });

    // 起動時に透明フレームを 1 回描き、D2D シェーダコンパイル・swapchain 初回
    // Present などの一時コストをここで消化する (初回 F9 の体感遅延対策)
    {
        let t = std::time::Instant::now();
        app.renderer.rebuild_baked(&[])?;
        app.renderer.draw_frame(&[], false)?;
        info!("renderer warmup: {:?}", t.elapsed());
    }

    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(app) as isize);
        RegisterHotKey(Some(hwnd), HOTKEY_TOGGLE, MOD_NOREPEAT, VK_F9.0 as u32)
            .context("RegisterHotKey F9 (他のアプリが使用中?)")?;
        tray::add(hwnd)?;
        // 初期状態はパススルー
        set_transparent(hwnd, true);
        SetTimer(Some(hwnd), PROJECTOR_TIMER_ID, PROJECTOR_INTERVAL_MS, None);
        // 追従モードでは初回検知が終わるまで隠しておく (poll_projector が表示する)
        let app_ref = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App);
        if app_ref.follow_projector {
            app_ref.poll_projector(hwnd);
        } else {
            app_ref.projector_visible = true;
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
    info!("ready — F9 で描画モードを切り替えます");

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

impl App {
    /// 現在のツール・色から Brush を組み立てる (テストページと同じマッピング)
    fn current_brush(&self) -> Brush {
        match self.tool {
            Tool::Pen => Brush {
                tool: Tool::Pen,
                color: self.color.clone(),
                opacity: 1.0,
                width_n: self.width_n,
                // M2 はマウスのみ (p=0.5 固定) のため実質無効。M3 でペン筆圧を有効化する
                pressure_width: true,
            },
            Tool::Marker => Brush {
                tool: Tool::Marker,
                color: self.color.clone(),
                opacity: 0.5,
                width_n: self.width_n * 3.0,
                pressure_width: true,
            },
            Tool::Eraser => Brush {
                tool: Tool::Eraser,
                color: "#000000".into(),
                opacity: 1.0,
                width_n: self.width_n * 3.0,
                pressure_width: true,
            },
        }
    }

    /// オーバーレイが操作可能な状態か (プロジェクター追従が無効なら常に可)
    fn overlay_enabled(&self) -> bool {
        !self.follow_projector || self.projector_visible
    }

    /// OBS プロジェクターの表示状態をポーリングし、変化があれば表示/非表示を切り替える
    fn poll_projector(&mut self, hwnd: HWND) {
        if !self.follow_projector {
            return;
        }
        let visible = projector::is_projector_visible(&self.monitor, hwnd);
        if visible == self.projector_visible {
            return;
        }
        self.projector_visible = visible;
        if visible {
            info!("OBS projector detected — overlay enabled");
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                // プロジェクターより上に来るよう topmost を再主張する
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
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

    /// 右クリックメニュー (描画モード中のみ呼ばれる)
    fn show_menu(&mut self, hwnd: HWND) {
        match menu::show(hwnd, &self.tool, &self.color) {
            Some(MenuAction::SelectTool(tool)) => {
                info!("tool: {tool:?}");
                self.tool = tool;
            }
            Some(MenuAction::SelectColor(color)) => {
                self.color = color.to_string();
                // 色を選んだ = 描く意図なので、消しゴム中ならペンに戻す
                if self.tool == Tool::Eraser {
                    self.tool = Tool::Pen;
                }
            }
            Some(MenuAction::Undo) => {
                let msgs = self.engine.undo();
                if !msgs.is_empty() {
                    self.web.send_all(msgs);
                    self.rebuild();
                    self.render();
                }
            }
            Some(MenuAction::Clear) => {
                let msgs = self.engine.clear();
                if !msgs.is_empty() {
                    self.web.send_all(msgs);
                    self.rebuild();
                    self.render();
                }
            }
            Some(MenuAction::Exit) => unsafe {
                let _ = DestroyWindow(hwnd);
            },
            None => {}
        }
    }

    // ローカルエコーは描画モード中のみ表示する。パススルー中は overlay
    // (プロジェクター内のブラウザソース) 側の表示だけが見える
    fn render(&mut self) {
        if !self.draw_mode {
            return;
        }
        let strokes = self.engine.shared_strokes();
        let strokes = strokes.lock().unwrap().clone();
        let visible = if self.local_echo { &strokes[..] } else { &[] };
        if let Err(e) = self.renderer.draw_frame(visible, self.draw_mode) {
            warn!("draw_frame: {e:#}");
        }
    }

    fn rebuild(&mut self) {
        if !self.local_echo {
            return;
        }
        let strokes = self.engine.shared_strokes();
        let strokes = strokes.lock().unwrap().clone();
        if let Err(e) = self.renderer.rebuild_baked(&strokes) {
            warn!("rebuild_baked: {e:#}");
        }
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
        } else if let Err(e) = self.renderer.clear_frame() {
            warn!("clear_frame: {e:#}");
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
        let x = (lparam.0 & 0xffff) as i16 as f64;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f64;
        self.content_screen.normalize(x, y)
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
        let msgs = self.engine.begin(self.current_brush(), u, v, 0.5, now_ms());
        self.web.send_all(msgs);
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
        let msgs = self.engine.move_to(u, v, 0.5, now_ms());
        if !msgs.is_empty() {
            // 総点数上限による強制確定
            self.web.send_all(msgs);
            unsafe {
                let _ = KillTimer(Some(hwnd), FLUSH_TIMER_ID);
            }
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
        self.rebuild();
        self.render();
    }

    fn on_flush_timer(&mut self) {
        let msgs = self.engine.flush();
        self.web.send_all(msgs);
    }
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
    let app = unsafe { &mut *app_ptr };

    match msg {
        WM_HOTKEY if wparam.0 as i32 == HOTKEY_TOGGLE => {
            app.toggle_mode(hwnd);
            LRESULT(0)
        }
        // パススルー中は「処理済み」にせず DefWindowProc に流す (握りつぶすと
        // 下のウィンドウへ届かない)。描画モード中のみ自分で処理する
        WM_POINTERDOWN if app.draw_mode => {
            // wparam の HIWORD にボタン種別フラグが入る (POINTER_MESSAGE_FLAG_*)
            let flags = ((wparam.0 >> 16) & 0xffff) as u32;
            if flags & POINTER_MESSAGE_FLAG_SECONDBUTTON != 0 {
                if !app.engine.is_drawing() {
                    app.show_menu(hwnd);
                }
            } else if flags & POINTER_MESSAGE_FLAG_FIRSTBUTTON != 0 {
                app.on_pointer_down(hwnd, lparam);
            }
            LRESULT(0)
        }
        WM_POINTERUPDATE if app.engine.is_drawing() => {
            app.on_pointer_update(hwnd, lparam);
            LRESULT(0)
        }
        WM_POINTERUP if app.engine.is_drawing() => {
            app.on_pointer_up(hwnd);
            LRESULT(0)
        }
        WM_TRAY => {
            match tray::on_message(hwnd, (lparam.0 & 0xffff) as u32) {
                Some(TrayCommand::ToggleMode) => app.toggle_mode(hwnd),
                Some(TrayCommand::Settings) => {
                    app.set_draw_mode(hwnd, false);
                    if let Err(error) = settings::open(hwnd) {
                        warn!("settings: {error:#}");
                        crate::win::message_box(&format!("設定画面を開けません:\n{error:#}"));
                    }
                }
                Some(TrayCommand::Licenses) => {
                    app.set_draw_mode(hwnd, false);
                    if let Err(error) = crate::win::open_url(hwnd, app.web.licenses_url()) {
                        warn!("licenses: {error:#}");
                        crate::win::message_box(&format!(
                            "第三者ライセンスを開けません:\n{error:#}"
                        ));
                    }
                }
                Some(TrayCommand::Exit) => unsafe {
                    let _ = DestroyWindow(hwnd);
                },
                None => {}
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == FLUSH_TIMER_ID => {
            app.on_flush_timer();
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == PROJECTOR_TIMER_ID => {
            app.poll_projector(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == PENDING_TIMER_ID => {
            app.on_pending_timer(hwnd);
            LRESULT(0)
        }
        WM_OBS_RESULT => {
            app.on_obs_result(hwnd, wparam.0 != 0);
            LRESULT(0)
        }
        WM_DESTROY => {
            tray::remove(hwnd);
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
