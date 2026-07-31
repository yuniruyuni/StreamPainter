//! タスクトレイから開くネイティブ設定画面。
//! 保存内容は config.toml に反映し、実行中の描画状態は変更しない。

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
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetDesktopWindow, GetDlgItem,
    GetMessageW, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    IsDialogMessageW, IsWindow, LoadCursorW, MessageBoxW, RegisterClassW, SendMessageW,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowTextW, ShowWindow, TranslateMessage,
    BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CBS_DROPDOWNLIST,
    CBS_HASSTRINGS, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, EN_CHANGE, ES_AUTOHSCROLL, ES_NUMBER,
    ES_PASSWORD, ES_READONLY, GWLP_USERDATA, HMENU, IDC_ARROW, LBN_SELCHANGE, LBS_NOINTEGRALHEIGHT,
    LBS_NOTIFY, LB_ADDSTRING, LB_GETCURSEL, LB_RESETCONTENT, LB_SETCURSEL, MB_ICONERROR,
    MB_ICONINFORMATION, MB_OK, MSG, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND,
    WM_NCDESTROY, WM_SETFONT, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE,
    WS_EX_CONTROLPARENT, WS_EX_DLGMODALFRAME, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use crate::config::{self, Config, StampConfig, MAX_STAMPS};
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

static SETTINGS_HWND: AtomicIsize = AtomicIsize::new(0);

struct SettingsState {
    monitors: Vec<Monitor>,
    font: Option<HFONT>,
    stamps: Vec<StampConfig>,
    selected_stamp: Option<usize>,
    new_stamp_files: Vec<PathBuf>,
    saved: bool,
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
    raw != 0
        && unsafe {
            let hwnd = hwnd_from_raw(raw);
            IsWindow(Some(hwnd)).as_bool() && IsDialogMessageW(hwnd, message).as_bool()
        }
}

/// 通常起動できない場合にも `stream-painter.exe --settings` で設定だけを編集できる。
pub fn run_standalone() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    open(unsafe { GetDesktopWindow() })?;

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
pub fn open(owner: HWND) -> Result<()> {
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
        let window_height = scale(820, dpi);
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
            monitors,
            font: None,
            stamps: config.stamps.clone(),
            selected_stamp: None,
            new_stamp_files: Vec::new(),
            saved: false,
            _foreground_ui: crate::win::projector::ForegroundUiGuard::new(),
        });
        let state_ptr = Box::into_raw(state);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);

        if let Err(error) = initialize_controls(hwnd, &config, dpi) {
            let _ = DestroyWindow(hwnd);
            return Err(error);
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
            field_width,
            row_height,
            ES_AUTOHSCROLL | ES_READONLY,
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
            "F9でOBSプロジェクターを自動的に開く（obs-websocket）",
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
            "標準ブラシ色（#RRGGBB）",
            label_x,
            s(397),
            label_width,
            row_height,
        )?;
        create_edit(
            hwnd,
            font,
            ID_BRUSH_COLOR,
            &config.brush.color,
            field_x,
            s(394),
            s(160),
            row_height,
            ES_AUTOHSCROLL,
        )?;

        create_label(
            hwnd,
            font,
            "標準ブラシ幅（正規化値）",
            label_x,
            s(431),
            label_width,
            row_height,
        )?;
        create_edit(
            hwnd,
            font,
            ID_BRUSH_WIDTH,
            &config.brush.width_n.to_string(),
            field_x,
            s(428),
            s(160),
            row_height,
            ES_AUTOHSCROLL,
        )?;

        create_label(
            hwnd,
            font,
            &format!("登録スタンプ（最大 {MAX_STAMPS} 個、PNGのみ）"),
            label_x,
            s(468),
            s(280),
            row_height,
        )?;
        create_listbox(hwnd, font, ID_STAMP_LIST, label_x, s(494), s(285), s(132))?;
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

        create_label(hwnd, font, "スタンプ名", s(330), s(494), s(130), row_height)?;
        create_edit(
            hwnd,
            font,
            ID_STAMP_NAME,
            "",
            s(330),
            s(518),
            s(310),
            row_height,
            ES_AUTOHSCROLL,
        )?;
        create_label(
            hwnd,
            font,
            "表示サイズ（キャンバス高の%）",
            s(330),
            s(554),
            s(220),
            row_height,
        )?;
        create_edit(
            hwnd,
            font,
            ID_STAMP_SIZE,
            "",
            s(550),
            s(551),
            s(90),
            row_height,
            ES_AUTOHSCROLL | ES_NUMBER,
        )?;
        create_label(
            hwnd,
            font,
            "不透明度（%）",
            s(330),
            s(590),
            s(150),
            row_height,
        )?;
        create_edit(
            hwnd,
            font,
            ID_STAMP_OPACITY,
            "",
            s(550),
            s(587),
            s(90),
            row_height,
            ES_AUTOHSCROLL | ES_NUMBER,
        )?;
        create_label(
            hwnd,
            font,
            "右クリックメニューの「スタンプ」から選び、クリック位置へ配置します。",
            s(330),
            s(626),
            s(310),
            s(40),
        )?;
        refresh_stamp_list(hwnd, state, (!state.stamps.is_empty()).then_some(0))?;

        create_label(
            hwnd,
            font,
            concat!(
                "保存した設定は StreamPainter の再起動後に反映されます。\n",
                "ポートを変えた場合は、OBS Browser Source のURLも上記URLへ変更してください。"
            ),
            label_x,
            s(688),
            s(620),
            s(48),
        )?;

        create_button(
            hwnd,
            font,
            ID_SAVE,
            "保存",
            s(430),
            s(748),
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
            s(748),
            s(100),
            s(32),
            false,
        )?;
    }

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
        follow_projector: checked(hwnd, ID_FOLLOW_PROJECTOR)?,
        obs_control: checked(hwnd, ID_OBS_CONTROL)?,
        obs_websocket_url: control_text(hwnd, ID_OBS_URL)?.trim().to_owned(),
        obs_websocket_password: control_text(hwnd, ID_OBS_PASSWORD)?,
        projector_view: projector_view.to_owned(),
        close_projector: checked(hwnd, ID_CLOSE_PROJECTOR)?,
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
}

fn show_error(hwnd: HWND, error: &anyhow::Error) {
    unsafe {
        MessageBoxW(
            Some(hwnd),
            &HSTRING::from(format!("設定を更新できません:\n{error:#}")),
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
                            config::save(&config)?;
                            state.saved = true;
                            Ok(())
                        })
                };
                match result {
                    Ok(()) => unsafe {
                        MessageBoxW(
                            Some(hwnd),
                            w!("設定を保存しました。\nStreamPainter を再起動すると反映されます。"),
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
