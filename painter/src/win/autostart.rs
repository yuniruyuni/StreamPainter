//! 現在のWindowsユーザー向けログオン自動起動。
//!
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` の `StreamPainter`
//! REG_SZへ、引数を付けずに現在のexeだけを登録する。portable exeの場所が変わった
//! 場合は実レジストリ値と現在のexeを比較し、設定画面から修復または解除できる。

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    GetLastError, LocalFree, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS, HLOCAL,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    REG_VALUE_TYPE,
};
use windows::Win32::UI::Shell::CommandLineToArgvW;

const MAX_RUN_VALUE_BYTES: u32 = 64 * 1024;
/// Microsoft documents Run/RunOnce command lines as at most 260 characters.
/// Count UTF-16 code units because that is what Windows stores and consumes.
const MAX_RUN_COMMAND_UNITS: usize = 260;

#[derive(Clone, Debug, PartialEq, Eq)]
struct RegistryValue {
    kind: u32,
    data: Vec<u8>,
}

trait RunValueStore: Clone {
    fn read_value(&self) -> Result<Option<RegistryValue>>;
    fn write_value(&self, value: &RegistryValue) -> Result<()>;
    fn delete_value(&self) -> Result<()>;
}

fn restore_value<S: RunValueStore>(store: &S, value: &Option<RegistryValue>) -> Result<()> {
    match value {
        Some(value) => store.write_value(value),
        None => store.delete_value(),
    }
}

#[derive(Clone, Copy, Default)]
struct SystemRunValueStore;

struct OwnedRegistryKey(HKEY);

impl Drop for OwnedRegistryKey {
    fn drop(&mut self) {
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

fn registry_error(
    operation: &str,
    error: windows::Win32::Foundation::WIN32_ERROR,
) -> anyhow::Error {
    anyhow!("{operation}に失敗しました (Win32 error {})", error.0)
}

impl SystemRunValueStore {
    fn open(
        &self,
        access: windows::Win32::System::Registry::REG_SAM_FLAGS,
    ) -> Result<Option<OwnedRegistryKey>> {
        let mut key = HKEY::default();
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
                None,
                access,
                &mut key,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status != ERROR_SUCCESS {
            return Err(registry_error("Windows自動起動キーの読み取り", status));
        }
        Ok(Some(OwnedRegistryKey(key)))
    }

    fn open_or_create(&self) -> Result<OwnedRegistryKey> {
        let mut key = HKEY::default();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(registry_error("Windows自動起動キーの作成", status));
        }
        Ok(OwnedRegistryKey(key))
    }
}

impl RunValueStore for SystemRunValueStore {
    fn read_value(&self) -> Result<Option<RegistryValue>> {
        let Some(key) = self.open(KEY_QUERY_VALUE)? else {
            return Ok(None);
        };

        // 2段階queryの間に値が変わる可能性を考慮して、ERROR_MORE_DATAだけ再試行する。
        for _ in 0..3 {
            let mut kind = REG_VALUE_TYPE::default();
            let mut byte_len = 0_u32;
            let status = unsafe {
                RegQueryValueExW(
                    key.0,
                    w!("StreamPainter"),
                    None,
                    Some(&mut kind),
                    None,
                    Some(&mut byte_len),
                )
            };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            if status != ERROR_SUCCESS && status != ERROR_MORE_DATA {
                return Err(registry_error("Windows自動起動値のサイズ取得", status));
            }
            if byte_len > MAX_RUN_VALUE_BYTES {
                bail!("Windows自動起動値が大きすぎます ({byte_len} bytes)");
            }
            if byte_len == 0 {
                return Ok(Some(RegistryValue {
                    kind: kind.0,
                    data: Vec::new(),
                }));
            }

            let mut data = vec![0_u8; byte_len as usize];
            let status = unsafe {
                RegQueryValueExW(
                    key.0,
                    w!("StreamPainter"),
                    None,
                    Some(&mut kind),
                    Some(data.as_mut_ptr()),
                    Some(&mut byte_len),
                )
            };
            if status == ERROR_MORE_DATA {
                continue;
            }
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            if status != ERROR_SUCCESS {
                return Err(registry_error("Windows自動起動値の読み取り", status));
            }
            data.truncate(byte_len as usize);
            return Ok(Some(RegistryValue { kind: kind.0, data }));
        }
        bail!("Windows自動起動値が読み取り中に繰り返し変更されました")
    }

    fn write_value(&self, value: &RegistryValue) -> Result<()> {
        let key = self.open_or_create()?;
        let status = unsafe {
            RegSetValueExW(
                key.0,
                w!("StreamPainter"),
                None,
                REG_VALUE_TYPE(value.kind),
                Some(&value.data),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(registry_error("Windows自動起動値の保存", status));
        }
        Ok(())
    }

    fn delete_value(&self) -> Result<()> {
        let Some(key) = self.open(KEY_SET_VALUE)? else {
            return Ok(());
        };
        let status = unsafe { RegDeleteValueW(key.0, w!("StreamPainter")) };
        if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
            return Err(registry_error("Windows自動起動値の削除", status));
        }
        Ok(())
    }
}

/// exeパスをWindows command lineの第1引数としてquoteする。
/// Windowsファイル名で禁止されるquoteは受け付けず、両端のquoteだけを加える。
fn quote_windows_argument(argument: &OsStr) -> Result<OsString> {
    let units = argument.encode_wide().collect::<Vec<_>>();
    if units.is_empty() {
        bail!("自動起動するexeのパスが空です");
    }
    if units.contains(&0) {
        bail!("自動起動するexeのパスにNUL文字が含まれています");
    }
    if units.contains(&u16::from(b'"')) {
        bail!("自動起動するexeのパスに使用できないquoteが含まれています");
    }

    let mut quoted = Vec::with_capacity(units.len() + 2);
    quoted.push(u16::from(b'"'));
    quoted.extend(units);
    quoted.push(u16::from(b'"'));
    Ok(OsString::from_wide(&quoted))
}

fn registry_string(value: &OsStr) -> RegistryValue {
    let mut data = Vec::new();
    for unit in value.encode_wide().chain(std::iter::once(0)) {
        data.extend_from_slice(&unit.to_le_bytes());
    }
    RegistryValue {
        kind: REG_SZ.0,
        data,
    }
}

fn decode_registry_string(value: &RegistryValue) -> Result<OsString> {
    if value.kind != REG_SZ.0 {
        bail!("REG_SZではありません");
    }
    if !value.data.len().is_multiple_of(2) {
        bail!("UTF-16のbyte数が不正です");
    }
    let mut units = value
        .data
        .chunks_exact(2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    while units.last() == Some(&0) {
        units.pop();
    }
    if units.is_empty() || units.contains(&0) {
        bail!("REG_SZの内容が空または途中にNULを含んでいます");
    }
    Ok(OsString::from_wide(&units))
}

fn validate_run_command_length(command: &OsStr) -> Result<()> {
    let units = command.encode_wide().count();
    if units > MAX_RUN_COMMAND_UNITS {
        bail!(
            "Windows Runキーのコマンド上限を超えています ({units} / {MAX_RUN_COMMAND_UNITS}文字)"
        );
    }
    Ok(())
}

struct CommandLineAllocation(*mut PWSTR);

impl Drop for CommandLineAllocation {
    fn drop(&mut self) {
        let _ = unsafe { LocalFree(Some(HLOCAL(self.0.cast()))) };
    }
}

fn parse_windows_command_line(command: &OsStr) -> Result<Vec<OsString>> {
    let mut units = command
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut count = 0_i32;
    let arguments = unsafe { CommandLineToArgvW(PCWSTR(units.as_mut_ptr()), &mut count) };
    if arguments.is_null() || count <= 0 {
        let error = unsafe { GetLastError() };
        bail!(
            "Windows command lineを解析できません (Win32 error {})",
            error.0
        );
    }
    let _allocation = CommandLineAllocation(arguments);
    let arguments = unsafe { std::slice::from_raw_parts(arguments, count as usize) };
    Ok(arguments
        .iter()
        .map(|argument| OsString::from_wide(unsafe { argument.as_wide() }))
        .collect())
}

fn registration_value(executable: &Path) -> Result<RegistryValue> {
    if !executable.is_file() {
        bail!(
            "現在のStreamPainter実行ファイルが見つかりません: {}",
            executable.display()
        );
    }
    let command = quote_windows_argument(executable.as_os_str())?;
    validate_run_command_length(&command)?;
    Ok(registry_string(&command))
}

fn same_executable(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegistrationProblem {
    UnsupportedValueType,
    MalformedCommand,
    CommandTooLong,
    UnexpectedArguments,
    MissingExecutable,
    DifferentExecutable,
}

impl RegistrationProblem {
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::UnsupportedValueType => "登録値の形式が対応外です",
            Self::MalformedCommand => "登録コマンドを解析できません",
            Self::CommandTooLong => "登録コマンドがWindowsの260文字上限を超えています",
            Self::UnexpectedArguments => "通常起動以外の引数が含まれています",
            Self::MissingExecutable => "登録先のexeが見つかりません",
            Self::DifferentExecutable => "別の場所にあるexeが登録されています",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegistrationStatus {
    Disabled,
    Enabled,
    NeedsRepair(RegistrationProblem),
}

impl RegistrationStatus {
    /// checkboxはWindows上に値が存在するという実状態を表す。
    pub(crate) fn is_registered(self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

#[derive(Clone)]
struct Autostart<S> {
    store: S,
    executable: PathBuf,
}

impl<S: RunValueStore> Autostart<S> {
    fn new(store: S, executable: PathBuf) -> Self {
        Self { store, executable }
    }

    fn inspect_value(&self, value: Option<&RegistryValue>) -> RegistrationStatus {
        let Some(value) = value else {
            return RegistrationStatus::Disabled;
        };
        if value.kind != REG_SZ.0 {
            return RegistrationStatus::NeedsRepair(RegistrationProblem::UnsupportedValueType);
        }
        let Ok(command) = decode_registry_string(value) else {
            return RegistrationStatus::NeedsRepair(RegistrationProblem::MalformedCommand);
        };
        if validate_run_command_length(&command).is_err() {
            return RegistrationStatus::NeedsRepair(RegistrationProblem::CommandTooLong);
        }
        let Ok(arguments) = parse_windows_command_line(&command) else {
            return RegistrationStatus::NeedsRepair(RegistrationProblem::MalformedCommand);
        };
        if arguments.len() != 1 {
            return RegistrationStatus::NeedsRepair(RegistrationProblem::UnexpectedArguments);
        }
        let registered = PathBuf::from(&arguments[0]);
        if !registered.is_file() {
            return RegistrationStatus::NeedsRepair(RegistrationProblem::MissingExecutable);
        }
        if same_executable(&registered, &self.executable) {
            RegistrationStatus::Enabled
        } else {
            RegistrationStatus::NeedsRepair(RegistrationProblem::DifferentExecutable)
        }
    }

    fn inspect(&self) -> Result<RegistrationStatus> {
        let value = self.store.read_value()?;
        Ok(self.inspect_value(value.as_ref()))
    }

    fn prepare(&self, enabled: bool) -> Result<PreparedChange<S>> {
        let before = self.store.read_value()?;
        let current = self.inspect_value(before.as_ref());
        let changed =
            (enabled && current != RegistrationStatus::Enabled) || (!enabled && before.is_some());
        if changed {
            let desired = enabled
                .then(|| registration_value(&self.executable))
                .transpose()?;
            let apply = match desired {
                Some(value) => self.store.write_value(&value),
                None => self.store.delete_value(),
            };
            if let Err(apply_error) = apply {
                let rollback = restore_value(&self.store, &before).err();
                let mut detail = format!("Windows自動起動を変更できませんでした: {apply_error:#}");
                if let Some(error) = rollback {
                    detail.push_str(&format!(
                        "\n変更前の登録を復元することにも失敗しました: {error:#}"
                    ));
                }
                bail!(detail);
            }
        }
        Ok(PreparedChange {
            store: self.store.clone(),
            before,
            changed,
            finished: false,
        })
    }
}

#[derive(Clone)]
pub(crate) struct SystemAutostart(Autostart<SystemRunValueStore>);

impl SystemAutostart {
    pub(crate) fn current() -> Result<Self> {
        let executable =
            std::env::current_exe().context("現在のStreamPainter実行ファイルを取得できません")?;
        Ok(Self(Autostart::new(SystemRunValueStore, executable)))
    }

    pub(crate) fn inspect(&self) -> Result<RegistrationStatus> {
        self.0.inspect()
    }

    pub(crate) fn prepare(&self, enabled: bool) -> Result<PreparedAutostartChange> {
        self.0.prepare(enabled).map(PreparedAutostartChange)
    }
}

struct PreparedChange<S: RunValueStore> {
    store: S,
    before: Option<RegistryValue>,
    changed: bool,
    finished: bool,
}

impl<S: RunValueStore> PreparedChange<S> {
    fn restore(&self) -> Result<()> {
        if !self.changed {
            return Ok(());
        }
        restore_value(&self.store, &self.before)
    }

    pub fn commit(&mut self) {
        self.finished = true;
    }

    pub fn rollback(&mut self) -> Result<()> {
        self.restore()?;
        self.finished = true;
        Ok(())
    }
}

impl<S: RunValueStore> Drop for PreparedChange<S> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.restore();
        }
    }
}

pub(crate) struct PreparedAutostartChange(PreparedChange<SystemRunValueStore>);

impl PreparedAutostartChange {
    pub(crate) fn commit(&mut self) {
        self.0.commit();
    }

    pub(crate) fn rollback(&mut self) -> Result<()> {
        self.0.rollback()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Default)]
    struct MemoryState {
        value: Option<RegistryValue>,
        writes: usize,
        deletes: usize,
        fail_next_write_after_change: bool,
    }

    #[derive(Clone, Default)]
    struct MemoryStore(Rc<RefCell<MemoryState>>);

    impl RunValueStore for MemoryStore {
        fn read_value(&self) -> Result<Option<RegistryValue>> {
            Ok(self.0.borrow().value.clone())
        }

        fn write_value(&self, value: &RegistryValue) -> Result<()> {
            let mut state = self.0.borrow_mut();
            state.value = Some(value.clone());
            state.writes += 1;
            if std::mem::take(&mut state.fail_next_write_after_change) {
                bail!("injected write failure");
            }
            Ok(())
        }

        fn delete_value(&self) -> Result<()> {
            let mut state = self.0.borrow_mut();
            state.value = None;
            state.deletes += 1;
            Ok(())
        }
    }

    struct TempExecutables {
        directory: PathBuf,
    }

    impl TempExecutables {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let directory = std::env::temp_dir().join(format!(
                "stream-painter-autostart-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&directory).unwrap();
            Self { directory }
        }

        fn create(&self, name: &str) -> PathBuf {
            let path = self.directory.join(name);
            std::fs::write(&path, b"test executable").unwrap();
            path
        }
    }

    impl Drop for TempExecutables {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    fn value_for_command(command: &OsStr) -> RegistryValue {
        registry_string(command)
    }

    #[test]
    fn quoting_round_trips_as_exactly_one_windows_argument() {
        let paths = [
            OsString::from(r"C:\Program Files\StreamPainter\stream-painter.exe"),
            OsString::from(r"C:\配信 ツール\StreamPainter.exe"),
            OsString::from(r"C:\portable\ends-with-backslash\"),
        ];
        for path in paths {
            let quoted = quote_windows_argument(&path).unwrap();
            assert_eq!(parse_windows_command_line(&quoted).unwrap(), vec![path]);
        }
        assert!(quote_windows_argument(OsStr::new("C:\\portable\\quoted\"name.exe")).is_err());
    }

    #[test]
    fn absent_registry_value_is_default_off() {
        let files = TempExecutables::new();
        let executable = files.create("stream-painter.exe");
        let autostart = Autostart::new(MemoryStore::default(), executable);
        assert_eq!(autostart.inspect().unwrap(), RegistrationStatus::Disabled);
    }

    #[test]
    fn enable_and_disable_are_idempotent_and_never_add_arguments() {
        let files = TempExecutables::new();
        let executable = files.create("stream painter.exe");
        let store = MemoryStore::default();
        let autostart = Autostart::new(store.clone(), executable.clone());

        autostart.prepare(true).unwrap().commit();
        assert_eq!(autostart.inspect().unwrap(), RegistrationStatus::Enabled);
        assert_eq!(store.0.borrow().writes, 1);
        let registered = store.0.borrow().value.clone().unwrap();
        assert_eq!(registered.kind, REG_SZ.0);
        assert!(registered.data.ends_with(&[0, 0]));
        let command = decode_registry_string(&registered).unwrap();
        assert_eq!(
            parse_windows_command_line(&command).unwrap(),
            vec![executable.into_os_string()]
        );

        autostart.prepare(true).unwrap().commit();
        assert_eq!(store.0.borrow().writes, 1);
        autostart.prepare(false).unwrap().commit();
        assert_eq!(store.0.borrow().deletes, 1);
        autostart.prepare(false).unwrap().commit();
        assert_eq!(store.0.borrow().deletes, 1);
        assert_eq!(autostart.inspect().unwrap(), RegistrationStatus::Disabled);
    }

    #[test]
    fn moved_or_deleted_portable_executable_is_detected_and_repaired() {
        let files = TempExecutables::new();
        let current = files.create("new stream-painter.exe");
        let previous = files.create("old stream-painter.exe");
        let store = MemoryStore::default();
        store.0.borrow_mut().value =
            Some(registration_value(&previous).expect("old executable should be registerable"));
        let autostart = Autostart::new(store.clone(), current.clone());

        assert_eq!(
            autostart.inspect().unwrap(),
            RegistrationStatus::NeedsRepair(RegistrationProblem::DifferentExecutable)
        );
        std::fs::remove_file(&previous).unwrap();
        assert_eq!(
            autostart.inspect().unwrap(),
            RegistrationStatus::NeedsRepair(RegistrationProblem::MissingExecutable)
        );
        autostart.prepare(true).unwrap().commit();
        assert_eq!(autostart.inspect().unwrap(), RegistrationStatus::Enabled);
        let command = decode_registry_string(store.0.borrow().value.as_ref().unwrap()).unwrap();
        assert_eq!(
            parse_windows_command_line(&command).unwrap(),
            vec![current.into_os_string()]
        );
    }

    #[test]
    fn settings_or_detect_arguments_require_repair_and_are_removed() {
        let files = TempExecutables::new();
        let executable = files.create("stream-painter.exe");
        for argument in ["--settings", "--detect"] {
            let store = MemoryStore::default();
            let quoted = quote_windows_argument(executable.as_os_str()).unwrap();
            let mut command = quoted;
            command.push(format!(" {argument}"));
            store.0.borrow_mut().value = Some(value_for_command(&command));
            let autostart = Autostart::new(store.clone(), executable.clone());

            assert_eq!(
                autostart.inspect().unwrap(),
                RegistrationStatus::NeedsRepair(RegistrationProblem::UnexpectedArguments)
            );
            autostart.prepare(true).unwrap().commit();
            let command = decode_registry_string(store.0.borrow().value.as_ref().unwrap()).unwrap();
            assert_eq!(
                parse_windows_command_line(&command).unwrap(),
                vec![executable.clone().into_os_string()]
            );
        }
    }

    #[test]
    fn malformed_registration_stays_checked_until_repaired_or_disabled() {
        let files = TempExecutables::new();
        let executable = files.create("stream-painter.exe");
        let store = MemoryStore::default();
        store.0.borrow_mut().value = Some(RegistryValue {
            kind: 2,
            data: vec![1, 2, 3, 4],
        });
        let autostart = Autostart::new(store.clone(), executable);

        let status = autostart.inspect().unwrap();
        assert_eq!(
            status,
            RegistrationStatus::NeedsRepair(RegistrationProblem::UnsupportedValueType)
        );
        assert!(status.is_registered());
        autostart.prepare(false).unwrap().commit();
        assert_eq!(autostart.inspect().unwrap(), RegistrationStatus::Disabled);
    }

    #[test]
    fn run_command_over_260_utf16_units_is_rejected_or_marked_for_repair() {
        let files = TempExecutables::new();
        let executable = files.create("stream-painter.exe");
        let store = MemoryStore::default();
        let oversized = OsString::from("a".repeat(MAX_RUN_COMMAND_UNITS + 1));
        store.0.borrow_mut().value = Some(value_for_command(&oversized));
        let autostart = Autostart::new(store, executable);

        assert_eq!(
            autostart.inspect().unwrap(),
            RegistrationStatus::NeedsRepair(RegistrationProblem::CommandTooLong)
        );
        assert!(validate_run_command_length(&oversized).is_err());
        assert!(
            validate_run_command_length(OsStr::new(&"a".repeat(MAX_RUN_COMMAND_UNITS))).is_ok()
        );
    }

    #[test]
    fn rollback_restores_the_exact_previous_registry_value() {
        let files = TempExecutables::new();
        let executable = files.create("stream-painter.exe");
        let original = RegistryValue {
            kind: 2,
            data: vec![1, 2, 3, 4],
        };
        let store = MemoryStore::default();
        store.0.borrow_mut().value = Some(original.clone());
        let autostart = Autostart::new(store.clone(), executable);

        let mut prepared = autostart.prepare(true).unwrap();
        assert_ne!(store.0.borrow().value, Some(original.clone()));
        prepared.rollback().unwrap();
        assert_eq!(store.0.borrow().value, Some(original));
    }

    #[test]
    fn failed_registry_write_restores_the_exact_previous_value() {
        let files = TempExecutables::new();
        let executable = files.create("stream-painter.exe");
        let original = RegistryValue {
            kind: 2,
            data: vec![5, 6, 7, 8],
        };
        let store = MemoryStore::default();
        {
            let mut state = store.0.borrow_mut();
            state.value = Some(original.clone());
            state.fail_next_write_after_change = true;
        }
        let autostart = Autostart::new(store.clone(), executable);

        let error = autostart
            .prepare(true)
            .err()
            .expect("injected registry failure must be returned");
        assert!(error.to_string().contains("変更できませんでした"));
        assert_eq!(store.0.borrow().value, Some(original));
    }

    #[test]
    fn dropping_an_uncommitted_change_restores_the_previous_state() {
        let files = TempExecutables::new();
        let executable = files.create("stream-painter.exe");
        let store = MemoryStore::default();
        let autostart = Autostart::new(store.clone(), executable);
        {
            let _prepared = autostart.prepare(true).unwrap();
            assert!(store.0.borrow().value.is_some());
        }
        assert!(store.0.borrow().value.is_none());
    }

    #[test]
    fn a_deleted_current_executable_cannot_create_a_broken_registration() {
        let files = TempExecutables::new();
        let executable = files.directory.join("deleted.exe");
        let store = MemoryStore::default();
        let autostart = Autostart::new(store.clone(), executable);
        let error = autostart
            .prepare(true)
            .err()
            .expect("deleted executable must be rejected");
        assert!(error.to_string().contains("見つかりません"));
        assert!(store.0.borrow().value.is_none());

        store.0.borrow_mut().value = Some(value_for_command(OsStr::new(
            r#""C:\deleted\stream-painter.exe""#,
        )));
        autostart.prepare(false).unwrap().commit();
        assert!(store.0.borrow().value.is_none());
    }
}
