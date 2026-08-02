//! 設定ファイル。%APPDATA%/StreamPainter/config/config.toml

use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use tracing::info;
use tracing::warn;

pub const MAX_STAMPS: usize = 32;
pub const MAX_STAMP_FILE_BYTES: u64 = 5 * 1024 * 1024;
pub const MAX_STAMP_DIMENSION: u32 = 2048;
pub const MAX_STAMP_PIXELS: u64 = 4_194_304;
/// 全スタンプをRGBAへ展開した場合に約64 MiBとなる上限。
pub const MAX_TOTAL_STAMP_PIXELS: u64 = 16_777_216;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// OBS Browser Source を配信する loopback HTTP サーバーのポート
    #[serde(default = "default_local_server_port")]
    pub local_server_port: u16,
    /// 対象モニタ index (EnumDisplayMonitors の列挙順)
    #[serde(default)]
    pub screen: usize,
    #[serde(default = "default_aspect")]
    pub canvas_aspect: String,
    #[serde(default = "default_true")]
    pub local_echo: bool,
    /// OBS 全画面プロジェクターの表示に追従してオーバーレイを自動で有効/無効化する
    #[serde(default = "default_true")]
    pub follow_projector: bool,
    /// obs-websocket 経由で F9 時にプロジェクターを自動で開く
    #[serde(default = "default_true")]
    pub obs_control: bool,
    #[serde(default = "default_obs_url")]
    pub obs_websocket_url: String,
    #[serde(default)]
    pub obs_websocket_password: String,
    /// "program" (視聴者に見えている映像) | "preview" (スタジオモードの編集側)
    #[serde(default = "default_projector_view")]
    pub projector_view: String,
    /// 描画モード終了時に、自動で開いたプロジェクターを閉じる (WM_CLOSE)
    #[serde(default = "default_true")]
    pub close_projector: bool,
    #[serde(default)]
    pub brush: BrushConfig,
    /// 管理ディレクトリ内の PNG スタンプ。外部パスや URL は保持しない。
    #[serde(default)]
    pub stamps: Vec<StampConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrushConfig {
    #[serde(default = "default_color")]
    pub color: String,
    #[serde(default = "default_width")]
    pub width_n: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StampConfig {
    /// アプリが生成する不透明な ID。ファイル名は `<id>.png`。
    pub id: String,
    pub name: String,
    pub width_px: u32,
    pub height_px: u32,
    /// キャンバス高に対する既定の表示高。
    #[serde(default = "default_stamp_height")]
    pub default_height_n: f64,
    #[serde(default = "default_stamp_opacity")]
    pub opacity: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            local_server_port: default_local_server_port(),
            screen: 0,
            canvas_aspect: default_aspect(),
            local_echo: true,
            follow_projector: true,
            obs_control: true,
            obs_websocket_url: default_obs_url(),
            obs_websocket_password: String::new(),
            projector_view: default_projector_view(),
            close_projector: true,
            brush: BrushConfig::default(),
            stamps: Vec::new(),
        }
    }
}

impl Default for BrushConfig {
    fn default() -> Self {
        Self {
            color: default_color(),
            width_n: default_width(),
        }
    }
}

fn default_local_server_port() -> u16 {
    16_873
}
fn default_aspect() -> String {
    "16:9".to_string()
}
fn default_color() -> String {
    "#ff4d6d".to_string()
}
fn default_width() -> f64 {
    0.005
}
fn default_stamp_height() -> f64 {
    0.15
}
fn default_stamp_opacity() -> f64 {
    1.0
}
fn default_true() -> bool {
    true
}
fn default_obs_url() -> String {
    "ws://localhost:4455".to_string()
}
fn default_projector_view() -> String {
    "program".to_string()
}

const TEMPLATE: &str = r##"# StreamPainter 設定 (通常はタスクトレイの「設定...」から編集します)

# OBS Browser Source URL: http://127.0.0.1:16873/overlay
# セキュリティ上、Webサーバーは常に 127.0.0.1 のみにバインドします。
local_server_port = 16873

# 対象モニタ (0 = プライマリから列挙順)
screen = 0

# OBS キャンバスのアスペクト比 (プロジェクター表示の黒帯計算に使用)
canvas_aspect = "16:9"

# ローカルエコー (自分の画面に即時描画するか)
local_echo = true

# OBS 全画面プロジェクターが対象モニタに表示されている間だけ有効化する
follow_projector = true

# obs-websocket 連携 (OBS 28+: ツール → WebSocket サーバー設定 で有効化)。
# F9 でプロジェクターが未表示なら自動で開く
obs_control = true
obs_websocket_url = "ws://localhost:4455"
obs_websocket_password = ""
# "program" = 視聴者に見えている映像 / "preview" = スタジオモードの編集側
projector_view = "program"
# 描画モード終了時に、F9 で自動で開いたプロジェクターを閉じる
# (手動で開いたプロジェクターは閉じない)
close_projector = true

