# Changelog

このプロジェクトの主な変更を記録します。形式は
[Keep a Changelog](https://keepachangelog.com/ja/1.1.0/)を参考にし、バージョン番号は
[Semantic Versioning](https://semver.org/lang/ja/)に従います。

## [Unreleased]

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

[Unreleased]: https://github.com/yuniruyuni/StreamPainter/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/yuniruyuni/StreamPainter/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/yuniruyuni/StreamPainter/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/yuniruyuni/StreamPainter/releases/tag/v0.1.0
