import { describe, expect, spyOn, test } from "bun:test";
import type { CanvasItem } from "~/protocol";
import { OverlayLayers, type OverlayLayersRuntime } from "./layers";

interface TimerTask {
  id: number;
  dueAt: number;
  callback: () => void;
}

class FakeTimers {
  nowMs = 0;
  timeoutDelays: number[] = [];
  private nextId = 1;
  private tasks = new Map<number, TimerTask>();

  setTimeout = (callback: () => void, delayMs: number): number => {
    const id = this.nextId++;
    this.timeoutDelays.push(delayMs);
    this.tasks.set(id, { id, dueAt: this.nowMs + delayMs, callback });
    return id;
  };

  clearTimeout = (handle: number): void => {
    this.tasks.delete(handle);
  };

  pendingDelays(): number[] {
    return [...this.tasks.values()]
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
      this.tasks.delete(next.id);
      this.nowMs = next.dueAt;
      next.callback();
    }
    this.nowMs = target;
  }
}

class FakeImage {
  decoding: HTMLImageElement["decoding"] = "auto";
  onload: HTMLImageElement["onload"] = null;
  onerror: HTMLImageElement["onerror"] = null;
  src = "";

  succeed(): void {
    this.onload?.call(this.asImage(), {} as Event);
  }

  fail(): void {
    this.onerror?.call(this.asImage(), {} as Event);
  }

  asImage(): HTMLImageElement {
    return this as unknown as HTMLImageElement;
  }
}

interface DrawImageCall {
  image: CanvasImageSource;
  args: number[];
}

class FakeCanvasContext {
  globalAlpha = 1;
  globalCompositeOperation: GlobalCompositeOperation = "source-over";
  clearRectCalls = 0;
  drawImageCalls: DrawImageCall[] = [];

  clearRect(): void {
    this.clearRectCalls++;
  }

  drawImage(image: CanvasImageSource, ...args: number[]): void {
    this.drawImageCalls.push({ image, args });
  }

  save(): void {}
  restore(): void {}
}

class FakeCanvas {
  readonly context = new FakeCanvasContext();

  constructor(
    public width: number,
    public height: number,
  ) {}

  getContext(): CanvasRenderingContext2D {
    return this.context as unknown as CanvasRenderingContext2D;
  }

  asCanvas(): HTMLCanvasElement {
    return this as unknown as HTMLCanvasElement;
  }
}

class FakeRuntime implements OverlayLayersRuntime {
  readonly timers = new FakeTimers();
  readonly images: FakeImage[] = [];

  createImage = (): HTMLImageElement => {
    const image = new FakeImage();
    this.images.push(image);
    return image.asImage();
  };

  setTimeout = this.timers.setTimeout;
  clearTimeout = this.timers.clearTimeout;
}

interface Harness {
  layers: OverlayLayers;
  baked: FakeCanvas;
  active: FakeCanvas;
  runtime: FakeRuntime;
}

function harness(): Harness {
  const baked = new FakeCanvas(1_000, 500);
  const active = new FakeCanvas(1_000, 500);
  const runtime = new FakeRuntime();
  return {
    layers: new OverlayLayers(baked.asCanvas(), active.asCanvas(), runtime),
    baked,
    active,
    runtime,
  };
}

function stamp(
  center: [number, number] = [0.5, 0.5],
): Extract<CanvasItem, { kind: "stamp" }> {
  return {
    kind: "stamp",
    itemId: "item-1",
    stampId: "stamp-1",
    center,
    widthN: 0.1,
    heightN: 0.2,
    opacity: 1,
    done: true,
    endedAt: 1,
  };
}

function withoutWarnings(callback: () => void): void {
  const warning = spyOn(console, "warn").mockImplementation(() => {});
  try {
    callback();
  } finally {
    warning.mockRestore();
  }
}