[brush]
color = "#ff4d6d"
width_n = 0.005
"##;

fn config_path_from_roaming_app_data(roaming_app_data: &Path) -> PathBuf {
    roaming_app_data
        .join("StreamPainter")
        .join("config")
        .join("config.toml")
}

fn legacy_config_path_from_roaming_app_data(roaming_app_data: &Path) -> PathBuf {
    roaming_app_data
        .join("obs-painter")
        .join("config")
        .join("config.toml")
}

fn stamps_dir_from_config_path(config_path: &Path) -> Result<PathBuf> {
    let config_dir = config_path
        .parent()
        .ok_or_else(|| anyhow!("failed to resolve StreamPainter data directory"))?;
    let app_dir = config_dir
        .parent()
        .ok_or_else(|| anyhow!("failed to resolve StreamPainter data directory"))?;
    Ok(app_dir.join("stamps"))
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("failed to resolve configuration file name"))?;
    let mut suffixed = file_name.to_os_string();
    suffixed.push(suffix);
    Ok(path.with_file_name(suffixed))
}

fn backup_path(path: &Path) -> Result<PathBuf> {
    sibling_with_suffix(path, ".bak")
}

fn read_validated_config(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let config: Config =
        toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
    config
        .validate()
        .with_context(|| format!("invalid configuration in {}", path.display()))?;
    Ok(config)
}

fn load_config_file(path: &Path) -> Result<Config> {
    match read_validated_config(path) {
        Ok(config) => Ok(config),
        Err(primary_error) => {
            let backup = backup_path(path)?;
            if !backup.exists() {
                return Err(primary_error);
            }
            warn!(
                "failed to load {}; trying backup {}: {primary_error:#}",
                path.display(),
                backup.display()
            );
            read_validated_config(&backup).with_context(|| {
                format!(
                    "failed to load both {} and its backup; primary error: {primary_error:#}",
                    path.display()
                )
            })
        }
    }
}

fn write_synced(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    let result = (|| {
        file.write_all(contents)
            .with_context(|| format!("failed to write {}", path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to flush {}", path.display()))
    })();
    drop(file);
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}

fn copy_synced(source: &Path, destination: &Path) -> Result<()> {
    let mut source_file =
        File::open(source).with_context(|| format!("failed to open {}", source.display()))?;
    let mut destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    let result = (|| {
        std::io::copy(&mut source_file, &mut destination_file).with_context(|| {
            format!(
                "failed to copy {} to {}",
                source.display(),
                destination.display()
            )
        })?;
        destination_file
            .sync_all()
            .with_context(|| format!("failed to flush {}", destination.display()))
    })();
    drop(destination_file);
    if result.is_err() {
        let _ = std::fs::remove_file(destination);
    }
    result
}

#[cfg(windows)]
fn wide_path(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
fn replace_file(replacement: &Path, destination: &Path) -> Result<()> {
    use windows::{
        core::PCWSTR,
        Win32::Storage::FileSystem::{
            MoveFileExW, ReplaceFileW, MOVEFILE_WRITE_THROUGH, REPLACEFILE_WRITE_THROUGH,
        },
    };

    let replacement_wide = wide_path(replacement);
    let destination_wide = wide_path(destination);
    if !destination.exists() {
        return unsafe {
            MoveFileExW(
                PCWSTR(replacement_wide.as_ptr()),
                PCWSTR(destination_wide.as_ptr()),
                MOVEFILE_WRITE_THROUGH,
            )
        }
        .with_context(|| format!("failed to atomically create {}", destination.display()));
    }

    unsafe {
        ReplaceFileW(
            PCWSTR(destination_wide.as_ptr()),
            PCWSTR(replacement_wide.as_ptr()),
            PCWSTR::null(),
            REPLACEFILE_WRITE_THROUGH,
            None,
            None,
        )
    }
    .with_context(|| format!("failed to atomically replace {}", destination.display()))
}

#[cfg(not(windows))]
fn replace_file(replacement: &Path, destination: &Path) -> Result<()> {
    std::fs::rename(replacement, destination)
        .with_context(|| format!("failed to atomically replace {}", destination.display()))
}

#[cfg(windows)]
fn sync_parent_directory(_path: &Path) -> Result<()> {
    // MoveFileExW / ReplaceFileW are both called with their write-through flags.
    Ok(())
}

#[cfg(not(windows))]
fn sync_parent_directory(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("failed to resolve parent directory"))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("failed to flush {}", parent.display()))
}

trait ConfigFileOps {
    fn write_synced(&self, path: &Path, contents: &[u8]) -> Result<()>;
    fn copy_synced(&self, source: &Path, destination: &Path) -> Result<()>;
    fn replace_file(&self, replacement: &Path, destination: &Path) -> Result<()>;
    fn remove_file(&self, path: &Path) -> Result<()>;
    fn sync_parent(&self, path: &Path) -> Result<()>;
}

struct SystemConfigFileOps;

impl ConfigFileOps for SystemConfigFileOps {
    fn write_synced(&self, path: &Path, contents: &[u8]) -> Result<()> {
        write_synced(path, contents)
    }

    fn copy_synced(&self, source: &Path, destination: &Path) -> Result<()> {
        copy_synced(source, destination)
    }

    fn replace_file(&self, replacement: &Path, destination: &Path) -> Result<()> {
        replace_file(replacement, destination)
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("failed to remove {}", path.display()))
            }
        }
    }

    fn sync_parent(&self, path: &Path) -> Result<()> {
        sync_parent_directory(path)
    }
}

