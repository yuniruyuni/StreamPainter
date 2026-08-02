import { describe, expect, spyOn, test } from "bun:test";
import { PROTOCOL_VERSION, type ServerToOverlayMessage } from "~/protocol";
import {
  connectOverlay,
  type OverlayConnectionRuntime,
  type OverlaySocket,
} from "./connection";

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

function snapshot(protocolVersion = PROTOCOL_VERSION): string {
  return JSON.stringify({
    type: "snapshot",
    protocolVersion,
    rev: 0,
    fadeAfterMs: null,
    items: [],
  });
}

function acceptSupportedMessage(message: ServerToOverlayMessage): boolean {
  return (
    message.type !== "snapshot" || message.protocolVersion === PROTOCOL_VERSION
  );
}

function socket(runtime: FakeRuntime, index: number): FakeWebSocket {
  const found = runtime.sockets[index];
  if (!found) throw new Error(`socket ${index} was not created`);
  return found;
}

describe("connectOverlay", () => {
  test("open直後のprotocol mismatch、不正message、early closeでbackoffを維持する", () => {
    const runtime = new FakeRuntime();
    connectOverlay("ws://localhost/ws", acceptSupportedMessage, runtime);

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
    connectOverlay("ws://localhost/ws", acceptSupportedMessage, runtime);

    const expected = [1_200, 2_400, 4_800, 9_600, 19_200, 36_000, 36_000];
    for (const [index, delay] of expected.entries()) {
      socket(runtime, index).remoteClose();
      expect(runtime.timers.pendingTimeoutDelays()[0]).toBeCloseTo(delay);
      runtime.timers.advanceBy(delay);
    }
  });

  test("受理したsnapshotだけがbackoffを最小値へ戻す", () => {
    const runtime = new FakeRuntime();
    connectOverlay("ws://localhost/ws", acceptSupportedMessage, runtime);

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
    connectOverlay("ws://localhost/ws", acceptSupportedMessage, runtime);
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
    connectOverlay("ws://localhost/ws", acceptSupportedMessage, runtime);
    const first = socket(runtime, 0);
    first.open();
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

    second.remoteClose();
    expect(runtime.timers.pendingTimeoutDelays()).toEqual([2_000]);
  });

  test("明示closeはopen中socketと待機中reconnectの両方を停止する", () => {
    const activeRuntime = new FakeRuntime();
    const active = connectOverlay(
      "ws://localhost/ws",
      acceptSupportedMessage,
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
      waitingRuntime,
    );
    socket(waitingRuntime, 0).remoteClose();
    waiting.close();
    waitingRuntime.timers.advanceBy(60_000);
    expect(waitingRuntime.sockets).toHaveLength(1);
    expect(waitingRuntime.timers.pendingTimeoutDelays()).toEqual([]);
  });
});
