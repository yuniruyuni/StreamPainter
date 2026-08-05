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
  strokeStyle?: string;
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
  translate(): void {}
  rotate(): void {}

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
      ...(kind === "stroke" ? { strokeStyle: String(this.strokeStyle) } : {}),
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

function resetRuntimeCanvasLogs(runtime: FakeRuntime): void {
  for (const canvas of runtime.canvases) canvas.context.resetLogs();
}

function stamp(
  center: [number, number] = [0.5, 0.5],
): Extract<CanvasItem, { kind: "stamp" }> {
  return {
    kind: "stamp",
    itemId: "item-1",
    layerId: "default",
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
    layerId: "default",
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
    layerId: "default",
    shape: "line",
    style: { color: "#00ffff", opacity: 0.7, widthN: 0.01 },
    start: [0.2, 0.8],
    end: [0.8, 0.2],
    done: true,
    endedAt: 1,
  };
}

function rectangleShape(
  itemId: string,
  color: string,
): Extract<CanvasItem, { kind: "shape" }> {
  return {
    kind: "shape",
    itemId,
    layerId: "default",
    shape: "rectangle",
    style: { color, opacity: 1, widthN: 0.01 },
    start: [0.2, 0.2],
    end: [0.4, 0.4],
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
    const { layers, runtime } = harness();
    const crossingPoints: Extract<CanvasItem, { kind: "stroke" }>["pts"] = [
      [0.2, 0.2, 1, 0, 0, 0],
      [0.8, 0.8, 1, 1, 0, 0],
      [0.2, 0.8, 1, 2, 0, 0],
      [0.8, 0.2, 1, 3, 0, 0],
      [0.2, 0.2, 1, 4, 0, 0],
    ];

    layers.rebuild([marker(1, 0.35, crossingPoints)]);

    expect(runtime.canvases).toHaveLength(2);
    const layerCanvas = runtime.canvases[0] as FakeCanvas;
    const scratch = runtime.canvases[1] as FakeCanvas;
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
    expect(layerCanvas.context.drawImageCalls).toEqual([
      { image: scratch.asCanvas(), args: [0, 0] },
    ]);
    expect(
      layerCanvas.context.operations.filter(
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
    resetRuntimeCanvasLogs(runtime);

    layers.rebuild([
      marker(1, 0.4),
      lineShape(),
      stampItem,
      eraser(2),
      marker(3, 0.6),
    ]);

    const layerCanvas = runtime.canvases[0] as FakeCanvas;
    const scratch = runtime.canvases[1] as FakeCanvas;
    const renderOperations = layerCanvas.context.operations.filter(
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
    expect(runtime.canvases).toHaveLength(2);
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
      expect(baked.context.drawImageCalls).toHaveLength(1);
      return elapsedMs;
    };

    rebuildAt(1_920, 1_080);
    expect(runtime.canvases).toHaveLength(2);
    const layerCanvas = runtime.canvases[0] as FakeCanvas;
    const scratch = runtime.canvases[1] as FakeCanvas;
    expect([scratch.width, scratch.height]).toEqual([1_920, 1_080]);
    expect(scratch.context.clearRectCalls).toBe(500);

    rebuildAt(3_840, 2_160);
    expect(runtime.canvases).toHaveLength(2);
    expect(runtime.canvases[1]).toBe(scratch);
    expect([scratch.width, scratch.height]).toEqual([3_840, 2_160]);
    expect(scratch.context.clearRectCalls).toBe(1_000);

    baked.context.resetLogs();
    const startedAt = performance.now();
    layers.rebuild(items);
    expect(performance.now() - startedAt).toBeLessThan(rebuildLimitMs);
    expect(runtime.canvases).toHaveLength(2);
    expect(baked.context.drawImageCalls).toHaveLength(1);
    expect(layerCanvas.context.drawImageCalls).toHaveLength(1_500);

    layers.dispose();
    expect([scratch.width, scratch.height]).toEqual([0, 0]);
  });
});

