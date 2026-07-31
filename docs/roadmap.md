# Roadmap

## 現在の到達点

- Windows透明描画オーバーレイ
- F9、タスクトレイ、右クリックツールメニュー
- ペン・マーカー・消しゴム・直線・矢印・四角形・楕円
- 設定画面で管理する固定サイズPNGスタンプ
- OBS全画面プロジェクター追従、Z-order維持、obs-websocket連携
- Rust内蔵loopback HTTP/WebSocketサーバー
- OBS Browser Source用assetsのexe埋め込み
- 接続時snapshot、自動再接続、bounded client queue
- Host/Origin/CSPによるlocalhostサービス保護
- Cloud Run、DB、Twitch認証、公開管理画面の撤去
- タスクトレイから開くネイティブ設定画面と入力検証
- MITライセンスと、生成・埋め込み式の第三者ライセンス表示

## 次の候補

- トレイメニューからOBS URLをクリップボードへコピー
- Windowsログイン時の自動起動を選べる設定
- 設定画面からのBrowser Source疎通確認
- ペンデバイスの筆圧・傾き取得
- 図形・スタンプの選択、移動、回転、拡大縮小
- テキスト、アニメーションスタンプ
- 実OBSを使ったWindows smoke testの自動化

## 非目標

- LANまたはインターネットへの直接公開
- マルチユーザー・チャンネル管理
- Twitchログイン
- クラウド上の常駐WebSocketサービス

これらが必要になった場合は、ローカルアプリを公開サーバー化せず、独立したrelay製品として
脅威モデルと運用コストを再検討します。
