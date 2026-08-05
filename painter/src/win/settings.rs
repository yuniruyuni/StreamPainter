//! タスクトレイから開くネイティブ設定画面。
//! 保存内容はconfig.tomlへ反映し、hotkeyとWindows自動起動は外部登録へtransactionalに反映する。

#![allow(clippy::too_many_arguments)]

use std::ffi::c_void;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, Ordering};

use anyhow::{anyhow, Context, Result};
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DeleteObject, GetSysColorBrush, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    COLOR_WINDOW, DEFAULT_CHARSET, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::Dialogs::{
    CommDlgExtendedError, GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST,
    OPENFILENAMEW,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetKeyState, SetFocus, VK_CONTROL, VK_ESCAPE, VK_LWIN, VK_MENU, VK_RWIN,
    VK_SHIFT, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetDesktopWindow, GetDlgItem,
    GetMessageW, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    IsDialogMessageW, IsWindow, LoadCursorW, MessageBoxW, PostMessageW, RegisterClassW,
    SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowTextW, ShowWindow,
    TranslateMessage, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON,
    CBS_DROPDOWNLIST, CBS_HASSTRINGS, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, EN_CHANGE,
    ES_AUTOHSCROLL, ES_NUMBER, ES_PASSWORD, ES_READONLY, GWLP_USERDATA, HMENU, IDC_ARROW,
    LBN_SELCHANGE, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY, LB_ADDSTRING, LB_GETCURSEL, LB_RESETCONTENT,
    LB_SETCURSEL, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MSG, SW_SHOW, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_KEYDOWN, WM_KEYUP, WM_NCDESTROY, WM_SETFONT,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE,
    WS_EX_CONTROLPARENT, WS_EX_DLGMODALFRAME, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use crate::config::{
    self, Config, HotkeyConfig, StampConfig, HOTKEY_MOD_ALT, HOTKEY_MOD_CTRL, HOTKEY_MOD_SHIFT,
    HOTKEY_MOD_WIN, MAX_STAMPS,
};
use crate::net::local_server::{
    LocalServerDiagnostics, LocalServerDiagnosticsSnapshot, LocalServerDiagnosticsSubscription,
    LocalServerReachability,
};
use crate::win::autostart::{PreparedAutostartChange, RegistrationStatus, SystemAutostart};
use crate::win::hotkey::{self, ChangeCommand, ProbeRegistration};
use crate::win::monitor::{self, Monitor};

const CLASS_NAME: PCWSTR = w!("stream-painter-settings");

const ID_SAVE: i32 = 1;
const ID_CANCEL: i32 = 2;
const ID_OVERLAY_URL: i32 = 100;
const ID_PORT: i32 = 101;
const ID_MONITOR: i32 = 102;
const ID_ASPECT: i32 = 103;
const ID_LOCAL_ECHO: i32 = 104;
const ID_FOLLOW_PROJECTOR: i32 = 105;
const ID_OBS_CONTROL: i32 = 106;
const ID_OBS_URL: i32 = 107;
const ID_OBS_PASSWORD: i32 = 108;
const ID_PROJECTOR_VIEW: i32 = 109;
const ID_CLOSE_PROJECTOR: i32 = 110;
const ID_BRUSH_COLOR: i32 = 111;
const ID_BRUSH_WIDTH: i32 = 112;
const ID_STAMP_LIST: i32 = 113;
const ID_STAMP_ADD: i32 = 114;
const ID_STAMP_REMOVE: i32 = 115;
const ID_STAMP_NAME: i32 = 116;
const ID_STAMP_SIZE: i32 = 117;
const ID_STAMP_OPACITY: i32 = 118;
const ID_COPY_OVERLAY_URL: i32 = 119;
const ID_SERVER_STATUS: i32 = 120;
const ID_BROWSER_STATUS: i32 = 121;
const ID_HOTKEY_CAPTURE: i32 = 122;
const ID_HOTKEY_CLEAR: i32 = 123;
const ID_HOTKEY_DEFAULT: i32 = 124;
const ID_AUTOSTART: i32 = 125;
const ID_AUTOSTART_STATUS: i32 = 126;
const ID_CONFIRM_BEFORE_CLEAR: i32 = 127;

const WM_DIAGNOSTICS_CHANGED: u32 = WM_APP + 1;

static SETTINGS_HWND: AtomicIsize = AtomicIsize::new(0);

enum AutostartUi {
    Available {
        controller: SystemAutostart,
        status: RegistrationStatus,
    },
    Unavailable,
}

impl AutostartUi {
    fn load() -> Self {
        let result = SystemAutostart::current().and_then(|controller| {
            controller
                .inspect()
                .map(|status| Self::Available { controller, status })
        });
        match result {
            Ok(state) => state,
            Err(error) => {
                tracing::warn!("Windows auto-start state is unavailable: {error:#}");
                Self::Unavailable
            }
        }
    }

    fn is_registered(&self) -> bool {
        match self {
            Self::Available { status, .. } => status.is_registered(),
            Self::Unavailable => false,
        }
    }

    fn is_available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }

    fn status_text(&self) -> String {
        match self {
            Self::Available {
                status: RegistrationStatus::Disabled,
                ..
            } => "Windows自動起動: OFF（登録なし）".to_owned(),
            Self::Available {
                status: RegistrationStatus::Enabled,
                ..
            } => "Windows自動起動: ON（現在のexeを通常モードで起動）".to_owned(),
            Self::Available {
                status: RegistrationStatus::NeedsRepair(problem),
                ..
            } => format!(
                "Windows自動起動: 登録あり・修復が必要（{}）",
                problem.description()
            ),
            Self::Unavailable => "Windows自動起動: 状態を取得できません（変更しません）".to_owned(),
        }
    }

    fn controller(&self) -> Option<&SystemAutostart> {
        match self {
            Self::Available { controller, .. } => Some(controller),
            Self::Unavailable => None,
        }
    }
}

struct SettingsState {
    /// Someなら起動中overlayへhotkey変更をtransactionalに反映する。
    live_owner: Option<HWND>,
    original_config: Config,
    monitors: Vec<Monitor>,
    font: Option<HFONT>,
    stamps: Vec<StampConfig>,
    selected_stamp: Option<usize>,
    new_stamp_files: Vec<PathBuf>,
    hotkey: HotkeyConfig,
    /// config.tomlではなく、Windows上の実登録から毎回初期化する。
    autostart: AutostartUi,
    saved: bool,
    diagnostics: Option<LocalServerDiagnostics>,
    _diagnostics_subscription: Option<LocalServerDiagnosticsSubscription>,
    /// 設定画面を overlay より前に保つ。WM_NCDESTROY で state と一緒に解放する。
    _foreground_ui: crate::win::projector::ForegroundUiGuard,
}

impl Drop for SettingsState {
    fn drop(&mut self) {
        if !self.saved {
            for path in self.new_stamp_files.drain(..) {
                let _ = std::fs::remove_file(path);
            }
        }
        if let Some(font) = self.font.take() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(font.0));
            }
        }
    }
}