describe("OverlayLayers user layer compositing", () => {
  const documentLayers = [
    { layerId: "default", name: "レイヤー 1" },
    { layerId: "top", name: "レイヤー 2" },
  ];

  test("layerを下から上へ合成しeraserは所属layerだけへ適用する", () => {
    const { layers, baked, runtime } = harness();
    const bottom = marker(1, 1);
    const top = { ...marker(2, 1), layerId: "top" };
    const topEraser = { ...eraser(3), layerId: "top" };

    layers.rebuild([bottom, top, topEraser], documentLayers);

    const bottomCanvas = runtime.canvases[0] as FakeCanvas;
    const topCanvas = runtime.canvases[1] as FakeCanvas;
    expect(
      bottomCanvas.context.operations.some(
        (operation) => operation.compositeOperation === "destination-out",
      ),
    ).toBe(false);
    expect(
      topCanvas.context.operations.some(
        (operation) => operation.compositeOperation === "destination-out",
      ),
    ).toBe(true);
    expect(
      baked.context.drawImageCalls.slice(-2).map((call) => call.image),
    ).toEqual([bottomCanvas.asCanvas(), topCanvas.asCanvas()]);
  });

  test("下位layerのactive strokeを上位layerより下へ合成する", () => {
    const { layers, active, runtime } = harness();
    const drawing = { ...marker(1, 1), done: false, endedAt: null };
    const upper = { ...marker(2, 1), layerId: "top" };
    layers.rebuild([drawing, upper], documentLayers);
    layers.beginActive(drawing);
    layers.appendActive(drawing);
    layers.renderActive();

    const upperCanvas = runtime.canvases.find((canvas) =>
      canvas.context.operations.some(
        (operation) =>
          operation.kind === "stroke" && operation.strokeStyle === "#ff00ff",
      ),
    );
    expect(upperCanvas).toBeDefined();
    const preview = runtime.canvases.at(-1) as FakeCanvas;
    expect(
      active.context.drawImageCalls.slice(-2).map((call) => call.image),
    ).toEqual([
      preview.asCanvas(),
      upperCanvas?.asCanvas() as CanvasImageSource,
    ]);
  });

  test("layer削除rebuildはcacheを解放しUndoで元の合成位置へ復元してRedoで再解放する", () => {
    const { layers, baked } = harness();
    const bottom = marker(1, 1);
    const top = { ...marker(2, 1), layerId: "top" };
    layers.rebuild([bottom, top], documentLayers);
    const [bottomCanvas, deletedTopCanvas] = baked.context.drawImageCalls
      .slice(-2)
      .map((call) => call.image as unknown as FakeCanvas);

    baked.context.resetLogs();
    layers.rebuild([bottom], [documentLayers[0] as (typeof documentLayers)[0]]);
    expect([deletedTopCanvas?.width, deletedTopCanvas?.height]).toEqual([0, 0]);
    expect(
      baked.context.drawImageCalls.map(
        (call) => call.image as unknown as FakeCanvas,
      ),
    ).toEqual([bottomCanvas]);

    baked.context.resetLogs();
    layers.rebuild([bottom, top], documentLayers);
    const restored = baked.context.drawImageCalls.map(
      (call) => call.image as unknown as FakeCanvas,
    );
    expect(restored[0]).toBe(bottomCanvas);
    expect(restored[1]).not.toBe(deletedTopCanvas);
    expect([restored[1]?.width, restored[1]?.height]).toEqual([1_000, 500]);

    baked.context.resetLogs();
    layers.rebuild([bottom], [documentLayers[0] as (typeof documentLayers)[0]]);
    expect([restored[1]?.width, restored[1]?.height]).toEqual([0, 0]);
    expect(
      baked.context.drawImageCalls.map(
        (call) => call.image as unknown as FakeCanvas,
      ),
    ).toEqual([bottomCanvas]);
  });

  test("レイヤー内容消去snapshotはcatalogを残してcacheだけ除去しUndoでitem順と合成順を戻す", () => {
    const { layers, baked } = harness();
    const catalog = [
      { layerId: "default", name: "レイヤー 1" },
      { layerId: "middle", name: "レイヤー 2" },
      { layerId: "top", name: "レイヤー 3" },
    ];
    const coloredMarker = (index: number, layerId: string, color: string) => {
      const item = marker(index, 1);
      return {
        ...item,
        layerId,
        brush: { ...item.brush, color },
      };
    };
    const before = [
      coloredMarker(1, "default", "#ff0000"),
      coloredMarker(2, "middle", "#00ff00"),
      coloredMarker(3, "top", "#0000ff"),
      coloredMarker(4, "middle", "#ffff00"),
      coloredMarker(5, "default", "#ff00ff"),
    ];
    const cleared = before.filter((item) => item.layerId !== "middle");

    layers.rebuild(before, catalog);
    const [bottomCanvas, clearedMiddleCanvas, topCanvas] =
      baked.context.drawImageCalls
        .slice(-3)
        .map((call) => call.image as unknown as FakeCanvas);

    baked.context.resetLogs();
    layers.rebuild(cleared, catalog);
    expect([clearedMiddleCanvas?.width, clearedMiddleCanvas?.height]).toEqual([
      0, 0,
    ]);
    expect(
      baked.context.drawImageCalls.map(
        (call) => call.image as unknown as FakeCanvas,
      ),
    ).toEqual([bottomCanvas, topCanvas]);

    baked.context.resetLogs();
    layers.rebuild(before, catalog);
    const restored = baked.context.drawImageCalls.map(
      (call) => call.image as unknown as FakeCanvas,
    );
    expect(restored[0]).toBe(bottomCanvas);
    expect(restored[1]).not.toBe(clearedMiddleCanvas);
    expect(restored[2]).toBe(topCanvas);
    expect(
      restored[1]?.context.operations
        .filter((operation) => operation.kind === "stroke")
        .map((operation) => operation.strokeStyle),
    ).toEqual(["#00ff00", "#ffff00"]);

    baked.context.resetLogs();
    layers.rebuild(cleared, catalog);
    expect([restored[1]?.width, restored[1]?.height]).toEqual([0, 0]);
    expect(
      baked.context.drawImageCalls.map(
        (call) => call.image as unknown as FakeCanvas,
      ),
    ).toEqual([bottomCanvas, topCanvas]);
  });

  test("active eraserの1点追加は所属layer scratchへ1segmentだけ追記する", () => {
    const { layers, runtime } = harness();
    const base = marker(1, 1);
    const drawing = {
      ...eraser(2),
      done: false,
      endedAt: null,
      pts: [
        [0.1, 0.5, 1, 0, 0, 0],
        [0.3, 0.5, 1, 1, 0, 0],
        [0.5, 0.5, 1, 2, 0, 0],
        [0.7, 0.5, 1, 3, 0, 0],
      ] as Extract<CanvasItem, { kind: "stroke" }>["pts"],
    };
    layers.rebuild([base, drawing]);
    const eraserScratch = runtime.canvases.find(
      (canvas) =>
        canvas.context.drawImageCalls.length > 0 &&
        canvas.context.operations.some(
          (operation) =>
            operation.kind === "stroke" &&
            operation.compositeOperation === "destination-out",
        ),
    );
    expect(eraserScratch).toBeDefined();

    resetRuntimeCanvasLogs(runtime);
    const updated = {
      ...drawing,
      pts: [...drawing.pts, [0.9, 0.5, 1, 4, 0, 0]] as Extract<
        CanvasItem,
        { kind: "stroke" }
      >["pts"],
    };
    layers.setDocument([base, updated], [{ layerId: "default", name: "L1" }]);
    layers.appendActive(updated);

    const incrementalEraserStrokes = runtime.canvases.flatMap((canvas) =>
      canvas.context.operations.filter(
        (operation) =>
          operation.kind === "stroke" &&
          operation.compositeOperation === "destination-out",
      ),
    );
    expect(incrementalEraserStrokes).toHaveLength(1);
    expect(
      eraserScratch?.context.operations.filter(
        (operation) => operation.kind === "stroke",
      ),
    ).toHaveLength(1);
  });
});

