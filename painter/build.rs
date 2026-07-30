use embed_manifest::manifest::DpiAwareness;
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    // build script はホスト用にコンパイルされるため #[cfg(windows)] では
    // ターゲット判定できない。Cargo が渡す CARGO_CFG_WINDOWS で判定する。
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        // Per-Monitor V2 DPI awareness をマニフェストで宣言する
        embed_manifest(new_manifest("StreamPainter").dpi_awareness(DpiAwareness::PerMonitorV2))
            .expect("failed to embed manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
