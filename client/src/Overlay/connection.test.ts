import { describe, expect, spyOn, test } from "bun:test";
import {
  type CanvasItem,
  MIN_COMPATIBLE_PROTOCOL_VERSION,
  PROTOCOL_VERSION,
  type ServerToOverlayMessage,
} from "~/protocol";
import {
  connectOverlay,
  type OverlayConnectionRuntime,
  type OverlayConnectionStatus,
  type OverlaySocket,
} from "./connection";
import {
  OVERLAY_DISCONNECT_GRACE_MS,
  OverlayDisconnectGuard,
} from "./disconnect-guard";
import { RenderQueue } from "./render-queue";
import { OverlayState } from "./state";

interface TimerTask {
  id: number;
  dueAt: number;
  intervalMs: number | null;
  callback: () => void;
}

class FakeTimers {
  nowMs = 0;
  timeoutDelays: number[] = [];
  private nextId = 1;
  private tasks = new Map<number, TimerTask>();

  setTimeout = (callback: () => void, delayMs: number): number => {
    this.timeoutDelays.push(delayMs);
    return this.schedule(callback, delayMs, null);
  };

  clearTimeout = (handle: number): void => {
    this.tasks.delete(handle);
  };

  setInterval = (callback: () => void, intervalMs: number): number =>
    this.schedule(callback, intervalMs, intervalMs);

  clearInterval = (handle: number): void => {
    this.tasks.delete(handle);
  };

  pendingTimeoutDelays(): number[] {
    return [...this.tasks.values()]
      .filter((task) => task.intervalMs === null)
      .map((task) => task.dueAt - this.nowMs)
      .sort((a, b) => a - b);
  }

  timeoutCallback(delayMs: number): () => void {
    const task = [...this.tasks.values()].find(
      (candidate) =>
        candidate.intervalMs === null &&
        candidate.dueAt - this.nowMs === delayMs,
    );
    if (!task) throw new Error(`timeout ${delayMs}ms was not found`);
    return task.callback;
  }

  advanceBy(elapsedMs: number): void {
    const target = this.nowMs + elapsedMs;
    for (;;) {
      const next = [...this.tasks.values()]
        .filter((task) => task.dueAt <= target)
        .sort((a, b) => a.dueAt - b.dueAt || a.id - b.id)[0];
      if (!next) break;

      this.nowMs = next.dueAt;
      if (next.intervalMs === null) {
        this.tasks.delete(next.id);
      } else {
        next.dueAt += next.intervalMs;
      }
      next.callback();
    }
    this.nowMs = target;
  }

  private schedule(
    callback: () => void,
    delayMs: number,
    intervalMs: number | null,
  ): number {
    const id = this.nextId++;
    this.tasks.set(id, {
      id,
      dueAt: this.nowMs + delayMs,
      intervalMs,
      callback,
    });
    return id;
  }
}

class FakeWebSocket implements OverlaySocket {
  readyState: WebSocket["readyState"] = 0;
  onopen: WebSocket["onopen"] = null;
  onmessage: WebSocket["onmessage"] = null;
  onclose: WebSocket["onclose"] = null;
  onerror: WebSocket["onerror"] = null;
  sent: string[] = [];
  closeCalls = 0;

  open(): void {
    this.readyState = 1;
    this.onopen?.call(this.asWebSocket(), {} as Event);
  }

  receive(data: string): void {
    this.onmessage?.call(this.asWebSocket(), { data } as MessageEvent);
  }

  remoteClose(): void {
    this.readyState = 3;
    this.onclose?.call(this.asWebSocket(), {} as CloseEvent);
  }

  send(data: string | ArrayBufferLike | Blob | ArrayBufferView): void {
    this.sent.push(String(data));
  }

  close(): void {
    this.closeCalls++;
    this.readyState = 3;
  }

  asWebSocket(): WebSocket {
    return this as unknown as WebSocket;
  }
}

class FakeRuntime implements OverlayConnectionRuntime {
  readonly timers = new FakeTimers();
  readonly sockets: FakeWebSocket[] = [];

  constructor(private readonly randomValue = 0.5) {}

  createWebSocket = (_url: string): OverlaySocket => {
    const socket = new FakeWebSocket();
    this.sockets.push(socket);
    return socket;
  };

