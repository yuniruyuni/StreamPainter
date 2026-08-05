// レイヤー管理 (docs/webapp.md)。
//
// - baked: 確定ストロークを焼き込んだ表示用キャンバス (下層)
// - active: 描画中ストロークの表示用キャンバス (上層)
// - 各描画中ストロークは専用 scratch (オフスクリーン) に不透明で増分描画し、
//   フレームごとに active へ globalAlpha = opacity で合成する。
// - eraser は所属レイヤーcacheの複製へ destination-out で増分適用する。
// - undo / clear / snapshot / トリム時は items 一覧からレイヤーcacheを全再構築する。
// - item transform中は対象より前の履歴をprefixへ一度だけcacheし、対象以降を
//   active上へ元の順序で再合成する。後続eraserもprefixへ正しく作用する。

import type {
  CanvasItem,
  CanvasLayer,
  ShapeItem,
  StampItem,
  Stroke,
} from "~/protocol";
import { DEFAULT_LAYER } from "~/protocol";
import { tailSegment } from "./geometry";
import {
  drawFullStroke,
  drawSegments,
  drawStableIncrement,
} from "./stroke-renderer";

interface ActiveEntry {
  stroke: Stroke;
  // pen/markerは透明、不透明eraserは開始時の所属layer cacheを複製する。
  scratch: HTMLCanvasElement;
  nextSegment: number; // 次に描く確定済みセグメント番号 (1-origin)
}

const STAMP_RETRY_MIN_MS = 1_000;
const STAMP_RETRY_MAX_MS = 30_000;

type TimerHandle = number;

type StampImageEntry =
  | {
      state: "loading";
      image: HTMLImageElement;
      failureCount: number;
    }
  | { state: "retry_wait"; timer: TimerHandle; failureCount: number }
  | { state: "ready"; image: HTMLImageElement };

/** Canvas・Image・timerを決定的なテストへ差し替えるための最小実行環境。 */
export interface OverlayLayersRuntime {
  createCanvas(): HTMLCanvasElement;
  createImage(): HTMLImageElement;
  setTimeout(callback: () => void, delayMs: number): TimerHandle;
  clearTimeout(handle: TimerHandle): void;
}

const browserRuntime: OverlayLayersRuntime = {
  createCanvas: () => document.createElement("canvas"),
  createImage: () => new Image(),
  setTimeout: (callback, delayMs) => window.setTimeout(callback, delayMs),
  clearTimeout: (handle) => window.clearTimeout(handle),
};

function context2d(canvas: HTMLCanvasElement): CanvasRenderingContext2D {
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("2d context unavailable");
  return ctx;
}

function itemId(item: CanvasItem): string {
  return item.kind === "stroke" ? item.strokeId : item.itemId;
}

export class OverlayLayers {
  private actives = new Map<string, ActiveEntry>();
  private latestItems: CanvasItem[] = [];
  private latestLayers: CanvasLayer[] = [{ ...DEFAULT_LAYER }];
  private layerCanvases = new Map<string, HTMLCanvasElement>();
  private layerPreview: HTMLCanvasElement | null = null;
  private transformPrefix: HTMLCanvasElement | null = null;
  private stampImages = new Map<string, StampImageEntry>();
  private strokeCompositingScratch: HTMLCanvasElement | null = null;
  private transformPrefixItemId: string | null = null;
  private transformPrefixLayerId: string | null = null;
  private transformingItem: CanvasItem | null = null;
  private bakedVisible = true;
  private disposed = false;

  constructor(
    private baked: HTMLCanvasElement,
    private active: HTMLCanvasElement,
    private runtime: OverlayLayersRuntime = browserRuntime,
  ) {}

  get width(): number {
    return this.baked.width;
  }
  get height(): number {
    return this.baked.height;
  }

