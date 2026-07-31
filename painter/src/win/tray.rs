//! タスクトレイ常駐 (docs/painter.md)。
//! 右クリックメニュー: 描画モード切替 (F9) / 設定 / ライセンス / 終了。
//! オーバーレイは WS_EX_NOACTIVATE で操作 UI を持たないため、終了導線はここが正となる。

use anyhow::{Context, Result};
use windows::core::w;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, LoadIconW, SetForegroundWindow,
    TrackPopupMenu, IDI_APPLICATION, MF_SEPARATOR, MF_STRING, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WM_APP, WM_CONTEXTMENU, WM_LBUTTONUP, WM_RBUTTONUP,
};

/// トレイからのコールバックメッセージ (window_proc で処理する)
pub const WM_TRAY: u32 = WM_APP + 1;

const TRAY_ID: u32 = 1;

/// メニュー選択の結果
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrayCommand {
    ToggleMode,
    Settings,
    Licenses,
    Exit,
}

const MENU_TOGGLE: usize = 1;
const MENU_SETTINGS: usize = 2;
const MENU_LICENSES: usize = 3;
const MENU_EXIT: usize = 4;

pub fn add(hwnd: HWND, hotkey_registered: bool) -> Result<()> {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: unsafe { LoadIconW(None, IDI_APPLICATION).context("LoadIconW")? },
        ..Default::default()
    };
    let tip_text = if hotkey_registered {
        "StreamPainter (F9: 描画モード切替)"
    } else {
        "StreamPainter (描画モードはトレイから切替)"
    };
    let tip: Vec<u16> = tip_text.encode_utf16().chain(std::iter::once(0)).collect();
    data.szTip[..tip.len().min(128)].copy_from_slice(&tip[..tip.len().min(128)]);

    if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
        anyhow::bail!("Shell_NotifyIconW(NIM_ADD) failed");
    }
    Ok(())
}

pub fn remove(hwnd: HWND) {
    let data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        ..Default::default()
    };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

/// WM_TRAY 受信時の処理。メニューを出し、選択されたコマンドを返す
pub fn on_message(hwnd: HWND, lparam_low: u32, hotkey_registered: bool) -> Option<TrayCommand> {
    if lparam_low != WM_RBUTTONUP && lparam_low != WM_LBUTTONUP && lparam_low != WM_CONTEXTMENU {
        return None;
    }
    // overlay / OBS projector の定期的な Z-order 再構成よりトレイメニューを上に保つ。
    let _foreground_ui = crate::win::projector::ForegroundUiGuard::new();
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let toggle_label = if hotkey_registered {
            w!("描画モード切替 (F9)")
        } else {
            w!("描画モード切替")
        };
        let _ = AppendMenuW(menu, MF_STRING, MENU_TOGGLE, toggle_label);
        let _ = AppendMenuW(menu, MF_STRING, MENU_SETTINGS, w!("設定..."));
        let _ = AppendMenuW(menu, MF_STRING, MENU_LICENSES, w!("第三者ライセンス..."));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("終了"));

        let mut pos = POINT::default();
        let _ = GetCursorPos(&mut pos);
        // メニューを閉じられるようにするための定石 (フォーカスを一時的に取る)
        let _ = SetForegroundWindow(hwnd);
        let selected = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
            pos.x,
            pos.y,
            None,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);

        match selected.0 as usize {
            MENU_TOGGLE => Some(TrayCommand::ToggleMode),
            MENU_SETTINGS => Some(TrayCommand::Settings),
            MENU_LICENSES => Some(TrayCommand::Licenses),
            MENU_EXIT => Some(TrayCommand::Exit),
            _ => None,
        }
    }
}
