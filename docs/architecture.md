# Architecture

## プロセス構成

`stream-painter.exe` が次の責務をすべて持ちます。

```text
Win32 UI thread
├─ pointer / hotkey / tray
├─ StrokeEngine
└─ Direct2D local echo
        │ bounded-size PaintEvent
        ▼
local-web thread (single-thread Tokio runtime)
├─ Paint hub: canonical OBS-side mirror state
├─ HTTP assets embedded in exe
└─ WebSocket subscribers
        ▼
OBS Browser Source / Canvas 2D
```

UIスレッドからハブへの送信はunbounded channelへの短いenqueueだけです。HTTP、JSON送信、
遅いBrowser Sourceを待つ処理はUIスレッドで行いません。

## ローカルハブ

ハブは専用ランタイム上で全コマンドを直列処理します。これにより、接続処理と描画イベント
の順序が一意になり、snapshotと増分イベントの競合を避けます。

- 新規接続: 現在の全ストロークをsnapshotとして最初に送信
- 通常描画: `stroke_*`、`undo`、`clear` を増分配信
- 再接続: 古いクライアント状態をsnapshotで全置換
- backpressure: 各接続は256メッセージの上限を持つ
- 遅延時: 接続をハブから除外し、Browser Source側の再接続に任せる

ストロークは最大500本、合計200,000点、1本10,000点に制限します。古い確定ストローク
を削除する必要が生じた場合は増分ではなくsnapshotを送り、全クライアントを再同期します。

## Web assets

`client/` はOBS用透明ページだけを生成します。ビルド成果物は固定名の
`index.html`、`index.css`、`index.js` で、Rust release build時にexeへ埋め込みます。
実行時に外部ファイルやCDNは必要ありません。

第三者ライセンスページも生成時に依存関係とlockfileを照合し、Rust本体へ埋め込みます。
タスクトレイから開くと、同じloopbackサーバーの `/licenses` で表示されます。

## 終了

アプリ終了時はローカルサーバーのfutureをキャンセルし、WebSocketを含む接続を閉じてから
専用スレッドをjoinします。OBS側はページが残っていれば再接続を続け、アプリ再起動後に
新しいsnapshotを受け取ります。
