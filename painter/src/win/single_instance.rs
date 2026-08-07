//! 通常常駐プロセスの多重起動を防ぐ名前付きMutex。

use std::time::{Duration, Instant};

use anyhow::Result;
use windows::core::HSTRING;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, SetLastError, ERROR_ALREADY_EXISTS, HANDLE, WIN32_ERROR,
};
use windows::Win32::System::Threading::CreateMutexW;

const MUTEX_NAME: &str = "Local\\StreamPainter.App.2D711CD6-61E0-4E20-AEA2-9A4906C85328";
/// アップデート適用後の再起動は、新プロセスの起動が旧プロセスのmutex解放
/// (グレースフルシャットダウン完了)より先に走り得るため、短時間だけ再試行する。
/// 通常の「二重起動」検知では、この上限まで待ってから案内を表示する。
const HANDOFF_RETRY_BUDGET: Duration = Duration::from_secs(2);
const HANDOFF_RETRY_INTERVAL: Duration = Duration::from_millis(50);

pub struct SingleInstanceGuard(HANDLE);

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn acquire_named(name: &str) -> Result<Option<SingleInstanceGuard>> {
    unsafe {
        // CreateMutexW成功時の既存判定だけにGetLastErrorを使えるよう、事前値を消す。
        SetLastError(WIN32_ERROR(0));
        let handle = CreateMutexW(None, false, &HSTRING::from(name))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            Ok(None)
        } else {
            Ok(Some(SingleInstanceGuard(handle)))
        }
    }
}

fn acquire_named_with_retry(
    name: &str,
    budget: Duration,
    interval: Duration,
) -> Result<Option<SingleInstanceGuard>> {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(guard) = acquire_named(name)? {
            return Ok(Some(guard));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(interval);
    }
}

pub fn acquire() -> Result<Option<SingleInstanceGuard>> {
    acquire_named_with_retry(MUTEX_NAME, HANDOFF_RETRY_BUDGET, HANDOFF_RETRY_INTERVAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_guard_for_the_same_name_is_rejected_until_drop() {
        let name = format!("Local\\StreamPainter.Test.{}", std::process::id());
        let first = acquire_named(&name).unwrap().expect("first guard");
        assert!(acquire_named(&name).unwrap().is_none());

        drop(first);
        assert!(acquire_named(&name).unwrap().is_some());
    }

    #[test]
    fn retry_succeeds_once_the_holder_releases_within_the_budget() {
        // SingleInstanceGuardは生HANDLEを持つためSendではない。holderは別スレッド自身が
        // acquire/drop する (guardをスレッド間で移動しない)。
        let name = format!("Local\\StreamPainter.Test.Retry.{}", std::process::id());
        let release_after = Duration::from_millis(120);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let held_name = name.clone();
        std::thread::spawn(move || {
            let guard = acquire_named(&held_name).unwrap().expect("holder guard");
            ready_tx.send(()).unwrap();
            std::thread::sleep(release_after);
            drop(guard);
        });
        ready_rx.recv().unwrap();

        let started = Instant::now();
        let retried =
            acquire_named_with_retry(&name, Duration::from_secs(2), Duration::from_millis(10))
                .unwrap();
        assert!(retried.is_some(), "retry must observe the release");
        assert!(started.elapsed() >= Duration::from_millis(50));
    }

    #[test]
    fn retry_gives_up_after_the_budget_while_the_holder_keeps_it() {
        let name = format!("Local\\StreamPainter.Test.Timeout.{}", std::process::id());
        let held = acquire_named(&name).unwrap().expect("first guard");

        let retried =
            acquire_named_with_retry(&name, Duration::from_millis(80), Duration::from_millis(10))
                .unwrap();
        assert!(retried.is_none(), "budget must expire while still held");
        drop(held);
    }
}