  now = (): number => this.timers.nowMs;
  random = (): number => this.randomValue;
  setTimeout = this.timers.setTimeout;
  clearTimeout = this.timers.clearTimeout;
  setInterval = this.timers.setInterval;
  clearInterval = this.timers.clearInterval;
}

function snapshot(
  protocolVersion = PROTOCOL_VERSION,
  items: CanvasItem[] = [],
): string {
  return JSON.stringify({
    type: "snapshot",
    protocolVersion,
    rev: 0,
    fadeAfterMs: null,
    items,
  });
}

const ignoreStatus = (_status: OverlayConnectionStatus): void => {};

function stampItem(itemId: string): CanvasItem {
  return {
    kind: "stamp",
    itemId,
    stampId: "stamp-1",
    center: [0.5, 0.5],
    widthN: 0.1,
    heightN: 0.1,
    opacity: 1,
    done: true,
    endedAt: 1,
  };
}

function acceptSupportedMessage(message: ServerToOverlayMessage): boolean {
  return (
    message.type !== "snapshot" ||
    (message.protocolVersion >= MIN_COMPATIBLE_PROTOCOL_VERSION &&
      message.protocolVersion <= PROTOCOL_VERSION)
  );
}

function socket(runtime: FakeRuntime, index: number): FakeWebSocket {
  const found = runtime.sockets[index];
  if (!found) throw new Error(`socket ${index} was not created`);
  return found;
}

function guardedConnection(
  runtime: FakeRuntime,
  onExpired: () => void,
  onMessage: (
    message: ServerToOverlayMessage,
  ) => boolean = acceptSupportedMessage,
) {
  const statuses: OverlayConnectionStatus[] = [];
  const guard = new OverlayDisconnectGuard(onExpired, runtime);
  const connection = connectOverlay(
    "ws://localhost/ws",
    onMessage,
    (status) => {
      statuses.push(status);
      guard.update(status);
    },
    runtime,
  );
  return { connection, guard, statuses };
}

