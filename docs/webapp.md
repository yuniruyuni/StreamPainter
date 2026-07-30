# OBS Browser Source overlay

`client/` はOBS Browser Source専用です。管理画面、ログイン、チャンネル、テスト描画UI、
一般公開ページは持ちません。

## Runtime

ページは2枚のCanvas 2Dを画面全体に重ねます。

- `baked`: 確定ストロークを焼き込む下層
- `active`: 描画中ストロークを表示する上層

WebSocketイベントはいったんqueueへ積み、`requestAnimationFrame`ごとにまとめて描画します。
Reactはマウントと接続ライフサイクルにだけ使い、ストローク受信ごとのReact renderは
行いません。

ペンとマーカーはストロークごとのscratch canvasへ不透明に描画してからopacity付きで合成し、
線の重なり部分だけが濃くなることを防ぎます。消しゴムは`destination-out`でbaked層へ作用します。

## Reconnect

WebSocket切断は画面上へエラーUIを出さず、自動再接続します。接続後のsnapshotで全状態を
置換するため、切断中の増分イベントを個別に再送する必要はありません。

## Build and embedding

```bash
bun run check
bun run build
```

成果物は `client/static/index.html`、`index.css`、`index.js` です。ディレクトリは生成物の
ためGit管理せず、Rustのrelease build時に `rust-embed` がexeへ取り込みます。CDNや外部
JavaScriptは使用しません。
