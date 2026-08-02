//! OBS WebSocket の秘密を OS のユーザー別保護ストレージへ分離する。

use anyhow::Result;

/// Generic credential の CredentialBlob 上限。
pub(crate) const MAX_OBS_PASSWORD_BYTES: usize = 2_560;

/// 資格情報の実装境界。
///
/// `write` と `delete` は失敗時に既存値を変更しないことを契約とする。設定ファイルとの
/// 順序制御は `config` 側で行い、Linux の単体テストではこの境界へ障害を注入する。
pub(crate) trait CredentialStore {
    fn read_obs_password(&self) -> Result<Option<String>>;
    fn write_obs_password(&self, password: &str) -> Result<()>;
    fn delete_obs_password(&self) -> Result<()>;
}

pub(crate) struct SystemCredentialStore;

#[cfg(windows)]
mod windows_store {
    use std::{ffi::c_void, ptr::null_mut, slice};

    use anyhow::{anyhow, Context, Result};
    use windows::{
        core::{w, HRESULT, PWSTR},
        Win32::{
            Foundation::ERROR_NOT_FOUND,
            Security::Credentials::{
                CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW,
                CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
            },
        },
    };

    use super::{CredentialStore, SystemCredentialStore, MAX_OBS_PASSWORD_BYTES};

    const TARGET_NAME: &str = "StreamPainter/OBS WebSocket";

    struct CredentialGuard(*mut CREDENTIALW);

    impl Drop for CredentialGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CredFree(self.0.cast::<c_void>()) };
            }
        }
    }

    impl CredentialStore for SystemCredentialStore {
        fn read_obs_password(&self) -> Result<Option<String>> {
            let mut raw = null_mut();
            if let Err(error) = unsafe {
                CredReadW(
                    w!("StreamPainter/OBS WebSocket"),
                    CRED_TYPE_GENERIC,
                    None,
                    &mut raw,
                )
            } {
                if error.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0) {
                    return Ok(None);
                }
                return Err(error).context("Windows 資格情報を読み込めません");
            }
            let guard = CredentialGuard(raw);
            let credential = unsafe {
                guard
                    .0
                    .as_ref()
                    .ok_or_else(|| anyhow!("Windows 資格情報の応答が不正です"))?
            };
            let bytes = if credential.CredentialBlobSize == 0 {
                &[][..]
            } else {
                if credential.CredentialBlob.is_null() {
                    anyhow::bail!("Windows 資格情報の応答が不正です");
                }
                unsafe {
                    slice::from_raw_parts(
                        credential.CredentialBlob,
                        credential.CredentialBlobSize as usize,
                    )
                }
            };
            let password = String::from_utf8(bytes.to_vec())
                .map_err(|_| anyhow!("Windows 資格情報の形式が不正です"))?;
            Ok(Some(password))
        }

        fn write_obs_password(&self, password: &str) -> Result<()> {
            let password = password.as_bytes();
            if password.len() > MAX_OBS_PASSWORD_BYTES {
                anyhow::bail!("OBS WebSocket パスワードが長すぎます");
            }

            let mut target: Vec<u16> = TARGET_NAME.encode_utf16().chain(Some(0)).collect();
            let mut user_name: Vec<u16> = "StreamPainter".encode_utf16().chain(Some(0)).collect();
            let credential = CREDENTIALW {
                Type: CRED_TYPE_GENERIC,
                TargetName: PWSTR(target.as_mut_ptr()),
                CredentialBlobSize: password.len() as u32,
                CredentialBlob: password.as_ptr().cast_mut(),
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                UserName: PWSTR(user_name.as_mut_ptr()),
                ..Default::default()
            };
            unsafe { CredWriteW(&credential, 0) }.context("Windows 資格情報を更新できません")
        }

        fn delete_obs_password(&self) -> Result<()> {
            match unsafe { CredDeleteW(w!("StreamPainter/OBS WebSocket"), CRED_TYPE_GENERIC, None) }
            {
                Ok(()) => Ok(()),
                Err(error) if error.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0) => Ok(()),
                Err(error) => Err(error).context("Windows 資格情報を削除できません"),
            }
        }
    }
}

#[cfg(not(windows))]
impl CredentialStore for SystemCredentialStore {
    fn read_obs_password(&self) -> Result<Option<String>> {
        anyhow::bail!("Windows の保護資格情報ストレージはこの環境では利用できません")
    }

    fn write_obs_password(&self, _password: &str) -> Result<()> {
        anyhow::bail!("Windows の保護資格情報ストレージはこの環境では利用できません")
    }

    fn delete_obs_password(&self) -> Result<()> {
        anyhow::bail!("Windows の保護資格情報ストレージはこの環境では利用できません")
    }
}
