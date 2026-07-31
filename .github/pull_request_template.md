## 変更内容

<!-- 何を、なぜ変更したかを簡潔に説明してください。 -->

## 検証

<!-- 実行したコマンドと、手動で確認した環境・操作を記載してください。 -->

- [ ] `bun run check`
- [ ] `bun run prepare:painter`
- [ ] `cargo fmt --check`
- [ ] `cargo test --locked`
- [ ] Windows固有の変更では `cargo clippy --all-targets --locked -- -D warnings`

## 確認事項

- [ ] 公開Webサービス、ログイン、外部DB、LAN listenerを意図せず追加していません。
- [ ] Rust/TypeScript間のプロトコル変更は両側と文書を同時に更新しました。
- [ ] 第三者のコード・画像・データを追加した場合、出典と再配布条件を記載しました。
- [ ] ユーザーデータや秘密情報をコミットしていません。
