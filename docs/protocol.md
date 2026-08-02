# Local WebSocket protocol

## Transport

- HTTP origin: `http://127.0.0.1:<port>`
- WebSocket: `ws://127.0.0.1:<port>/ws`
- JSON text frames
- 外部公開・TLS・認証トークンなし

TLSを使わないのはloopback内で完結するためです。代わりにサーバーはbind先、Host、Originを
検証します。

## Coordinates

点は `[u, v, pressure, dt, tiltX, tiltY]` です。

- `u`, `v`: OBS canvas content rectに対する正規化座標
- `pressure`: 0..1。0が無荷重、1が最大荷重
- `dt`: `stroke_begin` からの相対ミリ秒
- `tiltX`, `tiltY`: Windowsが返す±90度を-1..1へ正規化したペンの傾き。`tiltX`の正は
  画面右、`tiltY`の正はユーザー側

Windowsの`WM_POINTER`入力ではmouse、touch、pen、touchpadを区別し、penだけ
`POINTER_PEN_INFO`を読みます。`PEN_MASK_PRESSURE`がある場合は0..1024を0..1へ、
`PEN_MASK_TILT_X/Y`がある場合は-90..90度を-1..1へ変換し、範囲外のdriver値はclampします。
該当maskがない、API取得が失敗した、またはpen以外の場合は`pressure=1`、`tiltX=tiltY=0`です。
このfallbackにより、筆圧非対応ペン・マウス・touchの線幅は従来どおり一定になります。

`Brush`は入力値の反映方法をストロークごとに固定します。

- `pressureWidth`: 筆圧を線幅へ反映するか
- `pressureMin`: `pressure=0`での最小倍率。描画時は0.05..1へclamp
- `tiltWidth`: 傾きの大きさを線幅へ反映するか
- `tiltMaxScale`: 傾きの大きさが1のときの最大倍率。描画時は1..4へclamp

contentの高さを`H`とした線幅は、RustのDirect2D描画とBrowser SourceのCanvas 2D描画で
次の同じ式を使います。非有限値は筆圧1、傾き0へfallbackします。

```text
base = widthN * H
pressureScale = pressureWidth ? pressureMin + (1 - pressureMin) * pressure : 1
tiltMagnitude = min(hypot(tiltX, tiltY), 1)
tiltScale = tiltWidth ? 1 + (tiltMaxScale - 1) * tiltMagnitude : 1
width = base * pressureScale * tiltScale
```

現在のpresetは、ペンが`pressureMin=0.2`で筆圧のみ、マーカーが`pressureMin=0.65`で筆圧と
最大1.75倍の傾き、消しゴムが筆圧・傾きとも無効です。現時点では傾きの向きではなく大きさだけを
丸いマーカーの幅へ使います。

JSONフィールドはcamelCase、`type`値はsnake_caseです。

## Connection lifecycle

接続直後、サーバーは必ず現在状態を送ります。

```json
{
  "type": "snapshot",
  "protocolVersion": 7,
  "rev": 12,
  "fadeAfterMs": null,
  "items": []
}
```

`items` は描画順を保つ完全な履歴で、`kind` が `stroke`、`shape`、`stamp` のいずれかです。
overlayは`protocolVersion`が対応範囲（現在はv6〜v7）にあることを確認し、`items`で手元の状態を全置換します。
その後、以下の増分イベントを`rev`の連番どおりに適用します。

```json
{"type":"stroke_begin","rev":13,"strokeId":"...","brush":{"tool":"pen","color":"#ff4d6d","opacity":1,"widthN":0.005,"pressureWidth":true,"pressureMin":0.2,"tiltWidth":false,"tiltMaxScale":1}}
{"type":"stroke_points","rev":14,"strokeId":"...","pts":[[0.1,0.2,0.75,0,0.25,-0.5]]}
{"type":"stroke_end","rev":15,"strokeId":"...","endedAt":1785380000000}
{"type":"stroke_cancel","rev":16,"strokeId":"..."}
{"type":"shape_begin","rev":17,"shape":{"itemId":"...","shape":"arrow","style":{"color":"#ff4d6d","opacity":1,"widthN":0.005},"start":[0.1,0.2],"end":[0.1,0.2],"done":false,"endedAt":null}}
{"type":"shape_update","rev":18,"itemId":"...","end":[0.8,0.7]}
{"type":"shape_end","rev":19,"itemId":"...","endedAt":1785380000000,"transform":{"center":[0.45,0.45],"widthN":0.7,"heightN":0.5,"rotation":0}}
{"type":"shape_cancel","rev":20,"itemId":"..."}
{"type":"stamp_add","rev":21,"stamp":{"itemId":"...","stampId":"...","center":[0.5,0.5],"widthN":0.0844,"heightN":0.15,"rotation":0,"opacity":1,"done":true,"endedAt":1785380000000}}
{"type":"stamp_move_preview","rev":22,"itemId":"...","center":[0.7,0.55]}
{"type":"stamp_move","rev":23,"itemId":"...","center":[0.75,0.6]}
{"type":"item_transform_preview","rev":24,"itemId":"...","transform":{"center":[0.7,0.55],"widthN":0.12,"heightN":0.18,"rotation":0.4}}
{"type":"item_transform_commit","rev":25,"itemId":"...","transform":{"center":[0.75,0.6],"widthN":0.12,"heightN":0.18,"rotation":0.4}}
{"type":"undo","rev":26}
{"type":"redo","rev":27,"item":{"kind":"stamp","itemId":"...","stampId":"...","center":[0.5,0.5],"widthN":0.0844,"heightN":0.15,"rotation":0,"opacity":1,"done":true,"endedAt":1785380000000}}
{"type":"clear","rev":28}
```