describe("OverlayLayers item transform history ordering", () => {
  test("prefix cache後もtarget・後続marker・eraser・shapeを元の順で再合成する", () => {
    const { layers, baked, active, runtime } = harness();
    const target = rectangleShape("target", "#00ff00");
    const laterShape = rectangleShape("later", "#0000ff");
    const history: CanvasItem[] = [
      marker(1, 0.4),
      target,
      marker(2, 0.6),
      eraser(3),
      laterShape,
    ];
    layers.rebuild(history);
    const transformed = {
      ...target,
      transform: {
        center: [0.65, 0.55] as [number, number],
        widthN: 0.3,
        heightN: 0.2,
        rotation: 0.3,
      },
    };

    baked.context.resetLogs();
    active.context.resetLogs();
    resetRuntimeCanvasLogs(runtime);
    layers.prepareItemPreview(transformed);
    layers.previewItem(transformed, true);
    layers.renderActive();

    // visible bakedは空。prefixをactiveへ写してからsuffixを履歴順に合成するため、
    // 後続eraserはtargetとprefixの両方へ作用し、later shapeはその後に残る。
    expect(baked.context.operations.map((operation) => operation.kind)).toEqual(
      ["clear"],
    );
    const prefix = runtime.canvases.at(-1) as FakeCanvas;
    expect(
      prefix.context.operations.map((operation) => [
        operation.kind,
        operation.alpha,
        operation.compositeOperation,
        operation.strokeStyle,
        undefined,
      ]),
    ).toEqual([
      ["clear", 1, "source-over", undefined, undefined],
      ["draw_image", 1, "source-over", undefined, undefined],
      ["stroke", 1, "source-over", "#00ff00", undefined],
      ["draw_image", 0.6, "source-over", undefined, undefined],
      ["stroke", 1, "destination-out", "#000000", undefined],
      ["stroke", 1, "source-over", "#0000ff", undefined],
    ]);

    // commit時のsuffix順もpreviewと同一で、targetが最前面へ移動しない。
    baked.context.resetLogs();
    resetRuntimeCanvasLogs(runtime);
    layers.rebuild([
      history[0] as CanvasItem,
      transformed,
      ...history.slice(2),
    ]);
    expect(
      (runtime.canvases[0] as FakeCanvas).context.operations
        .filter((operation) => operation.kind !== "clear")
        .map((operation) => [
          operation.kind,
          operation.alpha,
          operation.compositeOperation,
          operation.strokeStyle,
        ]),
    ).toEqual([
      ["draw_image", 0.4, "source-over", undefined],
      ["stroke", 1, "source-over", "#00ff00"],
      ["draw_image", 0.6, "source-over", undefined],
      ["stroke", 1, "destination-out", "#000000"],
      ["stroke", 1, "source-over", "#0000ff"],
    ]);
  });

  test("first previewのRAF前resizeでもbaked ghostを作らずtransformと順序を維持する", () => {
    const { layers, baked, active, runtime } = harness();
    const target = rectangleShape("target", "#00ff00");
    const later = rectangleShape("later", "#0000ff");
    const prefix = marker(1, 0.4);
    layers.rebuild([prefix, target, later]);
    const transformed = {
      ...target,
      transform: {
        center: [0.75, 0.6] as [number, number],
        widthN: 0.2,
        heightN: 0.15,
        rotation: 0.5,
      },
    };

    // componentは受信直後にprepareし、実描画だけをRAFへ遅延する。
    layers.prepareItemPreview(transformed);
    baked.context.resetLogs();
    active.context.resetLogs();
    resetRuntimeCanvasLogs(runtime);
    layers.resize(1_200, 600, [prefix, transformed, later]);
    expect(
      baked.context.operations.filter(
        (operation) => operation.kind === "stroke",
      ),
    ).toEqual([]);

    // resizeが最初のqueueを取り消した後のrebuildBaked=false previewでも、
    // resize時に作ったprefixを保持してtargetを一度だけ正しい位置へ描く。
    active.context.resetLogs();
    resetRuntimeCanvasLogs(runtime);
    layers.previewItem(transformed, false);
    layers.renderActive();
    const preview = runtime.canvases.at(-1) as FakeCanvas;
    const strokes = preview.context.operations.filter(
      (operation) => operation.kind === "stroke",
    );
    expect(strokes.map((operation) => operation.strokeStyle)).toEqual([
      "#00ff00",
      "#0000ff",
    ]);
  });

  test("preview後のcommitがRAF待ちでもresizeは確定履歴を通常bakedへ再構築する", () => {
    const { layers, baked, active, runtime } = harness();
    const prefix = rectangleShape("prefix", "#ff0000");
    const target = rectangleShape("target", "#00ff00");
    const later = rectangleShape("later", "#0000ff");
    layers.rebuild([prefix, target, later]);
    const transformed = {
      ...target,
      transform: {
        center: [0.75, 0.6] as [number, number],
        widthN: 0.2,
        heightN: 0.15,
        rotation: 0.5,
      },
    };

    // componentはpreviewとcommitを同一RAFへqueueし得る。commit受信時に
    // transform状態だけ同期終了し、resizeがqueueを破棄しても確定履歴を使う。
    layers.prepareItemPreview(transformed);
    layers.prepareRebuild();
    baked.context.resetLogs();
    active.context.resetLogs();
    resetRuntimeCanvasLogs(runtime);
    layers.resize(1_200, 600, [prefix, transformed, later]);

    expect(
      (runtime.canvases[0] as FakeCanvas).context.operations
        .filter((operation) => operation.kind === "stroke")
        .map((operation) => operation.strokeStyle),
    ).toEqual(["#ff0000", "#00ff00", "#0000ff"]);
    expect(
      active.context.operations.filter(
        (operation) => operation.kind === "stroke",
      ),
    ).toEqual([]);
  });

  test("連続previewはselected前prefixと別layerの履歴を再生しない", () => {
    const { layers, runtime } = harness();
    const points = Array.from({ length: 40 }, (_, index) => [
      index / 39,
      0.2,
      1,
      index,
      0,
      0,
    ]) as Extract<CanvasItem, { kind: "stroke" }>["pts"];
    const prefix = {
      ...marker(10, 1, points),
      brush: { ...marker(10).brush, color: "#111111", opacity: 1 },
    };
    const target = rectangleShape("target-prefix", "#00ff00");
    const suffix = {
      ...marker(11, 1, points),
      brush: { ...marker(11).brush, color: "#222222", opacity: 1 },
    };
    const otherLayer = {
      ...marker(12, 1, points),
      layerId: "top",
      brush: { ...marker(12).brush, color: "#333333", opacity: 1 },
    };
    const documentLayers = [
      { layerId: "default", name: "L1" },
      { layerId: "top", name: "L2" },
    ];
    layers.rebuild([prefix, target, suffix, otherLayer], documentLayers);
    const transformed = {
      ...target,
      transform: {
        center: [0.6, 0.5] as [number, number],
        widthN: 0.3,
        heightN: 0.2,
        rotation: 0.2,
      },
    };
    layers.prepareItemPreview(transformed);
    layers.previewItem(transformed, true);
    layers.renderActive();

    resetRuntimeCanvasLogs(runtime);
    layers.previewItem(transformed, false);
    layers.renderActive();
    const replayedColors = runtime.canvases.flatMap((canvas) =>
      canvas.context.operations
        .filter((operation) => operation.kind === "stroke")
        .map((operation) => operation.strokeStyle),
    );
    expect(replayedColors).not.toContain("#111111");
    expect(replayedColors).not.toContain("#333333");
    expect(replayedColors).toContain("#222222");
  });
});

