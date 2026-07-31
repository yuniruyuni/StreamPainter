//! StreamPainter: OBS 全画面プロジェクター上の透明描画オーバーレイ (docs/painter.md)。

// GUI アプリとしてコンソールを出さない (デバッグビルドではログを見たいので release のみ)
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod config;
mod engine;
mod net;
mod protocol;
#[cfg(windows)]
mod win;

#[cfg(windows)]
fn main() {
    let _logging = win::logging::init();

    // 診断モード: モニタとウィンドウの一覧を表示して終了する
    if std::env::args().any(|a| a == "--detect") {
        win::projector::print_diagnosis();
        return;
    }
    // 通常起動できない状態（ポート競合など）でも設定を修正できる。
    if std::env::args().any(|a| a == "--settings") {
        if let Err(e) = win::settings::run_standalone() {
            win::message_box(&format!("設定画面を開けません:\n{e:#}"));
        }
        return;
    }

    let _single_instance = match win::single_instance::acquire() {
        Ok(Some(guard)) => guard,
        Ok(None) => {
            tracing::info!("another StreamPainter instance is already running");
            win::message_box_info(
                "StreamPainterは既に起動しています。\nタスクトレイを確認してください。",
            );
            return;
        }
        Err(e) => {
            tracing::error!("single-instance guard: {e:#}");
            win::message_box(&format!("多重起動の確認に失敗しました:\n{e:#}"));
            std::process::exit(1);
        }
    };

    if let Err(e) = win::app::run() {
        tracing::error!("fatal: {e:#}");
        win::message_box(&format!("StreamPainter を開始できません:\n{e:#}"));
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("StreamPainter は Windows 専用アプリケーションです (docs/painter.md)");
    std::process::exit(1);
}