struct TemporaryFileGuard {
    path: PathBuf,
    cleanup: bool,
}

impl TemporaryFileGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            cleanup: true,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn preserve(&mut self) {
        self.cleanup = false;
    }
}

impl Drop for TemporaryFileGuard {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn temporary_path(path: &Path, purpose: &str) -> Result<PathBuf> {
    sibling_with_suffix(path, &format!(".{purpose}.{}.tmp", uuid::Uuid::now_v7()))
}

fn files_have_same_contents(left: &Path, right: &Path) -> bool {
    std::fs::read(left)
        .and_then(|left| std::fs::read(right).map(|right| left == right))
        .unwrap_or(false)
}

fn rollback_save<O: ConfigFileOps>(
    ops: &O,
    path: &Path,
    backup: &Path,
    previous_primary: &mut TemporaryFileGuard,
    previous_backup: Option<&mut TemporaryFileGuard>,
    restore_backup: bool,
) -> Result<()> {
    let mut failures = Vec::new();

    if restore_backup {
        match previous_backup {
            Some(previous_backup) => {
                if let Err(error) = ops.replace_file(previous_backup.path(), backup) {
                    previous_backup.preserve();
                    failures.push(format!(
                        "failed to restore backup from {}: {error:#}",
                        previous_backup.path().display()
                    ));
                }
            }
            None => {
                if let Err(error) = ops.remove_file(backup) {
                    failures.push(format!(
                        "failed to remove newly-created backup {}: {error:#}",
                        backup.display()
                    ));
                }
            }
        }
    }

    if let Err(error) = ops.replace_file(previous_primary.path(), path) {
        previous_primary.preserve();
        failures.push(format!(
            "failed to restore primary from {}: {error:#}",
            previous_primary.path().display()
        ));
    }

    if let Err(error) = ops.sync_parent(path) {
        failures.push(format!(
            "failed to flush restored configuration directory: {error:#}"
        ));
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(failures.join("; ")))
    }
}

fn rollback_after<O: ConfigFileOps>(
    error: anyhow::Error,
    ops: &O,
    path: &Path,
    backup: &Path,
    previous_primary: &mut TemporaryFileGuard,
    previous_backup: Option<&mut TemporaryFileGuard>,
    restore_backup: bool,
) -> anyhow::Error {
    match rollback_save(
        ops,
        path,
        backup,
        previous_primary,
        previous_backup,
        restore_backup,
    ) {
        Ok(()) => error,
        Err(rollback_error) => error.context(format!("rollback also failed: {rollback_error:#}")),
    }
}

fn write_atomically_with_ops<O: ConfigFileOps>(
    path: &Path,
    contents: &[u8],
    ops: &O,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("failed to resolve configuration directory"))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;

    let temporary = temporary_path(path, "new")?;
    let temporary = TemporaryFileGuard::new(temporary);
    ops.write_synced(temporary.path(), contents)?;
    let backup = backup_path(path)?;

    if !path.exists() {
        if let Err(error) = ops
            .replace_file(temporary.path(), path)
            .and_then(|()| ops.sync_parent(path))
        {
            let rollback = ops.remove_file(path).and_then(|()| ops.sync_parent(path));
            return match rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => {
                    Err(error.context(format!("rollback also failed: {rollback_error:#}")))
                }
            };
        }
        return Ok(());
    }

    // 同期済みの復旧コピーを揃えるまで primary / backup には触れない。
    // backup 更新候補を別に持つことで、昇格後にも復旧コピーを残す。
    let previous_primary = temporary_path(path, "previous-primary")?;
    let mut previous_primary = TemporaryFileGuard::new(previous_primary);
    ops.copy_synced(path, previous_primary.path())?;

    let mut previous_backup = if backup.exists() {
        let previous_backup = temporary_path(&backup, "previous-backup")?;
        let previous_backup = TemporaryFileGuard::new(previous_backup);
        ops.copy_synced(&backup, previous_backup.path())?;
        Some(previous_backup)
    } else {
        None
    };

    let backup_candidate = temporary_path(&backup, "candidate")?;
    let backup_candidate = TemporaryFileGuard::new(backup_candidate);
    ops.copy_synced(previous_primary.path(), backup_candidate.path())?;

    let commit = ops
        .replace_file(temporary.path(), path)
        .map_err(|error| (error, false))
        .and_then(|()| {
            ops.replace_file(backup_candidate.path(), &backup)
                .map_err(|error| (error, true))
        })
        .and_then(|()| ops.sync_parent(path).map_err(|error| (error, true)));
    if let Err((error, restore_backup)) = commit {
        if !restore_backup && files_have_same_contents(path, previous_primary.path()) {
            return Err(error);
        }
        return Err(rollback_after(
            error,
            ops,
            path,
            &backup,
            &mut previous_primary,
            previous_backup.as_mut(),
            restore_backup,
        ));
    }

    Ok(())
}

