//! GitHub Releaseを使った手動アップデートの確認・適用。
//!
//! ユーザーが設定画面のボタンを押した時だけGitHubへ問い合わせる。バックグラウンドでの
//! 定期確認や、確認なしの自動適用は行わない (docs/code-signing.mdのprivacy statement参照)。
//! ネットワーク呼び出しは同期(blocking)なので、呼び出し側が専用スレッドで呼ぶこと。

use anyhow::{Context, Result};
use windows::Win32::UI::WindowsAndMessaging::WM_APP;

/// 設定画面から本体プロセスへ「更新を適用したので再起動してほしい」と伝える通知。
/// ペイロードは持たない。再起動対象のexeパスは受信側が`std::env::current_exe()`から
/// 自分で解決するため、任意pointerをmessageに含めない。
pub const WM_REQUEST_RESTART: u32 = WM_APP + 4;

const REPO_OWNER: &str = "yuniruyuni";
const REPO_NAME: &str = "StreamPainter";
const BIN_NAME: &str = "stream-painter";
/// Release asset名 (`StreamPainter-vX.Y.Z-windows-x64.exe`) に含まれる識別子。
const TARGET: &str = "windows-x64";

#[derive(Debug, Clone)]
pub struct AvailableUpdate {
    pub version: String,
    pub notes: String,
}

/// 最新リリースが現在より新しいかどうかを確認する。ダウンロードは行わない。
pub fn check(current_version: &str) -> Result<Option<AvailableUpdate>> {
    let release = self_update::backends::github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name(BIN_NAME)
        .target(TARGET)
        .current_version(current_version)
        .no_confirm(true)
        .show_output(false)
        .build()
        .context("アップデート確認の準備に失敗しました")?
        .is_update_available()
        .context("最新リリースの確認に失敗しました (ネットワーク接続を確認してください)")?;

    Ok(release.map(|release| AvailableUpdate {
        version: release.version().to_owned(),
        notes: release.body().unwrap_or_default().trim().to_owned(),
    }))
}

/// 指定バージョンをダウンロード・検証し、現在の実行ファイルを置き換える。
/// 実行中のプロセス自体は置き換わらず、次回起動時に新しいバイナリが使われる。
pub fn apply(current_version: &str, target_version: &str) -> Result<()> {
    let executable = std::env::current_exe().context("現在の実行ファイルの場所を取得できません")?;
    let tag = format!("v{target_version}");

    self_update::backends::github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name(BIN_NAME)
        .target(TARGET)
        .current_version(current_version)
        .release_tag(&tag)
        .bin_install_path(&executable)
        .no_confirm(true)
        .show_output(false)
        .build()
        .context("アップデート適用の準備に失敗しました")?
        .update_extended()
        .with_context(|| format!("バージョン {target_version} の適用に失敗しました"))?;

    Ok(())
}

/// 新しい実行ファイルを起動する。呼び出し側は成功後に現在のプロセスを終了すること。
pub fn relaunch() -> Result<()> {
    let executable = std::env::current_exe().context("現在の実行ファイルの場所を取得できません")?;
    std::process::Command::new(executable)
        .spawn()
        .context("更新後のStreamPainterを起動できません")?;
    Ok(())
}
