pub mod app;
pub mod clipboard;
pub mod hotkey;
pub mod logging;
pub mod menu;
pub mod monitor;
pub mod projector;
pub mod radial_menu;
pub mod render;
pub mod settings;
pub mod single_instance;
pub mod tray;

use anyhow::{bail, Result};
use std::path::Path;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK,
    MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, SW_SHOWNORMAL,
};

pub fn message_box(text: &str) {
    let _foreground_ui = projector::ForegroundUiGuard::new();
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from("StreamPainter"),
            MB_OK | MB_ICONERROR,
        );
    }
}

pub fn message_box_info(text: &str) {
    let _foreground_ui = projector::ForegroundUiGuard::new();
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from("StreamPainter"),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

/// Topmost overlayの起動直後でも、競合通知をその前面へ確実に出す。
pub fn message_box_warning(parent: HWND, text: &str) {
    let _foreground_ui = projector::ForegroundUiGuard::new();
    unsafe {
        MessageBoxW(
            Some(parent),
            &HSTRING::from(text),
            &HSTRING::from("StreamPainter"),
            MB_OK | MB_ICONWARNING | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }
}

pub fn confirm(parent: HWND, text: &str) -> bool {
    let _foreground_ui = projector::ForegroundUiGuard::new();
    unsafe {
        MessageBoxW(
            Some(parent),
            &HSTRING::from(text),
            &HSTRING::from("StreamPainter"),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        ) == IDYES
    }
}

pub fn open_url(parent: HWND, url: &str) -> Result<()> {
    let result = unsafe {
        ShellExecuteW(
            Some(parent),
            w!("open"),
            &HSTRING::from(url),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        bail!(
            "既定のWebブラウザを開けませんでした (ShellExecuteW: {})",
            result.0 as isize
        );
    }
    Ok(())
}

pub fn open_path(parent: HWND, path: &Path) -> Result<()> {
    let result = unsafe {
        ShellExecuteW(
            Some(parent),
            w!("open"),
            &HSTRING::from(path),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        bail!(
            "エクスプローラーを開けませんでした (ShellExecuteW: {})",
            result.0 as isize
        );
    }
    Ok(())
}