fn write_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    write_atomically_with_ops(path, contents, &SystemConfigFileOps)
}

#[cfg(windows)]
pub fn config_path() -> Result<PathBuf> {
    use known_folders::{get_known_folder_path, KnownFolder};

    let roaming_app_data = get_known_folder_path(KnownFolder::RoamingAppData)
        .ok_or_else(|| anyhow!("failed to resolve config directory"))?;
    Ok(config_path_from_roaming_app_data(&roaming_app_data))
}

pub fn stamps_dir() -> Result<PathBuf> {
    stamps_dir_from_config_path(&config_path()?)
}

pub fn stamp_path(stamp_id: &str) -> Result<PathBuf> {
    validate_stamp_id(stamp_id)?;
    Ok(stamps_dir()?.join(format!("{stamp_id}.png")))
}

/// PNG の形式・ファイルサイズ・デコード時リソース上限を検証する。
pub fn decode_stamp_png(source: &Path) -> Result<image::DynamicImage> {
    let metadata = std::fs::metadata(source)
        .with_context(|| format!("スタンプ画像を読み込めません: {}", source.display()))?;
    if !metadata.is_file() {
        anyhow::bail!("スタンプには PNG ファイルを指定してください");
    }
    if metadata.len() > MAX_STAMP_FILE_BYTES {
        anyhow::bail!("スタンプ画像は 5 MiB 以下にしてください");
    }
    let encoded = std::fs::read(source)
        .with_context(|| format!("スタンプ画像を読み込めません: {}", source.display()))?;
    reject_animated_png(&encoded)?;

    let mut reader = image::ImageReader::open(source)
        .with_context(|| format!("スタンプ画像を開けません: {}", source.display()))?
        .with_guessed_format()
        .context("スタンプ画像の形式を判定できません")?;
    if reader.format() != Some(image::ImageFormat::Png) {
        anyhow::bail!("スタンプに登録できる画像は PNG だけです");
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_STAMP_DIMENSION);
    limits.max_image_height = Some(MAX_STAMP_DIMENSION);
    limits.max_alloc = Some(MAX_STAMP_PIXELS * 16);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .context("PNG スタンプをデコードできません")?;
    let (width_px, height_px) = (decoded.width(), decoded.height());
    validate_stamp_dimensions(width_px, height_px)?;
    Ok(decoded)
}

fn reject_animated_png(encoded: &[u8]) -> Result<()> {
    const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if !encoded.starts_with(SIGNATURE) {
        anyhow::bail!("スタンプに登録できる画像は PNG だけです");
    }
    let mut offset = SIGNATURE.len();
    while offset.saturating_add(12) <= encoded.len() {
        let length = u32::from_be_bytes(
            encoded[offset..offset + 4]
                .try_into()
                .expect("four-byte PNG chunk length"),
        ) as usize;
        let Some(chunk_end) = offset
            .checked_add(12)
            .and_then(|base| base.checked_add(length))
        else {
            break;
        };
        if chunk_end > encoded.len() {
            break;
        }
        let chunk_type = &encoded[offset + 4..offset + 8];
        if chunk_type == b"acTL" {
            anyhow::bail!("アニメーションPNGは未対応です。静止PNGを指定してください");
        }
        offset = chunk_end;
        if chunk_type == b"IEND" {
            break;
        }
    }
    Ok(())
}

#[cfg(windows)]
/// 検証済みPNGを管理ディレクトリへコピーし、設定エントリを返す。
/// 設定画面のキャンセル時に呼び出し側が返却パスを削除できるよう、コピー先も返す。
pub fn import_stamp(source: &Path) -> Result<(StampConfig, PathBuf)> {
    let decoded = decode_stamp_png(source)?;
    let (width_px, height_px) = (decoded.width(), decoded.height());
    let id = uuid::Uuid::now_v7().to_string();
    let destination = stamp_path(&id)?;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("スタンプ保存先を作成できません: {}", parent.display()))?;
    }
    std::fs::copy(source, &destination)
        .with_context(|| format!("スタンプ画像を保存できません: {}", destination.display()))?;

    let fallback_name = "スタンプ";
    let raw_name = source
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(fallback_name);
    let name: String = raw_name
        .chars()
        .filter(|character| !character.is_control())
        .take(64)
        .collect();
    let name = if name.trim().is_empty() {
        fallback_name.to_owned()
    } else {
        name
    };

    Ok((
        StampConfig {
            id,
            name,
            width_px,
            height_px,
            default_height_n: default_stamp_height(),
            opacity: default_stamp_opacity(),
        },
        destination,
    ))
}

