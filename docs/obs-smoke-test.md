# Real OBS Windows smoke test

`.github/workflows/obs-smoke.yml` は、モックではなく公式の OBS Studio と配布用
`stream-painter.exe` を同じ Windows VM で起動し、ローカル連携を週次・手動で検証します。
通常の push / pull request では起動せず、branch protection の必須checkにも含めません。

## RunnerとGUI要件

最初の運用先は GitHub-hosted `windows-2025` x64 runnerです。ジョブごとに新しいVMを得られ、
GitHubの標準runnerとして環境を再現しやすく、self-hosted runnerの継続的な保守も不要です。
GitHubはWindows runnerをAzure VMとして提供し、管理者権限やCPU・メモリ等は文書化していますが、
公式runner仕様には物理ディスプレイ、GPU、ウィンドウのforeground制御、DirectCompositionや
CEFの描画成功に関する保証は記載されていません。runner imageも定期更新されます。そのため次の
条件をすべて満たすことを前提にしたGUI smoke testは、安定性を確認できるまでschedule /
workflow_dispatch専用です。

- 対話desktop sessionで `SendInput` 相当のF9・マウス入力がウィンドウへ届く
- Direct3D / DirectCompositionでStreamPainterの透明ウィンドウを初期化できる
- OBSのBrowser Source（CEF）が描画できる
- OBS Program projectorを仮想モニター上へ作成できる

同じrunner由来のGUI基盤エラーが継続する場合は、PR checkへ昇格させず、auto-logonした対話
sessionとGPUまたはWARP要件を固定できる一時的なWindows VMのself-hosted runnerへ移します。
Windowsサービスとして起動したsession 0 runnerではdesktop入力を検証できないため不適格です。

参考資料:

- [GitHub-hosted runners reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
- [GitHub-hosted runner images](https://github.com/actions/runner-images)

## 固定するOBS配布物

2026-08-03時点の公式stable releaseを、URLだけでなくSHA-256までworkflowに固定しています。
`latest` URLやパッケージマネージャーは使いません。

| 項目 | 固定値 |
| --- | --- |
| Version | `32.2.1` |
| Asset | `OBS-Studio-32.2.1-Windows-x64.zip` |
| SHA-256 | `db64a2934f8261f85b1410b84be011207a0afda5400d008289f1f1e211bcc7de` |
| Release | [OBS Studio 32.2.1](https://github.com/obsproject/obs-studio/releases/tag/32.2.1) |

ダウンロード後は展開前にSHA-256を照合し、portable modeでrunner内の一時ディレクトリだけを
使用します。OBS更新時は公式Releaseの非draft・非prereleaseとchecksumを再確認し、workflow、
helper test、この文書のversion / URL / digestを同じcommitで変更します。

OBSのportable modeと起動引数は
[OBS 32.2.1の起動処理](https://github.com/obsproject/obs-studio/blob/32.2.1/frontend/obs-main.cpp)、
WebSocketの起動引数と設定形式は
[obs-websocket Config.cpp](https://github.com/obsproject/obs-websocket/blob/master/src/Config.cpp)を
根拠にしています。smoke testではruntimeごとに使い捨てのランダムパスワードを生成し、
`--websocket_ipv4_only`も指定します。ログartifactへ同じ文字列が現れた場合は収集時に除去します。
OBSの`--safe-mode`はWebSocketも無効にするため使用せず、同梱pluginだけを許可する
`--only-bundled-plugins`を使用します。

## 検証する実経路

`scripts/windows/run-obs-smoke.ps1` は次の順序で検証します。

1. cleanなrunner profileへ通常のStreamPainter設定を書き、release executableを起動する。
2. `http://127.0.0.1:16873/health` が `200 ok` を返すことを確認する。
3. 起動したStreamPainterのPIDが所有し、class / title / 対象monitor矩形 / 可視状態が一致する
   実overlay windowだけを選び、F9で描画モードにしてWin32入力注入で中央へピンク色の線を描く。
4. loopback WebSocketへ正しいOriginで接続し、完了済みstrokeを含むsnapshotを確認する。
5. **この後で初めてOBSを起動する。** 公式zip内のobs-websocketへ認証し、Sceneと実際の
   `browser_source` inputを作成して `http://127.0.0.1:16873/overlay` を読み込ませる。
6. obs-websocketの `SaveSourceScreenshot` でBrowser Source自身の640x360 PNGを保存し、
   `#ff4d6d`に近い画素数、横方向の広がり、中央位置を検証する。
7. `OpenVideoMixProjector` を送り、要求前にはなかったOBSプロセス所有の全画面windowが対象
   monitorを覆うことをWin32で確認する。

OBSはstroke生成後に接続するため、手順6の線はliveな増分イベントでは受け取れません。
Browser SourceのHTTPページ、CEF内WebSocket接続、初回snapshot decode、Canvas描画、OBSのsource
renderingをすべて通過した場合だけ画素検証が成功します。単にWebSocket APIが応答しただけ、
透明PNGが保存できただけ、という見せかけの成功にはしません。

obs-websocket requestの意味は公式の
[protocol specification](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md)
を参照してください。

## Timeout、cleanup、診断

- workflow全体は40分、実OBS scriptは10分、各起動・接続・描画確認にも個別deadlineを持つ
- 成否にかかわらず、起動したOBSとStreamPainterのPIDを起点に子processごと終了する
- logsを収集した後、展開したOBS、archive、テスト専用StreamPainter設定を明示したpathから削除する
- `always()`で診断artifactを14日間保持する
- OBS portable logs、StreamPainter logs、標準出力・標準エラー、進捗、Browser Source PNGと
  画素統計を収集する
- projector表示後または失敗時にdesktop screenshotを試み、desktop capture自体が使えない場合も
  そのエラーをartifactへ残す
- 失敗時はrunnerのsession / window station / desktopと、現在のdesktop上にある全top-level windowの
  PID、process名、session、class、title、style、矩形、可視・最小化状態を
  `window-diagnostics.txt`へ保存する

main scriptはdesktopや既存設定を誤操作しないよう、`GITHUB_ACTIONS=true`でない環境では実行を
拒否します。ローカルでは副作用のない次のhelper testだけを実行してください。

```powershell
./scripts/windows/test-obs-smoke-lib.ps1
```