describe("OverlayLayers stamp image retry", () => {
  test("一時失敗後にretry成功するとbakedへ再描画する", () => {
    const { layers, active, runtime } = harness();
    layers.rebuild([stamp()]);
    expect(runtime.images).toHaveLength(1);
    expect(runtime.images[0]?.src).toBe("/stamps/stamp-1");

    withoutWarnings(() => runtime.images[0]?.fail());
    expect(runtime.timers.pendingDelays()).toEqual([1_000]);
    runtime.timers.advanceBy(1_000);
    expect(runtime.images).toHaveLength(2);
    expect(runtime.images[1]?.src).toBe("/stamps/stamp-1?retry=1");

    runtime.images[1]?.succeed();
    expect(
      runtime.canvases.some((canvas) =>
        canvas.context.drawImageCalls.some(
          (call) => call.image === runtime.images[1]?.asImage(),
        ),
      ),
    ).toBe(true);
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

    expect(baked.context.operations.at(-1)?.kind).toBe("clear");
    expect(
      runtime.canvases.some((canvas) =>
        canvas.context.drawImageCalls.some(
          (call) => call.image === runtime.images[1]?.asImage(),
        ),
      ),
    ).toBe(true);
    expect(active.context.drawImageCalls.length).toBeGreaterThan(0);
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
    const bakedDraws = loading.baked.context.drawImageCalls.length;
    loading.layers.setItems([]);
    expect(pending?.onload).toBeNull();
    expect(pending?.onerror).toBeNull();
    pending?.succeed();
    expect(loading.baked.context.drawImageCalls).toHaveLength(bakedDraws);
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
    const bakedDraws = loading.baked.context.drawImageCalls.length;
    const activeDraws = loading.active.context.drawImageCalls.length;
    loading.layers.dispose();
    expect(pending?.onload).toBeNull();
    expect(pending?.onerror).toBeNull();
    pending?.succeed();
    expect(loading.baked.context.drawImageCalls).toHaveLength(bakedDraws);
    expect(loading.active.context.drawImageCalls).toHaveLength(activeDraws);
  });
});