  resize(
    width: number,
    height: number,
    items: CanvasItem[],
    layers: CanvasLayer[] = this.latestLayers,
  ): void {
    const transformingItem = this.transformingItem;
    this.setDocument(items, layers);
    for (const canvas of [this.baked, this.active]) {
      canvas.width = width;
      canvas.height = height;
    }
    for (const canvas of this.layerCanvases.values()) {
      canvas.width = width;
      canvas.height = height;
    }
    if (this.layerPreview) {
      this.layerPreview.width = width;
      this.layerPreview.height = height;
    }
    if (this.transformPrefix) {
      this.transformPrefix.width = width;
      this.transformPrefix.height = height;
    }
    this.resizeStrokeCompositingScratch(width, height);
    this.rebuild(items, layers);
    if (transformingItem) {
      this.transformingItem = transformingItem;
      this.rebuildTransformPrefix(items, itemId(transformingItem));
      this.renderActive();
    }
  }

  setItems(items: CanvasItem[]): void {
    this.setDocument(items, this.latestLayers);
  }

  setDocument(items: CanvasItem[], layers: CanvasLayer[]): void {
    this.latestItems = items;
    this.latestLayers = layers;
    this.pruneUnusedStampLoads();
  }

  // 確定 CanvasItem 一覧から baked を全再構築する。
  rebuild(
    items: CanvasItem[],
    layers: CanvasLayer[] = this.latestLayers,
  ): void {
    this.prepareRebuild();
    this.setDocument(items, layers);
    this.rebuildBaked(items);
    this.renderActive();
  }

  /** 長期切断時に両表示canvasと描画中状態を透明へ戻す。 */
  clear(): void {
    this.rebuild([], this.latestLayers);
  }

  // WebSocket受信時点でtransform状態だけを記録する。描画はRAFまで行わないため、
  // coalescingを保ちつつ、RAF前のresizeでも対象を通常bakedへ焼き込まない。
  prepareItemPreview(item: CanvasItem): void {
    this.transformingItem = item;
  }

  // commit / undo / snapshot の受信時点でtransform状態を終了する。
  // 実際のrebuildがRAF待ちの間にresizeが発生してqueueが破棄されても、
  // resize側が古いpreviewをtransform中として再構築しないようにする。
  prepareRebuild(): void {
    this.transformingItem = null;
    this.transformPrefixItemId = null;
    this.transformPrefixLayerId = null;
  }

  // 最初のpreviewだけprefixを再構築し、以降はactive上のsuffixだけを更新する。
  previewStamp(stamp: StampItem, rebuildBaked: boolean): void {
    this.previewItem({ kind: "stamp", ...stamp }, rebuildBaked);
  }

  // transform previewでは対象以前をcacheし、対象と後続履歴を最大1frameごとに描く。
  previewItem(item: CanvasItem, rebuildBaked: boolean): void {
    const previous = this.transformingItem;
    this.transformingItem = item;
    if (
      rebuildBaked ||
      this.transformPrefixItemId !== itemId(item) ||
      this.transformPrefixLayerId !== item.layerId ||
      previous?.layerId !== item.layerId ||
      !this.transformPrefix
    ) {
      this.rebuildTransformPrefix(this.latestItems, itemId(item));
    }
  }

