// overlay の WS 接続管理 (docs/protocol.md)。
// 切断は異常扱いせずsilent reconnectし、同期状態だけをrendererへ通知する (docs/webapp.md)。

import type { ServerToOverlayMessage } from "~/protocol";

const PING_INTERVAL_MS = 15_000;
const IDLE_TIMEOUT_MS = 30_000;
const BACKOFF_MIN_MS = 1_000;
const BACKOFF_MAX_MS = 30_000;
const SOCKET_OPEN = 1;
const SOCKET_CLOSING = 2;

type TimerHandle = number;

export type OverlaySocket = Pick<
  WebSocket,
  | "readyState"
  | "onopen"
  | "onmessage"
  | "onclose"
  | "onerror"
  | "send"
  | "close"
>;

/** Browser APIを決定的な接続テストへ差し替えるための最小実行環境。 */
export interface OverlayConnectionRuntime {
  createWebSocket(url: string): OverlaySocket;
  now(): number;
  random(): number;
  setTimeout(callback: () => void, delayMs: number): TimerHandle;
  clearTimeout(handle: TimerHandle): void;
  setInterval(callback: () => void, intervalMs: number): TimerHandle;
  clearInterval(handle: TimerHandle): void;
}

const browserRuntime: OverlayConnectionRuntime = {
  createWebSocket: (url) => new WebSocket(url),
  now: () => Date.now(),
  random: () => Math.random(),
  setTimeout: (callback, delayMs) => window.setTimeout(callback, delayMs),
  clearTimeout: (handle) => window.clearTimeout(handle),
  setInterval: (callback, intervalMs) =>
    window.setInterval(callback, intervalMs),
  clearInterval: (handle) => window.clearInterval(handle),
};

export interface OverlayConnection {
  close(): void;
}

/** connectedはWebSocket openではなく、対応snapshotを受理した同期成立点を表す。 */
export type OverlayConnectionStatus = "connected" | "disconnected";

export type OverlayConnectionStatusCallback = (
  status: OverlayConnectionStatus,
) => void;

export function connectOverlay(
  url: string,
  onMessage: (msg: ServerToOverlayMessage) => boolean,
  onStatus: OverlayConnectionStatusCallback = () => {},
  runtime: OverlayConnectionRuntime = browserRuntime,
): OverlayConnection {
  let ws: OverlaySocket | null = null;
  let closed = false;
  let backoff = BACKOFF_MIN_MS;
  let pingTimer: TimerHandle | null = null;
  let reconnectTimer: TimerHandle | null = null;
  let lastReceived = 0;
  let lastStatus: OverlayConnectionStatus | null = null;

  function notifyStatus(status: OverlayConnectionStatus) {
    if (lastStatus === status) return;
    lastStatus = status;
    try {
      onStatus(status);
    } catch (error) {
      console.warn("overlay: failed to handle connection status", error);
    }
  }

  function clearPing() {
    if (pingTimer !== null) runtime.clearInterval(pingTimer);
    pingTimer = null;
  }

  function disconnect(socket: OverlaySocket) {
    socket.onopen = null;
    socket.onmessage = null;
    socket.onclose = null;
    socket.onerror = null;
    if (socket.readyState < SOCKET_CLOSING) socket.close();
    if (ws === socket) ws = null;
  }

  function scheduleReconnect(socket: OverlaySocket) {
    if (closed || socket !== ws) return;
    clearPing();
    disconnect(socket);
    notifyStatus("disconnected");
    if (reconnectTimer !== null) runtime.clearTimeout(reconnectTimer);
    const jitter = 1 + (runtime.random() * 0.4 - 0.2);
    reconnectTimer = runtime.setTimeout(connect, backoff * jitter);
    backoff = Math.min(backoff * 2, BACKOFF_MAX_MS);
  }

  function connect() {
    if (closed) return;
    reconnectTimer = null;
    const socket = runtime.createWebSocket(url);
    ws = socket;
    lastReceived = runtime.now();

    socket.onopen = () => {
      if (socket !== ws) return;
      clearPing();
      pingTimer = runtime.setInterval(() => {
        if (runtime.now() - lastReceived > IDLE_TIMEOUT_MS) {
          // pong が返らない: 死んだ接続とみなし再接続へ
          scheduleReconnect(socket);
          return;
        }
        if (socket.readyState === SOCKET_OPEN) {
          socket.send(JSON.stringify({ type: "ping", t: runtime.now() }));
        }
      }, PING_INTERVAL_MS);
    };

    socket.onmessage = (event) => {
      if (socket !== ws) return;
      lastReceived = runtime.now();
      try {
        const message = JSON.parse(
          String(event.data),
        ) as ServerToOverlayMessage;
        const keepConnection = onMessage(message);
        if (!keepConnection) {
          scheduleReconnect(socket);
          return;
        }
        // openだけでは、直後のprotocol mismatchや不正messageを成功扱いしてしまう。
        // overlay状態が受理したsnapshotを同期成立点として初めてbackoffを戻す。
        if (message.type === "snapshot") {
          backoff = BACKOFF_MIN_MS;
          notifyStatus("connected");
        }
      } catch (e) {
        console.warn("overlay: failed to handle message", e);
        scheduleReconnect(socket);
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
      if (reconnectTimer !== null) runtime.clearTimeout(reconnectTimer);
      reconnectTimer = null;
      clearPing();
      if (ws) disconnect(ws);
    },
  };
}
