# Changelog

このプロジェクトの主な変更を記録します。形式は
[Keep a Changelog](https://keepachangelog.com/ja/1.1.0/)を参考にし、バージョン番号は
[Semantic Versioning](https://semver.org/lang/ja/)に従います。

## [Unreleased]

### Added

- 設定画面の「アップデートを確認」ボタンによる手動の自己アップデート。GitHub Releaseの最新版を
  確認し、確認したバージョンをダウンロード・検証（GitHub公開digestとの照合）した上で実行ファイルを
  置き換える。確認・適用ともユーザーがボタンを押した時だけ通信し、バックグラウンドでの自動確認や
  無確認での自動適用は行わない。適用後の再起動は既存の正常終了経路を経由する
- 現在レイヤーの内容だけを、レイヤーカタログを残して消去する操作。ラジアルパネル、従来メニュー、
  描画モード中の`Delete`キーから確認なしで実行でき、Undo／Redoで内容を復元・再消去可能

### Changed

- レイヤーの追加・削除をUndo／Redo履歴へ統合。レイヤー削除時の確認画面と履歴破棄を廃止し、
  元のレイヤー順・項目順を完全snapshotで、選択レイヤーをネイティブ側で復元

## [0.8.0] - 2026-08-05

### Added

- 最大8枚の描画レイヤーをラジアルパネルまたは従来メニューから追加・選択・削除する機能。
  消しゴムと選択は現在レイヤーだけを対象にし、Undo／Redoは全レイヤーの操作順を維持
- 全消去を1回の履歴操作としてUndo／Redoし、復元時はbounded queueを圧迫しない完全snapshotで
  Browser Sourceを再同期。復元点を含む履歴メモリも既存の項目数・点数上限内に制限
- 全消去前の確認画面を設定から有効・無効にできる機能（既定は有効）。空のキャンバスでは
  設定にかかわらず確認を省略

### Changed

- ローカルWebSocket protocolをversion 8へ更新し、snapshotのレイヤーカタログ、CanvasItemの
  `layerId`、`layer_add`／`layer_delete`を追加。version 6/7の欠落レイヤー情報は既定レイヤーへ移行
- Windows／Browser Sourceの描画をレイヤー別cacheへ変更し、active strokeとtransform中も
  別レイヤーや選択対象より前の長い履歴を毎フレーム再生しない構成へ更新
- レイヤー削除時は描画またはUndo／Redo履歴があれば不可逆確認を表示し、削除後は安全のため
  履歴を破棄。全消去後もレイヤーカタログは維持
- 描画モード切替ホットキーの入力欄に実キー入力の方法と`F18`／`Ctrl+M`の例を表示

### Fixed

- 空のキャンバスで全消去を選んでも、待機中のRedo履歴を破棄しないよう修正

## [0.7.3] - 2026-08-03

### Changed

- 物理ペンvalidatorが各マーカーストローク終了時に座標／IDを含まない進捗と不足条件を表示し、
  timeout時にも最後のpoint数・筆圧範囲・X/Y傾き範囲を報告。2026-08-03にWindows 11 Pro上の
  StreamPainter 0.7.2とWacom Intuos Pro Lでprotocol 7の実機検証を行い、2本のストロークが
  completed/qualified 2/2、合計155 pointsを満たし、configが変更されないことを確認

## [0.7.2] - 2026-08-03

### Changed

- SignPath Foundation申請のreputation・本人同意要件と、GitHub App／独立environment reviewerを
  導入する条件をCode signing policyへ明記

## [0.7.1] - 2026-08-03

### Added

- 物理ペンの筆圧・X/Y傾きを設定非変更で検証し、device/driver/app情報を秘匿化して記録する
  Windows PowerShell 5.1／PowerShell 7対応の手動validator

### Changed

- GitHubのrepository-level immutable releasesを有効化し、今後公開するReleaseのtag／asset保護と
  release attestation自動生成を適用

## [0.7.0] - 2026-08-03

### Added

- 選択ツールで最前面の図形／スタンプを選び、専用ハンドルとカーソルで移動・縦横比固定の
  拡大縮小・回転を行い、`Escape`で操作前へ戻せるtransform機能
- 最新値へ集約した約60fpsのBrowser Sourceプレビューと、1ドラッグを1回として扱うUndo／Redo

### Changed

- transform中も元の描画順と後続の半透明stroke／eraserを維持し、線幅・矢印headを含むvisible inkを
  キャンバス内へ収めながら最小表示寸法と縦横比を保持
- ローカルWebSocketプロトコルを図形／スタンプtransform対応のversion 7へ更新。version 6 snapshotを
  移行可能とし、更新後はOBS Browser Sourceの「現在のページを再読み込み」を1回実行する必要あり

## [0.6.1] - 2026-08-03

### Added

- SignPath Foundation承認後にだけ有効化するfail-closedなWindowsコード署名scaffoldと、
  Cargo versionから生成するWindows VERSIONINFO

### Changed

- Windows Release workflowをbuild、署名またはunsigned確定、publishへ分離。外部承認前は
  `SIGNPATH_ENABLED`を設定せず、従来どおりunsigned配布を維持

### Fixed

- PowerShell 7で期待したSignTool失敗の終了コードが署名helper test成功後にも残り、
  Windows workflowを誤って失敗させる問題を修正

## [0.6.0] - 2026-08-03

### Added

- 設定画面から現在のWindowsユーザーだけを対象に自動起動をopt-inできる機能。実際の
  Registry状態を表示し、portable exeの移動・削除・不正な起動引数を検出して修復または解除可能
- Windows Pointer APIでペンの筆圧をペン／マーカーへ、傾きの大きさをマーカーへ反映し、
  能力を報告しないdeviceでは従来の一定幅を維持（物理ペン／固有driverの互換性確認は継続中）

### Changed

- ローカルWebSocketプロトコルを筆圧・傾きとtool別brush tuningを含むversion 6へ更新し、
  Direct2DとBrowser Sourceで同じ線幅計算を使用。v0.5.xから更新した後は、OBS Browser Sourceの
  「現在のページを再読み込み」を1回実行する必要あり

### Fixed

- 実OBS smoke workflowでoverlayを起動PID・class・title・monitor矩形・可視状態から特定し、
  検出失敗時にはsession／desktopと全top-level windowの詳細をartifactへ保存
- Windowsの実入力streamへ非結合mouse eventを注入して連続したpointer updateを検証し、
  描画snapshotの種類・完了状態・色・始終点をartifactへ保存

## [0.5.1] - 2026-08-03

### Fixed

- OBSプロジェクター起動要求へ世代IDと接続から表示確認まで共通の5秒deadlineを導入し、
  timeout後の再試行へ旧要求の成功・失敗が誤適用される競合を解消

## [0.5.0] - 2026-08-03

### Added

- OBS Browser Source URLをタスクトレイと設定画面からコピーし、ローカルサーバーの到達状態と
  Browser Source WebSocket接続数を別々に確認できる診断表示
- 描画モード切替のグローバルホットキーを設定画面でcapture・解除・既定値へ復元する機能。
  登録競合時は以前のキーまたはトレイ操作を維持し、保存と再登録を一体でrollback

### Changed

- Windowsネイティブ描画をGPU上のactive scratchと確定segment cursorで増分化し、連続入力を
  低優先度の`WM_PAINT` 1フレームへ集約。10,000点strokeでも1更新が全点数に比例しない構成へ変更
- 半透明マーカーをBrowser Sourceと同じくstroke単位のopacityで合成し、ローカル表示の見た目を統一

## [0.4.0] - 2026-08-03

### Added

- OBS WebSocketパスワードをWindows資格情報マネージャーへ移し、既存の平文設定とbackupを
  初回起動時に安全に移行
- RustとTypeScriptの全描画メッセージ、状態遷移、上限処理を共有fixtureで照合する
  protocol conformanceテスト

### Fixed

- letterbox／pillarbox時に描画内容がキャンバス外へはみ出す問題を修正し、選択枠や
  ラジアルメニューなどの操作UIは引き続き黒帯上にも表示

## [0.3.3] - 2026-08-03

### Changed

- Browser Sourceで半透明マーカーの履歴を再構築するとき、画面サイズの合成用Canvasを再利用

### Fixed

- ローカルhubの入力キュー復旧後、未送信ストロークの先頭点が重複する問題を修正
- 一時的に読み込めなかったスタンプ画像を、上限付き指数backoffで再試行
- 画面端や小さい解像度でもラジアルメニューの全項目を表示・操作可能に調整
- Browser SourceのWebSocket切断が3秒を超えた場合、古い描画を透明化して再接続後に復元
- Windowsの設定保存失敗時にも、直前の設定と既存backupを保持

## [0.3.2] - 2026-08-03

### Changed

- Release tagがmainへ反映済みのcommitだけを指すことを、build前に検証

### Fixed

- ペンとタッチなど複数ポインターの入力が混線し、別ポインターで描画が確定する問題を修正
- pointer cancelやcapture喪失時に、描画中のストローク／図形を安全に破棄
- Browser SourceのWebSocket接続直後に失敗した場合も指数backoffを維持

## [0.3.1] - 2026-08-02

### Changed

- Rust、Bun、TypeScript、Tokio、TOMLなどの開発・実行時依存関係を更新
- GitHub Actionsのcheckoutとartifact uploadをNode.js 24対応版へ更新

## [0.3.0] - 2026-08-01

### Added

- 配置済みスタンプを選択枠付きでドラッグ再配置する選択ツールと、移動操作のUndo／Redo
- スタンプ移動を約60fpsでOBSへ追従させ、移動中はスタンプだけを別レイヤーで更新し、確定位置を
  即時同期するローカルWebSocketプロトコルv5

## [0.2.0] - 2026-08-01

### Added

- 右ホールドでツール、色、サムネイル付きスタンプを選べるラジアルメニュー。短い右クリックで
  固定して左右クリックで選ぶ操作、中央の標準メニューボタン、円外のUndo／Redo／全消去ドック
  にも対応

## [0.1.1] - 2026-08-01

### Added

- Undoしたストローク・図形・スタンプを復元するRedo
- 日次ローテーションする診断ログと、タスクトレイからログフォルダーを開く機能
- 通常常駐プロセスの多重起動防止
- 全消去前の確認画面

### Changed

- 確定項目をネイティブ／OBS側の描画レイヤーへ増分合成し、通常描画時の全履歴再構築を削減
- OBS Browser Sourceの描画処理を上限付きキューへ集約
- ローカルWebSocketプロトコルをrevision付きsnapshotとRedoに対応したv4へ更新

### Fixed

- 設定保存を一時ファイルからの置換とバックアップ復旧に変更
- 登録スタンプのRGBA展開後メモリ総量に上限を追加
- マウス入力のブラシ幅が設定値より細くなる問題を修正
- ローカルハブの入力飽和時に完全snapshotから復旧
- F9が他アプリと競合してもタスクトレイ操作で起動を継続
- Preview指定失敗時に意図せずProgramプロジェクターを開く挙動を廃止
- GPUデバイス喪失後にキャンバス履歴から描画資源を復旧
- モニターの解像度・配置・接続変更へ実行中に追従
- OBSプロジェクター、オーバーレイ、StreamPainterのメニュー／ダイアログ間の前後関係を安定化

## [0.1.0] - 2026-07-31

- StreamPainterとしての最初の公開リリース
- Windows透明オーバーレイ、ローカルOBS Browser Source、描画ツール、PNGスタンプ、
  OBSプロジェクター連携、設定画面、第三者ライセンス表示を収録

[Unreleased]: https://github.com/yuniruyuni/StreamPainter/compare/v0.8.0...HEAD
[0.8.0]: https://github.com/yuniruyuni/StreamPainter/compare/v0.7.3...v0.8.0
[0.7.3]: https://github.com/yuniruyuni/StreamPainter/compare/v0.7.2...v0.7.3
[0.7.2]: https://github.com/yuniruyuni/StreamPainter/compare/v0.7.1...v0.7.2
[0.7.1]: https://github.com/yuniruyuni/StreamPainter/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/yuniruyuni/StreamPainter/compare/v0.6.1...v0.7.0
[0.6.1]: https://github.com/yuniruyuni/StreamPainter/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/yuniruyuni/StreamPainter/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/yuniruyuni/StreamPainter/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/yuniruyuni/StreamPainter/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/yuniruyuni/StreamPainter/compare/v0.3.3...v0.4.0
[0.3.3]: https://github.com/yuniruyuni/StreamPainter/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/yuniruyuni/StreamPainter/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/yuniruyuni/StreamPainter/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/yuniruyuni/StreamPainter/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/yuniruyuni/StreamPainter/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/yuniruyuni/StreamPainter/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/yuniruyuni/StreamPainter/releases/tag/v0.1.0
