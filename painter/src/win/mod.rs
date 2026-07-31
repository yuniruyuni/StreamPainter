pub mod app;
pub mod menu;
pub mod monitor;
pub mod projector;
pub mod render;
pub mod settings;
pub mod tray;

use anyhow::{bail, Result};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK, SW_SHOWNORMAL};

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
