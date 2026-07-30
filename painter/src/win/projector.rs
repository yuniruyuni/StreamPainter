//! OBS 全画面プロジェクターの検知。
//!
//! 対象モニタを全面で覆っている可視ウィンドウのうち、OBS プロセス
//! (obs64.exe / obs32.exe / obs.exe) に属するものを全画面プロジェクターと
//! みなす。ウィンドウタイトルはロケール依存 (「全画面プロジェクター」/
//! "Fullscreen Projector") のため判定に使わない。

use windows::core::BOOL;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
};

use crate::win::monitor::Monitor;

const OBS_EXE_NAMES: [&str; 3] = ["obs64.exe", "obs32.exe", "obs.exe"];

/// --detect 診断モード: モニタと全画面級ウィンドウの一覧を出力する
pub fn print_diagnosis() {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowTextW;

    let monitors = crate::win::monitor::enumerate();
    for (i, m) in monitors.iter().enumerate() {
        println!(
            "monitor {i}: {}x{} at ({},{}) primary={}",
            m.width, m.height, m.x, m.y, m.primary
        );
    }

    struct Dump {
        monitors: Vec<Monitor>,
    }
    unsafe extern "system" fn dump_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let dump = unsafe { &*(lparam.0 as *const Dump) };
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() || unsafe { IsIconic(hwnd) }.as_bool() {
            return BOOL::from(true);
        }
        let mut rect = RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
            return BOOL::from(true);
        }
        // 小さいウィンドウはノイズなので省く
        if (rect.right - rect.left) < 600 || (rect.bottom - rect.top) < 400 {
            return BOOL::from(true);
        }
        let mut title = [0u16; 128];
        let n = unsafe { GetWindowTextW(hwnd, &mut title) } as usize;
        let title = String::from_utf16_lossy(&title[..n]);
        let covers: Vec<usize> = dump
            .monitors
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                rect.left <= m.x + TOLERANCE
                    && rect.top <= m.y + TOLERANCE
                    && rect.right >= m.x + m.width - TOLERANCE
                    && rect.bottom >= m.y + m.height - TOLERANCE
            })
            .map(|(i, _)| i)
            .collect();
        println!(
            "hwnd={:?} rect=({},{})-({},{}) covers={:?} obs={} title={:?}",
            hwnd.0,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            covers,
            is_obs_process(hwnd),
            title,
        );
        BOOL::from(true)
    }
    let dump = Dump { monitors };
    unsafe {
        let _ = EnumWindows(Some(dump_proc), LPARAM(&dump as *const _ as isize));
    }
}
/// 全画面判定の許容誤差 (px)
const TOLERANCE: i32 = 2;

struct Search {
    monitor: Monitor,
    own: HWND,
    found: Option<HWND>,
}

/// 対象モニタ上の OBS 全画面プロジェクターのウィンドウを探す
pub fn find_projector(monitor: &Monitor, own: HWND) -> Option<HWND> {
    let mut search = Search {
        monitor: *monitor,
        own,
        found: None,
    };
    unsafe {
        // コールバックが false を返すと EnumWindows は Err になるが、それは
        // 発見による中断なのでエラーとして扱わない
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut search as *mut _ as isize));
    }
    search.found
}

/// 対象モニタ上に OBS の全画面プロジェクターが表示されているか
pub fn is_projector_visible(monitor: &Monitor, own: HWND) -> bool {
    find_projector(monitor, own).is_some()
}

/// プロジェクターを閉じる (WM_CLOSE を送る)。見つからなければ false
pub fn close_projector(monitor: &Monitor, own: HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
    match find_projector(monitor, own) {
        Some(hwnd) => {
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, Default::default(), Default::default());
            }
            true
        }
        None => false,
    }
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let search = unsafe { &mut *(lparam.0 as *mut Search) };
    if hwnd == search.own {
        return BOOL::from(true);
    }
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() || unsafe { IsIconic(hwnd) }.as_bool() {
        return BOOL::from(true);
    }

    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return BOOL::from(true);
    }
    let m = &search.monitor;
    let covers = rect.left <= m.x + TOLERANCE
        && rect.top <= m.y + TOLERANCE
        && rect.right >= m.x + m.width - TOLERANCE
        && rect.bottom >= m.y + m.height - TOLERANCE;
    if !covers {
        return BOOL::from(true);
    }

    if is_obs_process(hwnd) {
        search.found = Some(hwnd);
        return BOOL::from(false); // 列挙を打ち切る
    }
    BOOL::from(true)
}

fn is_obs_process(hwnd: HWND) -> bool {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return false;
    }
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return false;
    };

    let mut buf = [0u16; 512];
    let mut len = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .is_ok();
    unsafe {
        let _ = CloseHandle(handle);
    }
    if !ok {
        return false;
    }

    let path = String::from_utf16_lossy(&buf[..len as usize]).to_lowercase();
    let name = path.rsplit(['\\', '/']).next().unwrap_or("");
    OBS_EXE_NAMES.contains(&name)
}
