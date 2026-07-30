# Contributing to StreamPainter

バグ報告、改善提案、ドキュメント修正、コード変更を歓迎します。大きな仕様変更は、実装前に
GitHub Issueで目的と製品境界への影響を相談してください。

## 製品境界

StreamPainterは、Windows上で動作するローカル完結型アプリケーションです。公開Webサービス、
ログイン、外部データベース、LAN向けlistenerを追加する変更は、通常の機能追加とは分けて
設計・レビューします。

## 開発と検証

リポジトリ直下で次を実行します。

```bash
bun install --frozen-lockfile
bun run check
bun run build

cd painter
cargo fmt --check
cargo test
```

Windows固有コードは、Windows上で次も実行してください。

```powershell
cd painter
cargo clippy --all-targets -- -D warnings
cargo build --release
```

依存関係またはlockfileを変更した場合は、`cargo-about 0.9.1` を用意して通知ファイルを更新します。

```bash
bun run generate:licenses
bun run check:licenses
```

生成された `THIRD_PARTY_NOTICES.md` と
`painter/assets/third-party-licenses.html` も変更に含めてください。

## ライセンス

コントリビューションは、リポジトリの [MIT License](LICENSE) の下で提供されます。第三者の
コード、画像、データなどを追加する場合は、出典と再配布条件をPull Requestに記載してください。