  /** component破棄後に画像callbackやretry timerがcanvasへ触れないよう停止する。 */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    for (const entry of this.stampImages.values()) {
      this.cancelStampImageEntry(entry);
    }
    for (const entry of this.actives.values()) {
      this.releaseCanvas(entry.scratch);
    }
    if (this.strokeCompositingScratch) {
      this.releaseCanvas(this.strokeCompositingScratch);
      this.strokeCompositingScratch = null;
    }
    if (this.layerPreview) {
      this.releaseCanvas(this.layerPreview);
      this.layerPreview = null;
    }
    if (this.transformPrefix) {
      this.releaseCanvas(this.transformPrefix);
      this.transformPrefix = null;
    }
    for (const canvas of this.layerCanvases.values()) {
      this.releaseCanvas(canvas);
    }
    this.layerCanvases.clear();
    this.stampImages.clear();
    this.actives.clear();
    this.latestItems = [];
    this.latestLayers = [{ ...DEFAULT_LAYER }];
    this.transformingItem = null;
    this.transformPrefixItemId = null;
    this.transformPrefixLayerId = null;
    this.bakedVisible = true;
  }

  private rebuildBaked(items: CanvasItem[]): void {
    this.latestItems = items;
    const neededLayers = new Set(
      items.filter((item) => item.done).map((item) => item.layerId),
    );
    for (const [layerId, canvas] of this.layerCanvases) {
      if (!neededLayers.has(layerId)) {
        this.releaseCanvas(canvas);
        this.layerCanvases.delete(layerId);
      }
    }
    for (const layer of this.latestLayers) {
      if (!neededLayers.has(layer.layerId)) continue;
      const canvas = this.getLayerCanvas(layer.layerId);
      const ctx = context2d(canvas);
      ctx.globalAlpha = 1;
      ctx.globalCompositeOperation = "source-over";
      ctx.clearRect(0, 0, this.width, this.height);
      for (const item of items) {
        if (item.done && item.layerId === layer.layerId) {
          this.compositeItem(ctx, item);
        }
      }
    }

    this.resetActiveItems(items);
    this.compositeLayerCanvases();
  }

  private rebuildTransformPrefix(
    items: CanvasItem[],
    transformedItemId: string,
  ): void {
    this.latestItems = items;
    const transformedIndex = items.findIndex(
      (item) => item.done && itemId(item) === transformedItemId,
    );
    if (transformedIndex < 0) {
      this.transformingItem = null;
      this.transformPrefixItemId = null;
      this.transformPrefixLayerId = null;
      this.rebuildBaked(items);
      return;
    }
    const transformed = items[transformedIndex];
    if (!transformed) return;
    this.transformPrefixItemId = transformedItemId;
    this.transformPrefixLayerId = transformed.layerId;
    const prefix = this.getTransformPrefix();
    const ctx = context2d(prefix);
    ctx.globalAlpha = 1;
    ctx.globalCompositeOperation = "source-over";
    ctx.clearRect(0, 0, this.width, this.height);
    for (const item of items.slice(0, transformedIndex)) {
      if (item.done && item.layerId === transformed.layerId) {
        this.compositeItem(ctx, item);
      }
    }
    this.resetActiveItems(items);
  }

  private resetActiveItems(items: CanvasItem[]): void {
    // 描画中ストロークをリセットして描き直す (undo 等で消えたものは捨てる)
    const alive = new Set(
      items
        .filter(
          (item): item is Extract<CanvasItem, { kind: "stroke" }> =>
            item.kind === "stroke" && !item.done,
        )
        .map((stroke) => stroke.strokeId),
    );
    for (const id of this.actives.keys()) {
      if (!alive.has(id)) this.deleteActive(id);
    }
    for (const item of items) {
      if (item.kind === "stroke" && !item.done) {
        const stroke = item;
        this.deleteActive(stroke.strokeId);
        this.beginActive(stroke);
        this.appendActive(stroke);
      }
    }
  }

  bakeItem(item: CanvasItem): void {
    const canvas = this.getLayerCanvas(item.layerId);
    this.compositeItem(context2d(canvas), item);
    this.compositeLayerCanvases();
  }

  beginActive(stroke: Stroke): void {
    if (this.actives.has(stroke.strokeId)) return;
    const scratch = this.runtime.createCanvas();
    scratch.width = this.width;
    scratch.height = this.height;
    if (stroke.brush.tool === "eraser") {
      const cached = this.layerCanvases.get(stroke.layerId);
      if (cached) context2d(scratch).drawImage(cached, 0, 0);
    }
    this.actives.set(stroke.strokeId, { stroke, scratch, nextSegment: 1 });
  }

  // 受信済み点列のうち確定済みセグメントを、所属レイヤー用scratchへ描き足す
  appendActive(stroke: Stroke): void {
    const entry = this.actives.get(stroke.strokeId);
    if (!entry) return;
    this.appendStable(entry, stroke);
    this.renderActive();
  }

  private appendStable(entry: ActiveEntry, stroke: Stroke): void {
    entry.stroke = stroke;
    const ctx = context2d(entry.scratch);
    ctx.globalAlpha = 1;
    ctx.globalCompositeOperation =
      stroke.brush.tool === "eraser" ? "destination-out" : "source-over";
    entry.nextSegment = drawStableIncrement(
      ctx,
      stroke,
      this.width,
      this.height,
      entry.nextSegment,
    );
    ctx.globalCompositeOperation = "source-over";
  }

  // stroke_end: 未描画tailだけをscratchへ足し、所属layer cacheへ確定する。
  bake(stroke: Stroke): void {
    const entry = this.actives.get(stroke.strokeId);
    const layer = this.getLayerCanvas(stroke.layerId);
    const layerCtx = context2d(layer);
    if (!entry) {
      this.compositeStroke(layerCtx, stroke);
    } else {
      this.appendStable(entry, stroke);
      const scratchCtx = context2d(entry.scratch);
      scratchCtx.globalCompositeOperation =
        stroke.brush.tool === "eraser" ? "destination-out" : "source-over";
      if (stroke.pts.length === 1) {
        drawFullStroke(scratchCtx, stroke, this.width, this.height);
      } else {
        const tail = tailSegment(
          stroke.pts,
          this.width,
          this.height,
          stroke.brush,
        );
        if (tail) drawSegments(scratchCtx, [tail], stroke.brush);
      }
      scratchCtx.globalCompositeOperation = "source-over";
      if (stroke.brush.tool === "eraser") {
        layerCtx.globalAlpha = 1;
        layerCtx.globalCompositeOperation = "source-over";
        layerCtx.clearRect(0, 0, this.width, this.height);
        layerCtx.drawImage(entry.scratch, 0, 0);
      } else {
        layerCtx.globalCompositeOperation = "source-over";
        layerCtx.globalAlpha = stroke.brush.opacity;
        layerCtx.drawImage(entry.scratch, 0, 0);
        layerCtx.globalAlpha = 1;
      }
    }
    this.deleteActive(stroke.strokeId);
    this.compositeLayerCanvases();
    this.renderActive();
  }

  cancelActive(strokeId: string): void {
    this.deleteActive(strokeId);
    this.renderActive();
  }

  // active レイヤーを合成し直す (クリア + 各 scratch を opacity 付きで転写)
  renderActive(): void {
    const ctx = context2d(this.active);
    ctx.globalAlpha = 1;
    ctx.globalCompositeOperation = "source-over";
    ctx.clearRect(0, 0, this.width, this.height);

    const hasLiveItems = this.latestItems.some((item) => !item.done);
    if (!this.transformingItem && !hasLiveItems) {
      if (!this.bakedVisible) this.compositeLayerCanvases();
      return;
    }

    const bakedCtx = context2d(this.baked);
    bakedCtx.globalAlpha = 1;
    bakedCtx.globalCompositeOperation = "source-over";
    bakedCtx.clearRect(0, 0, this.width, this.height);
    this.bakedVisible = false;

    const transformingItem = this.transformingItem;
    const transformedId = transformingItem ? itemId(transformingItem) : null;
    for (const layer of this.latestLayers) {
      const hasLayerLive = this.latestItems.some(
        (item) => item.layerId === layer.layerId && !item.done,
      );
      const transformsLayer = transformingItem?.layerId === layer.layerId;
      if (!hasLayerLive && !transformsLayer) {
        const cached = this.layerCanvases.get(layer.layerId);
        if (cached) ctx.drawImage(cached, 0, 0);
        continue;
      }

      const preview = this.getLayerPreview();
      const previewCtx = context2d(preview);
      previewCtx.globalAlpha = 1;
      previewCtx.globalCompositeOperation = "source-over";
      previewCtx.clearRect(0, 0, this.width, this.height);
      if (transformsLayer) {
        if (
          this.transformPrefix &&
          this.transformPrefixLayerId === layer.layerId
        ) {
          previewCtx.drawImage(this.transformPrefix, 0, 0);
        }
        const transformedIndex = this.latestItems.findIndex(
          (item) =>
            item.done &&
            transformedId !== null &&
            itemId(item) === transformedId,
        );
        const suffix =
          transformedIndex >= 0
            ? this.latestItems.slice(transformedIndex)
            : this.latestItems;
        for (const original of suffix) {
          if (original.layerId !== layer.layerId) continue;
          const item =
            transformedId !== null && itemId(original) === transformedId
              ? (transformingItem ?? original)
              : original;
          if (item.done) this.compositeItem(previewCtx, item);
          else this.renderLiveItem(previewCtx, item);
        }
      } else {
        const cached = this.layerCanvases.get(layer.layerId);
        const activeEraser = this.latestItems.some(
          (item) =>
            item.kind === "stroke" &&
            !item.done &&
            item.layerId === layer.layerId &&
            item.brush.tool === "eraser",
        );
        if (cached && !activeEraser) previewCtx.drawImage(cached, 0, 0);
        for (const item of this.latestItems) {
          if (item.layerId === layer.layerId && !item.done) {
            this.renderLiveItem(previewCtx, item);
          }
        }
      }
      ctx.drawImage(preview, 0, 0);
    }
  }

  private renderLiveItem(
    ctx: CanvasRenderingContext2D,
    item: CanvasItem,
  ): void {
    if (item.kind === "shape") {
      this.drawShape(ctx, item);
      return;
    }
    if (item.kind !== "stroke") return;
    const entry = this.actives.get(item.strokeId);
    if (entry) {
      ctx.globalAlpha =
        entry.stroke.brush.tool === "eraser" ? 1 : entry.stroke.brush.opacity;
      ctx.globalCompositeOperation = "source-over";
      ctx.drawImage(entry.scratch, 0, 0);
      ctx.globalAlpha = 1;
    }
  }

  private compositeItem(ctx: CanvasRenderingContext2D, item: CanvasItem): void {
    switch (item.kind) {
      case "stroke":
        this.compositeStroke(ctx, item);
        return;
      case "shape":
        this.drawShape(ctx, item);
        return;
      case "stamp":
        this.drawStamp(ctx, item);
        return;
    }
  }

  // 確定ストロークを不透明 scratch 経由で opacity 合成する
  private compositeStroke(ctx: CanvasRenderingContext2D, stroke: Stroke): void {
    if (stroke.brush.tool === "eraser") {
      ctx.globalCompositeOperation = "destination-out";
      drawFullStroke(ctx, stroke, this.width, this.height);
      ctx.globalCompositeOperation = "source-over";
      return;
    }
    if (stroke.brush.opacity >= 1) {
      drawFullStroke(ctx, stroke, this.width, this.height);
      return;
    }
    const scratch = this.getStrokeCompositingScratch();
    const scratchCtx = context2d(scratch);
    scratchCtx.globalAlpha = 1;
    scratchCtx.globalCompositeOperation = "source-over";
    scratchCtx.clearRect(0, 0, this.width, this.height);
    drawFullStroke(scratchCtx, stroke, this.width, this.height);
    ctx.globalAlpha = stroke.brush.opacity;
    ctx.drawImage(scratch, 0, 0);
    ctx.globalAlpha = 1;
  }

  private getStrokeCompositingScratch(): HTMLCanvasElement {
    if (!this.strokeCompositingScratch) {
      this.strokeCompositingScratch = this.runtime.createCanvas();
      this.strokeCompositingScratch.width = this.width;
      this.strokeCompositingScratch.height = this.height;
    }
    return this.strokeCompositingScratch;
  }

  private getLayerCanvas(layerId: string): HTMLCanvasElement {
    let canvas = this.layerCanvases.get(layerId);
    if (!canvas) {
      canvas = this.runtime.createCanvas();
      canvas.width = this.width;
      canvas.height = this.height;
      this.layerCanvases.set(layerId, canvas);
    }
    return canvas;
  }

  private getLayerPreview(): HTMLCanvasElement {
    if (!this.layerPreview) {
      this.layerPreview = this.runtime.createCanvas();
      this.layerPreview.width = this.width;
      this.layerPreview.height = this.height;
    }
    return this.layerPreview;
  }

  private getTransformPrefix(): HTMLCanvasElement {
    if (!this.transformPrefix) {
      this.transformPrefix = this.runtime.createCanvas();
      this.transformPrefix.width = this.width;
      this.transformPrefix.height = this.height;
    }
    return this.transformPrefix;
  }

  private compositeLayerCanvases(): void {
    const ctx = context2d(this.baked);
    ctx.globalAlpha = 1;
    ctx.globalCompositeOperation = "source-over";
    ctx.clearRect(0, 0, this.width, this.height);
    for (const layer of this.latestLayers) {
      const canvas = this.layerCanvases.get(layer.layerId);
      if (canvas) ctx.drawImage(canvas, 0, 0);
    }
    this.bakedVisible = true;
  }

  private resizeStrokeCompositingScratch(width: number, height: number): void {
    const scratch = this.strokeCompositingScratch;
    if (!scratch) return;
    if (scratch.width !== width) scratch.width = width;
    if (scratch.height !== height) scratch.height = height;
  }

  private releaseCanvas(canvas: HTMLCanvasElement): void {
    canvas.width = 0;
    canvas.height = 0;
  }

  private deleteActive(strokeId: string): void {
    const entry = this.actives.get(strokeId);
    if (entry) this.releaseCanvas(entry.scratch);
    this.actives.delete(strokeId);
  }

  private drawShape(ctx: CanvasRenderingContext2D, shape: ShapeItem): void {
    const transform = shape.transform;
    const start = transform
      ? { x: (-transform.widthN * this.width) / 2, y: 0 }
      : { x: shape.start[0] * this.width, y: shape.start[1] * this.height };
    const end = transform
      ? { x: (transform.widthN * this.width) / 2, y: 0 }
      : { x: shape.end[0] * this.width, y: shape.end[1] * this.height };
    const lineWidth = shape.style.widthN * this.height;

    ctx.save();
    if (transform) {
      ctx.translate(
        transform.center[0] * this.width,
        transform.center[1] * this.height,
      );
      ctx.rotate(transform.rotation);
    }
    ctx.globalCompositeOperation = "source-over";
    ctx.globalAlpha = shape.style.opacity;
    ctx.strokeStyle = shape.style.color;
    ctx.lineWidth = lineWidth;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.beginPath();

    switch (shape.shape) {
      case "line":
        ctx.moveTo(start.x, start.y);
        ctx.lineTo(end.x, end.y);
        break;
      case "arrow": {
        ctx.moveTo(start.x, start.y);
        ctx.lineTo(end.x, end.y);
        const dx = end.x - start.x;
        const dy = end.y - start.y;
        const length = Math.hypot(dx, dy);
        if (length > 0) {
          const angle = Math.atan2(dy, dx);
          const headLength = Math.min(
            length * 0.4,
            Math.max(lineWidth * 4, this.height * 0.02),
          );
          const spread = Math.PI / 6;
          ctx.moveTo(end.x, end.y);
          ctx.lineTo(
            end.x - headLength * Math.cos(angle - spread),
            end.y - headLength * Math.sin(angle - spread),
          );
          ctx.moveTo(end.x, end.y);
          ctx.lineTo(
            end.x - headLength * Math.cos(angle + spread),
            end.y - headLength * Math.sin(angle + spread),
          );
        }
        break;
      }
      case "rectangle":
        if (transform) {
          ctx.rect(
            (-transform.widthN * this.width) / 2,
            (-transform.heightN * this.height) / 2,
            transform.widthN * this.width,
            transform.heightN * this.height,
          );
        } else {
          ctx.rect(
            Math.min(start.x, end.x),
            Math.min(start.y, end.y),
            Math.abs(end.x - start.x),
            Math.abs(end.y - start.y),
          );
        }
        break;
      case "ellipse":
        ctx.ellipse(
          transform ? 0 : (start.x + end.x) / 2,
          transform ? 0 : (start.y + end.y) / 2,
          transform
            ? (transform.widthN * this.width) / 2
            : Math.abs(end.x - start.x) / 2,
          transform
            ? (transform.heightN * this.height) / 2
            : Math.abs(end.y - start.y) / 2,
          0,
          0,
          Math.PI * 2,
        );
        break;
    }
    ctx.stroke();
    ctx.restore();
  }

  private drawStamp(ctx: CanvasRenderingContext2D, stamp: StampItem): void {
    if (this.disposed) return;
    const entry = this.stampImages.get(stamp.stampId);
    if (entry?.state === "ready") {
      const width = stamp.widthN * this.width;
      const height = stamp.heightN * this.height;
      const centerX = stamp.center[0] * this.width;
      const centerY = stamp.center[1] * this.height;
      ctx.save();
      ctx.globalCompositeOperation = "source-over";
      ctx.globalAlpha = stamp.opacity;
      ctx.translate(centerX, centerY);
      ctx.rotate(stamp.rotation ?? 0);
      ctx.drawImage(entry.image, -width / 2, -height / 2, width, height);
      ctx.restore();
      return;
    }
    if (entry) return;

    this.startStampImageLoad(stamp.stampId, 0);
  }

  private startStampImageLoad(stampId: string, failureCount: number): void {
    if (this.disposed || !this.stampIsNeeded(stampId)) return;

    const pending = this.runtime.createImage();
    const loading: StampImageEntry = {
      state: "loading",
      image: pending,
      failureCount,
    };
    this.stampImages.set(stampId, loading);
    pending.decoding = "async";
    pending.onload = () => {
      if (this.disposed || this.stampImages.get(stampId) !== loading) return;
      pending.onload = null;
      pending.onerror = null;
      this.stampImages.set(stampId, { state: "ready", image: pending });
      this.redrawAfterStampLoad();
    };
    pending.onerror = () => {
      if (this.disposed || this.stampImages.get(stampId) !== loading) return;
      pending.onload = null;
      pending.onerror = null;
      const nextFailureCount = failureCount + 1;
      const delayMs = stampRetryDelay(nextFailureCount);
      const waiting: Extract<StampImageEntry, { state: "retry_wait" }> = {
        state: "retry_wait",
        timer: 0,
        failureCount: nextFailureCount,
      };
      this.stampImages.set(stampId, waiting);
      waiting.timer = this.runtime.setTimeout(() => {
        if (
          this.disposed ||
          this.stampImages.get(stampId) !== waiting ||
          !this.stampIsNeeded(stampId)
        ) {
          return;
        }
        this.stampImages.delete(stampId);
        this.startStampImageLoad(stampId, nextFailureCount);
      }, delayMs);
      console.warn(
        `overlay: failed to load stamp ${stampId}; retrying in ${delayMs}ms`,
      );
    };
    const retryQuery = failureCount > 0 ? `?retry=${failureCount}` : "";
    pending.src = `/stamps/${encodeURIComponent(stampId)}${retryQuery}`;
  }

  private redrawAfterStampLoad(): void {
    const transformingItem = this.transformingItem;
    this.rebuild(this.latestItems, this.latestLayers);
    if (transformingItem) {
      this.transformingItem = transformingItem;
      this.rebuildTransformPrefix(this.latestItems, itemId(transformingItem));
      this.renderActive();
    }
  }

  private stampIsNeeded(stampId: string): boolean {
    return (
      (this.transformingItem?.kind === "stamp" &&
        this.transformingItem.stampId === stampId) ||
      this.latestItems.some(
        (item) => item.kind === "stamp" && item.stampId === stampId,
      )
    );
  }

  private pruneUnusedStampLoads(): void {
    for (const [stampId, entry] of this.stampImages) {
      if (entry.state === "ready" || this.stampIsNeeded(stampId)) continue;
      this.cancelStampImageEntry(entry);
      this.stampImages.delete(stampId);
    }
  }

  private cancelStampImageEntry(entry: StampImageEntry): void {
    if (entry.state === "loading") {
      entry.image.onload = null;
      entry.image.onerror = null;
    } else if (entry.state === "retry_wait") {
      this.runtime.clearTimeout(entry.timer);
    }
  }
}

function stampRetryDelay(failureCount: number): number {
  const maxExponent = Math.ceil(
    Math.log2(STAMP_RETRY_MAX_MS / STAMP_RETRY_MIN_MS),
  );
  const exponent = Math.min(Math.max(0, failureCount - 1), maxExponent);
  return Math.min(STAMP_RETRY_MIN_MS * 2 ** exponent, STAMP_RETRY_MAX_MS);
}
