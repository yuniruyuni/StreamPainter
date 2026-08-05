# Architecture

## プロセス構成

`stream-painter.exe` が次の責務をすべて持ちます。

```text
Win32 UI thread
├─ WM_POINTER device分類 / pen pressure・tilt正規化 / hotkey / tray
├─ CanvasItem engine
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

UIスレッドからハブへの送信は上限1,024件のchannelへの短いenqueueだけです。HTTP、JSON送信、
遅いBrowser Sourceを待つ処理はUIスレッドで行いません。万一入力上限へ達した場合は、
個別イベントを欠落させたままにせず、UI側のレイヤーカタログと完全なCanvasItem状態を1つの
documentとして世代付きで置換し、全接続をsnapshot再同期します。完全履歴の複製はこの異常復旧時だけ発生します。

OBSプロジェクターを開く単発のobs-websocket処理もworker threadで実行します。各要求は世代IDと
UI／worker共通の5秒絶対deadlineを持ち、接続・認証・モニター取得・起動要求の各段階でtimeoutを
作り直しません。worker結果はprocess内channelへ入れ、payloadを持たないWin32 messageでUIを
起こします。UIは現在pendingの世代だけを適用するため、timeout後の再試行へ旧要求の成功／失敗を
誤適用せず、終了後の結果も破棄します。

## ローカルハブ

ハブは専用ランタイム上で全コマンドを直列処理します。これにより、接続処理と描画イベント
の順序が一意になり、snapshotと増分イベントの競合を避けます。

- 新規接続: 現在の1〜8枚のレイヤーカタログと全CanvasItemをsnapshotとして最初に送信
- 通常描画: `stroke_*`、`shape_*`、`stamp_add`、legacy互換の`stamp_move_*`、
  `item_transform_*`、`layer_add`、`layer_delete`、`undo`、`redo`、`clear` を増分配信
- 全消去、レイヤー内容消去、レイヤー削除のUndo: 現在のレイヤーカタログと全CanvasItemを
  完全snapshot 1件で置換し、接続queueを項目数だけ消費せずにローカル表示とBrowser Sourceを再同期。
  live state・一括削除の復元点・Redo待ち項目の合計が通常上限を超える前に、最も古い復元境界以前の
  履歴を失効させ、部分復元と履歴メモリの膨張を防止
- 再接続: 古いクライアント状態をsnapshotで全置換
- backpressure: 各接続は256メッセージの上限を持つ
- 遅延時: 接続をハブから除外し、Browser Source側の再接続に任せる

ローカルサーバーの起動・停止とBrowser Source WebSocket session数は、認証情報を含まない
プロセス内diagnosticsとして保持します。状態が変わった時だけWin32メッセージを設定画面へ送り、
定期的なHTTP pollingは行いません。トレイメニューは開いた時点の同じsnapshotを表示します。
診断用の公開HTTP endpointやHost / Origin検証の例外は追加しません。

CanvasItemは合計500個、ストローク点は合計200,000点・1本10,000点に制限します。古い確定
アイテムを削除する必要が生じた場合は増分ではなくsnapshotを送り、全クライアントを再同期
します。増分イベントにはrevisionを付け、欠落を検出したoverlayは再接続snapshotで復旧します。

ローカルDirect2DとBrowser SourceのCanvas 2Dは、レイヤーごとの完成cacheを持ち、通常の確定操作では
新しい1項目を所属レイヤーだけへ追記して最大8枚のbitmapを再合成します。全履歴の再構築はUndo、
レイヤー内容消去、レイヤー削除、Clear、上限トリム、再接続snapshot、キャンバスのリサイズ時に
限定します。
図形／スタンプのtransform中はネイティブ側の対象レイヤーだけで対象より前の履歴をprefixとしてcacheし、
対象と同じレイヤーの後続履歴だけを元の順序で即時再合成します。他レイヤーは完成bitmapのままです。最新状態だけを
16ms間隔（約60fps）の`item_transform_preview`として送り、Browser Source側も開始時に同じprefixを
offscreen cacheへ構築して、同じレイヤーの対象以降だけを同一canvasへ履歴順に再合成します。後続eraserがprefixにも
作用するため、previewと確定後の前後関係・alpha合成は一致します。同一描画フレーム内の更新は最新状態へ
畳み込み、確定状態は`item_transform_commit`としてポインタを離した時点で即時送信します。

`WM_POINTER`のpointer IDはmessage処理中だけWin32 APIへ渡し、取得したscalar metadataだけを
platform非依存のengineへ渡します。mouse、touch、pen、touchpadを分類し、penの有効なmaskが示す
筆圧・傾きだけを正規化します。pointとtool別brush tuningをprotocolへ含めるため、Direct2Dと
Browser Sourceはplatform固有状態を参照せず同じgeometry式を再現できます。

## Web assets

`client/` はOBS用透明ページだけを生成します。ビルド成果物は固定名の
`index.html`、`index.css`、`index.js` で、Rust release build時にexeへ埋め込みます。
実行時に外部ファイルやCDNは必要ありません。

ユーザー登録スタンプだけは管理対象PNGとして`%APPDATA%\StreamPainter\stamps`へ保存し、
許可されたIDを`/stamps/<id>`から配信します。任意パスをHTTP routeへ渡すことはありません。

第三者ライセンスページも生成時に依存関係とlockfileを照合し、Rust本体へ埋め込みます。
タスクトレイから開くと、同じloopbackサーバーの `/licenses` で表示されます。

## 終了

アプリ終了時はローカルサーバーのfutureをキャンセルし、WebSocketを含む接続を閉じてから
専用スレッドをjoinします。OBS側はページが残っていれば3秒のgrace後に古い描画を透明化して
再接続を続け、アプリ再起動後に新しいsnapshotを受け取ります。
