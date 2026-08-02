# Security

## Threat model

ローカルWebサービスで主に考慮する対象は次の2つです。

1. LAN上の別端末が描画状態やWebSocketへアクセスすること
2. ブラウザで開いた悪意あるページがlocalhost WebSocketへ接続すること

同一Windowsユーザー権限で動くネイティブマルウェアからの防御は対象外です。

## Controls

- listenerはIPv4 loopbackの `127.0.0.1` のみにbindする
- `0.0.0.0` やLANアドレスへ変更する設定を提供しない
- HTTP `Host` は `127.0.0.1:<port>` または `localhost:<port>` のみ許可する
- WebSocket `Origin` は同じHTTP originのみ許可する
- Originが無いWebSocket upgradeも拒否する
- HTMLに厳格なContent Security Policyを付ける
- 第三者ライセンスページはscriptを持たず、外部resourceを読み込まない
- Web assetsはexe内に埋め込み、外部script・style・imageへ接続しない
- スタンプは静止PNGだけを管理ディレクトリへコピーし、生成IDと固定Content-Typeで配信する
- スタンプに任意URL・任意ファイルパス・SVGを登録できない
- スタンプの個数、ファイルサイズ、縦横寸法、総ピクセル数を制限する
- Browser Sourceから受け付けるアプリメッセージは`ping`だけ
- 受信WebSocket frameは4 KiBに制限する
- 遅い接続の送信queueを制限し、UIスレッドへのbackpressureを防ぐ
- OBS WebSocketパスワードはconfigとそのbackupへserializeせず、Windows資格情報マネージャーの
  現在ユーザー用Generic credentialに保存する
- 接続診断はプロセス内部の状態通知だけで実装し、外部向けdiagnostics endpointを追加しない
- ログオン自動起動は管理者権限不要のHKCU Run値だけを使い、現在のexeを引数なしで登録する

loopback構成ではBearer tokenは秘密になりにくく、配布・更新も複雑にするため採用しません。
ブラウザ経由の攻撃はOrigin検証、LAN経由の攻撃はbind先とHost検証で遮断します。

## Operational notes

Windows Firewallで外部受信規則を作る必要はありません。もし実行時に公開ネットワーク向けの
Firewall許可を求められた場合は拒否して構いません。

設定ファイルや配布exeのコピーにはOBS WebSocketパスワードが含まれません。別PC・別ユーザーへの
移行時はパスワードを再入力します。同じWindowsユーザー権限で動くプロセスによる資格情報の取得は
上記のthreat modelどおり防御対象外です。

他端末からoverlayを見る用途が必要になった場合、listenerを公開するのではなく、認証・TLS・
rate limitを備えた別製品のrelayとして設計してください。