fn hwnd_from_raw(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi as i64 + 48) / 96) as i32
}

/// 設定画面が開いている間は描画モードへの切替を抑止するために使う。
pub fn is_open() -> bool {
    let raw = SETTINGS_HWND.load(Ordering::SeqCst);
    if raw == 0 {
        return false;
    }
    if unsafe { IsWindow(Some(hwnd_from_raw(raw))).as_bool() } {
        true
    } else {
        let _ = SETTINGS_HWND.compare_exchange(raw, 0, Ordering::SeqCst, Ordering::SeqCst);
        false
    }
}

/// モデルレス設定画面に Tab / Enter / Esc のダイアログ操作を提供する。
pub fn handle_dialog_message(message: &MSG) -> bool {
    let raw = SETTINGS_HWND.load(Ordering::SeqCst);
    if raw == 0 {
        return false;
    }
    unsafe {
        let hwnd = hwnd_from_raw(raw);
        if !IsWindow(Some(hwnd)).as_bool() {
            return false;
        }
        if capture_hotkey_message(hwnd, message) {
            return true;
        }
        IsDialogMessageW(hwnd, message).as_bool()
    }
}

fn key_is_down(key: u32) -> bool {
    (unsafe { GetKeyState(key as i32) }) < 0
}

unsafe fn capture_hotkey_message(settings_hwnd: HWND, message: &MSG) -> bool {
    let Ok(capture) = control(settings_hwnd, ID_HOTKEY_CAPTURE) else {
        return false;
    };
    if message.hwnd != capture
        || !matches!(
            message.message,
            WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP
        )
    {
        return false;
    }

    let key = message.wParam.0 as u32;
    let mut modifiers = 0_u32;
    if key_is_down(VK_CONTROL.0.into()) {
        modifiers |= HOTKEY_MOD_CTRL;
    }
    if key_is_down(VK_MENU.0.into()) {
        modifiers |= HOTKEY_MOD_ALT;
    }
    if key_is_down(VK_SHIFT.0.into()) {
        modifiers |= HOTKEY_MOD_SHIFT;
    }
    if key_is_down(VK_LWIN.0.into()) || key_is_down(VK_RWIN.0.into()) {
        modifiers |= HOTKEY_MOD_WIN;
    }

    // Tab / Esc は修飾なしなら通常のdialog移動・キャンセルとして扱う。
    if modifiers == 0 && (key == u32::from(VK_TAB.0) || key == u32::from(VK_ESCAPE.0)) {
        return false;
    }
    if matches!(message.message, WM_KEYUP | WM_SYSKEYUP) {
        return true;
    }
    if [
        u32::from(VK_CONTROL.0),
        u32::from(VK_MENU.0),
        u32::from(VK_SHIFT.0),
        u32::from(VK_LWIN.0),
        u32::from(VK_RWIN.0),
    ]
    .contains(&key)
    {
        return true;
    }

    let state_ptr = GetWindowLongPtrW(settings_hwnd, GWLP_USERDATA) as *mut SettingsState;
    let Some(state) = state_ptr.as_mut() else {
        return true;
    };
    match HotkeyConfig::from_virtual_key(key, modifiers) {
        Ok(config) => {
            state.hotkey = config;
            let _ = update_hotkey_control(settings_hwnd, state);
        }
        Err(error) => {
            let _ = set_control_text(
                settings_hwnd,
                ID_HOTKEY_CAPTURE,
                &format!("使用できないキー: {error}"),
            );
        }
    }
    true
}

/// 通常起動できない場合にも `stream-painter.exe --settings` で設定だけを編集できる。
pub fn run_standalone() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    open_internal(unsafe { GetDesktopWindow() }, None, false)?;

    unsafe {
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).into() {
            let handled = handle_dialog_message(&message);
            if !is_open() {
                break;
            }
            if handled {
                continue;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
            if !is_open() {
                break;
            }
        }
    }
    Ok(())
}

/// 設定画面を開く。すでに開いている場合は既存画面を前面へ出す。
pub fn open(owner: HWND, diagnostics: Option<LocalServerDiagnostics>) -> Result<()> {
    open_internal(owner, diagnostics, true)
}

