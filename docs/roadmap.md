# Roadmap

## 現在の到達点

- Windows透明描画オーバーレイ
- 設定可能なグローバルホットキー（既定F9）、タスクトレイ、スタンプサムネイル対応の
  右ホールド式ラジアルメニューと従来メニュー
- ペン・マーカー・消しゴム・直線・矢印・四角形・楕円
- Windows Pointer APIによるペンの筆圧・傾き、能力欠落時の一定幅fallback
- 設定画面で管理する固定サイズPNGスタンプ
- 配置済みスタンプの選択、ドラッグ移動、Undo／Redo
- OBS全画面プロジェクター追従、Z-order維持、obs-websocket連携
- Rust内蔵loopback HTTP/WebSocketサーバー
- OBS Browser Source用assetsのexe埋め込み
- 接続時snapshot、自動再接続、bounded client queue
- Host/Origin/CSPによるlocalhostサービス保護
- Cloud Run、DB、Twitch認証、公開管理画面の撤去
- タスクトレイから開くネイティブ設定画面と入力検証
- 現在のWindowsユーザー向け、実レジストリ状態を検証・修復できるログオン自動起動
- 設定画面・トレイからのBrowser Source URLコピーと、イベント駆動の接続診断
- MITライセンスと、生成・埋め込み式の第三者ライセンス表示
- 公式OBSを固定・検証してBrowser Sourceのsnapshot表示とprojectorを確認する週次／手動smoke test

## 次の候補

- Surface Pen／Windows Ink対応タブレットでの実機・driver別の筆圧／傾き検証
- 図形の選択・移動と、図形／スタンプの回転・拡大縮小
- テキスト、アニメーションスタンプ

## 非目標

- LANまたはインターネットへの直接公開
- マルチユーザー・チャンネル管理
- Twitchログイン
- クラウド上の常駐WebSocketサービス

これらが必要になった場合は、ローカルアプリを公開サーバー化せず、独立したrelay製品として
脅威モデルと運用コストを再検討します。
