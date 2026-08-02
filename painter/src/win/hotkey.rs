//! 描画モード切替用グローバルホットキーの登録と、設定保存中のtransaction管理。
//!
//! 新しいキーを先に別IDで確保してから旧キーを外すことで、競合時にも旧キーを維持する。
//! 設定ファイルの保存が失敗した場合は `rollback` で旧登録を復元する。

use std::cell::RefCell;

use anyhow::{anyhow, Context, Result};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
};
use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_APP};

use crate::config::{HotkeyChord, HotkeyConfig};

const PRIMARY_ID: i32 = 1;
const SECONDARY_ID: i32 = 2;
pub const SETTINGS_PROBE_ID: i32 = 3;

/// 設定画面からoverlayへ同期送信する。request本体はUI thread-localへ置き、外部processが
/// 任意pointerを送ってAppにdereferenceさせる経路を作らない。
pub const WM_HOTKEY_CHANGE: u32 = WM_APP + 3;

#[derive(Debug, Clone)]
pub enum ChangeCommand {
    Prepare(HotkeyConfig),
    Commit,
    Rollback,
}

pub(crate) struct ChangeRequest {
    pub(crate) command: ChangeCommand,
    pub(crate) handled: bool,
    pub(crate) error: Option<String>,
}

impl ChangeRequest {
    fn new(command: ChangeCommand) -> Self {
        Self {
            command,
            handled: false,
            error: None,
        }
    }
}

thread_local! {
    static CHANGE_REQUEST: RefCell<Option<ChangeRequest>> = const { RefCell::new(None) };
}

/// settingsとoverlayは同じWin32 UI thread上にあるため、SendMessageWの同期dispatch中だけ
/// thread-local requestを公開する。外部から同じmessageを送られてもslotが空なら無視する。
pub fn request_change(owner: HWND, command: ChangeCommand) -> Result<()> {
    CHANGE_REQUEST.with(|slot| {
        let mut slot = slot
            .try_borrow_mut()
            .map_err(|_| anyhow!("別のホットキー通知が処理中です"))?;
        if slot.is_some() {
            anyhow::bail!("別のホットキー通知が処理中です");
        }
        *slot = Some(ChangeRequest::new(command));
        Ok(())
    })?;
    unsafe {
        SendMessageW(owner, WM_HOTKEY_CHANGE, None, None);
    }
    CHANGE_REQUEST.with(|slot| {
        let request = slot
            .try_borrow_mut()
            .map_err(|_| anyhow!("ホットキー変更の応答が別の通知で使用中です"))?
            .take()
            .ok_or_else(|| anyhow!("ホットキー変更の応答を取得できませんでした"))?;
        if !request.handled {
            anyhow::bail!("実行中のStreamPainterへホットキー変更を通知できませんでした");
        }
        if let Some(error) = request.error {
            anyhow::bail!("{error}");
        }
        Ok(())
    })
}

pub fn with_change_request(handler: impl FnOnce(&mut ChangeRequest)) {
    CHANGE_REQUEST.with(|slot| {
        // RegisterHotKey/Shell通知中に同じcustom messageがreentrantに届いても、
        // RefCell panicや同一transactionの二重処理を起こさず無視する。
        let Ok(mut slot) = slot.try_borrow_mut() else {
            return;
        };
        if let Some(request) = slot.as_mut() {
            handler(request);
        }
    });
}

pub(crate) trait RegistrationApi {
    fn register(&mut self, hwnd: HWND, id: i32, chord: HotkeyChord) -> Result<()>;
    fn unregister(&mut self, hwnd: HWND, id: i32) -> Result<()>;
}

pub struct SystemRegistrationApi;

impl RegistrationApi for SystemRegistrationApi {
    fn register(&mut self, hwnd: HWND, id: i32, chord: HotkeyChord) -> Result<()> {
        unsafe {
            RegisterHotKey(
                Some(hwnd),
                id,
                HOT_KEY_MODIFIERS(chord.modifiers | MOD_NOREPEAT.0),
                chord.virtual_key,
            )
        }
        .with_context(|| "RegisterHotKey failed")
    }

