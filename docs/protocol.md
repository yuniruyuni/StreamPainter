# Local WebSocket protocol

## Transport

- HTTP origin: `http://127.0.0.1:<port>`
- WebSocket: `ws://127.0.0.1:<port>/ws`
- JSON text frames
- 外部公開・TLS・認証トークンなし

TLSを使わないのはloopback内で完結するためです。代わりにサーバーはbind先、Host、Originを
検証します。

## Coordinates

点は `[u, v, pressure, dt]` です。

- `u`, `v`: OBS canvas content rectに対する正規化座標
- `pressure`: 0..1
- `dt`: `stroke_begin` からの相対ミリ秒

JSONフィールドはcamelCase、`type`値はsnake_caseです。

## Connection lifecycle

接続直後、サーバーは必ず現在状態を送ります。

```json
{
  "type": "snapshot",
  "rev": 12,
  "fadeAfterMs": null,
  "strokes": []
}
```

overlayは手元の状態をsnapshotで全置換します。その後、以下の増分イベントを順番に適用します。

```json
{"type":"stroke_begin","strokeId":"...","brush":{"tool":"pen","color":"#ff4d6d","opacity":1,"widthN":0.005,"pressureWidth":true}}
{"type":"stroke_points","strokeId":"...","pts":[[0.1,0.2,0.5,0]]}
{"type":"stroke_end","strokeId":"...","endedAt":1785380000000}
{"type":"stroke_cancel","strokeId":"..."}
{"type":"undo"}
{"type":"clear"}
```

`endedAt` はUnix epoch millisecondsです。

## Liveness

overlayは15秒ごとに次を送ります。

```json
{"type":"ping","t":1785380000000}
```

サーバーは同じ値で応答します。

```json
{"type":"pong","t":1785380000000}
```

30秒間応答がなければoverlayは接続を閉じ、1秒から30秒の指数backoffと±20% jitterで
再接続します。

## Limits and recovery

- 最大ストローク数: 500
- 最大合計点数: 200,000
- 1ストロークの最大点数: 10,000
- 1 `stroke_points` の最大点数: 512
- 接続ごとの送信待ち: 256メッセージ

送信待ちが上限に達したクライアントはハブから切断されます。描画入力は止めず、overlayが
再接続して最新snapshotを受けることで復旧します。