describe("connectOverlay", () => {
  test("open直後のprotocol mismatch、不正message、early closeでbackoffを維持する", () => {
    const runtime = new FakeRuntime();
    connectOverlay(
      "ws://localhost/ws",
      acceptSupportedMessage,
      ignoreStatus,
      runtime,
    );

    socket(runtime, 0).open();
    socket(runtime, 0).receive(snapshot(PROTOCOL_VERSION + 1));
    expect(runtime.timers.pendingTimeoutDelays()).toEqual([1_000]);

    runtime.timers.advanceBy(1_000);
    socket(runtime, 1).open();
    const warning = spyOn(console, "warn").mockImplementation(() => {});
    try {
      socket(runtime, 1).receive("{invalid json");
      expect(warning).toHaveBeenCalledTimes(1);
    } finally {
      warning.mockRestore();
    }
    expect(runtime.timers.pendingTimeoutDelays()).toEqual([2_000]);

    runtime.timers.advanceBy(2_000);
    socket(runtime, 2).open();
    socket(runtime, 2).remoteClose();
    expect(runtime.timers.pendingTimeoutDelays()).toEqual([4_000]);
    expect(runtime.timers.timeoutDelays).toEqual([1_000, 2_000, 4_000]);
  });

  test("backoffは30秒を上限に固定jitterを適用する", () => {
    const runtime = new FakeRuntime(1);
    connectOverlay(
      "ws://localhost/ws",
      acceptSupportedMessage,
      ignoreStatus,
      runtime,
    );

    const expected = [1_200, 2_400, 4_800, 9_600, 19_200, 36_000, 36_000];
    for (const [index, delay] of expected.entries()) {
      socket(runtime, index).remoteClose();
      expect(runtime.timers.pendingTimeoutDelays()[0]).toBeCloseTo(delay);
      runtime.timers.advanceBy(delay);
    }
  });

  test("受理したsnapshotだけがbackoffを最小値へ戻す", () => {
    const runtime = new FakeRuntime();
    connectOverlay(
      "ws://localhost/ws",
      acceptSupportedMessage,
      ignoreStatus,
      runtime,
    );

    socket(runtime, 0).remoteClose();
    runtime.timers.advanceBy(1_000);
    socket(runtime, 1).open();
    socket(runtime, 1).remoteClose();
    runtime.timers.advanceBy(2_000);

    socket(runtime, 2).open();
    socket(runtime, 2).receive(snapshot());
    socket(runtime, 2).remoteClose();
    expect(runtime.timers.pendingTimeoutDelays()).toEqual([1_000]);
  });

  test("idle接続を切断しping値にもfake clockを使う", () => {
    const runtime = new FakeRuntime();
    connectOverlay(
      "ws://localhost/ws",
      acceptSupportedMessage,
      ignoreStatus,
      runtime,
    );
    const first = socket(runtime, 0);
    first.open();
    first.receive(snapshot());

    runtime.timers.advanceBy(45_000);

    expect(first.sent.map((message) => JSON.parse(message))).toEqual([
      { type: "ping", t: 15_000 },
      { type: "ping", t: 30_000 },
    ]);
    expect(first.closeCalls).toBe(1);
    expect(runtime.timers.pendingTimeoutDelays()).toEqual([1_000]);
  });

  test("置換済みsocketの遅延eventはbackoffや現接続を変更しない", () => {
    const runtime = new FakeRuntime();
    const statuses: OverlayConnectionStatus[] = [];
    connectOverlay(
      "ws://localhost/ws",
      acceptSupportedMessage,
      (status) => statuses.push(status),
      runtime,
    );
    const first = socket(runtime, 0);
    first.open();
    first.receive(snapshot());
    const staleMessage = first.onmessage;
    const staleClose = first.onclose;
    first.remoteClose();
    runtime.timers.advanceBy(1_000);

    const second = socket(runtime, 1);
    second.open();
    staleMessage?.call(first.asWebSocket(), {
      data: snapshot(),
    } as MessageEvent);
    staleClose?.call(first.asWebSocket(), {} as CloseEvent);
    expect(runtime.timers.pendingTimeoutDelays()).toEqual([]);
    expect(statuses).toEqual(["connected", "disconnected"]);

    second.remoteClose();
    expect(runtime.timers.pendingTimeoutDelays()).toEqual([2_000]);
    expect(statuses).toEqual(["connected", "disconnected"]);
  });

  test("明示closeはopen中socketと待機中reconnectの両方を停止する", () => {
    const activeRuntime = new FakeRuntime();
    const active = connectOverlay(
      "ws://localhost/ws",
      acceptSupportedMessage,
      ignoreStatus,
      activeRuntime,
    );
    const activeSocket = socket(activeRuntime, 0);
    activeSocket.open();
    active.close();
    activeRuntime.timers.advanceBy(60_000);
    expect(activeSocket.closeCalls).toBe(1);
    expect(activeRuntime.sockets).toHaveLength(1);

    const waitingRuntime = new FakeRuntime();
    const waiting = connectOverlay(
      "ws://localhost/ws",
      acceptSupportedMessage,
      ignoreStatus,
      waitingRuntime,
    );
    socket(waitingRuntime, 0).remoteClose();
    waiting.close();
    waitingRuntime.timers.advanceBy(60_000);
    expect(waitingRuntime.sockets).toHaveLength(1);
    expect(waitingRuntime.timers.pendingTimeoutDelays()).toEqual([]);
  });
});

