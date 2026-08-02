//! obs-websocket (v5) クライアント。
//!
//! 設定した描画モード切替キーで OBS の全画面プロジェクターを開くための単発リクエスト用で、
//! 常時接続は持たない (接続 → 認証 → GetMonitorList → OpenVideoMixProjector → 切断)。
//! モニタは index ではなく座標・サイズでマッチングする (OBS 側と当方で採番が異なるため)。

// Linux ホストではコンパイル検証のみ行うため未使用警告を抑制する
#![cfg_attr(not(windows), allow(dead_code))]

use std::{fmt, time::Duration};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::Message;

const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProjectorView {
    Program,
    Preview,
}

#[derive(Clone)]
pub struct ObsSettings {
    pub url: String,
    pub password: String,
    pub view: ProjectorView,
}

impl fmt::Debug for ObsSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObsSettings")
            .field("url", &self.url)
            .field("password", &"[REDACTED]")
            .field("view", &self.view)
            .finish()
    }
}

/// 対象モニタ (物理ピクセル) に全画面プロジェクターを開く。
/// UI スレッドをブロックしないよう、呼び出し側が専用スレッドで呼ぶこと
pub fn open_projector(
    settings: &ObsSettings,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(open_async(settings, x, y, width, height))
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = SplitSink<WsStream, Message>;
type WsSource = futures_util::stream::SplitStream<WsStream>;

async fn open_async(settings: &ObsSettings, x: i32, y: i32, width: i32, height: i32) -> Result<()> {
    let (ws, _) = tokio::time::timeout(TIMEOUT, tokio_tungstenite::connect_async(&settings.url))
        .await
        .context("OBS WebSocket への接続がタイムアウトしました")?
        .context(
            "OBS WebSocket へ接続できません (OBS の ツール → WebSocket サーバー設定 を確認)",
        )?;
    let (mut sink, mut stream) = ws.split();

    // Hello (op 0) → Identify (op 1) → Identified (op 2)
    let hello = recv_op(&mut stream, 0).await?;
    let mut identify = json!({ "rpcVersion": 1, "eventSubscriptions": 0 });
    if let Some(auth) = hello.get("authentication") {
        let salt = auth["salt"].as_str().unwrap_or_default();
        let challenge = auth["challenge"].as_str().unwrap_or_default();
        if settings.password.is_empty() {
            bail!("OBS WebSocket にパスワードが設定されています (タスクトレイの「設定...」からパスワードを設定)");
        }
        identify["authentication"] = compute_auth(&settings.password, salt, challenge).into();
    }
    send(&mut sink, 1, identify).await?;
    recv_op(&mut stream, 2)
        .await
        .context("OBS WebSocket の認証に失敗しました (パスワードを確認)")?;

    // OBS 側のモニタ一覧から座標・サイズ一致で monitorIndex を特定する
    let monitors = request(&mut sink, &mut stream, "GetMonitorList", json!({}), "1").await?;
    let index = monitors["monitors"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| {
            m["monitorPositionX"].as_i64() == Some(x as i64)
                && m["monitorPositionY"].as_i64() == Some(y as i64)
                && m["monitorWidth"].as_i64() == Some(width as i64)
                && m["monitorHeight"].as_i64() == Some(height as i64)
        })
        .and_then(|m| m["monitorIndex"].as_i64())
        .ok_or_else(|| anyhow!("OBS 側に対象モニタ ({x},{y} {width}x{height}) が見つかりません"))?;

    let open = |view: ProjectorView| {
        json!({
            "videoMixType": match view {
                ProjectorView::Program => "OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM",
                ProjectorView::Preview => "OBS_WEBSOCKET_VIDEO_MIX_TYPE_PREVIEW",
            },
            "monitorIndex": index,
        })
    };
    request(
        &mut sink,
        &mut stream,
        "OpenVideoMixProjector",
        open(settings.view),
        "2",
    )
    .await
    .with_context(|| match settings.view {
        ProjectorView::Program => "OBSのProgramプロジェクターを開けませんでした",
        ProjectorView::Preview => {
            "OBSのPreviewプロジェクターを開けませんでした (OBSのスタジオモードを確認)"
        }
    })?;
    Ok(())
}

async fn send(sink: &mut WsSink, op: u32, d: Value) -> Result<()> {
    let msg = json!({ "op": op, "d": d });
    sink.send(Message::Text(msg.to_string().into())).await?;
    Ok(())
}

/// 指定 op のメッセージを受信する (イベント等は読み捨てる)
async fn recv_op(stream: &mut WsSource, op: u64) -> Result<Value> {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            match stream.next().await {
                Some(Ok(Message::Text(text))) => {
                    let v: Value = serde_json::from_str(&text)?;
                    if v["op"].as_u64() == Some(op) {
                        return Ok(v["d"].clone());
                    }
                }
                Some(Ok(Message::Close(_))) | None => {
                    bail!("OBS WebSocket が切断されました")
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
            }
        }
    })
    .await
    .context("OBS WebSocket の応答がタイムアウトしました")?
}

/// Request (op 6) を送り RequestResponse (op 7) を待つ
async fn request(
    sink: &mut WsSink,
    stream: &mut WsSource,
    request_type: &str,
    data: Value,
    id: &str,
) -> Result<Value> {
    send(
        sink,
        6,
        json!({ "requestType": request_type, "requestId": id, "requestData": data }),
    )
    .await?;
    loop {
        let d = recv_op(stream, 7).await?;
        if d["requestId"].as_str() != Some(id) {
            continue;
        }
        let status = &d["requestStatus"];
        if status["result"].as_bool() != Some(true) {
            bail!(
                "{request_type} が失敗しました (code={}, {})",
                status["code"],
                status["comment"].as_str().unwrap_or("")
            );
        }
        return Ok(d["responseData"].clone());
    }
}

/// obs-websocket v5 の認証文字列:
/// base64(sha256(base64(sha256(password + salt)) + challenge))
fn compute_auth(password: &str, salt: &str, challenge: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD;
    let secret = b64.encode(Sha256::digest(format!("{password}{salt}")));
    b64.encode(Sha256::digest(format!("{secret}{challenge}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_redacts_password() {
        let settings = ObsSettings {
            url: "ws://localhost:4455".into(),
            password: "never-log-this-password".into(),
            view: ProjectorView::Program,
        };
        let debug = format!("{settings:?}");
        assert!(!debug.contains("never-log-this-password"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn auth_string_matches_reference() {
        // obs-websocket ドキュメントの式に基づく既知値検証
        let auth = compute_auth(
            "supersecretpassword",
            "PZVbYpvAnZut2SS6JNJytDm9",
            "ztTBnnuqrqaKDzRM3xcVdbYm",
        );
        // 手計算基準値: secret = b64(sha256("supersecretpasswordPZVbYpvAnZut2SS6JNJytDm9"))
        let b64 = base64::engine::general_purpose::STANDARD;
        let secret = b64.encode(Sha256::digest(
            "supersecretpasswordPZVbYpvAnZut2SS6JNJytDm9",
        ));
        let expected = b64.encode(Sha256::digest(format!("{secret}ztTBnnuqrqaKDzRM3xcVdbYm")));
        assert_eq!(auth, expected);
        assert_eq!(auth.len(), 44); // sha256 の base64 長
    }
}
