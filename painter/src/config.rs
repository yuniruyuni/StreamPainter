//! 設定ファイル。%APPDATA%/StreamPainter/config/config.toml

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use tracing::info;

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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrushConfig {
    #[serde(default = "default_color")]
    pub color: String,
    #[serde(default = "default_width")]
    pub width_n: f64,
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

#[cfg(windows)]
pub fn config_path() -> Result<PathBuf> {
    use known_folders::{get_known_folder_path, KnownFolder};

    let roaming_app_data = get_known_folder_path(KnownFolder::RoamingAppData)
        .ok_or_else(|| anyhow!("failed to resolve config directory"))?;
    Ok(config_path_from_roaming_app_data(&roaming_app_data))
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
                std::fs::copy(&legacy_path, &path).with_context(|| {
                    format!(
                        "failed to migrate {} to {}",
                        legacy_path.display(),
                        path.display()
                    )
                })?;
                info!(
                    "migrated legacy config from {} to {}",
                    legacy_path.display(),
                    path.display()
                );
            }
        }
    }
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, TEMPLATE)?;
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let config: Config =
        toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
    config
        .validate()
        .with_context(|| format!("invalid configuration in {}", path.display()))?;
    Ok(config)
}

/// 検証済みの設定を保存する。設定画面からの書き込み経路はここに集約する。
pub fn save(config: &Config) -> Result<()> {
    config.validate()?;
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let body = toml::to_string_pretty(config).context("failed to serialize configuration")?;
    let text =
        format!("# StreamPainter 設定 — 通常はタスクトレイの「設定...」から編集します。\n{body}");
    std::fs::write(&path, text).with_context(|| format!("failed to write {}", path.display()))
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let config = Config::default();
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
}