#[cfg(windows)]
fn legacy_config_path() -> Result<PathBuf> {
    use known_folders::{get_known_folder_path, KnownFolder};

    let roaming_app_data = get_known_folder_path(KnownFolder::RoamingAppData)
        .ok_or_else(|| anyhow!("failed to resolve legacy config directory"))?;
    Ok(legacy_config_path_from_roaming_app_data(&roaming_app_data))
}

#[cfg(not(windows))]
pub fn config_path() -> Result<PathBuf> {
    anyhow::bail!("config directory is only available on Windows")
}

/// 設定を読む。無ければデフォルト設定を書き出し、そのまま起動する。
pub fn load() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        #[cfg(windows)]
        {
            let legacy_path = legacy_config_path()?;
            if legacy_path.exists() {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let legacy = std::fs::read(&legacy_path).with_context(|| {
                    format!(
                        "failed to read legacy configuration {}",
                        legacy_path.display()
                    )
                })?;
                write_atomically(&path, &legacy)?;
                info!(
                    "migrated legacy config from {} to {}",
                    legacy_path.display(),
                    path.display()
                );
            }
        }
    }
    if !path.exists() {
        write_atomically(&path, TEMPLATE.as_bytes())?;
        return Ok(Config::default());
    }
    load_config_file(&path)
}

/// 検証済みの設定を保存する。設定画面からの書き込み経路はここに集約する。
pub fn save(config: &Config) -> Result<()> {
    config.validate()?;
    let path = config_path()?;
    let body = toml::to_string_pretty(config).context("failed to serialize configuration")?;
    let text =
        format!("# StreamPainter 設定 — 通常はタスクトレイの「設定...」から編集します。\n{body}");
    write_atomically(&path, text.as_bytes())?;
    Ok(())
}

impl Config {
    pub fn overlay_url(&self) -> String {
        format!("http://127.0.0.1:{}/overlay", self.local_server_port)
    }

    pub fn validate(&self) -> Result<()> {
        if self.local_server_port == 0 {
            anyhow::bail!("ローカルサーバーのポートには 1〜65535 を指定してください");
        }

        let Some((width, height)) = self.canvas_aspect.split_once(':') else {
            anyhow::bail!("キャンバスのアスペクト比は 16:9 の形式で指定してください");
        };
        let width: f64 = width
            .trim()
            .parse()
            .map_err(|_| anyhow!("キャンバスのアスペクト比は 16:9 の形式で指定してください"))?;
        let height: f64 = height
            .trim()
            .parse()
            .map_err(|_| anyhow!("キャンバスのアスペクト比は 16:9 の形式で指定してください"))?;
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            anyhow::bail!("キャンバスのアスペクト比には正の有限値を指定してください");
        }

        if self.obs_control
            && !self.obs_websocket_url.starts_with("ws://")
            && !self.obs_websocket_url.starts_with("wss://")
        {
            anyhow::bail!("OBS WebSocket URL は ws:// または wss:// で始めてください");
        }
        if !matches!(self.projector_view.as_str(), "program" | "preview") {
            anyhow::bail!("プロジェクター表示は program または preview を指定してください");
        }

        let color = self.brush.color.as_bytes();
        if color.len() != 7 || color[0] != b'#' || !color[1..].iter().all(u8::is_ascii_hexdigit) {
            anyhow::bail!("ブラシ色は #RRGGBB の形式で指定してください");
        }
        if !self.brush.width_n.is_finite() || self.brush.width_n <= 0.0 || self.brush.width_n > 1.0
        {
            anyhow::bail!("ブラシ幅には 0 より大きく 1 以下の値を指定してください");
        }

        validate_stamp_catalog(&self.stamps)?;
        Ok(())
    }
}

