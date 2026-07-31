//! モニタ列挙 (docs/painter.md)。座標はすべて物理ピクセル (Per-Monitor V2)。

use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Monitor {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub primary: bool,
}

pub fn enumerate() -> Vec<Monitor> {
    let mut monitors: Vec<Monitor> = Vec::new();
    unsafe extern "system" fn callback(
        hmonitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let monitors = unsafe { &mut *(lparam.0 as *mut Vec<Monitor>) };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetMonitorInfoW(hmonitor, &mut info) }.as_bool() {
            let r = info.rcMonitor;
            monitors.push(Monitor {
                x: r.left,
                y: r.top,
                width: r.right - r.left,
                height: r.bottom - r.top,
                primary: (info.dwFlags & 1) != 0, // MONITORINFOF_PRIMARY
            });
        }
        BOOL::from(true)
    }
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(callback),
            LPARAM(&mut monitors as *mut _ as isize),
        );
    }
    // プライマリを先頭にした安定順 (config.screen の index が指しやすいように)
    monitors.sort_by_key(|m| (!m.primary, m.x, m.y));
    monitors
}
