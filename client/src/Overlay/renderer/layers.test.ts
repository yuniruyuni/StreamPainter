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

interface ContextOperation {
  kind: "clear" | "draw_image" | "fill" | "stroke";
  alpha: number;
  compositeOperation: GlobalCompositeOperation;
  image?: CanvasImageSource;
}

class FakeCanvasContext {
  globalAlpha = 1;
  globalCompositeOperation: GlobalCompositeOperation = "source-over";
  fillStyle: string | CanvasGradient | CanvasPattern = "#000000";
  lineCap: CanvasLineCap = "butt";
  lineJoin: CanvasLineJoin = "miter";
  lineWidth = 1;
  strokeStyle: string | CanvasGradient | CanvasPattern = "#000000";
  clearRectCalls = 0;
  drawImageCalls: DrawImageCall[] = [];
  operations: ContextOperation[] = [];
  private stateStack: Array<{
    alpha: number;
    compositeOperation: GlobalCompositeOperation;
  }> = [];

  clearRect(): void {
    this.clearRectCalls++;
    this.record("clear");
  }

  drawImage(image: CanvasImageSource, ...args: number[]): void {
    this.drawImageCalls.push({ image, args });
    this.record("draw_image", image);
  }

  beginPath(): void {}
  moveTo(): void {}
  lineTo(): void {}
  quadraticCurveTo(): void {}
  arc(): void {}
  rect(): void {}
  ellipse(): void {}

  fill(): void {
    this.record("fill");
  }

  stroke(): void {
    this.record("stroke");
  }

  save(): void {
    this.stateStack.push({
      alpha: this.globalAlpha,
      compositeOperation: this.globalCompositeOperation,
    });
  }

  restore(): void {
    const state = this.stateStack.pop();
    if (!state) return;
    this.globalAlpha = state.alpha;
    this.globalCompositeOperation = state.compositeOperation;
  }

  resetLogs(): void {
    this.clearRectCalls = 0;
    this.drawImageCalls = [];
    this.operations = [];
  }

  private record(
    kind: ContextOperation["kind"],
    image?: CanvasImageSource,
  ): void {
    this.operations.push({
      kind,
      alpha: this.globalAlpha,
      compositeOperation: this.globalCompositeOperation,
      ...(image ? { image } : {}),
    });
  }
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
  readonly canvases: FakeCanvas[] = [];
  readonly images: FakeImage[] = [];