describe("OverlayLayers stamp image retry", () => {
  test("一時失敗後にretry成功するとbakedへ再描画する", () => {
    const { layers, baked, active, runtime } = harness();
    layers.rebuild([stamp()]);
    expect(runtime.images).toHaveLength(1);
    expect(runtime.images[0]?.src).toBe("/stamps/stamp-1");

    withoutWarnings(() => runtime.images[0]?.fail());
    expect(runtime.timers.pendingDelays()).toEqual([1_000]);
    runtime.timers.advanceBy(1_000);
    expect(runtime.images).toHaveLength(2);
    expect(runtime.images[1]?.src).toBe("/stamps/stamp-1?retry=1");

    runtime.images[1]?.succeed();
    expect(baked.context.drawImageCalls).toHaveLength(1);
    expect(baked.context.drawImageCalls[0]?.image).toBe(
      runtime.images[1]?.asImage(),
    );
    expect(active.context.drawImageCalls).toHaveLength(0);
    expect(runtime.timers.pendingDelays()).toEqual([]);
  });

  test("移動中stampのretry成功はbakedから除外してactiveへ描画する", () => {
    const { layers, baked, active, runtime } = harness();
    const original = stamp();
    layers.rebuild([original]);
    withoutWarnings(() => runtime.images[0]?.fail());

    const moving = stamp([0.8, 0.7]);
    layers.previewStamp(moving, true);
    runtime.timers.advanceBy(1_000);
    runtime.images[1]?.succeed();

    expect(baked.context.drawImageCalls).toHaveLength(0);
    expect(active.context.drawImageCalls).toEqual([
      {
        image: runtime.images[1]?.asImage(),
        args: [750, 300, 100, 100],
      },
    ]);
  });

  test("連続失敗は30秒上限のexponential backoffでrebuild時も重複しない", () => {
    const { layers, runtime } = harness();
    const item = stamp();
    layers.rebuild([item]);
    const expected = [1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000];

    withoutWarnings(() => {
      for (const [index, delay] of expected.entries()) {
        runtime.images[index]?.fail();
        expect(runtime.timers.pendingDelays()).toEqual([delay]);

        // reconnect snapshotなどで同じ履歴を再構築してもload/timerを増やさない。
        layers.rebuild([item]);
        expect(runtime.images).toHaveLength(index + 1);
        expect(runtime.timers.pendingDelays()).toEqual([delay]);

        runtime.timers.advanceBy(delay);
        expect(runtime.images).toHaveLength(index + 2);
      }
    });
    expect(runtime.timers.timeoutDelays).toEqual(expected);
  });

  test("不要になったstampのretry timerとloading callbackを破棄する", () => {
    const waiting = harness();
    waiting.layers.rebuild([stamp()]);
    withoutWarnings(() => waiting.runtime.images[0]?.fail());
    waiting.layers.setItems([]);
    expect(waiting.runtime.timers.pendingDelays()).toEqual([]);
    waiting.runtime.timers.advanceBy(60_000);
    expect(waiting.runtime.images).toHaveLength(1);

    const loading = harness();
    loading.layers.rebuild([stamp()]);
    const pending = loading.runtime.images[0];
    loading.layers.setItems([]);
    expect(pending?.onload).toBeNull();
    expect(pending?.onerror).toBeNull();
    pending?.succeed();
    expect(loading.baked.context.drawImageCalls).toHaveLength(0);
  });

  test("disposeはretryとload callbackを停止し再描画しない", () => {
    const waiting = harness();
    waiting.layers.rebuild([stamp()]);
    withoutWarnings(() => waiting.runtime.images[0]?.fail());
    waiting.layers.dispose();
    waiting.layers.dispose();
    waiting.runtime.timers.advanceBy(60_000);
    expect(waiting.runtime.timers.pendingDelays()).toEqual([]);
    expect(waiting.runtime.images).toHaveLength(1);

    const loading = harness();
    loading.layers.rebuild([stamp()]);
    const pending = loading.runtime.images[0];
    loading.layers.dispose();
    expect(pending?.onload).toBeNull();
    expect(pending?.onerror).toBeNull();
    pending?.succeed();
    expect(loading.baked.context.drawImageCalls).toHaveLength(0);
    expect(loading.active.context.drawImageCalls).toHaveLength(0);
  });
});