    fn unregister(&mut self, hwnd: HWND, id: i32) -> Result<()> {
        unsafe { UnregisterHotKey(Some(hwnd), id) }.with_context(|| "UnregisterHotKey failed")
    }
}

#[derive(Clone)]
struct ActiveHotkey {
    id: i32,
    chord: HotkeyChord,
    display_name: String,
}

enum PendingChange {
    Unchanged,
    Changed { previous: Option<ActiveHotkey> },
}

pub struct HotkeyManager<A: RegistrationApi = SystemRegistrationApi> {
    hwnd: HWND,
    api: A,
    active: Option<ActiveHotkey>,
    /// 解除失敗でOSに残った追加登録。ID再利用や二重発火を避けるため、存在中は
    /// 再設定を拒否し、process終了時にもう一度解除を試す。
    stray: Option<ActiveHotkey>,
    pending: Option<PendingChange>,
}

impl HotkeyManager<SystemRegistrationApi> {
    pub fn new(hwnd: HWND) -> Self {
        Self::with_api(hwnd, SystemRegistrationApi)
    }
}

impl<A: RegistrationApi> HotkeyManager<A> {
    fn with_api(hwnd: HWND, api: A) -> Self {
        Self {
            hwnd,
            api,
            active: None,
            stray: None,
            pending: None,
        }
    }

    pub fn register_initial(&mut self, config: &HotkeyConfig) -> Result<()> {
        self.prepare(config)?;
        self.commit()
    }

    /// candidate登録失敗時は旧登録へ一切触れない。
    pub fn prepare(&mut self, config: &HotkeyConfig) -> Result<()> {
        if let Some(stray) = &self.stray {
            anyhow::bail!(
                "解除できなかったホットキー {} が残っているため、安全に再設定できません。StreamPainterを再起動してください",
                stray.display_name
            );
        }
        if self.pending.is_some() {
            anyhow::bail!("別のホットキー設定変更が処理中です");
        }
        let candidate = config.chord()?;
        if self.active.as_ref().map(|active| active.chord) == candidate {
            self.pending = Some(PendingChange::Unchanged);
            return Ok(());
        }

        let previous = self.active.clone();
        let next = if let Some(chord) = candidate {
            let id = match previous.as_ref().map(|active| active.id) {
                Some(PRIMARY_ID) => SECONDARY_ID,
                _ => PRIMARY_ID,
            };
            if let Err(error) = self.api.register(self.hwnd, id, chord) {
                return Err(anyhow!(
                    "ホットキー {} を登録できません。他のアプリやWindowsで使用されていないか確認してください: {error:#}",
                    config.display_name()
                ));
            }
            Some(ActiveHotkey {
                id,
                chord,
                display_name: config.display_name(),
            })
        } else {
            None
        };

        if let Some(old) = &previous {
            if let Err(error) = self.api.unregister(self.hwnd, old.id) {
                if let Some(candidate) = &next {
                    if let Err(cleanup_error) = self.api.unregister(self.hwnd, candidate.id) {
                        // candidateは実際にはOSへ登録済みなので、IDを再利用せずDropで
                        // 再解除できるよう追跡する。activeは従来どおりoldを維持する。
                        self.stray = Some(candidate.clone());
                        return Err(anyhow!(
                            "旧ホットキー {} を解除できず、変更後のホットキー {} の後始末にも失敗しました。以前のキーは維持しますが、StreamPainterを再起動してください: {error:#}; cleanup: {cleanup_error:#}",
                            old.display_name,
                            candidate.display_name
                        ));
                    }
                }
                return Err(anyhow!(
                    "旧ホットキー {} を安全に解除できなかったため、変更を中止しました: {error:#}",
                    old.display_name
                ));
            }
        }

        self.active = next;
        self.pending = Some(PendingChange::Changed { previous });
        Ok(())
    }

