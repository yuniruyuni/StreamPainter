# StreamPainter

Windows 上で OBS の映像に直接描き込む、ローカル完結型のオーバーレイです。

`stream-painter.exe` を常駐させると、アプリ自身が `127.0.0.1` に小さな HTTP/WebSocket
サーバーを起動します。OBS Browser Source はそこから透明な描画ページを読み込みます。
Cloud Run、外部DB、Twitchログイン、公開Webアプリは使いません。

```text
ポインター入力
  → stream-painter.exe / CanvasItem engine
      ├─ Windows透明オーバーレイ（ローカルエコー）
      └─ http://127.0.0.1:16873
           └─ WebSocket → OBS Browser Source
```

## 使い方

1. `stream-painter.exe` を起動したままにする。
2. タスクトレイアイコンの「設定...」で対象モニターやOBS連携を設定し、アプリを再起動する。
3. 設定画面またはタスクトレイの「OBS Browser Source URLをコピー」でURLをコピーし、
   OBSの「ブラウザ」ソースへ設定する。
4. Browser Source の幅と高さを OBS の基本キャンバス解像度に合わせる。
5. 既定の `F9`（設定画面で変更・解除可能）で描画モードを切り替える。右ボタンを押したまま
   移動すると、内周の描画ツールと
   外周の色を選び、ボタンを離した時点で確定できる。内周の「スタンプ」を通過すると外周が
   サムネイル付きのスタンプ一覧へ切り替わる。動かさずに右クリックするとメニューが固定され、
   左右どちらのクリックでも選択できる。中央をクリックすると従来メニュー、円外のドックから
   Undo、Redo、全消去を操作できる。「選択」ツールでは配置済みスタンプを左ドラッグで移動
   でき、空いている場所のクリックで選択を解除できる。

標準のBrowser Source URLは `http://127.0.0.1:16873/overlay` です。設定は画面から保存でき、
`%APPDATA%\StreamPainter\config\config.toml` を直接編集する必要はありません。ポート競合などで
通常起動できない場合も、`stream-painter.exe --settings` で設定画面だけを開けます。
PNGスタンプの登録・削除・名称・既定サイズ・不透明度も同じ設定画面で管理できます。
OBS WebSocketパスワードは設定ファイルへ含めず、現在のWindowsユーザーの資格情報マネージャーへ
保存します。そのため設定バックアップやportableなexeのコピー時にはパスワードを再入力します。
設定画面とトレイでは、ローカルサーバーが到達可能か、OBS Browser SourceのWebSocketが実際に
接続済みかを別々に確認できます。

## ソースからビルド

必要なものは Bun 1.3.14、Rust 1.97.1、`cargo-about 0.9.1`、Visual Studio Build Tools
（Desktop development with C++）です。

```powershell
bun install --frozen-lockfile
bun run check
cargo install cargo-about --version 0.9.1 --locked --features cli
bun run prepare:painter
cd painter
cargo test --locked
cargo build --release --locked
```

`bun run prepare:painter` がOBS用ページと依存関係のライセンスページを生成します。生成物は
Git管理せず、続くrelease buildが両方をexeへ埋め込みます。配布物は
`painter/target/release/stream-painter.exe` です。

公式Releaseではバージョン付きexeとSHA-256チェックサムを配布します。リリース手順は
[CONTRIBUTING.md](CONTRIBUTING.md)を参照してください。現在の配布バイナリはコード署名して
いません。

詳しい設計と設定は [docs/README.md](docs/README.md) を参照してください。
開発への参加方法は [CONTRIBUTING.md](CONTRIBUTING.md) にまとめています。
主な変更は [CHANGELOG.md](CHANGELOG.md)、脆弱性の非公開報告方法は
[SECURITY.md](SECURITY.md) を参照してください。

## ライセンス

StreamPainterは [MIT License](LICENSE) で提供されます。配布バイナリに含まれる依存関係の
ライセンスはlockfileからビルド時に生成してexeへ埋め込み、実行中はタスクトレイの
「第三者ライセンス...」から確認できます。
