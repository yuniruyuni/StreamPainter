# Changelog

このプロジェクトの主な変更を記録します。形式は
[Keep a Changelog](https://keepachangelog.com/ja/1.1.0/)を参考にし、バージョン番号は
[Semantic Versioning](https://semver.org/lang/ja/)に従います。

## [Unreleased]

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

[Unreleased]: https://github.com/yuniruyuni/StreamPainter/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/yuniruyuni/StreamPainter/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/yuniruyuni/StreamPainter/compare/v0.3.3...v0.4.0
[0.3.3]: https://github.com/yuniruyuni/StreamPainter/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/yuniruyuni/StreamPainter/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/yuniruyuni/StreamPainter/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/yuniruyuni/StreamPainter/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/yuniruyuni/StreamPainter/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/yuniruyuni/StreamPainter/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/yuniruyuni/StreamPainter/releases/tag/v0.1.0
