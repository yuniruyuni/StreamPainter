//! OBS Browser Source 向けの loopback HTTP/WebSocket サーバー。
//!
//! UI スレッドから受け取った描画イベントを専用 tokio スレッド上のハブで直列化し、
//! `127.0.0.1` だけに配信する。新しい WebSocket 接続には必ずハブの snapshot を送り、
//! 遅延した接続は切断して再接続時の snapshot で回復させる。

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{bail, Context, Result};
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, HOST, ORIGIN, REFERRER_POLICY,
    X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use rust_embed::RustEmbed;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::config::{StampConfig, MAX_STAMP_FILE_BYTES};
use crate::protocol::{
    CanvasItem, OverlayClientMessage, OverlayControlMessage, PainterMessage, Stroke, MAX_ITEMS,
    MAX_STROKE_POINTS, MAX_TOTAL_POINTS, PROTOCOL_VERSION,
};

const SUBSCRIBER_QUEUE_CAPACITY: usize = 256;
const THIRD_PARTY_LICENSES_HTML: &str = include_str!("../../assets/third-party-licenses.html");

#[derive(RustEmbed)]
#[folder = "../client/static/"]
#[exclude = ".gitkeep"]
struct OverlayAssets;

/// Win32 UI スレッドからローカルハブへ描画イベントを渡すハンドル。
pub struct LocalServerHandle {
    hub: HubHandle,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    overlay_url: String,
    licenses_url: String,
}

impl LocalServerHandle {
    pub fn send(&self, message: PainterMessage) {
        if self.hub.tx.send(HubCommand::Apply(message)).is_err() {
            warn!("local overlay hub is not running");
        }
    }

    pub fn send_all(&self, messages: Vec<PainterMessage>) {
        for message in messages {
            self.send(message);
        }
    }

    pub fn overlay_url(&self) -> &str {
        &self.overlay_url
    }

    pub fn licenses_url(&self) -> &str {
        &self.licenses_url
    }
}

impl Drop for LocalServerHandle {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// アセットを検証してから同期的にポートを確保するため、ポート競合は起動時に報告される。
pub fn spawn(port: u16, stamps: &[StampConfig]) -> Result<LocalServerHandle> {
    if port == 0 {
        bail!("local_server_port に 0 は指定できません");
    }
    if OverlayAssets::get("index.html").is_none() {
        bail!(
            "OBS overlay assets がありません。リポジトリ直下で `bun install && bun run build` を実行してください"
        );
    }

    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let listener = TcpListener::bind(address).with_context(|| {
        format!(
            "ローカルWebサーバーのポート {port} を使用できません。設定画面からポートを変更してください (起動できない場合: stream-painter.exe --settings)"
        )
    })?;
    listener
        .set_nonblocking(true)
        .context("failed to configure local listener")?;

    let stamp_paths: HashMap<String, PathBuf> = stamps
        .iter()
        .filter_map(|stamp| match crate::config::stamp_path(&stamp.id) {
            Ok(path) => match std::fs::symlink_metadata(&path) {
                Ok(metadata)
                    if metadata.is_file()
                        && !metadata.file_type().is_symlink()
                        && metadata.len() <= MAX_STAMP_FILE_BYTES =>
                {
                    Some((stamp.id.clone(), path))
                }
                _ => {
                    warn!("stamp asset is missing or invalid: {}", path.display());
                    None
                }
            },
            Err(error) => {
                warn!("invalid stamp asset {}: {error:#}", stamp.id);
                None
            }
        })
        .collect();

    let (hub_tx, hub_rx) = mpsc::unbounded_channel();
    let hub = HubHandle { tx: hub_tx };
    let web_state = WebState {
        hub: hub.clone(),
        port,
        stamp_paths: Arc::new(stamp_paths),
    };
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    let thread = std::thread::Builder::new()
        .name("local-web".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    warn!("failed to build local web runtime: {error}");
                    return;
                }
            };
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(listener) => listener,
                    Err(error) => {
                        warn!("failed to start local listener: {error}");
                        return;
                    }
                };
                let hub_task = tokio::spawn(run_hub(hub_rx));
                let app = router(web_state);
                tokio::select! {
                    result = async { axum::serve(listener, app).await } => {
                        if let Err(error) = result {
                            warn!("local web server stopped: {error}");
                        }
                    }
                    _ = shutdown_rx => {}
                }
                hub_task.abort();
            });
        })
        .context("failed to spawn local web thread")?;

    let overlay_url = format!("http://127.0.0.1:{port}/overlay");
    let licenses_url = format!("http://127.0.0.1:{port}/licenses");
    info!("OBS Browser Source URL: {overlay_url}");
    Ok(LocalServerHandle {
        hub,
        shutdown: Some(shutdown_tx),
        thread: Some(thread),
        overlay_url,
        licenses_url,
    })
}