  createCanvas = (): HTMLCanvasElement => {
    const canvas = new FakeCanvas(0, 0);
    this.canvases.push(canvas);
    return canvas.asCanvas();
  };

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

function marker(
  index: number,
  opacity = 0.4,
  pts: Extract<CanvasItem, { kind: "stroke" }>["pts"] = [
    [0.1, 0.1, 1, 0, 0, 0],
    [0.9, 0.9, 1, 1, 0, 0],
  ],
): Extract<CanvasItem, { kind: "stroke" }> {
  return {
    kind: "stroke",
    strokeId: `marker-${index}`,
    brush: {
      tool: "marker",
      color: "#ff00ff",
      opacity,
      widthN: 0.02,
      pressureWidth: false,
      pressureMin: 1,
      tiltWidth: false,
      tiltMaxScale: 1,
    },
    pts,
    done: true,
    endedAt: index,
  };
}

function eraser(index: number): Extract<CanvasItem, { kind: "stroke" }> {
  const item = marker(index, 1);
  return {
    ...item,
    strokeId: `eraser-${index}`,
    brush: { ...item.brush, tool: "eraser" },
  };
}

function lineShape(): Extract<CanvasItem, { kind: "shape" }> {
  return {
    kind: "shape",
    itemId: "shape-1",
    shape: "line",
    style: { color: "#00ffff", opacity: 0.7, widthN: 0.01 },
    start: [0.2, 0.8],
    end: [0.8, 0.2],
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

describe("OverlayLayers marker compositing scratch", () => {
  test("clearはbaked・activeの両canvasを同期的に透明化する", () => {
    const { layers, baked, active } = harness();
    layers.rebuild([marker(1)]);
    baked.context.resetLogs();
    active.context.resetLogs();

    layers.clear();

    expect(baked.context.operations.map((operation) => operation.kind)).toEqual(
      ["clear"],
    );
    expect(
      active.context.operations.map((operation) => operation.kind),
    ).toEqual(["clear"]);
    expect(baked.context.drawImageCalls).toEqual([]);
    expect(active.context.drawImageCalls).toEqual([]);
  });

  test("自己交差する半透明strokeを不透明scratchから1回だけ合成する", () => {
    const { layers, baked, runtime } = harness();
    const crossingPoints: Extract<CanvasItem, { kind: "stroke" }>["pts"] = [
      [0.2, 0.2, 1, 0, 0, 0],
      [0.8, 0.8, 1, 1, 0, 0],
      [0.2, 0.8, 1, 2, 0, 0],
      [0.8, 0.2, 1, 3, 0, 0],
      [0.2, 0.2, 1, 4, 0, 0],
    ];

    layers.rebuild([marker(1, 0.35, crossingPoints)]);

    expect(runtime.canvases).toHaveLength(1);
    const scratch = runtime.canvases[0] as FakeCanvas;
    const scratchStrokes = scratch.context.operations.filter(
      (operation) => operation.kind === "stroke",
    );
    expect(scratchStrokes).toHaveLength(4);
    expect(
      scratchStrokes.every(
        (operation) =>
          operation.alpha === 1 &&
          operation.compositeOperation === "source-over",
      ),
    ).toBe(true);
    expect(baked.context.drawImageCalls).toEqual([
      { image: scratch.asCanvas(), args: [0, 0] },
    ]);
    expect(
      baked.context.operations.filter(
        (operation) => operation.kind === "draw_image",
      ),
    ).toEqual([
      {
        kind: "draw_image",
        alpha: 0.35,
        compositeOperation: "source-over",
        image: scratch.asCanvas(),
      },
    ]);
  });

  test("marker・shape・stamp・eraserの履歴順を保って同じscratchを再利用する", () => {
    const { layers, baked, runtime } = harness();
    const stampItem = { ...stamp(), opacity: 0.8 };
    layers.rebuild([stampItem]);
    runtime.images[0]?.succeed();
    baked.context.resetLogs();

    layers.rebuild([
      marker(1, 0.4),
      lineShape(),
      stampItem,
      eraser(2),
      marker(3, 0.6),
    ]);

    const scratch = runtime.canvases[0] as FakeCanvas;
    const renderOperations = baked.context.operations.filter(
      (operation) => operation.kind !== "clear",
    );
    expect(
      renderOperations.map((operation) => [
        operation.kind,
        operation.alpha,
        operation.compositeOperation,
      ]),
    ).toEqual([
      ["draw_image", 0.4, "source-over"],
      ["stroke", 0.7, "source-over"],
      ["draw_image", 0.8, "source-over"],
      ["stroke", 1, "destination-out"],
      ["draw_image", 0.6, "source-over"],
    ]);
    expect(
      renderOperations
        .filter((operation) => operation.kind === "draw_image")
        .map((operation) => operation.image),
    ).toEqual([
      scratch.asCanvas(),
      runtime.images[0]?.asImage(),
      scratch.asCanvas(),
    ]);
    expect(runtime.canvases).toHaveLength(1);
  });

  test("1080p・4Kの500 markersを1枚のscratchで再構築する", () => {
    const { layers, baked, runtime } = harness();
    const items = Array.from({ length: 500 }, (_, index) => marker(index));
    const rebuildLimitMs = 250;

    const rebuildAt = (width: number, height: number): number => {
      baked.context.resetLogs();
      const startedAt = performance.now();
      layers.resize(width, height, items);
      const elapsedMs = performance.now() - startedAt;
      expect(elapsedMs).toBeLessThan(rebuildLimitMs);
      expect(baked.context.drawImageCalls).toHaveLength(500);
      return elapsedMs;
    };

    rebuildAt(1_920, 1_080);
    expect(runtime.canvases).toHaveLength(1);
    const scratch = runtime.canvases[0] as FakeCanvas;
    expect([scratch.width, scratch.height]).toEqual([1_920, 1_080]);
    expect(scratch.context.clearRectCalls).toBe(500);

    rebuildAt(3_840, 2_160);
    expect(runtime.canvases).toHaveLength(1);
    expect(runtime.canvases[0]).toBe(scratch);
    expect([scratch.width, scratch.height]).toEqual([3_840, 2_160]);
    expect(scratch.context.clearRectCalls).toBe(1_000);

    baked.context.resetLogs();
    const startedAt = performance.now();
    layers.rebuild(items);
    expect(performance.now() - startedAt).toBeLessThan(rebuildLimitMs);
    expect(runtime.canvases).toHaveLength(1);
    expect(baked.context.drawImageCalls).toHaveLength(500);

    layers.dispose();
    expect([scratch.width, scratch.height]).toEqual([0, 0]);
  });
});

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
