# Documentation

- [overview.md](overview.md): 製品の境界と利用フロー
- [architecture.md](architecture.md): Rust本体、ローカルWebサーバー、OBS overlayの責務
- [painter.md](painter.md): 導入、設定、操作、トラブルシュート
- [webapp.md](webapp.md): OBS Browser Sourceの描画実装
- [protocol.md](protocol.md): ローカルWebSocketプロトコル
- [security.md](security.md): loopbackサービスの脅威モデルと防御
- [obs-smoke-test.md](obs-smoke-test.md): 実OBSを使うWindows定期smoke test
- [roadmap.md](roadmap.md): 現在の到達点と今後の候補

現在の正式構成はローカル完結型です。旧Cloud Runサーバー、Twitch認証、PostgreSQL、
公開管理画面は製品境界から削除されています。配布物に含まれる依存関係のライセンスは
ビルド時に生成してexeへ埋め込み、タスクトレイの「第三者ライセンス...」から表示できます。
