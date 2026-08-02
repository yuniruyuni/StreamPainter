//! OBS Browser Source 向けの loopback HTTP/WebSocket サーバー。
//!
//! UI スレッドから受け取った描画イベントを専用 tokio スレッド上のハブで直列化し、
//! `127.0.0.1` だけに配信する。新しい WebSocket 接続には必ずハブの snapshot を送り、
//! 遅延した接続は切断して再接続時の snapshot で回復させる。

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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
use crate::engine::canvas_engine::SharedItems;
use crate::protocol::{
    CanvasItem, OverlayClientMessage, OverlayControlMessage, OverlayEvent, PainterMessage, Stroke,
    MAX_ITEMS, MAX_STROKE_POINTS, MAX_TOTAL_POINTS, PROTOCOL_VERSION,
};

const SUBSCRIBER_QUEUE_CAPACITY: usize = 256;
const HUB_INPUT_QUEUE_CAPACITY: usize = 1024;
const THIRD_PARTY_LICENSES_HTML: &str = include_str!("../../assets/third-party-licenses.html");

#[derive(RustEmbed)]
#[folder = "../client/static/"]
#[exclude = ".gitkeep"]
struct OverlayAssets;

/// Win32 UI スレッドからローカルハブへ描画イベントを渡すハンドル。
pub struct LocalServerHandle {
    hub: HubHandle,
    source_items: SharedItems,
    recovery: Arc<HubRecovery>,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    overlay_url: String,
    licenses_url: String,
}

