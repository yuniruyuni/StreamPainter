# StreamPainter Windows application

Rust/Win32で動く本体です。透明なDirectCompositionオーバーレイ、入力処理、
OBSプロジェクター連携、OBS Browser Source向けローカルWebサーバーを1プロセスで提供します。

## ビルド

リポジトリ直下でOBS用ページを先に生成します。

```powershell
bun install --frozen-lockfile
bun run build
cd painter
cargo build --release
```

release buildには `client/static/` のHTML/CSS/JavaScriptが埋め込まれるため、配布物は
`target/release/stream-painter.exe` だけです。

## 設定

タスクトレイアイコンの「設定...」から編集します。保存した設定はアプリの再起動後に反映
されます。OBS Browser SourceのURLも設定画面に表示されます。

```text
http://127.0.0.1:16873/overlay
```

保存先の `%APPDATA%\StreamPainter\config\config.toml` は自動管理されるため、直接編集する必要は
ありません。通常起動できない場合は `stream-painter.exe --settings` で設定画面だけを開けます。

設定項目の詳細は [../docs/painter.md](../docs/painter.md) を参照してください。

## 検証

```powershell
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```
