# OBS Browser Source overlay

`client/` はOBS Browser Source専用です。管理画面、ログイン、チャンネル、テスト描画UI、
一般公開ページは持ちません。

## Runtime

ページは2枚のCanvas 2Dを画面全体に重ねます。

- `baked`: 確定したストローク・図形・スタンプを描画順に焼き込む下層
- `active`: 描画中ストロークと図形プレビューを表示する上層

WebSocketイベントはいったん上限128件のqueueへ積み、`requestAnimationFrame`ごとにまとめて
描画します。同じストロークの連続更新と図形previewは最新1件へ集約し、上限を超える場合は
最新CanvasItem状態からの1回の再構築へ置換します。このため、OBSが非表示のBrowser Sourceの
フレーム更新を抑制しても、描画待ちqueueは無制限に増えません。Reactはマウントと接続
ライフサイクルにだけ使い、ストローク受信ごとのReact renderは行いません。

ペンとマーカーはストロークごとのscratch canvasへ不透明に描画してからopacity付きで合成し、
線の重なり部分だけが濃くなることを防ぎます。消しゴムは`destination-out`でbaked層へ作用します。
直線・矢印・四角形・楕円はCanvas 2D primitivesで描き、スタンプPNGは同一originから遅延取得
して画像キャッシュへ保持します。画像のロード完了時は現在のCanvasItem履歴を再構築します。

protocol version 6の各stroke pointは`[u,v,pressure,dt,tiltX,tiltY]`です。Browser Sourceは
`pressureWidth`／`pressureMin`と`tiltWidth`／`tiltMaxScale`を含むbrushを受け取り、
[protocol](protocol.md)に定義したDirect2D側と同じ式でsegment幅を計算します。欠落能力のfallbackは
server側ですでに`pressure=1`、傾き0として直列化され、非有限値や範囲外値もrenderer側で防御します。
versionが一致しないsnapshotは描画せず再接続するため、古い4要素pointを新しいrendererが誤解して
部分描画することはありません。

## Reconnect

WebSocket切断は画面上へエラーUIを出さず、自動再接続します。接続後のsnapshotで全状態を
置換するため、切断中の増分イベントを個別に再送する必要はありません。

切断直後は一時的なネットワーク揺らぎで表示がちらつかないよう、最後の描画を3秒間だけ維持
します。このgrace期間内に対応snapshotを受理できれば表示を消さず、その内容へ置換します。
3秒を超えて切断が続く場合は、StreamPainterの終了・クラッシュ時に古い注釈を配信へ残さない
ことを優先し、描画待ちqueueと手元の状態を破棄してbaked／active両Canvasを透明化します。
その後も再接続は継続し、アプリ再起動後にsnapshotを受理すると全描画を復元します。

## Build and embedding

```bash
bun run check
bun run build
```

成果物は `client/static/index.html`、`index.css`、`index.js` です。ディレクトリは生成物の
ためGit管理せず、Rustのrelease build時に `rust-embed` がexeへ取り込みます。CDNや外部
JavaScriptは使用しません。