pub fn validate_stamp_catalog(stamps: &[StampConfig]) -> Result<()> {
    if stamps.len() > MAX_STAMPS {
        anyhow::bail!("登録できるスタンプは最大 {MAX_STAMPS} 個です");
    }
    let mut ids = std::collections::HashSet::new();
    let mut total_pixels = 0_u64;
    for stamp in stamps {
        validate_stamp_id(&stamp.id)?;
        if !ids.insert(&stamp.id) {
            anyhow::bail!("スタンプ ID が重複しています: {}", stamp.id);
        }
        let name = stamp.name.trim();
        if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
            anyhow::bail!("スタンプ名は 1〜64 文字で指定してください");
        }
        validate_stamp_dimensions(stamp.width_px, stamp.height_px)?;
        total_pixels = total_pixels
            .checked_add(u64::from(stamp.width_px) * u64::from(stamp.height_px))
            .ok_or_else(|| anyhow!("スタンプ画像の合計ピクセル数が大きすぎます"))?;
        if !stamp.default_height_n.is_finite() || !(0.01..=1.0).contains(&stamp.default_height_n) {
            anyhow::bail!("スタンプの表示サイズは 1〜100% で指定してください");
        }
        if !stamp.opacity.is_finite() || !(0.0..=1.0).contains(&stamp.opacity) {
            anyhow::bail!("スタンプの不透明度は 0〜100% で指定してください");
        }
    }
    if total_pixels > MAX_TOTAL_STAMP_PIXELS {
        anyhow::bail!(
            "登録するスタンプ画像は合計 {MAX_TOTAL_STAMP_PIXELS} ピクセル以下にしてください"
        );
    }
    Ok(())
}

fn validate_stamp_id(stamp_id: &str) -> Result<()> {
    if stamp_id.is_empty()
        || stamp_id.len() > 64
        || !stamp_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        anyhow::bail!("スタンプ ID が不正です");
    }
    Ok(())
}

fn validate_stamp_dimensions(width: u32, height: u32) -> Result<()> {
    if width == 0
        || height == 0
        || width > MAX_STAMP_DIMENSION
        || height > MAX_STAMP_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_STAMP_PIXELS
    {
        anyhow::bail!(
            "スタンプ画像は最大 {MAX_STAMP_DIMENSION}×{MAX_STAMP_DIMENSION} px（合計 {MAX_STAMP_PIXELS} px）です"
        );
    }
    Ok(())
}

