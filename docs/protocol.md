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
  "protocolVersion": 2,
  "rev": 12,
  "fadeAfterMs": null,
  "strokes": [],
  "items": []
}
```

`items` は描画順を保つ完全な履歴で、`kind` が `stroke`、`shape`、`stamp` のいずれかです。
`strokes` はprotocol v1クライアントとの互換用で、`items`内のストロークだけを複製します。
v2 overlayは`items`で手元の状態を全置換し、その後、以下の増分イベントを順番に適用します。

```json
{"type":"stroke_begin","strokeId":"...","brush":{"tool":"pen","color":"#ff4d6d","opacity":1,"widthN":0.005,"pressureWidth":true}}
{"type":"stroke_points","strokeId":"...","pts":[[0.1,0.2,0.5,0]]}
{"type":"stroke_end","strokeId":"...","endedAt":1785380000000}
{"type":"stroke_cancel","strokeId":"..."}
{"type":"shape_begin","shape":{"itemId":"...","shape":"arrow","style":{"color":"#ff4d6d","opacity":1,"widthN":0.005},"start":[0.1,0.2],"end":[0.1,0.2],"done":false,"endedAt":null}}
{"type":"shape_update","itemId":"...","end":[0.8,0.7]}
{"type":"shape_end","itemId":"...","endedAt":1785380000000}
{"type":"shape_cancel","itemId":"..."}
{"type":"stamp_add","stamp":{"itemId":"...","stampId":"...","center":[0.5,0.5],"widthN":0.0844,"heightN":0.15,"opacity":1,"done":true,"endedAt":1785380000000}}
{"type":"undo"}
{"type":"clear"}
```

図形の`shape`は`line`、`arrow`、`rectangle`、`ellipse`です。スタンプの画像は同じoriginの
`/stamps/<stampId>`から取得します。`widthN`と`heightN`をイベントに固定しているため、設定を
後で変更しても配置済みスタンプの寸法は変わりません。

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

- 最大CanvasItem数（ストローク・図形・スタンプ合計）: 500
- 最大合計点数: 200,000
- 1ストロークの最大点数: 10,000
- 1 `stroke_points` の最大点数: 512
- 接続ごとの送信待ち: 256メッセージ

送信待ちが上限に達したクライアントはハブから切断されます。描画入力は止めず、overlayが
再接続して最新snapshotを受けることで復旧します。
