//! 通常常駐プロセスの多重起動を防ぐ名前付きMutex。

use anyhow::Result;
use windows::core::HSTRING;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, SetLastError, ERROR_ALREADY_EXISTS, HANDLE, WIN32_ERROR,
};
use windows::Win32::System::Threading::CreateMutexW;

const MUTEX_NAME: &str = "Local\\StreamPainter.App.2D711CD6-61E0-4E20-AEA2-9A4906C85328";

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

pub fn acquire() -> Result<Option<SingleInstanceGuard>> {
    acquire_named(MUTEX_NAME)
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
}
