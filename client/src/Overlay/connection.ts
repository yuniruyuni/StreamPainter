// overlay の WS 接続管理 (docs/protocol.md)。
// 切断は異常扱いせず silent reconnect する。画面には何も表示しない (docs/webapp.md)。

import type { ServerToOverlayMessage } from "~/protocol";

const PING_INTERVAL_MS = 15_000;
const IDLE_TIMEOUT_MS = 30_000;
const BACKOFF_MIN_MS = 1_000;
const BACKOFF_MAX_MS = 30_000;

export interface OverlayConnection {
  close(): void;
}

export function connectOverlay(
  url: string,
  onMessage: (msg: ServerToOverlayMessage) => void,
): OverlayConnection {
  let ws: WebSocket | null = null;
  let closed = false;
  let backoff = BACKOFF_MIN_MS;
  let pingTimer: ReturnType<typeof setInterval> | null = null;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let lastReceived = 0;

  function clearPing() {
    if (pingTimer) clearInterval(pingTimer);
    pingTimer = null;
  }

  function disconnect(socket: WebSocket) {
    socket.onopen = null;
    socket.onmessage = null;
    socket.onclose = null;
    socket.onerror = null;
    if (socket.readyState < WebSocket.CLOSING) socket.close();
    if (ws === socket) ws = null;
  }

  function scheduleReconnect(socket: WebSocket) {
    if (closed || socket !== ws) return;
    clearPing();
    disconnect(socket);
    if (reconnectTimer) clearTimeout(reconnectTimer);
    const jitter = 1 + (Math.random() * 0.4 - 0.2);
    reconnectTimer = setTimeout(connect, backoff * jitter);
    backoff = Math.min(backoff * 2, BACKOFF_MAX_MS);
  }

  function connect() {
    if (closed) return;
    reconnectTimer = null;
    const socket = new WebSocket(url);
    ws = socket;
    lastReceived = Date.now();

    socket.onopen = () => {
      if (socket !== ws) return;
      backoff = BACKOFF_MIN_MS;
      clearPing();
      pingTimer = setInterval(() => {
        if (Date.now() - lastReceived > IDLE_TIMEOUT_MS) {
          // pong が返らない: 死んだ接続とみなし再接続へ
          scheduleReconnect(socket);
          return;
        }
        if (socket.readyState === WebSocket.OPEN) {
          socket.send(JSON.stringify({ type: "ping", t: Date.now() }));
        }
      }, PING_INTERVAL_MS);
    };

    socket.onmessage = (event) => {
      if (socket !== ws) return;
      lastReceived = Date.now();
      try {
        onMessage(JSON.parse(String(event.data)) as ServerToOverlayMessage);
      } catch (e) {
        console.warn("overlay: failed to handle message", e);
      }
    };

    socket.onclose = () => scheduleReconnect(socket);
    socket.onerror = () => {
      // onclose が続けて呼ばれるため何もしない
    };
  }

  connect();

  return {
    close() {
      closed = true;
      if (reconnectTimer) clearTimeout(reconnectTimer);
      clearPing();
      if (ws) disconnect(ws);
    },
  };
}