図形の`shape`は`line`、`arrow`、`rectangle`、`ellipse`です。スタンプの画像は同じoriginの
`/stamps/<stampId>`から取得します。`widthN`と`heightN`をイベントに固定しているため、設定を
後で変更しても配置済みスタンプの寸法は変わりません。

`transform`は図形とスタンプに共通する永続geometryです。`center`はcontent正規化座標、`widthN`は
content幅、`heightN`はcontent高さに対する比率、`rotation`はcanvas上で時計回りのradianです。
`item_transform_preview`と`item_transform_commit`は描画順を変えず、指定した図形またはスタンプの
位置・サイズ・回転をまとめて更新します。ドラッグ中は16ms間隔で最新previewだけを送信し、
Browser Sourceは対象より前をprefixへcacheし、対象と後続履歴を元の順序で同じpreview canvasへ
`requestAnimationFrame`ごとに再合成します。このため後続の半透明strokeやeraserも確定時と同じ結果になります。
確定、キャンセル、Undo／Redoは`item_transform_commit`で最終状態を同期し、1ドラッグを履歴上の
1操作として扱います。

`stamp_move_preview`と`stamp_move`は描画順を変えずに指定した`itemId`の`center`を更新します。
ドラッグ中は最新座標を16ms間隔（約60fps）の`stamp_move_preview`で送り、同じ間隔内の中間座標は
送らずに上書きします。overlayは最初のpreviewで対象より前をcacheし、以降は対象から後ろだけを
`requestAnimationFrame`単位で履歴順に更新します。ドラッグ確定時は待機中の
最終座標を`stamp_move`として即時送信し、通常の描画順へ戻します。キャンセルとスタンプ移動の
Undo／Redoも、確定位置だけを示す`stamp_move`として配信します。項目追加のUndo／Redoは従来
どおり`undo`／`redo`を使います。

v7 readerはv6 snapshot/eventと互換です。v6の6要素pointと筆圧・傾きbrushはそのまま保持し、
v6図形に`transform`がない場合は`start`/`end`から復元、v6スタンプに`rotation`がない場合は0として
扱います。v6の`shape_end`に`transform`がない場合も同じfallbackを使い、
`stamp_move_preview`/`stamp_move`はlegacy互換のため残します。v7でtransformを確定したsnapshotは
v6 clientへ戻せない一方向migrationですが、exeは同じversionのBrowser Source assetsを内蔵するため
通常運用ではversionが分離しません。

`endedAt` はUnix epoch millisecondsです。

version 6はversion 5の4要素point prefix `[u,v,pressure,dt]`へ傾きを追加し、brushにも筆圧・傾き
tuningを追加した版です。v5にはこれらがないためv7 readerの対応範囲へは含めず、曖昧な部分描画を
避けて再接続します。serverとBrowser Source assetsは同じexeへ埋め込み、同じbuildで更新されます。
更新時にOBSが古いページを保持している場合、WebSocketの再接続だけでは読み込み済みJavaScriptは
置き換わりません。OBSのBrowser Sourceで「現在のページを再読み込み」し、version 7のassetsと
snapshotへ揃えてください。

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

`protocolVersion`が対応範囲（v6〜v7）外の場合、または増分イベントの`rev`が欠落・重複した場合も接続を
閉じ、再接続直後のsnapshotから状態を復旧します。

## Limits and recovery

- 最大CanvasItem数（ストローク・図形・スタンプ合計）: 500
- 最大合計点数: 200,000
- 1ストロークの最大点数: 10,000
- 1 `stroke_points` の最大点数: 512
- 接続ごとの送信待ち: 256メッセージ

送信待ちが上限に達したクライアントはハブから切断されます。描画入力は止めず、overlayが
再接続して最新snapshotを受けることで復旧します。

## Conformance fixture

`protocol-fixtures/canonical.json` はRustのserde型とローカルハブ状態機械から生成する、
追跡対象のcanonical fixtureです。全server message variant、JSONフィールド、定数、正常な
状態遷移、revision欠落・重複、未知version、各上限でのtrim結果を含みます。TypeScriptのテストは
このJSONをdecodeして`OverlayState`へ適用し、Rustが記録した状態と照合します。

プロトコルを変更したときは次を実行し、RustとTypeScriptを同じコミットで更新してください。

```console
bun run generate:protocol-fixtures
bun run check
cargo test --locked --manifest-path painter/Cargo.toml
```

Rustテストは追跡fixtureをその場で生成した内容と比較するため、fixtureを更新し忘れた変更や、
新しいmessage variantをTypeScript側へ反映し忘れた変更はCIで失敗します。
