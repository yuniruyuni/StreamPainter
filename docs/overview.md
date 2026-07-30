# Overview

StreamPainterは、配信PC上でのみ動くWindows常駐アプリです。利用者が描いたストロークを
Windows透明オーバーレイへ即時表示し、同じ状態をOBS Browser Sourceへ配信します。

## 正式な製品境界

- 対応OS: Windows
- Webサービス: `stream-painter.exe` が `127.0.0.1` に内蔵
- OBS表示: 同一PC上のBrowser Source
- 永続化: なし。ストロークはプロセス内メモリのみ
- アカウント・認証・DB・クラウド: なし
- インターネット接続: 不要

この構成ではクラウドの常駐インスタンスやWebSocket接続に対する利用料金は発生しません。

## 利用フロー

1. Windowsログイン後または配信前に `stream-painter.exe` を起動する。
2. アプリが `http://127.0.0.1:16873/overlay` を配信する。
3. OBS Browser SourceがページとWebSocketを同じloopback originから読み込む。
4. `F9` で描画モードへ入り、入力イベントをストロークへ変換する。
5. ローカルエコーとOBS overlayが同じイベント列を描画する。
6. Browser Sourceが再接続した場合は、接続時snapshotで現在状態を復元する。

アプリより先にOBSが起動した場合はページを取得できないため、アプリを先に起動するか、
OBSのBrowser Sourceを更新してください。