    pub fn commit(&mut self) -> Result<()> {
        if self.pending.take().is_none() {
            anyhow::bail!("確定するホットキー設定変更がありません");
        }
        Ok(())
    }

    /// 旧キーの再登録に失敗した場合はcandidateを残す。両方を失うより、設定画面に
    /// 表示済みのcandidateまたはトレイfallbackを維持する方を優先する。
    pub fn rollback(&mut self) -> Result<()> {
        let Some(pending) = self.pending.take() else {
            anyhow::bail!("取り消すホットキー設定変更がありません");
        };
        let PendingChange::Changed { previous } = pending else {
            return Ok(());
        };
        let candidate = self.active.clone();

        match (previous, candidate) {
            (Some(previous), Some(candidate)) => {
                if let Err(error) = self.api.register(self.hwnd, previous.id, previous.chord) {
                    // candidateはまだ登録済みなので、安全な操作経路は残る。
                    return Err(anyhow!(
                        "保存前のホットキー {} を復元できませんでした。現在は {} が有効です: {error:#}",
                        previous.display_name,
                        candidate.display_name
                    ));
                }
                if let Err(error) = self.api.unregister(self.hwnd, candidate.id) {
                    // 二重発火を避けるため、復元した旧キーを再び外しcandidateを正とする。
                    let cleanup = self.api.unregister(self.hwnd, previous.id);
                    return Err(match cleanup {
                        Ok(()) => anyhow!(
                            "変更後のホットキー {} を解除できず、現在もこのキーが有効です: {error:#}",
                            candidate.display_name
                        ),
                        Err(cleanup_error) => {
                            // 旧・新の両登録がOSに残った。candidateだけをactiveとして扱い、
                            // oldのIDを再利用しないよう追跡して以後の変更を止める。
                            self.stray = Some(previous.clone());
                            anyhow!(
                                "ホットキーのrollback中に {} と {} の解除が失敗しました。StreamPainterを再起動してください: {error:#}; cleanup: {cleanup_error:#}",
                                candidate.display_name,
                                previous.display_name
                            )
                        }
                    });
                }
                self.active = Some(previous);
            }
            (Some(previous), None) => {
                self.api
                    .register(self.hwnd, previous.id, previous.chord)
                    .with_context(|| {
                        format!(
                            "保存前のホットキー {} を復元できませんでした。トレイから切り替えてください",
                            previous.display_name
                        )
                    })?;
                self.active = Some(previous);
            }
            (None, Some(candidate)) => {
                self.api
                    .unregister(self.hwnd, candidate.id)
                    .with_context(|| {
                        format!(
                            "変更後のホットキー {} を解除できませんでした。現在もこのキーが有効です",
                            candidate.display_name
                        )
                    })?;
                self.active = None;
            }
            (None, None) => {}
        }
        Ok(())
    }

    pub fn handles_message(&self, id: i32) -> bool {
        self.active.as_ref().is_some_and(|active| active.id == id)
    }

    pub fn active_display_name(&self) -> Option<&str> {
        self.active
            .as_ref()
            .map(|active| active.display_name.as_str())
    }
}

impl<A: RegistrationApi> Drop for HotkeyManager<A> {
    fn drop(&mut self) {
        if let Some(stray) = self.stray.take() {
            let _ = self.api.unregister(self.hwnd, stray.id);
        }
        if let Some(active) = self.active.take() {
            let _ = self.api.unregister(self.hwnd, active.id);
        }
    }
}

/// standalone設定画面で、保存中だけcandidateを予約し競合を検出する。
pub struct ProbeRegistration {
    hwnd: HWND,
    registered: bool,
}

impl ProbeRegistration {
    pub fn acquire(hwnd: HWND, config: &HotkeyConfig) -> Result<Self> {
        let Some(chord) = config.chord()? else {
            return Ok(Self {
                hwnd,
                registered: false,
            });
        };
        SystemRegistrationApi
            .register(hwnd, SETTINGS_PROBE_ID, chord)
            .map_err(|error| {
                anyhow!(
                    "ホットキー {} を登録できません。他のアプリやWindowsで使用されていないか確認してください: {error:#}",
                    config.display_name()
                )
            })?;
        Ok(Self {
            hwnd,
            registered: true,
        })
    }
}

