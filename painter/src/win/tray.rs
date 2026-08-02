//! タスクトレイ常駐 (docs/painter.md)。
//! 右クリックメニュー: 描画モード切替 / URLコピー・接続診断 / 設定 / 終了。
//! オーバーレイは WS_EX_NOACTIVATE で操作 UI を持たないため、終了導線はここが正となる。

use anyhow::{Context, Result};
use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, LoadIconW, SetForegroundWindow,
    TrackPopupMenu, IDI_APPLICATION, MF_GRAYED, MF_SEPARATOR, MF_STRING, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_CONTEXTMENU, WM_LBUTTONUP, WM_RBUTTONUP,
};

use crate::net::local_server::{LocalServerDiagnosticsSnapshot, LocalServerReachability};

/// トレイからのコールバックメッセージ (window_proc で処理する)
pub const WM_TRAY: u32 = WM_APP + 1;

const TRAY_ID: u32 = 1;

/// メニュー選択の結果
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrayCommand {
    ToggleMode,
    CopyOverlayUrl,
    Settings,
    Logs,
    Licenses,
    Exit,
}

const MENU_TOGGLE: usize = 1;
const MENU_COPY_OVERLAY_URL: usize = 2;
const MENU_SETTINGS: usize = 3;
const MENU_LOGS: usize = 4;
const MENU_LICENSES: usize = 5;
const MENU_EXIT: usize = 6;
const MENU_SERVER_STATUS: usize = 100;
const MENU_BROWSER_STATUS: usize = 101;

fn set_tip(data: &mut NOTIFYICONDATAW, hotkey: Option<&str>) {
    let tip_text = match hotkey {
        Some(hotkey) => format!("StreamPainter ({hotkey}: 描画モード切替)"),
        None => "StreamPainter (描画モードはトレイから切替)".to_owned(),
    };
    let tip: Vec<u16> = tip_text.encode_utf16().chain(std::iter::once(0)).collect();
    let copy_len = tip.len().min(data.szTip.len());
    data.szTip[..copy_len].copy_from_slice(&tip[..copy_len]);
    // 上限で切れた場合も必ず終端する。
    data.szTip[data.szTip.len() - 1] = 0;
}

pub fn add(hwnd: HWND, hotkey: Option<&str>) -> Result<()> {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: unsafe { LoadIconW(None, IDI_APPLICATION).context("LoadIconW")? },
        ..Default::default()
    };
    set_tip(&mut data, hotkey);

    if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
        anyhow::bail!("Shell_NotifyIconW(NIM_ADD) failed");
    }
    Ok(())
}

pub fn update_hotkey(hwnd: HWND, hotkey: Option<&str>) -> Result<()> {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        uFlags: NIF_TIP,
        ..Default::default()
    };
    set_tip(&mut data, hotkey);
    if !unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool() {
        anyhow::bail!("Shell_NotifyIconW(NIM_MODIFY) failed");
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
fn diagnostics_labels(diagnostics: LocalServerDiagnosticsSnapshot) -> (String, String) {
    let server = match diagnostics.reachability {
        LocalServerReachability::Starting => {
            format!("ローカルサーバー: 起動中 ({})", diagnostics.port)
        }
        LocalServerReachability::Reachable => {
            format!("ローカルサーバー: 到達可能 ({})", diagnostics.port)
        }
        LocalServerReachability::Stopped => "ローカルサーバー: 停止".to_owned(),
    };
    let browser = if diagnostics.reachability == LocalServerReachability::Reachable
        && diagnostics.browser_subscribers > 0
    {
        format!(
            "OBS Browser Source: 接続済み ({}接続)",
            diagnostics.browser_subscribers
        )
    } else {
        "OBS Browser Source: 未接続".to_owned()
    };
    (server, browser)
}

pub fn on_message(
    hwnd: HWND,
    lparam_low: u32,
    hotkey: Option<&str>,
    diagnostics: LocalServerDiagnosticsSnapshot,
) -> Option<TrayCommand> {
    if lparam_low != WM_RBUTTONUP && lparam_low != WM_LBUTTONUP && lparam_low != WM_CONTEXTMENU {
        return None;
    }
    // overlay / OBS projector の定期的な Z-order 再構成よりトレイメニューを上に保つ。
    let _foreground_ui = crate::win::projector::ForegroundUiGuard::new();
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let toggle_label = if let Some(hotkey) = hotkey {
            HSTRING::from(format!("描画モード切替 ({hotkey})"))
        } else {
            HSTRING::from("描画モード切替")
        };
        let _ = AppendMenuW(menu, MF_STRING, MENU_TOGGLE, &toggle_label);
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            MENU_COPY_OVERLAY_URL,
            w!("OBS Browser Source URLをコピー"),
        );
        let (server_status, browser_status) = diagnostics_labels(diagnostics);
        let _ = AppendMenuW(
            menu,
            MF_STRING | MF_GRAYED,
            MENU_SERVER_STATUS,
            &HSTRING::from(server_status),
        );
        let _ = AppendMenuW(
            menu,
            MF_STRING | MF_GRAYED,
            MENU_BROWSER_STATUS,
            &HSTRING::from(browser_status),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, MENU_SETTINGS, w!("設定..."));
        let _ = AppendMenuW(menu, MF_STRING, MENU_LOGS, w!("ログフォルダー..."));
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
            MENU_COPY_OVERLAY_URL => Some(TrayCommand::CopyOverlayUrl),
            MENU_SETTINGS => Some(TrayCommand::Settings),
            MENU_LOGS => Some(TrayCommand::Logs),
            MENU_LICENSES => Some(TrayCommand::Licenses),
            MENU_EXIT => Some(TrayCommand::Exit),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_labels_keep_server_and_browser_state_separate() {
        let (server, browser) = diagnostics_labels(LocalServerDiagnosticsSnapshot {
            port: 16_873,
            reachability: LocalServerReachability::Reachable,
            browser_subscribers: 0,
        });
        assert!(server.contains("到達可能"));
        assert!(browser.contains("未接続"));

        let (_, browser) = diagnostics_labels(LocalServerDiagnosticsSnapshot {
            port: 16_873,
            reachability: LocalServerReachability::Reachable,
            browser_subscribers: 2,
        });
        assert!(browser.contains("接続済み"));
        assert!(browser.contains("2接続"));

        let (server, browser) = diagnostics_labels(LocalServerDiagnosticsSnapshot {
            port: 16_873,
            reachability: LocalServerReachability::Stopped,
            browser_subscribers: 0,
        });
        assert!(server.contains("停止"));
        assert!(browser.contains("未接続"));
    }
}
