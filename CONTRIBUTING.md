# Contributing to StreamPainter

バグ報告、改善提案、ドキュメント修正、コード変更を歓迎します。大きな仕様変更は、実装前に
GitHub Issueで目的と製品境界への影響を相談してください。

## 製品境界

StreamPainterは、Windows上で動作するローカル完結型アプリケーションです。公開Webサービス、
ログイン、外部データベース、LAN向けlistenerを追加する変更は、通常の機能追加とは分けて
設計・レビューします。

## 開発と検証

リポジトリ直下で次を実行します。
Rustはルートの`rust-toolchain.toml`で1.94.1、Bunは`package.json`で1.3.12に固定しています。

```bash
bun install --frozen-lockfile
bun run check
cargo install cargo-about --version 0.9.1 --locked --features cli
bun run prepare:painter

cd painter
cargo fmt --check
cargo test --locked
```

Windows固有コードは、Windows上で次も実行してください。

```powershell
cd painter
cargo clippy --all-targets --locked -- -D warnings
cargo build --release --locked
```

`bun run prepare:painter` はOBS用ページを生成するとともに、現在のlockfileとインストール済み
依存関係を検査してexeへ埋め込むライセンスページを生成します。これらの生成物はGit管理しない
ため、変更へ含めないでください。CIもクリーンなcheckoutから同じ生成を行ってからRustを検証・
ビルドします。

`bun.lock`と`painter/Cargo.lock`は、Pull Request時に`bun audit`とRustSecの`cargo audit`で
検査します。同じ監査は既知の脆弱性データ更新を拾うため毎週自動実行します。

## リリース（メンテナー向け）

1. `painter/Cargo.toml`のバージョンと`CHANGELOG.md`を更新してmainへ反映する。
2. 同じバージョンの`vX.Y.Z`タグを作成し、originへpushする。
3. Release workflowが全検証、Windows release build、SHA-256生成、GitHub Release公開を
   完了したことを確認する。

タグとCargoのバージョンが一致しない場合、または検証・asset uploadのいずれかが失敗した
場合はReleaseを公開しません。公開後の同じタグ・assetの上書きは行わず、新しい修正版として
バージョンを上げてください。Windowsコード署名は現在のフローには含まれません。

## ライセンス

コントリビューションは、リポジトリの [MIT License](LICENSE) の下で提供されます。第三者の
コード、画像、データなどを追加する場合は、出典と再配布条件をPull Requestに記載してください。
