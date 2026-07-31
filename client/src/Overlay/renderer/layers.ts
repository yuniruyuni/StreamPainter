// レイヤー管理 (docs/webapp.md)。
//
// - baked: 確定ストロークを焼き込んだ表示用キャンバス (下層)
// - active: 描画中ストロークの表示用キャンバス (上層)
// - 各描画中ストロークは専用 scratch (オフスクリーン) に不透明で増分描画し、
//   フレームごとに active へ globalAlpha = opacity で合成する。
// - eraser は baked へ直接 destination-out で増分適用する。
// - undo / clear / snapshot / トリム時は strokes 一覧から baked を全再構築する。

import type { CanvasItem, ShapeItem, StampItem, Stroke } from "~/protocol";
import { stableSegments, tailSegment } from "./geometry";
import {
  drawFullStroke,
  drawSegments,
  drawStableIncrement,
} from "./stroke-renderer";

interface ActiveEntry {
  stroke: Stroke;
  scratch: HTMLCanvasElement | null; // eraser は scratch を持たない
  nextSegment: number; // 次に描く確定済みセグメント番号 (1-origin)
}

function context2d(canvas: HTMLCanvasElement): CanvasRenderingContext2D {
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("2d context unavailable");
  return ctx;
}

export class OverlayLayers {
  private actives = new Map<string, ActiveEntry>();
  private latestItems: CanvasItem[] = [];
  private stampImages = new Map<string, HTMLImageElement | null>();

  constructor(
    private baked: HTMLCanvasElement,
    private active: HTMLCanvasElement,
  ) {}

  get width(): number {
    return this.baked.width;
  }
  get height(): number {
    return this.baked.height;
  }

  resize(width: number, height: number, items: CanvasItem[]): void {
    for (const canvas of [this.baked, this.active]) {
      canvas.width = width;
      canvas.height = height;
    }
    this.rebuild(items);
  }

  setItems(items: CanvasItem[]): void {
    this.latestItems = items;
  }