#[derive(Clone)]
struct WebState {
    hub: HubHandle,
    port: u16,
    stamp_paths: Arc<HashMap<String, PathBuf>>,
}

fn router(state: WebState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/overlay", get(index))
        .route("/licenses", get(licenses))
        .route("/health", get(health))
        .route("/ws", get(websocket))
        .route("/stamps/{stamp_id}", get(stamp))
        .route("/{*path}", get(asset))
        .with_state(state)
}

async fn index(State(state): State<WebState>, headers: HeaderMap) -> Response {
    if !trusted_host(&headers, state.port) {
        return StatusCode::FORBIDDEN.into_response();
    }
    embedded_response("index.html", state.port)
}

async fn health(State(state): State<WebState>, headers: HeaderMap) -> Response {
    if !trusted_host(&headers, state.port) {
        return StatusCode::FORBIDDEN.into_response();
    }
    plain_response(StatusCode::OK, "text/plain; charset=utf-8", "ok")
}

async fn licenses(State(state): State<WebState>, headers: HeaderMap) -> Response {
    if !trusted_host(&headers, state.port) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let csp = "default-src 'none'; style-src 'unsafe-inline'; \
               base-uri 'none'; frame-ancestors 'none'";
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/html; charset=utf-8")
        .header(CACHE_CONTROL, "no-store")
        .header(CONTENT_SECURITY_POLICY, csp)
        .header(X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(REFERRER_POLICY, "no-referrer")
        .body(Body::from(THIRD_PARTY_LICENSES_HTML))
        .expect("valid licenses response")
}

async fn stamp(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(stamp_id): Path<String>,
) -> Response {
    if !trusted_host(&headers, state.port) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(path) = state.stamp_paths.get(&stamp_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            warn!("failed to read stamp {}: {error}", path.display());
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "image/png")
        .header(CACHE_CONTROL, "no-store")
        .header(X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(REFERRER_POLICY, "no-referrer")
        .body(Body::from(bytes))
        .expect("valid stamp response")
}

async fn asset(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(path): Path<String>,
) -> Response {
    if !trusted_host(&headers, state.port) {
        return StatusCode::FORBIDDEN.into_response();
    }
    embedded_response(&path, state.port)
}

async fn websocket(
    State(state): State<WebState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !trusted_host(&headers, state.port) || !trusted_origin(&headers, state.port) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.max_message_size(4 * 1024)
        .max_frame_size(4 * 1024)
        .on_upgrade(move |socket| websocket_session(socket, state.hub))
}

fn trusted_host(headers: &HeaderMap, port: u16) -> bool {
    let Some(host) = headers.get(HOST).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let expected_ip = format!("127.0.0.1:{port}");
    let expected_name = format!("localhost:{port}");
    host.eq_ignore_ascii_case(&expected_ip) || host.eq_ignore_ascii_case(&expected_name)
}

fn trusted_origin(headers: &HeaderMap, port: u16) -> bool {
    let Some(origin) = headers.get(ORIGIN).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let expected_ip = format!("http://127.0.0.1:{port}");
    let expected_name = format!("http://localhost:{port}");
    origin.eq_ignore_ascii_case(&expected_ip) || origin.eq_ignore_ascii_case(&expected_name)
}

fn embedded_response(path: &str, port: u16) -> Response {
    let Some(asset) = OverlayAssets::get(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    };
    let csp = format!(
        "default-src 'none'; script-src 'self'; style-src 'self'; \
         connect-src ws://127.0.0.1:{port} ws://localhost:{port}; img-src 'self'; \
         base-uri 'none'; frame-ancestors 'self'"
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, content_type)
        .header(CACHE_CONTROL, "no-store")
        .header(CONTENT_SECURITY_POLICY, csp)
        .header(X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(REFERRER_POLICY, "no-referrer")
        .body(Body::from(asset.data.into_owned()))
        .expect("valid embedded asset response")
}

fn plain_response(status: StatusCode, content_type: &'static str, body: &'static str) -> Response {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, content_type)
        .header(CACHE_CONTROL, "no-store")
        .header(X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from(body))
        .expect("valid plain response")
}

#[derive(Clone)]
struct HubHandle {
    tx: mpsc::UnboundedSender<HubCommand>,
}

impl HubHandle {
    async fn subscribe(&self) -> Option<(u64, mpsc::Receiver<String>)> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(HubCommand::Subscribe { reply: reply_tx })
            .ok()?;
        reply_rx.await.ok()
    }

    fn unsubscribe(&self, id: u64) {
        let _ = self.tx.send(HubCommand::Unsubscribe { id });
    }
}

enum HubCommand {
    Apply(PainterMessage),
    Subscribe {
        reply: oneshot::Sender<(u64, mpsc::Receiver<String>)>,
    },
    Unsubscribe {
        id: u64,
    },
}

struct Subscriber {
    id: u64,
    tx: mpsc::Sender<String>,
}

#[derive(Default)]
struct HubState {
    items: Vec<CanvasItem>,
    revision: u64,
    total_points: usize,
}

impl HubState {
    fn apply(&mut self, message: PainterMessage) -> Option<String> {
        let (outbound, force_snapshot) = match message {
            PainterMessage::StrokeBegin { stroke_id, brush } => {
                if self.items.iter().any(|item| item.item_id() == stroke_id) {
                    return None;
                }
                self.items.push(CanvasItem::Stroke {
                    stroke: Stroke {
                        stroke_id: stroke_id.clone(),
                        brush: brush.clone(),
                        pts: Vec::new(),
                        done: false,
                        ended_at: None,
                    },
                });
                (PainterMessage::StrokeBegin { stroke_id, brush }, false)
            }
            PainterMessage::StrokePoints { stroke_id, pts } => {
                let stroke = self.items.iter_mut().find_map(|item| match item {
                    CanvasItem::Stroke { stroke }
                        if stroke.stroke_id == stroke_id && !stroke.done =>
                    {
                        Some(stroke)
                    }
                    _ => None,
                })?;
                let available = MAX_STROKE_POINTS.saturating_sub(stroke.pts.len());
                let accepted: Vec<_> = pts.into_iter().take(available).collect();
                if accepted.is_empty() {
                    return None;
                }
                self.total_points += accepted.len();
                stroke.pts.extend_from_slice(&accepted);
                (
                    PainterMessage::StrokePoints {
                        stroke_id,
                        pts: accepted,
                    },
                    false,
                )
            }
            PainterMessage::StrokeEnd {
                stroke_id,
                ended_at,
            } => {
                let stroke = self.items.iter_mut().find_map(|item| match item {
                    CanvasItem::Stroke { stroke }
                        if stroke.stroke_id == stroke_id && !stroke.done =>
                    {
                        Some(stroke)
                    }
                    _ => None,
                })?;
                stroke.done = true;
                stroke.ended_at = Some(ended_at);
                (
                    PainterMessage::StrokeEnd {
                        stroke_id,
                        ended_at,
                    },
                    false,
                )
            }
            PainterMessage::StrokeCancel { stroke_id } => {
                let index = self.items.iter().position(|item| {
                    matches!(
                        item,
                        CanvasItem::Stroke { stroke }
                            if stroke.stroke_id == stroke_id && !stroke.done
                    )
                })?;
                let removed = self.items.remove(index);
                self.total_points = self.total_points.saturating_sub(removed.point_count());
                (PainterMessage::StrokeCancel { stroke_id }, false)
            }
            PainterMessage::ShapeBegin { mut shape } => {
                if self
                    .items
                    .iter()
                    .any(|item| item.item_id() == shape.item_id)
                {
                    return None;
                }
                shape.done = false;
                shape.ended_at = None;
                self.items.push(CanvasItem::Shape {
                    shape: shape.clone(),
                });
                (PainterMessage::ShapeBegin { shape }, false)
            }
            PainterMessage::ShapeUpdate { item_id, end } => {
                let shape = self.items.iter_mut().find_map(|item| match item {
                    CanvasItem::Shape { shape } if shape.item_id == item_id && !shape.done => {
                        Some(shape)
                    }
                    _ => None,
                })?;
                shape.end = end;
                (PainterMessage::ShapeUpdate { item_id, end }, false)
            }
            PainterMessage::ShapeEnd { item_id, ended_at } => {
                let shape = self.items.iter_mut().find_map(|item| match item {
                    CanvasItem::Shape { shape } if shape.item_id == item_id && !shape.done => {
                        Some(shape)
                    }
                    _ => None,
                })?;
                shape.done = true;
                shape.ended_at = Some(ended_at);
                (PainterMessage::ShapeEnd { item_id, ended_at }, false)
            }
            PainterMessage::ShapeCancel { item_id } => {
                let index = self.items.iter().position(|item| {
                    matches!(
                        item,
                        CanvasItem::Shape { shape }
                            if shape.item_id == item_id && !shape.done
                    )
                })?;
                self.items.remove(index);
                (PainterMessage::ShapeCancel { item_id }, false)
            }
            PainterMessage::StampAdd { mut stamp } => {
                if self
                    .items
                    .iter()
                    .any(|item| item.item_id() == stamp.item_id)
                {
                    return None;
                }
                stamp.done = true;
                stamp.ended_at?;
                self.items.push(CanvasItem::Stamp {
                    stamp: stamp.clone(),
                });
                (PainterMessage::StampAdd { stamp }, false)
            }
            PainterMessage::Undo {} => {
                let index = self.items.iter().rposition(CanvasItem::is_done)?;
                let removed = self.items.remove(index);
                let removed_non_stroke = !matches!(&removed, CanvasItem::Stroke { .. });
                self.total_points = self.total_points.saturating_sub(removed.point_count());
                // v1 client の stroke 履歴を誤って 1 本戻さないよう snapshot を送る。
                (PainterMessage::Undo {}, removed_non_stroke)
            }
            PainterMessage::Clear {} => {
                if self.items.is_empty() {
                    return None;
                }
                self.items.clear();
                self.total_points = 0;
                (PainterMessage::Clear {}, false)
            }
        };

        let trimmed = self.trim();
        self.revision = self.revision.saturating_add(1);
        if trimmed || force_snapshot {
            self.snapshot()
        } else {
            serde_json::to_string(&outbound).ok()
        }
    }

    fn snapshot(&self) -> Option<String> {
        let strokes = self
            .items
            .iter()
            .filter_map(CanvasItem::as_stroke)
            .cloned()
            .collect();
        serde_json::to_string(&OverlayControlMessage::Snapshot {
            protocol_version: PROTOCOL_VERSION,
            rev: self.revision,
            fade_after_ms: None,
            strokes,
            items: self.items.clone(),
        })
        .ok()
    }

    fn trim(&mut self) -> bool {
        let mut trimmed = false;
        while self.items.len() > MAX_ITEMS {
            let Some(index) = self.items.iter().position(CanvasItem::is_done) else {
                break;
            };
            let removed = self.items.remove(index);
            self.total_points = self.total_points.saturating_sub(removed.point_count());
            trimmed = true;
        }
        while self.total_points > MAX_TOTAL_POINTS {
            let Some(index) = self.items.iter().position(CanvasItem::is_done) else {
                break;
            };
            let removed = self.items.remove(index);
            self.total_points = self.total_points.saturating_sub(removed.point_count());
            trimmed = true;
        }
        trimmed
    }
}

async fn run_hub(mut commands: mpsc::UnboundedReceiver<HubCommand>) {
    let mut state = HubState::default();
    let mut subscribers: Vec<Subscriber> = Vec::new();
    let mut next_subscriber_id = 1_u64;

    while let Some(command) = commands.recv().await {
        match command {
            HubCommand::Apply(message) => {
                let Some(text) = state.apply(message) else {
                    continue;
                };
                subscribers.retain(|subscriber| subscriber.tx.try_send(text.clone()).is_ok());
            }
            HubCommand::Subscribe { reply } => {
                let Some(snapshot) = state.snapshot() else {
                    continue;
                };
                let (tx, rx) = mpsc::channel(SUBSCRIBER_QUEUE_CAPACITY);
                if tx.try_send(snapshot).is_err() {
                    continue;
                }
                let id = next_subscriber_id;
                next_subscriber_id = next_subscriber_id.saturating_add(1);
                if reply.send((id, rx)).is_ok() {
                    subscribers.push(Subscriber { id, tx });
                }
            }
            HubCommand::Unsubscribe { id } => {
                subscribers.retain(|subscriber| subscriber.id != id);
            }
        }
    }
}

async fn websocket_session(socket: WebSocket, hub: HubHandle) {
    let Some((subscriber_id, mut outbound)) = hub.subscribe().await else {
        return;
    };
    let (mut socket_tx, mut socket_rx) = socket.split();

    loop {
        tokio::select! {
            message = outbound.recv() => {
                let Some(message) = message else { break };
                if socket_tx.send(Message::Text(message.into())).await.is_err() {
                    break;
                }
            }
            frame = socket_rx.next() => {
                match frame {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(OverlayClientMessage::Ping { t }) =
                            serde_json::from_str::<OverlayClientMessage>(&text)
                        {
                            let Ok(pong) = serde_json::to_string(&OverlayControlMessage::Pong { t }) else {
                                continue;
                            };
                            if socket_tx.send(Message::Text(pong.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if socket_tx.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
    hub.unsubscribe(subscriber_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Brush, LineStyle, ShapeItem, ShapeKind, StampItem, Tool};
    use std::io::{Read, Write};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::header::ORIGIN as WS_ORIGIN;
    use tokio_tungstenite::tungstenite::http::HeaderValue;
    use tokio_tungstenite::tungstenite::Message as ClientMessage;

    fn brush() -> Brush {
        Brush {
            tool: Tool::Pen,
            color: "#ff4d6d".into(),
            opacity: 1.0,
            width_n: 0.005,
            pressure_width: true,
        }
    }

    fn test_hub() -> HubHandle {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run_hub(rx));
        HubHandle { tx }
    }

    fn available_port() -> u16 {
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[tokio::test]
    async fn new_subscriber_receives_current_snapshot() {
        let hub = test_hub();
        hub.tx
            .send(HubCommand::Apply(PainterMessage::StrokeBegin {
                stroke_id: "s1".into(),
                brush: brush(),
            }))
            .unwrap();
        hub.tx
            .send(HubCommand::Apply(PainterMessage::StrokePoints {
                stroke_id: "s1".into(),
                pts: vec![(0.1, 0.2, 0.5, 0.0)],
            }))
            .unwrap();
        hub.tx
            .send(HubCommand::Apply(PainterMessage::StrokeEnd {
                stroke_id: "s1".into(),
                ended_at: 1234.0,
            }))
            .unwrap();

        let (_, mut receiver) = hub.subscribe().await.unwrap();
        let snapshot = receiver.recv().await.unwrap();
        let message: OverlayControlMessage = serde_json::from_str(&snapshot).unwrap();
        match message {
            OverlayControlMessage::Snapshot {
                rev,
                strokes,
                items,
                ..
            } => {
                assert_eq!(rev, 3);
                assert_eq!(strokes.len(), 1);
                assert!(strokes[0].done);
                assert_eq!(strokes[0].ended_at, Some(1234.0));
                assert_eq!(items.len(), 1);
                assert!(matches!(items[0], CanvasItem::Stroke { .. }));
            }
            OverlayControlMessage::Pong { .. } => panic!("expected snapshot"),
        }
    }

    #[tokio::test]
    async fn subscriber_gets_snapshot_then_incremental_events() {
        let hub = test_hub();
        let (_, mut receiver) = hub.subscribe().await.unwrap();
        let initial = receiver.recv().await.unwrap();
        assert!(initial.contains("\"type\":\"snapshot\""));

        hub.tx
            .send(HubCommand::Apply(PainterMessage::StrokeBegin {
                stroke_id: "s1".into(),
                brush: brush(),
            }))
            .unwrap();
        let event = receiver.recv().await.unwrap();
        assert!(event.contains("\"type\":\"stroke_begin\""));
    }

    #[tokio::test]
    async fn hub_preserves_shape_and_stamp_order_in_v2_snapshot() {
        let hub = test_hub();
        let shape = ShapeItem {
            item_id: "shape-1".into(),
            shape: ShapeKind::Rectangle,
            style: LineStyle {
                color: "#ffffff".into(),
                opacity: 1.0,
                width_n: 0.005,
            },
            start: (0.1, 0.2),
            end: (0.1, 0.2),
            done: false,
            ended_at: None,
        };
        hub.tx
            .send(HubCommand::Apply(PainterMessage::ShapeBegin { shape }))
            .unwrap();
        hub.tx
            .send(HubCommand::Apply(PainterMessage::ShapeUpdate {
                item_id: "shape-1".into(),
                end: (0.8, 0.7),
            }))
            .unwrap();
        hub.tx
            .send(HubCommand::Apply(PainterMessage::ShapeEnd {
                item_id: "shape-1".into(),
                ended_at: 10.0,
            }))
            .unwrap();
        hub.tx
            .send(HubCommand::Apply(PainterMessage::StampAdd {
                stamp: StampItem {
                    item_id: "stamp-item-1".into(),
                    stamp_id: "stamp-1".into(),
                    center: (0.5, 0.5),
                    width_n: 0.1,
                    height_n: 0.2,
                    opacity: 1.0,
                    done: true,
                    ended_at: Some(20.0),
                },
            }))
            .unwrap();

        let (_, mut receiver) = hub.subscribe().await.unwrap();
        let snapshot = receiver.recv().await.unwrap();
        match serde_json::from_str::<OverlayControlMessage>(&snapshot).unwrap() {
            OverlayControlMessage::Snapshot {
                protocol_version,
                strokes,
                items,
                ..
            } => {
                assert_eq!(protocol_version, PROTOCOL_VERSION);
                assert!(strokes.is_empty());
                assert!(matches!(items[0], CanvasItem::Shape { .. }));
                assert!(matches!(items[1], CanvasItem::Stamp { .. }));
            }
            OverlayControlMessage::Pong { .. } => panic!("expected snapshot"),
        }
    }

    #[test]
    fn only_expected_loopback_authorities_are_trusted() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, "127.0.0.1:16873".parse().unwrap());
        headers.insert(ORIGIN, "http://127.0.0.1:16873".parse().unwrap());
        assert!(trusted_host(&headers, 16_873));
        assert!(trusted_origin(&headers, 16_873));

        headers.insert(HOST, "attacker.example".parse().unwrap());
        headers.insert(ORIGIN, "https://attacker.example".parse().unwrap());
        assert!(!trusted_host(&headers, 16_873));
        assert!(!trusted_origin(&headers, 16_873));
    }

    #[tokio::test]
    async fn stamp_handler_only_serves_catalogued_files_to_trusted_hosts() {
        let path =
            std::env::temp_dir().join(format!("stream-painter-{}.png", uuid::Uuid::now_v7()));
        std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
        let mut paths = HashMap::new();
        paths.insert("stamp-1".to_owned(), path.clone());
        let state = WebState {
            hub: test_hub(),
            port: 16_873,
            stamp_paths: Arc::new(paths),
        };
        let mut headers = HeaderMap::new();
        headers.insert(HOST, "127.0.0.1:16873".parse().unwrap());

        let response = stamp(
            State(state.clone()),
            headers.clone(),
            Path("stamp-1".to_owned()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "image/png");

        let missing = stamp(
            State(state.clone()),
            headers,
            Path("not-registered".to_owned()),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let mut foreign = HeaderMap::new();
        foreign.insert(HOST, "attacker.example".parse().unwrap());
        let forbidden = stamp(State(state), foreign, Path("stamp-1".to_owned())).await;
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn release_input_contains_overlay_assets() {
        assert!(OverlayAssets::get("index.html").is_some());
        assert!(OverlayAssets::get("index.js").is_some());
        assert!(OverlayAssets::get("index.css").is_some());
        assert!(THIRD_PARTY_LICENSES_HTML.contains("<title>StreamPainter ライセンス</title>"));
    }

    #[tokio::test]
    async fn running_server_streams_events_and_rejects_foreign_origins() {
        let port = available_port();
        let server = spawn(port, &[]).unwrap();
        let url = format!("ws://127.0.0.1:{port}/ws");

        let mut http = std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        http.write_all(
            format!("GET /overlay HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
        let mut response = String::new();
        http.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("content-security-policy:"));
        assert!(response.contains("<title>StreamPainter overlay</title>"));

        let mut licenses = std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        licenses
            .write_all(
                format!(
                    "GET /licenses HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .unwrap();
        let mut licenses_response = String::new();
        licenses.read_to_string(&mut licenses_response).unwrap();
        assert!(licenses_response.starts_with("HTTP/1.1 200 OK"));
        assert!(licenses_response.contains("frame-ancestors 'none'"));
        assert!(licenses_response.contains("<title>StreamPainter ライセンス</title>"));

        let mut foreign = url.clone().into_client_request().unwrap();
        foreign.headers_mut().insert(
            WS_ORIGIN,
            HeaderValue::from_static("https://attacker.example"),
        );
        let error = tokio_tungstenite::connect_async(foreign)
            .await
            .expect_err("foreign origin must be rejected");
        assert!(error.to_string().contains("403"));

        let mut request = url.into_client_request().unwrap();
        request.headers_mut().insert(
            WS_ORIGIN,
            HeaderValue::from_str(&format!("http://127.0.0.1:{port}")).unwrap(),
        );
        let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();

        let initial = socket.next().await.unwrap().unwrap();
        assert!(initial
            .into_text()
            .unwrap()
            .contains("\"type\":\"snapshot\""));

        server.send(PainterMessage::StrokeBegin {
            stroke_id: "integration".into(),
            brush: brush(),
        });
        let event = socket.next().await.unwrap().unwrap();
        assert!(event
            .into_text()
            .unwrap()
            .contains("\"type\":\"stroke_begin\""));

        socket
            .send(ClientMessage::Text(
                r#"{"type":"ping","t":42}"#.to_string().into(),
            ))
            .await
            .unwrap();
        let pong = socket.next().await.unwrap().unwrap();
        assert_eq!(pong.into_text().unwrap(), r#"{"type":"pong","t":42.0}"#);

        socket.close(None).await.unwrap();
        drop(server);
    }
}