fn open_internal(
    owner: HWND,
    diagnostics: Option<LocalServerDiagnostics>,
    live_hotkey: bool,
) -> Result<()> {
    if is_open() {
        let hwnd = hwnd_from_raw(SETTINGS_HWND.load(Ordering::SeqCst));
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
        return Ok(());
    }

    let config = config::load()?;
    let monitors = monitor::enumerate();
    if monitors.is_empty() {
        anyhow::bail!("利用可能なモニターが見つかりません");
    }

    unsafe {
        register_class()?;

        let dpi = GetDpiForWindow(owner).max(96);
        let window_width = scale(680, dpi);
        let window_height = scale(900, dpi);
        let mut owner_rect = RECT::default();
        let (x, y) = if GetWindowRect(owner, &mut owner_rect).is_ok() {
            (
                owner_rect.left + ((owner_rect.right - owner_rect.left - window_width) / 2),
                owner_rect.top + ((owner_rect.bottom - owner_rect.top - window_height) / 2),
            )
        } else {
            (0, 0)
        };

        let hinstance = GetModuleHandleW(None)?;
        let hwnd = CreateWindowExW(
            WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            CLASS_NAME,
            w!("StreamPainter 設定"),
            WS_CAPTION | WS_SYSMENU,
            x,
            y,
            window_width,
            window_height,
            Some(owner),
            None,
            Some(hinstance.into()),
            None,
        )
        .context("設定ウィンドウを作成できません")?;

        let state = Box::new(SettingsState {
            live_owner: live_hotkey.then_some(owner),
            original_config: config.clone(),
            monitors,
            font: None,
            stamps: config.stamps.clone(),
            selected_stamp: None,
            new_stamp_files: Vec::new(),
            hotkey: config.hotkey.clone(),
            autostart: AutostartUi::load(),
            saved: false,
            diagnostics,
            _diagnostics_subscription: None,
            _foreground_ui: crate::win::projector::ForegroundUiGuard::new(),
        });
        let state_ptr = Box::into_raw(state);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);

        if let Err(error) = initialize_controls(hwnd, &config, dpi) {
            let _ = DestroyWindow(hwnd);
            return Err(error);
        }

        if let Some(diagnostics) = (&*state_ptr).diagnostics.clone() {
            let raw = hwnd.0 as isize;
            let subscription = diagnostics.subscribe(move || {
                if SETTINGS_HWND.load(Ordering::SeqCst) == raw {
                    let _ = PostMessageW(
                        Some(hwnd_from_raw(raw)),
                        WM_DIAGNOSTICS_CHANGED,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            });
            (&mut *state_ptr)._diagnostics_subscription = Some(subscription);
            let _ = PostMessageW(Some(hwnd), WM_DIAGNOSTICS_CHANGED, WPARAM(0), LPARAM(0));
        }

        SETTINGS_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        if let Ok(port) = control(hwnd, ID_PORT) {
            let _ = SetFocus(Some(port));
        }
        Ok(())
    }
}

unsafe fn register_class() -> Result<()> {
    let hinstance = unsafe { GetModuleHandleW(None)? };
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: hinstance.into(),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        hbrBackground: unsafe { GetSysColorBrush(COLOR_WINDOW) },
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    if unsafe { RegisterClassW(&class) } == 0 {
        let error = unsafe { GetLastError() };
        if error != ERROR_CLASS_ALREADY_EXISTS {
            return Err(anyhow!(
                "設定ウィンドウのクラス登録に失敗しました: {error:?}"
            ));
        }
    }
    Ok(())
}

unsafe fn initialize_controls(hwnd: HWND, config: &Config, dpi: u32) -> Result<()> {
    let state_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut SettingsState;
    let state = unsafe {
        state_ptr
            .as_mut()
            .ok_or_else(|| anyhow!("設定画面の初期化状態がありません"))?
    };

    let font_height = -scale(12, dpi);
    let font = unsafe {
        CreateFontW(
            font_height,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Segoe UI"),
        )
    };
    if font.is_invalid() {
        anyhow::bail!("設定画面のフォントを作成できません");
    }
    state.font = Some(font);

    let s = |value| scale(value, dpi);
    let label_x = s(20);
    let field_x = s(210);
    let label_width = s(180);
    let field_width = s(430);
    let row_height = s(24);

    unsafe {
        create_label(
            hwnd,
            font,
            "OBS Browser Source URL",
            label_x,
            s(19),
            label_width,
            row_height,
        )?;
        create_edit(
            hwnd,
            font,
            ID_OVERLAY_URL,
            &config.overlay_url(),
            field_x,
            s(16),
            s(315),
            row_height,
            ES_AUTOHSCROLL | ES_READONLY,
        )?;
        create_button(
            hwnd,
            font,
            ID_COPY_OVERLAY_URL,
            "URLをコピー",
            s(535),
            s(14),
            s(105),
            s(28),
            false,
        )?;

        create_label(
            hwnd,
            font,
            "ローカルサーバーのポート",
            label_x,
            s(53),
            label_width,
            row_height,
        )?;
        create_edit(
            hwnd,
            font,
            ID_PORT,
            &config.local_server_port.to_string(),
            field_x,
            s(50),
            s(120),
            row_height,
            ES_AUTOHSCROLL | ES_NUMBER,
        )?;

        create_label(
            hwnd,
            font,
            "描画対象モニター",
            label_x,
            s(87),
            label_width,
            row_height,
        )?;
        let monitor_combo =
            create_combo(hwnd, font, ID_MONITOR, field_x, s(84), field_width, s(240))?;
        for (index, monitor) in state.monitors.iter().enumerate() {
            let primary = if monitor.primary {
                " — プライマリ"
            } else {
                ""
            };
            combo_add(
                monitor_combo,
                &format!(
                    "{}: {}×{} ({:+}, {:+}){}",
                    index, monitor.width, monitor.height, monitor.x, monitor.y, primary
                ),
            );
        }
        combo_select(
            monitor_combo,
            if config.screen < state.monitors.len() {
                config.screen
            } else {
                0
            },
        );

        create_label(
            hwnd,
            font,
            "キャンバスのアスペクト比",
            label_x,
            s(121),
            label_width,
            row_height,
        )?;
        create_edit(
            hwnd,
            font,
            ID_ASPECT,
            &config.canvas_aspect,
            field_x,
            s(118),
            s(120),
            row_height,
            ES_AUTOHSCROLL,
        )?;

        create_checkbox(
            hwnd,
            font,
            ID_LOCAL_ECHO,
            "描画中の線をこのPCのオーバーレイにも表示する",
            config.local_echo,
            label_x,
            s(158),
            s(620),
            row_height,
        )?;
        create_checkbox(
            hwnd,
            font,
            ID_FOLLOW_PROJECTOR,
            "OBS全画面プロジェクターの表示中だけオーバーレイを有効にする",
            config.follow_projector,
            label_x,
            s(188),
            s(620),
            row_height,
        )?;
        create_checkbox(
            hwnd,
            font,
            ID_OBS_CONTROL,
            "ホットキーでOBSプロジェクターを自動的に開く（obs-websocket）",
            config.obs_control,
            label_x,
            s(218),
            s(620),
            row_height,
        )?;

        create_label(
            hwnd,
            font,
            "OBS WebSocket URL",
            label_x,
            s(257),
            label_width,
            row_height,
        )?;
        create_edit(
            hwnd,
            font,
            ID_OBS_URL,
            &config.obs_websocket_url,
            field_x,
            s(254),
            field_width,
            row_height,
            ES_AUTOHSCROLL,
        )?;

        create_label(
            hwnd,
            font,
            "OBS WebSocket パスワード",
            label_x,
            s(291),
            label_width,
            row_height,
        )?;
        create_edit(
            hwnd,
            font,
            ID_OBS_PASSWORD,
            &config.obs_websocket_password,
            field_x,
            s(288),
            field_width,
            row_height,
            ES_AUTOHSCROLL | ES_PASSWORD,
        )?;

        create_label(
            hwnd,
            font,
            "プロジェクター表示",
            label_x,
            s(325),
            label_width,
            row_height,
        )?;
        let view_combo = create_combo(
            hwnd,
            font,
            ID_PROJECTOR_VIEW,
            field_x,
            s(322),
            s(260),
            s(120),
        )?;
        combo_add(view_combo, "Program（配信映像）");
        combo_add(view_combo, "Preview（編集映像）");
        combo_select(view_combo, usize::from(config.projector_view == "preview"));

        create_checkbox(
            hwnd,
            font,
            ID_CLOSE_PROJECTOR,
            "描画モード終了時、自動で開いたプロジェクターを閉じる",
            config.close_projector,
            label_x,
            s(358),
            s(620),
            row_height,
        )?;

        create_label(
            hwnd,
            font,
            "切替キー（欄を選びキー入力）",
            label_x,
            s(397),
            label_width,
            row_height,
        )?;
        create_edit(
            hwnd,
            font,
            ID_HOTKEY_CAPTURE,
            &state.hotkey.display_name(),
            field_x,
            s(394),
            s(220),
            row_height,
            ES_READONLY,
        )?;
        create_button(
            hwnd,
            font,
            ID_HOTKEY_CLEAR,
            "解除",
            s(440),
            s(393),
            s(85),
            s(28),
            false,
        )?;
        create_button(
            hwnd,
            font,
            ID_HOTKEY_DEFAULT,
            "既定(F9)",
            s(535),
            s(393),
            s(105),
            s(28),
            false,
        )?;

        create_label(
            hwnd,
            font,
            "標準ブラシ色（#RRGGBB）",
            label_x,
            s(431),
            label_width,
            row_height,
        )?;
        create_edit(
            hwnd,
            font,
            ID_BRUSH_COLOR,
            &config.brush.color,
            field_x,
            s(428),
            s(160),
            row_height,
            ES_AUTOHSCROLL,
        )?;

        create_label(
            hwnd,
            font,
            "標準ブラシ幅（正規化値）",
            label_x,
            s(465),
            label_width,
            row_height,
        )?;
        create_edit(
            hwnd,
            font,
            ID_BRUSH_WIDTH,
            &config.brush.width_n.to_string(),
            field_x,
            s(462),
            s(160),
            row_height,
            ES_AUTOHSCROLL,
        )?;

        create_checkbox(
            hwnd,
            font,
            ID_CONFIRM_BEFORE_CLEAR,
            "全消去の前に確認画面を表示する（推奨）",
            config.confirm_before_clear,
            s(330),
            s(494),
            s(310),
            row_height,
        )?;

        create_label(
            hwnd,
            font,
            &format!("登録スタンプ（最大 {MAX_STAMPS} 個、PNGのみ）"),
            label_x,
            s(502),
            s(280),
            row_height,
        )?;
        create_listbox(hwnd, font, ID_STAMP_LIST, label_x, s(528), s(285), s(98))?;
        create_button(
            hwnd,
            font,
            ID_STAMP_ADD,
            "PNGを追加...",
            label_x,
            s(634),
            s(130),
            s(30),
            false,
        )?;
        create_button(
            hwnd,
            font,
            ID_STAMP_REMOVE,
            "選択を削除",
            s(160),
            s(634),
            s(130),
            s(30),
            false,
        )?;

        create_label(hwnd, font, "スタンプ名", s(330), s(528), s(130), row_height)?;
        create_edit(
            hwnd,
            font,
            ID_STAMP_NAME,
            "",
            s(330),
            s(552),
            s(310),
            row_height,
            ES_AUTOHSCROLL,
        )?;
        create_label(
            hwnd,
            font,
            "表示サイズ（キャンバス高の%）",
            s(330),
            s(582),
            s(220),
            row_height,
        )?;
        create_edit(
            hwnd,
            font,
            ID_STAMP_SIZE,
            "",
            s(550),
            s(579),
            s(90),
            row_height,
            ES_AUTOHSCROLL | ES_NUMBER,
        )?;
        create_label(
            hwnd,
            font,
            "不透明度（%）",
            s(330),
            s(614),
            s(150),
            row_height,
        )?;
        create_edit(
            hwnd,
            font,
            ID_STAMP_OPACITY,
            "",
            s(550),
            s(611),
            s(90),
            row_height,
            ES_AUTOHSCROLL | ES_NUMBER,
        )?;
        create_label(
            hwnd,
            font,
            "右クリックメニューの「スタンプ」から選び、クリック位置へ配置します。",
            s(330),
            s(642),
            s(310),
            s(30),
        )?;
        refresh_stamp_list(hwnd, state, (!state.stamps.is_empty()).then_some(0))?;

        create_label_with_id(
            hwnd,
            font,
            ID_SERVER_STATUS,
            "ローカルサーバー: 状態を確認中...",
            label_x,
            s(680),
            s(620),
            row_height,
        )?;
        create_label_with_id(
            hwnd,
            font,
            ID_BROWSER_STATUS,
            "OBS Browser Source: 状態を確認中...",
            label_x,
            s(704),
            s(620),
            row_height,
        )?;
        let autostart_checkbox = create_checkbox(
            hwnd,
            font,
            ID_AUTOSTART,
            "Windowsログイン時にStreamPainterを自動起動する（現在のユーザー）",
            state.autostart.is_registered(),
            label_x,
            s(732),
            s(620),
            row_height,
        )?;
        if !state.autostart.is_available() {
            let _ = EnableWindow(autostart_checkbox, false);
        }
        create_label_with_id(
            hwnd,
            font,
            ID_AUTOSTART_STATUS,
            &format!(
                "{}\n登録が古い場合はONのまま保存で現在のexeへ修復、OFFで解除します。\nその他の設定は再起動後に反映します。ポート変更時はOBSのURLも更新してください。",
                state.autostart.status_text()
            ),
            label_x,
            s(756),
            s(620),
            s(62),
        )?;

        create_button(
            hwnd,
            font,
            ID_SAVE,
            "保存",
            s(430),
            s(820),
            s(100),
            s(32),
            true,
        )?;
        create_button(
            hwnd,
            font,
            ID_CANCEL,
            "キャンセル",
            s(540),
            s(820),
            s(100),
            s(32),
            false,
        )?;
    }

    update_connection_status(hwnd, state)?;

    Ok(())
}

unsafe fn create_control(
    parent: HWND,
    font: HFONT,
    class: PCWSTR,
    text: &str,
    id: Option<i32>,
    ex_style: WINDOW_EX_STYLE,
    style: WINDOW_STYLE,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Result<HWND> {
    let text = HSTRING::from(text);
    let menu = id.map(|id| HMENU(id as usize as *mut c_void));
    let hwnd = unsafe {
        CreateWindowExW(
            ex_style,
            class,
            &text,
            style,
            x,
            y,
            width,
            height,
            Some(parent),
            menu,
            None,
            None,
        )
    }
    .context("設定画面の入力欄を作成できません")?;
    unsafe {
        SendMessageW(
            hwnd,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
    }
    Ok(hwnd)
}

unsafe fn create_label(
    parent: HWND,
    font: HFONT,
    text: &str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Result<HWND> {
    unsafe {
        create_control(
            parent,
            font,
            w!("STATIC"),
            text,
            None,
            WINDOW_EX_STYLE::default(),
            WS_CHILD | WS_VISIBLE,
            x,
            y,
            width,
            height,
        )
    }
}

unsafe fn create_label_with_id(
    parent: HWND,
    font: HFONT,
    id: i32,
    text: &str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Result<HWND> {
    unsafe {
        create_control(
            parent,
            font,
            w!("STATIC"),
            text,
            Some(id),
            WINDOW_EX_STYLE::default(),
            WS_CHILD | WS_VISIBLE,
            x,
            y,
            width,
            height,
        )
    }
}

unsafe fn create_edit(
    parent: HWND,
    font: HFONT,
    id: i32,
    text: &str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    edit_style: i32,
) -> Result<HWND> {
    unsafe {
        create_control(
            parent,
            font,
            w!("EDIT"),
            text,
            Some(id),
            WS_EX_CLIENTEDGE,
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(edit_style as u32),
            x,
            y,
            width,
            height,
        )
    }
}

unsafe fn create_combo(
    parent: HWND,
    font: HFONT,
    id: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Result<HWND> {
    unsafe {
        create_control(
            parent,
            font,
            w!("COMBOBOX"),
            "",
            Some(id),
            WS_EX_CLIENTEDGE,
            WS_CHILD
                | WS_VISIBLE
                | WS_TABSTOP
                | WINDOW_STYLE((CBS_DROPDOWNLIST | CBS_HASSTRINGS) as u32),
            x,
            y,
            width,
            height,
        )
    }
}

unsafe fn create_listbox(
    parent: HWND,
    font: HFONT,
    id: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Result<HWND> {
    unsafe {
        create_control(
            parent,
            font,
            w!("LISTBOX"),
            "",
            Some(id),
            WS_EX_CLIENTEDGE,
            WS_CHILD
                | WS_VISIBLE
                | WS_TABSTOP
                | WS_VSCROLL
                | WINDOW_STYLE((LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32),
            x,
            y,
            width,
            height,
        )
    }
}

unsafe fn create_checkbox(
    parent: HWND,
    font: HFONT,
    id: i32,
    text: &str,
    checked: bool,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Result<HWND> {
    let checkbox = unsafe {
        create_control(
            parent,
            font,
            w!("BUTTON"),
            text,
            Some(id),
            WINDOW_EX_STYLE::default(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
            x,
            y,
            width,
            height,
        )?
    };
    unsafe {
        SendMessageW(
            checkbox,
            BM_SETCHECK,
            Some(WPARAM(usize::from(checked))),
            None,
        );
    }
    Ok(checkbox)
}

unsafe fn create_button(
    parent: HWND,
    font: HFONT,
    id: i32,
    text: &str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    default: bool,
) -> Result<HWND> {
    let button_style = if default {
        BS_DEFPUSHBUTTON
    } else {
        BS_PUSHBUTTON
    };
    unsafe {
        create_control(
            parent,
            font,
            w!("BUTTON"),
            text,
            Some(id),
            WINDOW_EX_STYLE::default(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(button_style as u32),
            x,
            y,
            width,
            height,
        )
    }
}

fn combo_add(combo: HWND, text: &str) {
    let text = HSTRING::from(text);
    unsafe {
        SendMessageW(
            combo,
            CB_ADDSTRING,
            None,
            Some(LPARAM(text.as_ptr() as isize)),
        );
    }
}

fn combo_select(combo: HWND, index: usize) {
    unsafe {
        SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(index)), None);
    }
}

fn control(hwnd: HWND, id: i32) -> Result<HWND> {
    unsafe { GetDlgItem(Some(hwnd), id) }.with_context(|| format!("設定項目 {id} が見つかりません"))
}

fn control_text(hwnd: HWND, id: i32) -> Result<String> {
    let control = control(hwnd, id)?;
    let length = unsafe { GetWindowTextLengthW(control) };
    let mut buffer = vec![0u16; length.max(0) as usize + 1];
    let copied = unsafe { GetWindowTextW(control, &mut buffer) };
    Ok(String::from_utf16_lossy(&buffer[..copied.max(0) as usize]))
}

fn checked(hwnd: HWND, id: i32) -> Result<bool> {
    let control = control(hwnd, id)?;
    Ok(unsafe { SendMessageW(control, BM_GETCHECK, None, None) }.0 == 1)
}

fn selected(hwnd: HWND, id: i32) -> Result<usize> {
    let control = control(hwnd, id)?;
    let selected = unsafe { SendMessageW(control, CB_GETCURSEL, None, None) }.0;
    if selected < 0 {
        anyhow::bail!("選択されていない設定項目があります");
    }
    Ok(selected as usize)
}

fn set_control_text(hwnd: HWND, id: i32, text: &str) -> Result<()> {
    let control = control(hwnd, id)?;
    unsafe {
        SetWindowTextW(control, &HSTRING::from(text))
            .with_context(|| format!("設定項目 {id} を更新できません"))?;
    }
    Ok(())
}

fn update_hotkey_control(hwnd: HWND, state: &SettingsState) -> Result<()> {
    set_control_text(hwnd, ID_HOTKEY_CAPTURE, &state.hotkey.display_name())
}

fn selected_stamp_index(hwnd: HWND) -> Result<Option<usize>> {
    let list = control(hwnd, ID_STAMP_LIST)?;
    let selected = unsafe { SendMessageW(list, LB_GETCURSEL, None, None) }.0;
    Ok((selected >= 0).then_some(selected as usize))
}

fn refresh_stamp_list(
    hwnd: HWND,
    state: &mut SettingsState,
    selected: Option<usize>,
) -> Result<()> {
    let list = control(hwnd, ID_STAMP_LIST)?;
    unsafe {
        SendMessageW(list, LB_RESETCONTENT, None, None);
    }
    for stamp in &state.stamps {
        let label = HSTRING::from(format!(
            "{}  ({}×{})",
            stamp.name, stamp.width_px, stamp.height_px
        ));
        unsafe {
            SendMessageW(
                list,
                LB_ADDSTRING,
                None,
                Some(LPARAM(label.as_ptr() as isize)),
            );
        }
    }
    let selected = selected.filter(|index| *index < state.stamps.len());
    if let Some(index) = selected {
        unsafe {
            SendMessageW(list, LB_SETCURSEL, Some(WPARAM(index)), None);
        }
    }
    state.selected_stamp = selected;
    load_stamp_editor(hwnd, state)
}

fn load_stamp_editor(hwnd: HWND, state: &SettingsState) -> Result<()> {
    if let Some(stamp) = state
        .selected_stamp
        .and_then(|index| state.stamps.get(index))
    {
        set_control_text(hwnd, ID_STAMP_NAME, &stamp.name)?;
        set_control_text(
            hwnd,
            ID_STAMP_SIZE,
            &format!("{:.0}", stamp.default_height_n * 100.0),
        )?;
        set_control_text(
            hwnd,
            ID_STAMP_OPACITY,
            &format!("{:.0}", stamp.opacity * 100.0),
        )?;
    } else {
        set_control_text(hwnd, ID_STAMP_NAME, "")?;
        set_control_text(hwnd, ID_STAMP_SIZE, "")?;
        set_control_text(hwnd, ID_STAMP_OPACITY, "")?;
    }
    Ok(())
}

fn commit_stamp_editor(hwnd: HWND, state: &mut SettingsState) -> Result<()> {
    let Some(index) = state.selected_stamp else {
        return Ok(());
    };
    let Some(stamp) = state.stamps.get_mut(index) else {
        state.selected_stamp = None;
        return Ok(());
    };
    let name = control_text(hwnd, ID_STAMP_NAME)?.trim().to_owned();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        anyhow::bail!("スタンプ名は 1〜64 文字で指定してください");
    }
    let size_percent = control_text(hwnd, ID_STAMP_SIZE)?
        .trim()
        .parse::<f64>()
        .map_err(|_| anyhow!("スタンプの表示サイズには 1〜100 の数値を指定してください"))?;
    if !size_percent.is_finite() || !(1.0..=100.0).contains(&size_percent) {
        anyhow::bail!("スタンプの表示サイズには 1〜100 を指定してください");
    }
    let opacity_percent = control_text(hwnd, ID_STAMP_OPACITY)?
        .trim()
        .parse::<f64>()
        .map_err(|_| anyhow!("スタンプの不透明度には 0〜100 の数値を指定してください"))?;
    if !opacity_percent.is_finite() || !(0.0..=100.0).contains(&opacity_percent) {
        anyhow::bail!("スタンプの不透明度には 0〜100 を指定してください");
    }
    stamp.name = name;
    stamp.default_height_n = size_percent / 100.0;
    stamp.opacity = opacity_percent / 100.0;
    Ok(())
}

fn choose_png(hwnd: HWND) -> Result<Option<PathBuf>> {
    let filter: Vec<u16> = "PNG画像 (*.png)\0*.png\0\0".encode_utf16().collect();
    let title = HSTRING::from("登録するPNGスタンプを選択");
    let mut filename = vec![0u16; 32_768];
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(filename.as_mut_ptr()),
        nMaxFile: filename.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        lpstrDefExt: w!("png"),
        ..Default::default()
    };
    if unsafe { GetOpenFileNameW(&mut dialog) }.as_bool() {
        let length = filename
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(filename.len());
        return Ok(Some(PathBuf::from(std::ffi::OsString::from_wide(
            &filename[..length],
        ))));
    }
    let error = unsafe { CommDlgExtendedError() };
    if error.0 != 0 {
        anyhow::bail!(
            "ファイル選択ダイアログでエラーが発生しました: 0x{:x}",
            error.0
        );
    }
    Ok(None)
}

fn add_stamp(hwnd: HWND, state: &mut SettingsState) -> Result<()> {
    commit_stamp_editor(hwnd, state)?;
    if state.stamps.len() >= MAX_STAMPS {
        anyhow::bail!("登録できるスタンプは最大 {MAX_STAMPS} 個です");
    }
    let Some(source) = choose_png(hwnd)? else {
        let selected = state.selected_stamp;
        return refresh_stamp_list(hwnd, state, selected);
    };
    let (stamp, path) = config::import_stamp(&source)?;
    let mut updated_stamps = state.stamps.clone();
    updated_stamps.push(stamp);
    if let Err(error) = config::validate_stamp_catalog(&updated_stamps) {
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    state.stamps = updated_stamps;
    state.new_stamp_files.push(path);
    let selected = state.stamps.len() - 1;
    refresh_stamp_list(hwnd, state, Some(selected))
}

fn remove_stamp(hwnd: HWND, state: &mut SettingsState) -> Result<()> {
    commit_stamp_editor(hwnd, state)?;
    let Some(index) = state.selected_stamp else {
        return Ok(());
    };
    if index >= state.stamps.len() {
        return Ok(());
    }
    let removed = state.stamps.remove(index);
    if let Ok(path) = config::stamp_path(&removed.id) {
        if let Some(new_index) = state
            .new_stamp_files
            .iter()
            .position(|new_path| new_path == &path)
        {
            let _ = std::fs::remove_file(&path);
            state.new_stamp_files.remove(new_index);
        }
    }
    let next = (!state.stamps.is_empty()).then(|| index.min(state.stamps.len() - 1));
    refresh_stamp_list(hwnd, state, next)
}

fn change_stamp_selection(hwnd: HWND, state: &mut SettingsState) -> Result<()> {
    let newly_selected = selected_stamp_index(hwnd)?;
    let old_selected = state.selected_stamp;
    if let Err(error) = commit_stamp_editor(hwnd, state) {
        if let Some(index) = old_selected {
            let list = control(hwnd, ID_STAMP_LIST)?;
            unsafe {
                SendMessageW(list, LB_SETCURSEL, Some(WPARAM(index)), None);
            }
        }
        return Err(error);
    }
    refresh_stamp_list(hwnd, state, newly_selected)
}

fn read_config(hwnd: HWND, state: &mut SettingsState) -> Result<Config> {
    commit_stamp_editor(hwnd, state)?;
    let port = control_text(hwnd, ID_PORT)?
        .trim()
        .parse::<u16>()
        .map_err(|_| anyhow!("ローカルサーバーのポートには 1〜65535 を指定してください"))?;
    let screen = selected(hwnd, ID_MONITOR)?;
    if screen >= state.monitors.len() {
        anyhow::bail!("選択したモニターが見つかりません");
    }
    let projector_view = match selected(hwnd, ID_PROJECTOR_VIEW)? {
        0 => "program",
        1 => "preview",
        _ => anyhow::bail!("プロジェクター表示を選択してください"),
    };

    let config = Config {
        local_server_port: port,
        screen,
        canvas_aspect: control_text(hwnd, ID_ASPECT)?.trim().to_owned(),
        local_echo: checked(hwnd, ID_LOCAL_ECHO)?,
        confirm_before_clear: checked(hwnd, ID_CONFIRM_BEFORE_CLEAR)?,
        follow_projector: checked(hwnd, ID_FOLLOW_PROJECTOR)?,
        obs_control: checked(hwnd, ID_OBS_CONTROL)?,
        obs_websocket_url: control_text(hwnd, ID_OBS_URL)?.trim().to_owned(),
        obs_websocket_password: control_text(hwnd, ID_OBS_PASSWORD)?,
        projector_view: projector_view.to_owned(),
        close_projector: checked(hwnd, ID_CLOSE_PROJECTOR)?,
        hotkey: state.hotkey.clone(),
        brush: crate::config::BrushConfig {
            color: control_text(hwnd, ID_BRUSH_COLOR)?
                .trim()
                .to_ascii_lowercase(),
            width_n: control_text(hwnd, ID_BRUSH_WIDTH)?
                .trim()
                .parse()
                .map_err(|_| anyhow!("ブラシ幅には数値を指定してください"))?,
        },
        stamps: state.stamps.clone(),
    };
    config.validate()?;
    Ok(config)
}

fn update_overlay_url(hwnd: HWND) {
    let Ok(port) = control_text(hwnd, ID_PORT) else {
        return;
    };
    let Ok(url_control) = control(hwnd, ID_OVERLAY_URL) else {
        return;
    };
    let url = format!("http://127.0.0.1:{}/overlay", port.trim());
    unsafe {
        let _ = SetWindowTextW(url_control, &HSTRING::from(url));
    }
    let _ = set_control_text(hwnd, ID_COPY_OVERLAY_URL, "URLをコピー");
}

fn connection_status_labels(
    port_text: &str,
    diagnostics: Option<LocalServerDiagnosticsSnapshot>,
) -> (String, String) {
    let Some(diagnostics) = diagnostics else {
        return (
            "ローカルサーバー: この設定専用プロセスでは停止中".to_owned(),
            "OBS Browser Source: 未接続（通常起動後にトレイから確認してください）".to_owned(),
        );
    };

    let entered_port = port_text.trim().parse::<u16>().ok();
    let port_matches = entered_port == Some(diagnostics.port);
    let server = match diagnostics.reachability {
        LocalServerReachability::Starting => {
            format!("ローカルサーバー: 起動中（ポート {}）", diagnostics.port)
        }
        LocalServerReachability::Reachable if port_matches => format!(
            "ローカルサーバー: 到達可能（127.0.0.1:{}）",
            diagnostics.port
        ),
        LocalServerReachability::Reachable => format!(
            "ローカルサーバー: 到達可能（稼働ポート {}。入力中のURLとは不一致）",
            diagnostics.port
        ),
        LocalServerReachability::Stopped => {
            "ローカルサーバー: 停止（StreamPainterを再起動してください）".to_owned()
        }
    };
    let browser = match (diagnostics.reachability, diagnostics.browser_subscribers) {
        (LocalServerReachability::Starting, _) => {
            "OBS Browser Source: 未接続（ローカルサーバー起動中）".to_owned()
        }
        (LocalServerReachability::Stopped, _) => {
            "OBS Browser Source: 未接続（ローカルサーバーが停止中）".to_owned()
        }
        (LocalServerReachability::Reachable, 0) => {
            "OBS Browser Source: 未接続（OBSのURLを確認してソースを更新してください）".to_owned()
        }
        (LocalServerReachability::Reachable, count) => {
            format!("OBS Browser Source: 接続済み（{count}接続）")
        }
    };
    (server, browser)
}

fn update_connection_status(hwnd: HWND, state: &SettingsState) -> Result<()> {
    let port = control_text(hwnd, ID_PORT)?;
    let snapshot = state
        .diagnostics
        .as_ref()
        .map(LocalServerDiagnostics::snapshot);
    let (server, browser) = connection_status_labels(&port, snapshot);
    set_control_text(hwnd, ID_SERVER_STATUS, &server)?;
    set_control_text(hwnd, ID_BROWSER_STATUS, &browser)
}

fn copy_overlay_url(hwnd: HWND) -> Result<()> {
    let url = control_text(hwnd, ID_OVERLAY_URL)?;
    crate::win::clipboard::copy_text(hwnd, &url)
        .context("OBS Browser Source URLをコピーできません")?;
    set_control_text(hwnd, ID_COPY_OVERLAY_URL, "コピー済み")
}

fn send_hotkey_change(owner: HWND, command: ChangeCommand) -> Result<()> {
    hotkey::request_change(owner, command)
}

enum PreparedHotkeyMode {
    Live(HWND),
    Standalone(Option<ProbeRegistration>),
}

struct PreparedHotkeyChange {
    mode: PreparedHotkeyMode,
    finished: bool,
}

impl PreparedHotkeyChange {
    fn prepare(
        live_owner: Option<HWND>,
        settings_hwnd: HWND,
        previous: &HotkeyConfig,
        config: &HotkeyConfig,
    ) -> Result<Self> {
        let mode = if let Some(owner) = live_owner {
            send_hotkey_change(owner, ChangeCommand::Prepare(config.clone()))?;
            PreparedHotkeyMode::Live(owner)
        } else {
            // `--settings`を通常版と同時に開いた場合、変更していないキーは通常版自身が
            // 保持している。再登録を試すと自己競合するため、意味が変わる時だけprobeする。
            let changed = previous.chord()? != config.chord()?;
            PreparedHotkeyMode::Standalone(
                changed
                    .then(|| ProbeRegistration::acquire(settings_hwnd, config))
                    .transpose()?,
            )
        };
        Ok(Self {
            mode,
            finished: false,
        })
    }

    fn commit(&mut self) -> Result<()> {
        if let PreparedHotkeyMode::Live(owner) = &self.mode {
            send_hotkey_change(*owner, ChangeCommand::Commit)?;
        }
        self.finished = true;
        Ok(())
    }

    fn rollback(&mut self) -> Result<()> {
        if let PreparedHotkeyMode::Live(owner) = &self.mode {
            send_hotkey_change(*owner, ChangeCommand::Rollback)?;
        }
        self.finished = true;
        Ok(())
    }
}

impl Drop for PreparedHotkeyChange {
    fn drop(&mut self) {
        if !self.finished {
            if let PreparedHotkeyMode::Live(owner) = &self.mode {
                let _ = send_hotkey_change(*owner, ChangeCommand::Rollback);
            }
        }
        // Standalone probeはfieldのDropで必ず解除される。
        if let PreparedHotkeyMode::Standalone(probe) = &self.mode {
            let _ = probe;
        }
    }
}

fn rollback_autostart(change: &mut Option<PreparedAutostartChange>) -> Option<anyhow::Error> {
    change.as_mut().and_then(|change| change.rollback().err())
}

fn save_with_transactions(
    hwnd: HWND,
    state: &mut SettingsState,
    config: &Config,
    autostart_enabled: bool,
) -> Result<()> {
    let mut hotkey_change = PreparedHotkeyChange::prepare(
        state.live_owner,
        hwnd,
        &state.original_config.hotkey,
        &config.hotkey,
    )?;
    // Registry値を先に更新するが、config/hotkey確定までは元の生値を保持する。
    // REG_SZ以外の壊れた値を修復した場合も、後続失敗時は型とdataをそのまま戻す。
    let mut autostart_change = state
        .autostart
        .controller()
        .map(|controller| controller.prepare(autostart_enabled))
        .transpose()?;
    if let Err(save_error) = config::save(config) {
        let hotkey_rollback = hotkey_change.rollback().err();
        let autostart_rollback = rollback_autostart(&mut autostart_change);
        // 保護資格情報の更新失敗など、設定ファイルcommit後に失敗する経路もあるため、
        // 読み込み時の内容へbest-effortで戻す。
        let config_rollback = config::save(&state.original_config).err();
        let mut detail = format!("設定を保存できませんでした: {save_error:#}");
        if let Some(error) = hotkey_rollback {
            detail.push_str(&format!(
                "\nホットキー登録の復元にも失敗しました: {error:#}"
            ));
        }
        if let Some(error) = autostart_rollback {
            detail.push_str(&format!(
                "\nWindows自動起動登録の復元にも失敗しました: {error:#}"
            ));
        }
        if let Some(error) = config_rollback {
            detail.push_str(&format!("\n設定ファイルの復元にも失敗しました: {error:#}"));
        }
        anyhow::bail!(detail);
    }
    if let Err(commit_error) = hotkey_change.commit() {
        let hotkey_rollback = hotkey_change.rollback().err();
        let autostart_rollback = rollback_autostart(&mut autostart_change);
        let config_rollback = config::save(&state.original_config).err();
        let mut detail = format!("保存後のホットキー変更を確定できませんでした: {commit_error:#}");
        if let Some(error) = hotkey_rollback {
            detail.push_str(&format!(
                "\nホットキー登録の復元にも失敗しました: {error:#}"
            ));
        }
        if let Some(error) = autostart_rollback {
            detail.push_str(&format!(
                "\nWindows自動起動登録の復元にも失敗しました: {error:#}"
            ));
        }
        if let Some(error) = config_rollback {
            detail.push_str(&format!("\n設定ファイルの復元にも失敗しました: {error:#}"));
        }
        anyhow::bail!(detail);
    }
    if let Some(change) = autostart_change.as_mut() {
        change.commit();
    }
    Ok(())
}

fn show_error(hwnd: HWND, error: &anyhow::Error) {
    show_operation_error(hwnd, "設定を更新できません", error);
}

fn show_operation_error(hwnd: HWND, summary: &str, error: &anyhow::Error) {
    unsafe {
        MessageBoxW(
            Some(hwnd),
            &HSTRING::from(format!("{summary}:\n{error:#}")),
            w!("StreamPainter 設定"),
            MB_OK | MB_ICONERROR,
        );
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as i32;
            let notification = ((wparam.0 >> 16) & 0xffff) as u32;

            if id == ID_PORT && notification == EN_CHANGE {
                update_overlay_url(hwnd);
                let state_ptr =
                    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut SettingsState;
                if let Some(state) = unsafe { state_ptr.as_ref() } {
                    let _ = update_connection_status(hwnd, state);
                }
                return LRESULT(0);
            }
            if id == ID_COPY_OVERLAY_URL {
                if let Err(error) = copy_overlay_url(hwnd) {
                    show_operation_error(hwnd, "URLをコピーできません", &error);
                }
                return LRESULT(0);
            }
            if id == ID_HOTKEY_CLEAR || id == ID_HOTKEY_DEFAULT {
                let state_ptr =
                    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut SettingsState;
                let result = unsafe {
                    state_ptr
                        .as_mut()
                        .ok_or_else(|| anyhow!("設定画面の状態がありません"))
                        .and_then(|state| {
                            state.hotkey = if id == ID_HOTKEY_CLEAR {
                                HotkeyConfig::disabled()
                            } else {
                                HotkeyConfig::default()
                            };
                            update_hotkey_control(hwnd, state)
                        })
                };
                if let Err(error) = result {
                    show_error(hwnd, &error);
                }
                return LRESULT(0);
            }
            if id == ID_STAMP_LIST && notification == LBN_SELCHANGE {
                let state_ptr =
                    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut SettingsState;
                let result = unsafe {
                    state_ptr
                        .as_mut()
                        .ok_or_else(|| anyhow!("設定画面の状態がありません"))
                        .and_then(|state| change_stamp_selection(hwnd, state))
                };
                if let Err(error) = result {
                    show_error(hwnd, &error);
                }
                return LRESULT(0);
            }
            if id == ID_STAMP_ADD || id == ID_STAMP_REMOVE {
                let state_ptr =
                    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut SettingsState;
                let result = unsafe {
                    state_ptr
                        .as_mut()
                        .ok_or_else(|| anyhow!("設定画面の状態がありません"))
                        .and_then(|state| {
                            if id == ID_STAMP_ADD {
                                add_stamp(hwnd, state)
                            } else {
                                remove_stamp(hwnd, state)
                            }
                        })
                };
                if let Err(error) = result {
                    show_error(hwnd, &error);
                }
                return LRESULT(0);
            }
            if id == ID_SAVE {
                let state_ptr =
                    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut SettingsState;
                let result = unsafe {
                    state_ptr
                        .as_mut()
                        .ok_or_else(|| anyhow!("設定画面の状態がありません"))
                        .and_then(|state| {
                            let config = read_config(hwnd, state)?;
                            let autostart_enabled = checked(hwnd, ID_AUTOSTART)?;
                            save_with_transactions(hwnd, state, &config, autostart_enabled)?;
                            let live_hotkey = state.live_owner.is_some();
                            let autostart_available = state.autostart.is_available();
                            state.saved = true;
                            Ok((live_hotkey, autostart_available))
                        })
                };
                match result {
                    Ok((live_hotkey, autostart_available)) => unsafe {
                        let message = if !autostart_available {
                            "設定を保存しました。\nWindows自動起動は状態を取得できなかったため変更していません。その他の設定は再起動すると反映されます。"
                        } else if live_hotkey {
                            "設定を保存しました。\nホットキーとWindows自動起動はすぐに反映されます。その他の設定は再起動すると反映されます。"
                        } else {
                            "設定を保存しました。\nWindows自動起動はすぐに反映されます。--settingsで変更したホットキーを含むその他の設定は再起動後に反映されます。"
                        };
                        MessageBoxW(
                            Some(hwnd),
                            &HSTRING::from(message),
                            w!("StreamPainter 設定"),
                            MB_OK | MB_ICONINFORMATION,
                        );
                        let _ = DestroyWindow(hwnd);
                    },
                    Err(error) => show_error(hwnd, &error),
                }
                return LRESULT(0);
            }
            if id == ID_CANCEL {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_DIAGNOSTICS_CHANGED => {
            let state_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut SettingsState;
            if let Some(state) = unsafe { state_ptr.as_ref() } {
                if let Err(error) = update_connection_status(hwnd, state) {
                    show_error(hwnd, &error);
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            SETTINGS_HWND.store(0, Ordering::SeqCst);
            let state_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut SettingsState;
            if !state_ptr.is_null() {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    drop(Box::from_raw(state_ptr));
                }
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostics(
        reachability: LocalServerReachability,
        browser_subscribers: usize,
    ) -> LocalServerDiagnosticsSnapshot {
        LocalServerDiagnosticsSnapshot {
            port: 16_873,
            reachability,
            browser_subscribers,
        }
    }

    #[test]
    fn status_labels_distinguish_reachable_server_from_browser_connection() {
        let (server, browser) = connection_status_labels(
            "16873",
            Some(diagnostics(LocalServerReachability::Reachable, 0)),
        );
        assert!(server.contains("到達可能"));
        assert!(browser.contains("未接続"));

        let (server, browser) = connection_status_labels(
            "16873",
            Some(diagnostics(LocalServerReachability::Reachable, 1)),
        );
        assert!(server.contains("到達可能"));
        assert!(browser.contains("接続済み"));
    }

    #[test]
    fn status_labels_explain_port_mismatch_and_server_stop() {
        let (server, _) = connection_status_labels(
            "16874",
            Some(diagnostics(LocalServerReachability::Reachable, 0)),
        );
        assert!(server.contains("入力中のURLとは不一致"));
        assert!(server.contains("16873"));

        let (server, browser) = connection_status_labels(
            "16873",
            Some(diagnostics(LocalServerReachability::Stopped, 0)),
        );
        assert!(server.contains("停止"));
        assert!(browser.contains("未接続"));
    }

    #[test]
    fn standalone_settings_does_not_claim_a_browser_connection() {
        let (server, browser) = connection_status_labels("16873", None);
        assert!(server.contains("停止中"));
        assert!(browser.contains("未接続"));
    }
}