  // 確定 CanvasItem 一覧から baked を全再構築する。
  rebuild(items: CanvasItem[]): void {
    this.latestItems = items;
    const ctx = context2d(this.baked);
    ctx.globalCompositeOperation = "source-over";
    ctx.clearRect(0, 0, this.width, this.height);

    for (const item of items) {
      if (item.done) {
        this.compositeItem(ctx, item);
      }
    }

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
      if (!alive.has(id)) this.actives.delete(id);
    }
    for (const item of items) {
      if (item.kind === "stroke" && !item.done) {
        const stroke = item;
        this.actives.delete(stroke.strokeId);
        this.beginActive(stroke);
        this.appendActive(stroke);
      }
    }
    this.renderActive();
  }

  beginActive(stroke: Stroke): void {
    if (this.actives.has(stroke.strokeId)) return;
    let scratch: HTMLCanvasElement | null = null;
    if (stroke.brush.tool !== "eraser") {
      scratch = document.createElement("canvas");
      scratch.width = this.width;
      scratch.height = this.height;
    }
    this.actives.set(stroke.strokeId, { stroke, scratch, nextSegment: 1 });
  }

  // 受信済み点列のうち確定済みセグメントを scratch (eraser は baked) へ描き足す
  appendActive(stroke: Stroke): void {
    const entry = this.actives.get(stroke.strokeId);
    if (!entry) return;
    entry.stroke = stroke;
    if (entry.scratch) {
      const ctx = context2d(entry.scratch);
      entry.nextSegment = drawStableIncrement(
        ctx,
        stroke,
        this.width,
        this.height,
        entry.nextSegment,
      );
    } else {
      // eraser: baked へ直接 destination-out で増分適用
      const ctx = context2d(this.baked);
      ctx.globalCompositeOperation = "destination-out";
      const segments = stableSegments(
        stroke.pts,
        this.width,
        this.height,
        stroke.brush,
        entry.nextSegment,
      );
      drawSegments(ctx, segments, stroke.brush);
      entry.nextSegment += segments.length;
      ctx.globalCompositeOperation = "source-over";
    }
  }

  // stroke_end: 全体を描き直して baked へ合成し、active から外す
  bake(stroke: Stroke): void {
    const entry = this.actives.get(stroke.strokeId);
    const ctx = context2d(this.baked);

    if (stroke.brush.tool === "eraser" && entry) {
      // 未適用の確定済みセグメントを適用してから末尾を足す
      this.appendActive(stroke);
      ctx.globalCompositeOperation = "destination-out";
      if (stroke.pts.length === 1) {
        drawFullStroke(ctx, stroke, this.width, this.height);
      } else {
        const tail = tailSegment(
          stroke.pts,
          this.width,
          this.height,
          stroke.brush,
        );
        if (tail) drawSegments(ctx, [tail], stroke.brush);
      }
      ctx.globalCompositeOperation = "source-over";
    } else {
      // pen / marker、または begin を経ていないストロークは全体を描き直す
      this.compositeStroke(ctx, stroke);
    }
    this.actives.delete(stroke.strokeId);
    this.renderActive();
  }

  cancelActive(strokeId: string): void {
    this.actives.delete(strokeId);
    this.renderActive();
  }

  // active レイヤーを合成し直す (クリア + 各 scratch を opacity 付きで転写)
  renderActive(): void {
    const ctx = context2d(this.active);
    ctx.clearRect(0, 0, this.width, this.height);
    for (const entry of this.actives.values()) {
      if (!entry.scratch) continue;
      ctx.globalAlpha = entry.stroke.brush.opacity;
      ctx.drawImage(entry.scratch, 0, 0);
      ctx.globalAlpha = 1;
    }
    for (const item of this.latestItems) {
      if (item.kind === "shape" && !item.done) {
        this.drawShape(ctx, item);
      }
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
    const scratch = document.createElement("canvas");
    scratch.width = this.width;
    scratch.height = this.height;
    drawFullStroke(context2d(scratch), stroke, this.width, this.height);
    ctx.globalAlpha = stroke.brush.opacity;
    ctx.drawImage(scratch, 0, 0);
    ctx.globalAlpha = 1;
  }

  private drawShape(ctx: CanvasRenderingContext2D, shape: ShapeItem): void {
    const start = {
      x: shape.start[0] * this.width,
      y: shape.start[1] * this.height,
    };
    const end = {
      x: shape.end[0] * this.width,
      y: shape.end[1] * this.height,
    };
    const lineWidth = shape.style.widthN * this.height;

    ctx.save();
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
        ctx.rect(
          Math.min(start.x, end.x),
          Math.min(start.y, end.y),
          Math.abs(end.x - start.x),
          Math.abs(end.y - start.y),
        );
        break;
      case "ellipse":
        ctx.ellipse(
          (start.x + end.x) / 2,
          (start.y + end.y) / 2,
          Math.abs(end.x - start.x) / 2,
          Math.abs(end.y - start.y) / 2,
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
    const image = this.stampImages.get(stamp.stampId);
    if (image) {
      const width = stamp.widthN * this.width;
      const height = stamp.heightN * this.height;
      const centerX = stamp.center[0] * this.width;
      const centerY = stamp.center[1] * this.height;
      ctx.save();
      ctx.globalCompositeOperation = "source-over";
      ctx.globalAlpha = stamp.opacity;
      ctx.drawImage(
        image,
        centerX - width / 2,
        centerY - height / 2,
        width,
        height,
      );
      ctx.restore();
      return;
    }
    if (this.stampImages.has(stamp.stampId)) return;

    this.stampImages.set(stamp.stampId, null);
    const pending = new Image();
    pending.decoding = "async";
    pending.onload = () => {
      this.stampImages.set(stamp.stampId, pending);
      this.rebuild(this.latestItems);
    };
    pending.onerror = () => {
      console.warn(`overlay: failed to load stamp ${stamp.stampId}`);
    };
    pending.src = `/stamps/${encodeURIComponent(stamp.stampId)}`;
  }
}
