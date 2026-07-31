//! OBS 全画面プロジェクターの検知。
//!
//! 対象モニタを全面で覆っている可視ウィンドウのうち、OBS プロセス
//! (obs64.exe / obs32.exe / obs.exe) に属するものを全画面プロジェクターと
//! みなす。ウィンドウタイトルはロケール依存 (「全画面プロジェクター」/
//! "Fullscreen Projector") のため判定に使わない。

use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;

use windows::core::{Result as WindowsResult, BOOL};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, SetWindowPos, GWL_EXSTYLE, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, WS_EX_TOPMOST,
};

use crate::win::monitor::Monitor;

const OBS_EXE_NAMES: [&str; 3] = ["obs64.exe", "obs32.exe", "obs.exe"];

thread_local! {
    /// Win32 の popup menu / dialog は独自のメッセージループを持つ。同じ UI
    /// スレッドでこれらが開いている間は overlay を Topmost 帯の先頭へ移さない。
    static FOREGROUND_UI_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// StreamPainter 自身のメニューやダイアログを overlay より前に保つためのガード。
///
/// UI は Win32 UI スレッドだけで扱うため thread-local とし、`Rc` marker で別スレッドへ
/// 移動できないようにする。ネストした MessageBox なども depth で扱う。
pub struct ForegroundUiGuard {
    _not_send: PhantomData<Rc<()>>,
}

impl ForegroundUiGuard {
    pub fn new() -> Self {
        FOREGROUND_UI_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
        Self {
            _not_send: PhantomData,
        }
    }
}

impl Drop for ForegroundUiGuard {
    fn drop(&mut self) {
        FOREGROUND_UI_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

pub fn foreground_ui_active() -> bool {
    FOREGROUND_UI_DEPTH.with(|depth| depth.get() != 0)
}

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
            "hwnd={:?} rect=({},{})-({},{}) covers={:?} obs={} topmost={} title={:?}",
            hwnd.0,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            covers,
            is_obs_process(hwnd),
            is_topmost(hwnd),
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

/// OBS プロジェクターとStreamPainterオーバーレイのTopmost順を管理する。
///
/// OBSを先に、オーバーレイを後にTopmost帯の先頭へ移すことで、
/// `overlay > projector > other windows` の順序を作る。フォーカスは変更しない。
/// StreamPainterがTopmostへ昇格させたプロジェクターだけ、検出終了時やDrop時に元へ戻す。
#[derive(Default)]
pub struct ZOrderGuard {
    projector: Option<HWND>,
    restore_not_topmost: bool,
}

impl ZOrderGuard {
    pub fn enforce(&mut self, projector: Option<HWND>, overlay: HWND) -> WindowsResult<()> {
        // TrackPopupMenu / MessageBox / 設定画面より後から overlay を Topmost 帯の
        // 先頭へ動かすと、StreamPainter 自身の UI を覆ってしまう。UI が閉じた後の
        // 次回 poll で最新状態へ再同期する。
        if foreground_ui_active() {
            return Ok(());
        }

        if self.projector != projector {
            self.restore();
            self.projector = projector;
            self.restore_not_topmost = projector.is_some_and(|hwnd| !is_topmost(hwnd));
        } else if let Some(hwnd) = projector {
            // 外部操作でTopmostが外された場合も、終了時に元へ戻す対象として記録する。
            self.restore_not_topmost |= !is_topmost(hwnd);
        }

        let Some(projector) = projector else {
            return Ok(());
        };
        set_topmost(projector, true)?;
        // 必ずOBSの後に移動し、Topmost帯でもオーバーレイを一段上に置く。
        set_topmost(overlay, true)
    }

    fn restore(&mut self) {
        if self.restore_not_topmost {
            if let Some(projector) = self.projector {
                let _ = set_topmost(projector, false);
            }
        }
        self.projector = None;
        self.restore_not_topmost = false;
    }
}

impl Drop for ZOrderGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

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

fn is_topmost(hwnd: HWND) -> bool {
    let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    ex_style & WS_EX_TOPMOST.0 as isize != 0
}

fn set_topmost(hwnd: HWND, topmost: bool) -> WindowsResult<()> {
    unsafe {
        SetWindowPos(
            hwnd,
            Some(if topmost {
                HWND_TOPMOST
            } else {
                HWND_NOTOPMOST
            }),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, GetTopWindow, GetWindow, GW_HWNDNEXT, WINDOW_EX_STYLE,
        WS_EX_TOOLWINDOW, WS_POPUP,
    };

    struct TestWindow(HWND);

    impl TestWindow {
        fn new(topmost: bool) -> Self {
            let ex_style = WS_EX_TOOLWINDOW
                | if topmost {
                    WS_EX_TOPMOST
                } else {
                    WINDOW_EX_STYLE(0)
                };
            let hwnd = unsafe {
                CreateWindowExW(
                    ex_style,
                    w!("STATIC"),
                    w!("StreamPainter Z-order test"),
                    WS_POPUP,
                    0,
                    0,
                    1,
                    1,
                    None,
                    None,
                    None,
                    None,
                )
            }
            .expect("test window");
            Self(hwnd)
        }
    }

    impl Drop for TestWindow {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }

    fn is_above(upper: HWND, lower: HWND) -> bool {
        let mut current = unsafe { GetTopWindow(None) }.ok();
        while let Some(hwnd) = current {
            if hwnd == upper {
                return true;
            }
            if hwnd == lower {
                return false;
            }
            current = unsafe { GetWindow(hwnd, GW_HWNDNEXT) }.ok();
        }
        false
    }

    #[test]
    fn guard_stacks_overlay_and_restores_only_promoted_projector() {
        let projector = TestWindow::new(false);
        let overlay = TestWindow::new(true);

        let mut guard = ZOrderGuard::default();
        guard
            .enforce(Some(projector.0), overlay.0)
            .expect("enforce Z-order");
        assert!(is_topmost(projector.0));
        assert!(is_topmost(overlay.0));
        assert!(is_above(overlay.0, projector.0));
        drop(guard);
        assert!(!is_topmost(projector.0));
        assert!(is_topmost(overlay.0));

        set_topmost(projector.0, true).expect("make projector topmost");
        let mut guard = ZOrderGuard::default();
        guard
            .enforce(Some(projector.0), overlay.0)
            .expect("enforce existing Topmost Z-order");
        drop(guard);
        assert!(is_topmost(projector.0));
    }

    #[test]
    fn foreground_ui_suspends_z_order_changes_until_it_closes() {
        let projector = TestWindow::new(false);
        let overlay = TestWindow::new(true);
        let mut z_order = ZOrderGuard::default();

        {
            let _foreground_ui = ForegroundUiGuard::new();
            z_order
                .enforce(Some(projector.0), overlay.0)
                .expect("suppressed enforcement succeeds");
            assert!(!is_topmost(projector.0));

            {
                let _nested_ui = ForegroundUiGuard::new();
                assert!(foreground_ui_active());
            }
            assert!(foreground_ui_active());
        }

        assert!(!foreground_ui_active());
        z_order
            .enforce(Some(projector.0), overlay.0)
            .expect("enforcement resumes");
        assert!(is_topmost(projector.0));
        drop(z_order);
        assert!(!is_topmost(projector.0));
    }
}