impl LocalServerHandle {
    /// `true` は同じバッチの後続イベントを送ってよいことを表す。
    fn enqueue(&self, message: PainterMessage) -> bool {
        let generation = self.recovery.generation.load(Ordering::Acquire);
        match self.hub.tx.try_send(HubCommand::Apply {
            generation,
            message,
        }) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Closed(_)) => {
                warn!("local overlay hub is not running");
                false
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                // イベントを1件だけ落とすとハブの状態が恒久的に壊れる。完全状態を退避し、
                // ハブ側で古い世代の待機イベントを無視してsnapshotへ置換する。
                let generation = self.recovery.generation.fetch_add(1, Ordering::AcqRel) + 1;
                let items = self.source_items.lock().unwrap().clone();
                *self.recovery.snapshot.lock().unwrap() = Some((generation, items));
                warn!(
                    "local overlay hub input queue was full; scheduled snapshot recovery generation {generation}"
                );
                false
            }
        }
    }

    pub fn send_all(&self, messages: Vec<PainterMessage>) {
        for message in messages {
            if !self.enqueue(message) {
                break;
            }
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
pub fn spawn(
    port: u16,
    stamps: &[StampConfig],
    source_items: SharedItems,
) -> Result<LocalServerHandle> {
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

    let (hub_tx, hub_rx) = mpsc::channel(HUB_INPUT_QUEUE_CAPACITY);
    let hub = HubHandle { tx: hub_tx };
    let recovery = Arc::new(HubRecovery::default());
    let web_state = WebState {
        hub: hub.clone(),
        port,
        stamp_paths: Arc::new(stamp_paths),
    };
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let recovery_for_thread = Arc::clone(&recovery);

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
                let hub_task = tokio::spawn(run_hub(hub_rx, Arc::clone(&recovery_for_thread)));
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
        source_items,
        recovery,
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
    tx: mpsc::Sender<HubCommand>,
}

impl HubHandle {
    async fn subscribe(&self) -> Option<(u64, mpsc::Receiver<String>)> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(HubCommand::Subscribe { reply: reply_tx })
            .await
            .ok()?;
        reply_rx.await.ok()
    }

    fn unsubscribe(&self, id: u64) {
        let _ = self.tx.try_send(HubCommand::Unsubscribe { id });
    }
}

enum HubCommand {
    Apply {
        generation: u64,
        message: PainterMessage,
    },
    Subscribe {
        reply: oneshot::Sender<(u64, mpsc::Receiver<String>)>,
    },
    Unsubscribe {
        id: u64,
    },
}

#[derive(Default)]
struct HubRecovery {
    generation: AtomicU64,
    snapshot: Mutex<Option<(u64, Vec<CanvasItem>)>>,
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
            PainterMessage::StrokePoints {
                stroke_id,
                offset,
                pts,
            } => {
                let stroke = self.items.iter_mut().find_map(|item| match item {
                    CanvasItem::Stroke { stroke }
                        if stroke.stroke_id == stroke_id && !stroke.done =>
                    {
                        Some(stroke)
                    }
                    _ => None,
                })?;
                // snapshot は source canvas の未来側まで含む場合がある。絶対offsetで
                // 既に含まれるprefixを照合し、未適用のsuffixだけを追記する。
                if offset > stroke.pts.len() {
                    return None;
                }
                let overlap = (stroke.pts.len() - offset).min(pts.len());
                if stroke.pts[offset..offset + overlap] != pts[..overlap] {
                    return None;
                }
                let available = MAX_STROKE_POINTS.saturating_sub(stroke.pts.len());
                let accepted: Vec<_> = pts.into_iter().skip(overlap).take(available).collect();
                if accepted.is_empty() {
                    return None;
                }
                let accepted_offset = offset + overlap;
                self.total_points += accepted.len();
                stroke.pts.extend_from_slice(&accepted);
                (
                    PainterMessage::StrokePoints {
                        stroke_id,
                        offset: accepted_offset,
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
            PainterMessage::StampMovePreview { item_id, center } => {
                let stamp = self.items.iter_mut().find_map(|item| match item {
                    CanvasItem::Stamp { stamp } if stamp.done && stamp.item_id == item_id => {
                        Some(stamp)
                    }
                    _ => None,
                })?;
                stamp.center = center;
                (PainterMessage::StampMovePreview { item_id, center }, false)
            }
            PainterMessage::StampMove { item_id, center } => {
                let stamp = self.items.iter_mut().find_map(|item| match item {
                    CanvasItem::Stamp { stamp } if stamp.done && stamp.item_id == item_id => {
                        Some(stamp)
                    }
                    _ => None,
                })?;
                stamp.center = center;
                (PainterMessage::StampMove { item_id, center }, false)
            }
            PainterMessage::Undo {} => {
                let index = self.items.iter().rposition(CanvasItem::is_done)?;
                let removed = self.items.remove(index);
                let removed_non_stroke = !matches!(&removed, CanvasItem::Stroke { .. });
                self.total_points = self.total_points.saturating_sub(removed.point_count());
                // v1 client の stroke 履歴を誤って 1 本戻さないよう snapshot を送る。
                (PainterMessage::Undo {}, removed_non_stroke)
            }
            PainterMessage::Redo { item } => {
                if !item.is_done()
                    || self
                        .items
                        .iter()
                        .any(|existing| existing.item_id() == item.item_id())
                {
                    return None;
                }
                self.total_points += item.point_count();
                self.items.push(item.clone());
                (PainterMessage::Redo { item }, false)
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
            serde_json::to_string(&OverlayEvent {
                rev: self.revision,
                event: &outbound,
            })
            .ok()
        }
    }

    fn snapshot(&self) -> Option<String> {
        serde_json::to_string(&OverlayControlMessage::Snapshot {
            protocol_version: PROTOCOL_VERSION,
            rev: self.revision,
            fade_after_ms: None,
            items: self.items.clone(),
        })
        .ok()
    }

    fn replace_items(&mut self, items: Vec<CanvasItem>) -> Option<String> {
        self.total_points = items.iter().map(CanvasItem::point_count).sum();
        self.items = items;
        self.trim();
        self.revision = self.revision.saturating_add(1);
        self.snapshot()
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

async fn run_hub(mut commands: mpsc::Receiver<HubCommand>, recovery: Arc<HubRecovery>) {
    let mut state = HubState::default();
    let mut subscribers: Vec<Subscriber> = Vec::new();
    let mut next_subscriber_id = 1_u64;
    let mut generation = 0_u64;

    while let Some(command) = commands.recv().await {
        let pending_recovery = recovery.snapshot.lock().unwrap().take();
        if let Some((recovery_generation, items)) = pending_recovery {
            if recovery_generation >= generation {
                generation = recovery_generation;
                if let Some(snapshot) = state.replace_items(items) {
                    subscribers
                        .retain(|subscriber| subscriber.tx.try_send(snapshot.clone()).is_ok());
                }
            }
        }
        match command {
            HubCommand::Apply {
                generation: message_generation,
                message,
            } => {
                if message_generation != generation {
                    continue;
                }
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
mod protocol_conformance {
    use super::*;
    use crate::protocol::{
        canonical_overlay_control_messages, canonical_painter_messages, Brush, LineStyle,
        ShapeItem, ShapeKind, StampItem, Tool, MAX_POINTS_PER_MESSAGE,
    };
    use serde_json::{json, Value};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    const FIXTURE_REVISION: u64 = 40;
    const UPDATE_FIXTURE_ENV: &str = "UPDATE_PROTOCOL_FIXTURES";

    fn brush(tool: Tool) -> Brush {
        Brush {
            tool,
            color: "#4455aa".into(),
            opacity: 0.8,
            width_n: 0.0075,
            pressure_width: true,
        }
    }

    fn stroke_item(id: &str, tool: Tool, done: bool, point_count: usize) -> CanvasItem {
        CanvasItem::Stroke {
            stroke: Stroke {
                stroke_id: id.into(),
                brush: brush(tool),
                pts: (0..point_count)
                    .map(|index| (0.1, 0.2, 0.5, index as f64))
                    .collect(),
                done,
                ended_at: done.then_some(1_700_000_000_100.0),
            },
        }
    }

    fn shape_item(id: &str, kind: ShapeKind, done: bool) -> CanvasItem {
        CanvasItem::Shape {
            shape: ShapeItem {
                item_id: id.into(),
                shape: kind,
                style: LineStyle {
                    color: "#aabbcc".into(),
                    opacity: 0.7,
                    width_n: 0.01,
                },
                start: (0.15, 0.25),
                end: (0.35, 0.45),
                done,
                ended_at: done.then_some(1_700_000_000_200.0),
            },
        }
    }

    fn stamp_item(id: &str, done: bool) -> CanvasItem {
        CanvasItem::Stamp {
            stamp: StampItem {
                item_id: id.into(),
                stamp_id: "fixture-stamp".into(),
                center: (0.45, 0.55),
                width_n: 0.1,
                height_n: 0.2,
                opacity: 0.9,
                done,
                ended_at: done.then_some(1_700_000_000_300.0),
            },
        }
    }

    fn hub_state(items: Vec<CanvasItem>) -> HubState {
        let total_points = items.iter().map(CanvasItem::point_count).sum();
        HubState {
            items,
            revision: FIXTURE_REVISION,
            total_points,
        }
    }

    // この exhaustive match が、enum macro に追加されたfixtureを各状態遷移へ
    // 必ず接続させる。新variantのsetupを追加し忘れるとRustのコンパイルが失敗する。
    fn initial_items(message: &PainterMessage) -> Vec<CanvasItem> {
        match message {
            PainterMessage::StrokeBegin { .. } => {
                vec![stamp_item("fixture-preserved-stamp", true)]
            }
            PainterMessage::StrokePoints { stroke_id, .. } => {
                vec![stroke_item(stroke_id, Tool::Pen, false, 1)]
            }
            PainterMessage::StrokeEnd { stroke_id, .. } => {
                vec![stroke_item(stroke_id, Tool::Marker, false, 2)]
            }
            PainterMessage::StrokeCancel { stroke_id } => {
                vec![stroke_item(stroke_id, Tool::Eraser, false, 2)]
            }
            PainterMessage::ShapeBegin { .. } => {
                vec![stroke_item("fixture-preserved-stroke", Tool::Pen, true, 1)]
            }
            PainterMessage::ShapeUpdate { item_id, .. }
            | PainterMessage::ShapeEnd { item_id, .. }
            | PainterMessage::ShapeCancel { item_id } => {
                vec![shape_item(item_id, ShapeKind::Rectangle, false)]
            }
            PainterMessage::StampAdd { .. } => {
                vec![shape_item("fixture-preserved-shape", ShapeKind::Line, true)]
            }
            PainterMessage::StampMovePreview { item_id, .. }
            | PainterMessage::StampMove { item_id, .. } => {
                vec![stamp_item(item_id, true)]
            }
            PainterMessage::Undo {} => vec![
                stroke_item("fixture-undo-target", Tool::Pen, true, 2),
                shape_item("fixture-undo-active", ShapeKind::Ellipse, false),
            ],
            PainterMessage::Redo { .. } => {
                vec![shape_item("fixture-redo-preserved", ShapeKind::Arrow, true)]
            }
            PainterMessage::Clear {} => vec![
                stroke_item("fixture-clear-stroke", Tool::Pen, true, 1),
                shape_item("fixture-clear-shape", ShapeKind::Ellipse, true),
                stamp_item("fixture-clear-stamp", true),
            ],
        }
    }

    fn parse_json(text: &str) -> Value {
        serde_json::from_str(text).expect("hub output must be JSON")
    }

    fn message_type(value: &Value) -> &str {
        value
            .get("type")
            .and_then(Value::as_str)
            .expect("canonical message must have a type")
    }

    fn top_level_fields(value: &Value) -> Vec<String> {
        let mut fields: Vec<_> = value
            .as_object()
            .expect("canonical message must be an object")
            .keys()
            .cloned()
            .collect();
        fields.sort();
        fields
    }

    fn event_case(message: PainterMessage) -> Value {
        let raw = serde_json::to_value(&message).expect("event fixture must serialize");
        let name = message_type(&raw).to_owned();
        let mut state = hub_state(initial_items(&message));
        let initial = parse_json(&state.snapshot().expect("initial snapshot must serialize"));
        let outbound = parse_json(
            &state
                .apply(message)
                .unwrap_or_else(|| panic!("canonical {name} event was rejected by the hub")),
        );
        assert_eq!(message_type(&outbound), name);
        json!({
            "name": name,
            "initial": initial,
            "message": outbound,
            "expected": {
                "rev": state.revision,
                "items": state.items,
            },
        })
    }

    fn revisioned_value(rev: u64, message: &PainterMessage) -> Value {
        serde_json::to_value(OverlayEvent {
            rev,
            event: message,
        })
        .expect("revisioned event must serialize")
    }

    fn state_summary(state: &HubState) -> Value {
        json!({
            "rev": state.revision,
            "itemIds": state.items.iter().map(CanvasItem::item_id).collect::<Vec<_>>(),
            "pointCounts": state.items.iter().map(CanvasItem::point_count).collect::<Vec<_>>(),
            "totalPoints": state.total_points,
        })
    }

    fn apply_for_trim_fixture(state: &mut HubState, messages: Vec<PainterMessage>) -> Vec<Value> {
        messages
            .into_iter()
            .map(|message| {
                let wire = revisioned_value(state.revision + 1, &message);
                state
                    .apply(message)
                    .expect("trim fixture event must be accepted");
                wire
            })
            .collect()
    }

    fn trim_cases() -> Vec<Value> {
        let mut item_state = hub_state(
            (0..MAX_ITEMS)
                .map(|index| stamp_item(&format!("fixture-limit-item-{index:03}"), true))
                .collect(),
        );
        let item_messages = apply_for_trim_fixture(
            &mut item_state,
            vec![PainterMessage::StampAdd {
                stamp: match stamp_item("fixture-limit-item-new", true) {
                    CanvasItem::Stamp { stamp } => stamp,
                    _ => unreachable!(),
                },
            }],
        );

        assert_eq!(MAX_TOTAL_POINTS % MAX_STROKE_POINTS, 0);
        let point_item_count = MAX_TOTAL_POINTS / MAX_STROKE_POINTS;
        let mut total_point_state = hub_state(
            (0..point_item_count)
                .map(|index| {
                    stroke_item(
                        &format!("fixture-limit-stroke-{index:03}"),
                        Tool::Pen,
                        true,
                        MAX_STROKE_POINTS,
                    )
                })
                .collect(),
        );
        let total_point_messages = apply_for_trim_fixture(
            &mut total_point_state,
            vec![
                PainterMessage::StrokeBegin {
                    stroke_id: "fixture-limit-stroke-new".into(),
                    brush: brush(Tool::Pen),
                },
                PainterMessage::StrokePoints {
                    stroke_id: "fixture-limit-stroke-new".into(),
                    offset: 0,
                    pts: vec![(0.8, 0.9, 0.6, 0.0)],
                },
            ],
        );

        let initial_stroke_points = MAX_STROKE_POINTS - 1;
        let mut stroke_point_state = hub_state(vec![stroke_item(
            "fixture-stroke-cap",
            Tool::Marker,
            false,
            initial_stroke_points,
        )]);
        let stroke_point_messages = apply_for_trim_fixture(
            &mut stroke_point_state,
            vec![PainterMessage::StrokePoints {
                stroke_id: "fixture-stroke-cap".into(),
                offset: initial_stroke_points,
                pts: vec![
                    (0.6, 0.7, 0.8, 16.0),
                    (0.7, 0.8, 0.9, 32.0),
                    (0.8, 0.9, 1.0, 48.0),
                ],
            }],
        );

        vec![
            json!({
                "name": "max_items",
                "initial": {
                    "kind": "done_stamps",
                    "revision": FIXTURE_REVISION,
                    "count": MAX_ITEMS,
                    "idPrefix": "fixture-limit-item-",
                },
                "messages": item_messages,
                "expected": state_summary(&item_state),
            }),
            json!({
                "name": "max_total_points",
                "initial": {
                    "kind": "done_strokes",
                    "revision": FIXTURE_REVISION,
                    "count": point_item_count,
                    "pointsPerItem": MAX_STROKE_POINTS,
                    "idPrefix": "fixture-limit-stroke-",
                },
                "messages": total_point_messages,
                "expected": state_summary(&total_point_state),
            }),
            json!({
                "name": "max_stroke_points",
                "initial": {
                    "kind": "active_stroke",
                    "revision": FIXTURE_REVISION,
                    "id": "fixture-stroke-cap",
                    "points": initial_stroke_points,
                },
                "messages": stroke_point_messages,
                "expected": state_summary(&stroke_point_state),
            }),
        ]
    }

    fn revision_cases() -> Vec<Value> {
        let initial_items = vec![stroke_item(
            "fixture-revision-preserved",
            Tool::Pen,
            true,
            1,
        )];
        let state = HubState {
            items: initial_items.clone(),
            revision: 10,
            total_points: 1,
        };
        let initial = parse_json(&state.snapshot().expect("revision snapshot must serialize"));
        let expected = json!({ "rev": 10, "items": initial_items });

        vec![
            json!({
                "name": "missing_revision",
                "initial": initial,
                "message": revisioned_value(12, &PainterMessage::Clear {}),
                "expectedEffect": "resync",
                "expected": expected,
            }),
            json!({
                "name": "duplicate_revision",
                "initial": initial,
                "message": revisioned_value(10, &PainterMessage::Clear {}),
                "expectedEffect": "resync",
                "expected": expected,
            }),
            json!({
                "name": "unknown_protocol_version",
                "initial": initial,
                "message": serde_json::to_value(OverlayControlMessage::Snapshot {
                    protocol_version: PROTOCOL_VERSION + 1,
                    rev: 99,
                    fade_after_ms: Some(1_000.0),
                    items: Vec::new(),
                })
                .expect("unknown-version snapshot must serialize"),
                "expectedEffect": "resync",
                "expected": expected,
            }),
        ]
    }

    fn canonical_fixture() -> Value {
        let event_cases: Vec<_> = canonical_painter_messages()
            .into_iter()
            .map(event_case)
            .collect();
        let control_messages: Vec<Value> = canonical_overlay_control_messages()
            .into_iter()
            .map(|message| serde_json::to_value(message).expect("control fixture must serialize"))
            .collect();

        let mut message_fields = BTreeMap::new();
        for value in control_messages
            .iter()
            .chain(event_cases.iter().map(|case| &case["message"]))
        {
            let name = message_type(value).to_owned();
            assert!(message_fields
                .insert(name, top_level_fields(value))
                .is_none());
        }

        let server_message_types: Vec<_> = control_messages
            .iter()
            .map(message_type)
            .chain(
                event_cases
                    .iter()
                    .map(|case| message_type(&case["message"])),
            )
            .collect();
        let snapshot = control_messages
            .iter()
            .find(|message| message_type(message) == "snapshot")
            .expect("control fixtures must include a snapshot");
        let snapshot_items = &snapshot["items"];
        let enum_values = json!({
            "tools": [Tool::Pen, Tool::Marker, Tool::Eraser],
            "shapeKinds": [
                ShapeKind::Line,
                ShapeKind::Arrow,
                ShapeKind::Rectangle,
                ShapeKind::Ellipse,
            ],
            "canvasKinds": snapshot_items
                .as_array()
                .expect("snapshot items must be an array")
                .iter()
                .map(|item| item["kind"].clone())
                .collect::<Vec<_>>(),
        });

        let ping = json!({ "type": "ping", "t": 1_700_000_001_500.0 });
        serde_json::from_value::<OverlayClientMessage>(ping.clone())
            .expect("client ping fixture must decode in Rust");

        json!({
            "fixtureVersion": 1,
            "protocolVersion": PROTOCOL_VERSION,
            "limits": {
                "maxItems": MAX_ITEMS,
                "maxTotalPoints": MAX_TOTAL_POINTS,
                "maxStrokePoints": MAX_STROKE_POINTS,
                "maxPointsPerMessage": MAX_POINTS_PER_MESSAGE,
            },
            "serverMessageTypes": server_message_types,
            "messageFields": message_fields,
            "objectFields": {
                "brush": top_level_fields(&snapshot_items[0]["brush"]),
                "strokeItem": top_level_fields(&snapshot_items[0]),
                "lineStyle": top_level_fields(&snapshot_items[1]["style"]),
                "shapeItem": top_level_fields(&snapshot_items[1]),
                "stampItem": top_level_fields(&snapshot_items[2]),
            },
            "enumValues": enum_values,
            "controlMessages": control_messages,
            "clientMessages": [ping],
            "eventCases": event_cases,
            "revisionCases": revision_cases(),
            "trimCases": trim_cases(),
        })
    }

    fn fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../protocol-fixtures/canonical.json")
    }

    #[test]
    fn canonical_protocol_fixture_is_current() {
        let path = fixture_path();
        let generated = format!(
            "{}\n",
            serde_json::to_string_pretty(&canonical_fixture())
                .expect("canonical fixture must serialize")
        );
        if std::env::var_os(UPDATE_FIXTURE_ENV).is_some() {
            fs::create_dir_all(path.parent().expect("fixture must have a parent"))
                .expect("fixture directory must be writable");
            fs::write(&path, &generated).expect("fixture must be writable");
        }
        let tracked = fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "failed to read {}: {error}; run `bun run generate:protocol-fixtures`",
                path.display()
            )
        });
        assert_eq!(
            tracked, generated,
            "canonical protocol fixture is stale; run `bun run generate:protocol-fixtures`"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::canvas_engine::CanvasEngine;
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
        let (tx, rx) = mpsc::channel(HUB_INPUT_QUEUE_CAPACITY);
        tokio::spawn(run_hub(rx, Arc::new(HubRecovery::default())));
        HubHandle { tx }
    }

    fn queued_server(
        capacity: usize,
        source_items: SharedItems,
    ) -> (
        LocalServerHandle,
        HubHandle,
        mpsc::Receiver<HubCommand>,
        Arc<HubRecovery>,
    ) {
        let recovery = Arc::new(HubRecovery::default());
        let (tx, rx) = mpsc::channel(capacity);
        let hub = HubHandle { tx };
        let server = LocalServerHandle {
            hub: hub.clone(),
            source_items,
            recovery: Arc::clone(&recovery),
            shutdown: None,
            thread: None,
            overlay_url: String::new(),
            licenses_url: String::new(),
        };
        (server, hub, rx, recovery)
    }

    async fn current_hub_items(hub: &HubHandle) -> Vec<CanvasItem> {
        let (_, mut receiver) = hub.subscribe().await.unwrap();
        let snapshot = receiver.recv().await.unwrap();
        match serde_json::from_str::<OverlayControlMessage>(&snapshot).unwrap() {
            OverlayControlMessage::Snapshot { items, .. } => items,
            OverlayControlMessage::Pong { .. } => panic!("expected snapshot"),
        }
    }

    async fn apply(hub: &HubHandle, message: PainterMessage) {
        hub.tx
            .send(HubCommand::Apply {
                generation: 0,
                message,
            })
            .await
            .unwrap();
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
        apply(
            &hub,
            PainterMessage::StrokeBegin {
                stroke_id: "s1".into(),
                brush: brush(),
            },
        )
        .await;
        apply(
            &hub,
            PainterMessage::StrokePoints {
                stroke_id: "s1".into(),
                offset: 0,
                pts: vec![(0.1, 0.2, 0.5, 0.0)],
            },
        )
        .await;
        apply(
            &hub,
            PainterMessage::StrokeEnd {
                stroke_id: "s1".into(),
                ended_at: 1234.0,
            },
        )
        .await;

        let (_, mut receiver) = hub.subscribe().await.unwrap();
        let snapshot = receiver.recv().await.unwrap();
        assert!(!snapshot.contains("\"strokes\""));
        let message: OverlayControlMessage = serde_json::from_str(&snapshot).unwrap();
        match message {
            OverlayControlMessage::Snapshot { rev, items, .. } => {
                assert_eq!(rev, 3);
                assert_eq!(items.len(), 1);
                match &items[0] {
                    CanvasItem::Stroke { stroke } => {
                        assert!(stroke.done);
                        assert_eq!(stroke.ended_at, Some(1234.0));
                    }
                    _ => panic!("expected stroke"),
                }
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

        apply(
            &hub,
            PainterMessage::StrokeBegin {
                stroke_id: "s1".into(),
                brush: brush(),
            },
        )
        .await;
        let event = receiver.recv().await.unwrap();
        assert!(event.contains("\"type\":\"stroke_begin\""));
        assert!(event.contains("\"rev\":1"));
    }

    #[test]
    fn hub_applies_stroke_points_idempotently_by_absolute_offset() {
        let p0 = (0.1, 0.2, 0.5, 0.0);
        let p1 = (0.2, 0.3, 0.5, 16.0);
        let p2 = (0.3, 0.4, 0.5, 32.0);
        let mut state = HubState::default();
        state.replace_items(vec![CanvasItem::Stroke {
            stroke: Stroke {
                stroke_id: "s1".into(),
                brush: brush(),
                pts: vec![p0, p1],
                done: false,
                ended_at: None,
            },
        }]);

        let event = state
            .apply(PainterMessage::StrokePoints {
                stroke_id: "s1".into(),
                offset: 0,
                pts: vec![p0, p1, p2],
            })
            .unwrap();
        assert!(!event.contains("offset"));
        assert!(event.contains("\"pts\":[[0.3,0.4,0.5,32.0]]"));
        let CanvasItem::Stroke { stroke } = &state.items[0] else {
            panic!("expected stroke");
        };
        assert_eq!(stroke.pts, [p0, p1, p2]);

        let revision = state.revision;
        assert!(state
            .apply(PainterMessage::StrokePoints {
                stroke_id: "s1".into(),
                offset: 0,
                pts: vec![p0, p1, p2],
            })
            .is_none());
        assert!(state
            .apply(PainterMessage::StrokePoints {
                stroke_id: "s1".into(),
                offset: 4,
                pts: vec![(0.5, 0.6, 0.5, 48.0)],
            })
            .is_none());
        assert!(state
            .apply(PainterMessage::StrokePoints {
                stroke_id: "s1".into(),
                offset: 1,
                pts: vec![(0.9, 0.9, 0.5, 16.0)],
            })
            .is_none());
        assert_eq!(state.revision, revision);
        let CanvasItem::Stroke { stroke } = &state.items[0] else {
            panic!("expected stroke");
        };
        assert_eq!(stroke.pts, [p0, p1, p2]);
    }

    #[tokio::test]
    async fn stroke_begin_recovery_does_not_duplicate_the_pending_prefix() {
        let mut engine = CanvasEngine::new();
        let source_items = engine.shared_items();
        let (server, hub, rx, recovery) = queued_server(1, Arc::clone(&source_items));

        // stale commandで容量を埋め、StrokeBeginをsnapshot復旧へ切り替える。
        server.send_all(vec![PainterMessage::Clear {}]);
        let begin = engine.begin(7, brush(), 0.1, 0.2, 0.5, 1000.0);
        server.send_all(begin);
        assert_eq!(recovery.generation.load(Ordering::Acquire), 1);

        tokio::spawn(run_hub(rx, Arc::clone(&recovery)));
        assert_eq!(current_hub_items(&hub).await, *source_items.lock().unwrap());

        // begin時点からpendingに残る先頭点と新規点を同時にflushする。
        engine.move_to(7, 0.3, 0.4, 0.5, 1016.0);
        let flushed = engine.flush();
        assert!(matches!(
            &flushed[..],
            [PainterMessage::StrokePoints { offset: 0, pts, .. }] if pts.len() == 2
        ));
        server.send_all(flushed);

        let actual = current_hub_items(&hub).await;
        let expected = source_items.lock().unwrap().clone();
        assert_eq!(actual, expected);
        let CanvasItem::Stroke { stroke } = &actual[0] else {
            panic!("expected stroke");
        };
        assert_eq!(stroke.pts.len(), 2);
    }

    #[tokio::test]
    async fn recovery_during_chunked_stroke_points_preserves_later_offsets() {
        let mut engine = CanvasEngine::new();
        let source_items = engine.shared_items();
        let begin = engine.begin(7, brush(), 0.0, 0.0, 0.5, 1000.0);
        for index in 1..=600 {
            engine.move_to(7, index as f64 * 0.001, 0.0, 0.5, 1000.0 + index as f64);
        }
        let chunks = engine.flush();
        assert_eq!(chunks.len(), 2);

        let (server, hub, rx, recovery) = queued_server(2, Arc::clone(&source_items));
        server.send_all(begin);
        // begin + 1 chunkで容量を使い切り、2 chunk目でsnapshotへ切り替わる。
        server.send_all(chunks);
        assert_eq!(recovery.generation.load(Ordering::Acquire), 1);

        tokio::spawn(run_hub(rx, Arc::clone(&recovery)));
        assert_eq!(current_hub_items(&hub).await, *source_items.lock().unwrap());

        engine.move_to(7, 0.7, 0.0, 0.5, 1700.0);
        let later = engine.flush();
        assert!(matches!(
            &later[..],
            [PainterMessage::StrokePoints { offset: 601, pts, .. }] if pts.len() == 1
        ));
        server.send_all(later);
        assert_eq!(current_hub_items(&hub).await, *source_items.lock().unwrap());
    }

    #[tokio::test]
    async fn shape_update_queue_recovery_preserves_source_state() {
        let mut engine = CanvasEngine::new();
        let source_items = engine.shared_items();
        let begin = engine.begin_shape(
            7,
            ShapeKind::Rectangle,
            LineStyle {
                color: "#ffffff".into(),
                opacity: 1.0,
                width_n: 0.005,
            },
            0.1,
            0.2,
        );
        let (server, hub, rx, recovery) = queued_server(1, Arc::clone(&source_items));
        server.send_all(begin);
        engine.move_to(7, 0.8, 0.7, 0.5, 1016.0);
        server.send_all(engine.flush());
        assert_eq!(recovery.generation.load(Ordering::Acquire), 1);

        tokio::spawn(run_hub(rx, Arc::clone(&recovery)));
        assert_eq!(current_hub_items(&hub).await, *source_items.lock().unwrap());

        server.send_all(engine.end(7, 1100.0));
        assert_eq!(current_hub_items(&hub).await, *source_items.lock().unwrap());
    }

    #[tokio::test]
    async fn stamp_preview_queue_recovery_preserves_source_state() {
        let mut engine = CanvasEngine::new();
        let source_items = engine.shared_items();
        let add = engine.add_stamp("stamp-1".into(), (0.2, 0.3), 0.1, 0.2, 1.0, 10.0);
        let item_id = engine.stamp_at(0.2, 0.3).unwrap().item_id;
        let (server, hub, rx, recovery) = queued_server(1, Arc::clone(&source_items));
        server.send_all(add);

        assert!(engine.begin_stamp_move(&item_id));
        assert!(engine.preview_stamp_move(&item_id, (0.7, 0.6)));
        server.send_all(engine.flush());
        assert_eq!(recovery.generation.load(Ordering::Acquire), 1);

        tokio::spawn(run_hub(rx, Arc::clone(&recovery)));
        assert_eq!(current_hub_items(&hub).await, *source_items.lock().unwrap());

        server.send_all(engine.end_stamp_move(30.0));
        assert_eq!(current_hub_items(&hub).await, *source_items.lock().unwrap());
    }

    #[tokio::test]
    async fn full_input_queue_recovers_from_the_shared_canvas_snapshot() {
        let source_items = Arc::new(Mutex::new(vec![CanvasItem::Stamp {
            stamp: StampItem {
                item_id: "latest".into(),
                stamp_id: "stamp-1".into(),
                center: (0.5, 0.5),
                width_n: 0.1,
                height_n: 0.2,
                opacity: 1.0,
                done: true,
                ended_at: Some(10.0),
            },
        }]));
        let recovery = Arc::new(HubRecovery::default());
        let (tx, rx) = mpsc::channel(1);
        let hub = HubHandle { tx };
        let server = LocalServerHandle {
            hub: hub.clone(),
            source_items,
            recovery: Arc::clone(&recovery),
            shutdown: None,
            thread: None,
            overlay_url: String::new(),
            licenses_url: String::new(),
        };

        // 1件目で容量を使い切り、2件目は完全状態による復旧へ切り替わる。
        server.send_all(vec![PainterMessage::StrokeBegin {
            stroke_id: "stale".into(),
            brush: brush(),
        }]);
        server.send_all(vec![PainterMessage::Clear {}]);
        assert_eq!(recovery.generation.load(Ordering::Acquire), 1);

        tokio::spawn(run_hub(rx, recovery));
        let (_, mut receiver) = hub.subscribe().await.unwrap();
        let snapshot = receiver.recv().await.unwrap();
        match serde_json::from_str::<OverlayControlMessage>(&snapshot).unwrap() {
            OverlayControlMessage::Snapshot { items, .. } => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].item_id(), "latest");
            }
            OverlayControlMessage::Pong { .. } => panic!("expected snapshot"),
        }
    }

    #[tokio::test]
    async fn hub_preserves_shape_and_stamp_order_in_v5_snapshot() {
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
        apply(&hub, PainterMessage::ShapeBegin { shape }).await;
        apply(
            &hub,
            PainterMessage::ShapeUpdate {
                item_id: "shape-1".into(),
                end: (0.8, 0.7),
            },
        )
        .await;
        apply(
            &hub,
            PainterMessage::ShapeEnd {
                item_id: "shape-1".into(),
                ended_at: 10.0,
            },
        )
        .await;
        apply(
            &hub,
            PainterMessage::StampAdd {
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
            },
        )
        .await;

        let (_, mut receiver) = hub.subscribe().await.unwrap();
        let snapshot = receiver.recv().await.unwrap();
        match serde_json::from_str::<OverlayControlMessage>(&snapshot).unwrap() {
            OverlayControlMessage::Snapshot {
                protocol_version,
                items,
                ..
            } => {
                assert_eq!(protocol_version, PROTOCOL_VERSION);
                assert!(matches!(items[0], CanvasItem::Shape { .. }));
                assert!(matches!(items[1], CanvasItem::Stamp { .. }));
            }
            OverlayControlMessage::Pong { .. } => panic!("expected snapshot"),
        }
    }

    #[tokio::test]
    async fn hub_applies_stamp_move_preview_and_commit_to_snapshots() {
        let hub = test_hub();
        apply(
            &hub,
            PainterMessage::StampAdd {
                stamp: StampItem {
                    item_id: "stamp-item-1".into(),
                    stamp_id: "stamp-1".into(),
                    center: (0.2, 0.3),
                    width_n: 0.1,
                    height_n: 0.2,
                    opacity: 1.0,
                    done: true,
                    ended_at: Some(20.0),
                },
            },
        )
        .await;
        let (_, mut receiver) = hub.subscribe().await.unwrap();
        receiver.recv().await.unwrap();

        apply(
            &hub,
            PainterMessage::StampMovePreview {
                item_id: "stamp-item-1".into(),
                center: (0.5, 0.45),
            },
        )
        .await;
        let event = receiver.recv().await.unwrap();
        assert!(event.contains("\"type\":\"stamp_move_preview\""));
        assert!(event.contains("\"center\":[0.5,0.45]"));

        apply(
            &hub,
            PainterMessage::StampMove {
                item_id: "stamp-item-1".into(),
                center: (0.75, 0.6),
            },
        )
        .await;
        let event = receiver.recv().await.unwrap();
        assert!(event.contains("\"type\":\"stamp_move\""));
        assert!(event.contains("\"center\":[0.75,0.6]"));

        let (_, mut latest) = hub.subscribe().await.unwrap();
        let snapshot = latest.recv().await.unwrap();
        match serde_json::from_str::<OverlayControlMessage>(&snapshot).unwrap() {
            OverlayControlMessage::Snapshot { rev, items, .. } => {
                assert_eq!(rev, 3);
                match &items[0] {
                    CanvasItem::Stamp { stamp } => assert_eq!(stamp.center, (0.75, 0.6)),
                    _ => panic!("expected stamp"),
                }
            }
            OverlayControlMessage::Pong { .. } => panic!("expected snapshot"),
        }
    }

    #[tokio::test]
    async fn hub_restores_a_redone_item() {
        let hub = test_hub();
        let item = CanvasItem::Stamp {
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
        };
        apply(&hub, PainterMessage::Redo { item }).await;

        let (_, mut receiver) = hub.subscribe().await.unwrap();
        let snapshot = receiver.recv().await.unwrap();
        match serde_json::from_str::<OverlayControlMessage>(&snapshot).unwrap() {
            OverlayControlMessage::Snapshot { rev, items, .. } => {
                assert_eq!(rev, 1);
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].item_id(), "stamp-item-1");
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
        let source_items = Arc::new(Mutex::new(Vec::new()));
        let server = spawn(port, &[], Arc::clone(&source_items)).unwrap();
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

        let begin = PainterMessage::StrokeBegin {
            stroke_id: "integration".into(),
            brush: brush(),
        };
        source_items.lock().unwrap().push(CanvasItem::Stroke {
            stroke: Stroke {
                stroke_id: "integration".into(),
                brush: brush(),
                pts: Vec::new(),
                done: false,
                ended_at: None,
            },
        });
        server.send_all(vec![begin]);
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
