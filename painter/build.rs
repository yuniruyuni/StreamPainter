use embed_manifest::manifest::DpiAwareness;
use embed_manifest::new_manifest;

fn main() {
    // build script はホスト用にコンパイルされるため #[cfg(windows)] では
    // ターゲット判定できない。Cargo が渡す CARGO_CFG_WINDOWS で判定する。
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        // VERSIONINFOとmanifestを1つのWindows resourceへまとめる。SignPathのartifact
        // configurationが製品名・versionを署名前に強制できるよう、Cargo versionを使う。
        let manifest = new_manifest("StreamPainter")
            .dpi_awareness(DpiAwareness::PerMonitorV2)
            .to_string();
        let mut resource = winresource::WindowsResource::new();
        resource
            .set("ProductName", "StreamPainter")
            .set("FileDescription", "StreamPainter")
            .set("InternalName", "stream-painter.exe")
            .set("OriginalFilename", "stream-painter.exe")
            .set_manifest(&manifest);
        resource
            .compile()
            .expect("failed to embed Windows resources");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