/// アプリ起動時に、現在の設定から参照されない管理対象PNGを除去する。
/// 設定保存直後には実行しない。再起動前のサーバーが旧スタンプを使える状態を保つため。
pub fn cleanup_unregistered_stamps(config: &Config) -> Result<()> {
    let directory = stamps_dir()?;
    if !directory.exists() {
        return Ok(());
    }
    let registered: std::collections::HashSet<&str> = config
        .stamps
        .iter()
        .map(|stamp| stamp.id.as_str())
        .collect();
    for entry in std::fs::read_dir(&directory)
        .with_context(|| format!("failed to inspect {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file()
            || path.extension().and_then(|extension| extension.to_str()) != Some("png")
        {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if !registered.contains(stem) {
            std::fs::remove_file(&path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum SaveFailure {
        WriteTemporary,
        CopyPreviousPrimary,
        CopyPreviousBackup,
        CopyBackupCandidate,
        ReplacePrimaryBefore,
        ReplacePrimaryAfter,
        PromoteBackupBefore,
        PromoteBackupAfter,
        SyncCommittedFiles,
    }

    struct InjectedConfigFileOps {
        failure: SaveFailure,
        fired: Cell<bool>,
    }

    impl InjectedConfigFileOps {
        fn new(failure: SaveFailure) -> Self {
            Self {
                failure,
                fired: Cell::new(false),
            }
        }

        fn inject(&self, point: SaveFailure) -> bool {
            self.failure == point && !self.fired.replace(true)
        }

        fn injected_error(point: SaveFailure) -> anyhow::Error {
            anyhow!("injected save failure at {point:?}")
        }
    }

    fn temporary_purpose(path: &Path) -> Option<SaveFailure> {
        let name = path.file_name()?.to_string_lossy();
        if name.contains(".previous-primary.") {
            Some(SaveFailure::CopyPreviousPrimary)
        } else if name.contains(".previous-backup.") {
            Some(SaveFailure::CopyPreviousBackup)
        } else if name.contains(".candidate.") {
            Some(SaveFailure::CopyBackupCandidate)
        } else {
            None
        }
    }

    impl ConfigFileOps for InjectedConfigFileOps {
        fn write_synced(&self, path: &Path, contents: &[u8]) -> Result<()> {
            if self.inject(SaveFailure::WriteTemporary) {
                std::fs::write(path, b"partial")?;
                return Err(Self::injected_error(SaveFailure::WriteTemporary));
            }
            SystemConfigFileOps.write_synced(path, contents)
        }

        fn copy_synced(&self, source: &Path, destination: &Path) -> Result<()> {
            if let Some(point) = temporary_purpose(destination) {
                if self.inject(point) {
                    std::fs::write(destination, b"partial")?;
                    return Err(Self::injected_error(point));
                }
            }
            SystemConfigFileOps.copy_synced(source, destination)
        }

        fn replace_file(&self, replacement: &Path, destination: &Path) -> Result<()> {
            let is_backup = destination
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".bak"));
            let (before, after) = if is_backup {
                (
                    SaveFailure::PromoteBackupBefore,
                    SaveFailure::PromoteBackupAfter,
                )
            } else {
                (
                    SaveFailure::ReplacePrimaryBefore,
                    SaveFailure::ReplacePrimaryAfter,
                )
            };
            if before == SaveFailure::ReplacePrimaryBefore && self.failure == before {
                self.fired.set(true);
                return Err(Self::injected_error(before));
            }
            if self.inject(before) {
                return Err(Self::injected_error(before));
            }
            if self.failure == after && !self.fired.get() {
                SystemConfigFileOps.replace_file(replacement, destination)?;
                self.fired.set(true);
                return Err(Self::injected_error(after));
            }
            SystemConfigFileOps.replace_file(replacement, destination)
        }

        fn remove_file(&self, path: &Path) -> Result<()> {
            SystemConfigFileOps.remove_file(path)
        }

        fn sync_parent(&self, path: &Path) -> Result<()> {
            if self.inject(SaveFailure::SyncCommittedFiles) {
                return Err(Self::injected_error(SaveFailure::SyncCommittedFiles));
            }
            SystemConfigFileOps.sync_parent(path)
        }
    }

    fn assert_no_temporary_files(directory: &Path) {
        assert!(std::fs::read_dir(directory).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
    }

    #[test]
    fn config_paths_use_new_name_and_retain_legacy_lookup() {
        let roaming_app_data = Path::new("RoamingAppData");
        assert_eq!(
            config_path_from_roaming_app_data(roaming_app_data),
            roaming_app_data
                .join("StreamPainter")
                .join("config")
                .join("config.toml")
        );
        assert_eq!(
            legacy_config_path_from_roaming_app_data(roaming_app_data),
            roaming_app_data
                .join("obs-painter")
                .join("config")
                .join("config.toml")
        );
    }

    #[test]
    fn overlay_url_is_loopback() {
        let config = Config::default();
        assert_eq!(config.overlay_url(), "http://127.0.0.1:16873/overlay");
    }

    #[test]
    fn defaults_applied() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.local_server_port, 16_873);
        assert_eq!(config.canvas_aspect, "16:9");
        assert_eq!(config.screen, 0);
        assert!(config.local_echo);
        assert_eq!(config.brush.width_n, 0.005);
        assert!(config.stamps.is_empty());
    }

    #[test]
    fn stamp_directory_is_a_sibling_of_config_directory() {
        let path = Path::new("RoamingAppData")
            .join("StreamPainter")
            .join("config")
            .join("config.toml");
        assert_eq!(
            stamps_dir_from_config_path(&path).unwrap(),
            Path::new("RoamingAppData")
                .join("StreamPainter")
                .join("stamps")
        );
    }

    #[test]
    fn stamp_validation_rejects_unsafe_ids_and_dimensions() {
        let mut config = Config::default();
        config.stamps.push(StampConfig {
            id: "../outside".into(),
            name: "bad".into(),
            width_px: 64,
            height_px: 64,
            default_height_n: 0.15,
            opacity: 1.0,
        });
        assert!(config.validate().is_err());

        config.stamps[0].id = "safe-id".into();
        config.stamps[0].width_px = MAX_STAMP_DIMENSION + 1;
        assert!(config.validate().is_err());
    }

    #[test]
    fn stamp_validation_bounds_total_decoded_memory() {
        let mut config = Config::default();
        for index in 0..5 {
            config.stamps.push(StampConfig {
                id: format!("stamp-{index}"),
                name: format!("stamp {index}"),
                width_px: 2048,
                height_px: 2048,
                default_height_n: 0.15,
                opacity: 1.0,
            });
        }
        assert!(config.validate().is_err());

        config.stamps.pop();
        config.validate().unwrap();
    }

    #[test]
    fn stamp_png_decoder_accepts_a_small_valid_png() {
        let path =
            std::env::temp_dir().join(format!("stream-painter-{}.png", uuid::Uuid::now_v7()));
        let image = image::RgbaImage::from_pixel(2, 3, image::Rgba([255, 0, 0, 128]));
        image
            .save_with_format(&path, image::ImageFormat::Png)
            .unwrap();
        let decoded = decode_stamp_png(&path).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (2, 3));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn animated_png_control_chunk_is_rejected() {
        let mut encoded = b"\x89PNG\r\n\x1a\n".to_vec();
        encoded.extend_from_slice(&8_u32.to_be_bytes());
        encoded.extend_from_slice(b"acTL");
        encoded.extend_from_slice(&[0; 8]);
        encoded.extend_from_slice(&[0; 4]);
        assert!(reject_animated_png(&encoded).is_err());
    }

    #[test]
    fn old_remote_fields_are_ignored_for_migration() {
        let config: Config = toml::from_str(
            r#"
server_url = "https://painter.example.com"
painter_token = "pt_old"
local_server_port = 18080
"#,
        )
        .unwrap();
        assert_eq!(config.local_server_port, 18_080);
    }

    #[test]
    fn config_round_trips_through_toml() {
        let mut config = Config::default();
        config.stamps.push(StampConfig {
            id: "stamp-1".into(),
            name: "テスト".into(),
            width_px: 320,
            height_px: 180,
            default_height_n: 0.2,
            opacity: 0.75,
        });
        let text = toml::to_string_pretty(&config).unwrap();
        let decoded: Config = toml::from_str(&text).unwrap();
        assert_eq!(decoded, config);
        decoded.validate().unwrap();
    }

    #[test]
    fn validation_rejects_values_that_cannot_be_used() {
        let config = Config {
            local_server_port: 0,
            ..Config::default()
        };
        assert!(config.validate().is_err());

        let config = Config {
            canvas_aspect: "NaN:9".into(),
            ..Config::default()
        };
        assert!(config.validate().is_err());

        let config = Config {
            obs_websocket_url: "http://localhost:4455".into(),
            ..Config::default()
        };
        assert!(config.validate().is_err());

        let config = Config {
            brush: BrushConfig {
                color: "red".into(),
                ..BrushConfig::default()
            },
            ..Config::default()
        };
        assert!(config.validate().is_err());

        let config = Config {
            brush: BrushConfig {
                width_n: f64::INFINITY,
                ..BrushConfig::default()
            },
            ..Config::default()
        };
        assert!(config.validate().is_err());

        let config = Config {
            brush: BrushConfig {
                width_n: 1.01,
                ..BrushConfig::default()
            },
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn atomic_write_keeps_the_previous_file_as_backup() {
        let directory =
            std::env::temp_dir().join(format!("stream-painter-config-{}", uuid::Uuid::now_v7()));
        let path = directory.join("config.toml");
        let backup = backup_path(&path).unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&path, b"previous").unwrap();
        std::fs::write(&backup, b"older").unwrap();

        write_atomically(&path, b"current").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"current");
        assert_eq!(std::fs::read(backup).unwrap(), b"previous");
        assert_no_temporary_files(&directory);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn atomic_write_failure_preserves_primary_and_existing_backup() {
        let failure_points = [
            SaveFailure::WriteTemporary,
            SaveFailure::CopyPreviousPrimary,
            SaveFailure::CopyPreviousBackup,
            SaveFailure::CopyBackupCandidate,
            SaveFailure::ReplacePrimaryBefore,
            SaveFailure::ReplacePrimaryAfter,
            SaveFailure::PromoteBackupBefore,
            SaveFailure::PromoteBackupAfter,
            SaveFailure::SyncCommittedFiles,
        ];

        for failure in failure_points {
            let directory = std::env::temp_dir().join(format!(
                "stream-painter-config-failure-{}",
                uuid::Uuid::now_v7()
            ));
            let path = directory.join("config.toml");
            let backup = backup_path(&path).unwrap();
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(&path, b"previous").unwrap();
            std::fs::write(&backup, b"older").unwrap();
            let ops = InjectedConfigFileOps::new(failure);

            let error = write_atomically_with_ops(&path, b"current", &ops).unwrap_err();

            assert!(
                ops.fired.get(),
                "failure was not injected for {failure:?}: {error:#}"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                b"previous",
                "primary changed after {failure:?}: {error:#}"
            );
            assert_eq!(
                std::fs::read(&backup).unwrap(),
                b"older",
                "backup changed after {failure:?}: {error:#}"
            );
            assert_no_temporary_files(&directory);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn atomic_write_failure_restores_an_absent_backup() {
        let directory = std::env::temp_dir().join(format!(
            "stream-painter-config-no-backup-{}",
            uuid::Uuid::now_v7()
        ));
        let path = directory.join("config.toml");
        let backup = backup_path(&path).unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&path, b"previous").unwrap();
        let ops = InjectedConfigFileOps::new(SaveFailure::PromoteBackupAfter);

        write_atomically_with_ops(&path, b"current", &ops).unwrap_err();

        assert_eq!(std::fs::read(&path).unwrap(), b"previous");
        assert!(!backup.exists());
        assert_no_temporary_files(&directory);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn invalid_primary_configuration_falls_back_to_backup() {
        let directory =
            std::env::temp_dir().join(format!("stream-painter-config-{}", uuid::Uuid::now_v7()));
        let path = directory.join("config.toml");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&path, "this is not toml").unwrap();
        let expected = Config {
            local_server_port: 18_080,
            ..Config::default()
        };
        std::fs::write(
            backup_path(&path).unwrap(),
            toml::to_string_pretty(&expected).unwrap(),
        )
        .unwrap();

        assert_eq!(load_config_file(&path).unwrap(), expected);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