impl Drop for ProbeRegistration {
    fn drop(&mut self) {
        if self.registered {
            let _ = SystemRegistrationApi.unregister(self.hwnd, SETTINGS_PROBE_ID);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;
    use crate::config::{HotkeyModifier, HOTKEY_MOD_CTRL};

    #[derive(Default)]
    struct FakeApi {
        registered: HashMap<i32, HotkeyChord>,
        rejected: HashSet<HotkeyChord>,
        fail_unregister: HashSet<i32>,
    }

    impl RegistrationApi for FakeApi {
        fn register(&mut self, _hwnd: HWND, id: i32, chord: HotkeyChord) -> Result<()> {
            if self.rejected.contains(&chord) {
                anyhow::bail!("injected registration conflict");
            }
            if self.registered.contains_key(&id) {
                anyhow::bail!("id already registered");
            }
            self.registered.insert(id, chord);
            Ok(())
        }

        fn unregister(&mut self, _hwnd: HWND, id: i32) -> Result<()> {
            if self.fail_unregister.contains(&id) {
                anyhow::bail!("injected unregister failure");
            }
            self.registered
                .remove(&id)
                .map(|_| ())
                .ok_or_else(|| anyhow!("id is not registered"))
        }
    }

    fn ctrl_a() -> HotkeyConfig {
        HotkeyConfig {
            enabled: true,
            modifiers: vec![HotkeyModifier::Ctrl],
            key: "A".to_owned(),
        }
    }

    fn ctrl_b() -> HotkeyConfig {
        HotkeyConfig {
            enabled: true,
            modifiers: vec![HotkeyModifier::Ctrl],
            key: "B".to_owned(),
        }
    }

    #[test]
    fn initial_registration_failure_keeps_tray_fallback() {
        let f9 = HotkeyConfig::default();
        let chord = f9.chord().unwrap().unwrap();
        let mut api = FakeApi::default();
        api.rejected.insert(chord);
        let mut manager = HotkeyManager::with_api(HWND::default(), api);

        assert!(manager.register_initial(&f9).is_err());
        assert_eq!(manager.active_display_name(), None);
        assert!(manager.api.registered.is_empty());
    }

    #[test]
    fn conflicting_reregistration_preserves_old_hotkey() {
        let mut manager = HotkeyManager::with_api(HWND::default(), FakeApi::default());
        manager.register_initial(&HotkeyConfig::default()).unwrap();
        let rejected = ctrl_a().chord().unwrap().unwrap();
        manager.api.rejected.insert(rejected);

        assert!(manager.prepare(&ctrl_a()).is_err());
        assert_eq!(manager.active_display_name(), Some("F9"));
        assert!(manager.handles_message(PRIMARY_ID));
        assert_eq!(manager.api.registered.len(), 1);
    }

    #[test]
    fn dual_unregister_failure_tracks_the_candidate_and_blocks_id_reuse() {
        let mut manager = HotkeyManager::with_api(HWND::default(), FakeApi::default());
        manager.register_initial(&HotkeyConfig::default()).unwrap();
        manager
            .api
            .fail_unregister
            .extend([PRIMARY_ID, SECONDARY_ID]);

        let error = manager.prepare(&ctrl_a()).unwrap_err().to_string();
        assert!(error.contains("後始末にも失敗"));
        assert!(error.contains("再起動"));
        assert_eq!(manager.active_display_name(), Some("F9"));
        assert!(manager.handles_message(PRIMARY_ID));
        assert!(!manager.handles_message(SECONDARY_ID));
        assert_eq!(
            manager
                .stray
                .as_ref()
                .map(|stray| stray.display_name.as_str()),
            Some("Ctrl+A")
        );
        assert_eq!(manager.api.registered.len(), 2);

        manager.api.fail_unregister.clear();
        let retry = manager.prepare(&ctrl_b()).unwrap_err().to_string();
        assert!(retry.contains("安全に再設定できません"));
        assert_eq!(manager.api.registered.len(), 2);
    }

    #[test]
    fn save_failure_rolls_registration_back_to_old_hotkey() {
        let mut manager = HotkeyManager::with_api(HWND::default(), FakeApi::default());
        manager.register_initial(&HotkeyConfig::default()).unwrap();
        manager.prepare(&ctrl_a()).unwrap();
        assert_eq!(manager.active_display_name(), Some("Ctrl+A"));

        // config::save が失敗した場合に設定画面が呼ぶ経路。
        manager.rollback().unwrap();
        assert_eq!(manager.active_display_name(), Some("F9"));
        assert!(manager.handles_message(PRIMARY_ID));
        assert_eq!(manager.api.registered.len(), 1);
    }

    #[test]
    fn rollback_restore_failure_keeps_the_new_hotkey_active() {
        let old = HotkeyConfig::default();
        let mut manager = HotkeyManager::with_api(HWND::default(), FakeApi::default());
        manager.register_initial(&old).unwrap();
        manager.prepare(&ctrl_a()).unwrap();
        manager.api.rejected.insert(old.chord().unwrap().unwrap());

        assert!(manager.rollback().is_err());
        assert_eq!(manager.active_display_name(), Some("Ctrl+A"));
        assert!(manager.handles_message(SECONDARY_ID));
        assert_eq!(manager.api.registered.len(), 1);
    }

    #[test]
    fn rollback_dual_unregister_failure_tracks_the_old_registration() {
        let mut manager = HotkeyManager::with_api(HWND::default(), FakeApi::default());
        manager.register_initial(&HotkeyConfig::default()).unwrap();
        manager.prepare(&ctrl_a()).unwrap();
        manager
            .api
            .fail_unregister
            .extend([PRIMARY_ID, SECONDARY_ID]);

        let error = manager.rollback().unwrap_err().to_string();
        assert!(error.contains("rollback中"));
        assert!(error.contains("再起動"));
        assert_eq!(manager.active_display_name(), Some("Ctrl+A"));
        assert!(!manager.handles_message(PRIMARY_ID));
        assert!(manager.handles_message(SECONDARY_ID));
        assert_eq!(
            manager
                .stray
                .as_ref()
                .map(|stray| stray.display_name.as_str()),
            Some("F9")
        );
        assert_eq!(manager.api.registered.len(), 2);

        manager.api.fail_unregister.clear();
        assert!(manager.prepare(&ctrl_b()).is_err());
        assert_eq!(manager.api.registered.len(), 2);
    }

    #[test]
    fn disabling_can_be_rolled_back_or_committed() {
        let mut manager = HotkeyManager::with_api(HWND::default(), FakeApi::default());
        manager.register_initial(&HotkeyConfig::default()).unwrap();
        manager.prepare(&HotkeyConfig::disabled()).unwrap();
        assert_eq!(manager.active_display_name(), None);
        manager.rollback().unwrap();
        assert_eq!(manager.active_display_name(), Some("F9"));

        manager.prepare(&HotkeyConfig::disabled()).unwrap();
        manager.commit().unwrap();
        assert_eq!(manager.active_display_name(), None);
        assert!(manager.api.registered.is_empty());
    }

    #[test]
    fn committed_change_uses_alternate_message_id() {
        let mut manager = HotkeyManager::with_api(HWND::default(), FakeApi::default());
        manager.register_initial(&HotkeyConfig::default()).unwrap();
        manager.prepare(&ctrl_a()).unwrap();
        manager.commit().unwrap();

        assert!(!manager.handles_message(PRIMARY_ID));
        assert!(manager.handles_message(SECONDARY_ID));
        assert_eq!(
            manager.api.registered.get(&SECONDARY_ID),
            Some(&HotkeyChord {
                modifiers: HOTKEY_MOD_CTRL,
                virtual_key: u32::from(b'A'),
            })
        );
    }
}