describe("overlay disconnect grace", () => {
  test("close後もgrace中は維持し、期限到達時に一度だけclearする", () => {
    const runtime = new FakeRuntime();
    let clearCalls = 0;
    const managed = guardedConnection(runtime, () => clearCalls++);
    const first = socket(runtime, 0);
    first.open();
    first.receive(snapshot());
    first.remoteClose();

    expect(managed.statuses).toEqual(["connected", "disconnected"]);
    runtime.timers.advanceBy(OVERLAY_DISCONNECT_GRACE_MS - 1);
    expect(clearCalls).toBe(0);
    runtime.timers.advanceBy(1);
    expect(clearCalls).toBe(1);
    runtime.timers.advanceBy(OVERLAY_DISCONNECT_GRACE_MS);
    expect(clearCalls).toBe(1);

    managed.guard.dispose();
    managed.connection.close();
  });

  test("grace内のsnapshotはclearを止め、取り出し済みの旧timerも無効化する", () => {
    const runtime = new FakeRuntime();
    let clearCalls = 0;
    const managed = guardedConnection(runtime, () => clearCalls++);
    const first = socket(runtime, 0);
    first.open();
    first.receive(snapshot());
    first.remoteClose();
    const staleGraceCallback = runtime.timers.timeoutCallback(
      OVERLAY_DISCONNECT_GRACE_MS,
    );

    runtime.timers.advanceBy(1_000);
    const second = socket(runtime, 1);
    second.open();
    second.receive(snapshot());
    expect(managed.statuses).toEqual([
      "connected",
      "disconnected",
      "connected",
    ]);

    // clearTimeoutより先にevent loopへ取り出されていたcallbackもgenerationで拒否する。
    staleGraceCallback();
    runtime.timers.advanceBy(OVERLAY_DISCONNECT_GRACE_MS * 2);
    expect(clearCalls).toBe(0);

    managed.guard.dispose();
    managed.connection.close();
  });

  test("clear後も後続snapshotから状態と描画待ちを完全復元できる", () => {
    const runtime = new FakeRuntime();
    const state = new OverlayState();
    const pending = new RenderQueue();
    let clearCalls = 0;
    let visibleItemIds: string[] = [];

    const flush = () => {
      for (const effect of pending.drain()) {
        if (effect.kind === "rebuild") {
          visibleItemIds = state.items.map((item) =>
            item.kind === "stroke" ? item.strokeId : item.itemId,
          );
        }
      }
    };
    const managed = guardedConnection(
      runtime,
      () => {
        clearCalls++;
        pending.clear();
        state.reset();
        visibleItemIds = [];
      },
      (message) => {
        const effect = state.apply(message);
        if (effect.kind === "resync") return false;
        if (effect.kind !== "none") pending.enqueue(effect);
        return true;
      },
    );

    const first = socket(runtime, 0);
    first.open();
    first.receive(snapshot(PROTOCOL_VERSION, [stampItem("old")]));
    flush();
    expect(visibleItemIds).toEqual(["old"]);
    pending.enqueue({ kind: "preview" });

    first.remoteClose();
    runtime.timers.advanceBy(OVERLAY_DISCONNECT_GRACE_MS);
    expect(clearCalls).toBe(1);
    expect(state.items).toEqual([]);
    expect(pending.drain()).toEqual([]);
    expect(visibleItemIds).toEqual([]);

    const second = socket(runtime, 1);
    second.open();
    second.receive(snapshot(PROTOCOL_VERSION, [stampItem("new")]));
    flush();
    expect(state.items).toEqual([stampItem("new")]);
    expect(visibleItemIds).toEqual(["new"]);

    managed.guard.dispose();
    managed.connection.close();
  });

  test("連続するreconnect失敗で最初のgrace期限を延長しない", () => {
    const runtime = new FakeRuntime();
    let clearCalls = 0;
    const managed = guardedConnection(runtime, () => clearCalls++);
    const first = socket(runtime, 0);
    first.open();
    first.receive(snapshot());
    first.remoteClose();

    runtime.timers.advanceBy(1_000);
    socket(runtime, 1).remoteClose();
    runtime.timers.advanceBy(OVERLAY_DISCONNECT_GRACE_MS - 1_001);
    expect(clearCalls).toBe(0);
    runtime.timers.advanceBy(1);
    expect(clearCalls).toBe(1);
    expect(managed.statuses).toEqual(["connected", "disconnected"]);

    managed.guard.dispose();
    managed.connection.close();
  });

  test("明示closeとguard disposeはreconnect・grace・clearをすべて停止する", () => {
    const runtime = new FakeRuntime();
    let clearCalls = 0;
    const managed = guardedConnection(runtime, () => clearCalls++);
    const first = socket(runtime, 0);
    first.open();
    first.receive(snapshot());
    first.remoteClose();

    managed.guard.dispose();
    managed.connection.close();
    runtime.timers.advanceBy(60_000);
    expect(clearCalls).toBe(0);
    expect(runtime.sockets).toHaveLength(1);
    expect(runtime.timers.pendingTimeoutDelays()).toEqual([]);
  });
});
