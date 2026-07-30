// レイヤー管理 (docs/webapp.md)。
//
// - baked: 確定ストロークを焼き込んだ表示用キャンバス (下層)
// - active: 描画中ストロークの表示用キャンバス (上層)
// - 各描画中ストロークは専用 scratch (オフスクリーン) に不透明で増分描画し、
//   フレームごとに active へ globalAlpha = opacity で合成する。
// - eraser は baked へ直接 destination-out で増分適用する。
// - undo / clear / snapshot / トリム時は strokes 一覧から baked を全再構築する。

import type { Stroke } from "~/protocol";
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

  resize(width: number, height: number, strokes: Stroke[]): void {
    for (const canvas of [this.baked, this.active]) {
      canvas.width = width;
      canvas.height = height;
    }
    this.rebuild(strokes);
  }

  // 確定ストローク一覧から baked を全再構築する。描画中ストロークの scratch も引き直す
  rebuild(strokes: Stroke[]): void {
    const ctx = context2d(this.baked);
    ctx.globalCompositeOperation = "source-over";
    ctx.clearRect(0, 0, this.width, this.height);

    for (const stroke of strokes) {
      if (stroke.done) {
        this.compositeStroke(ctx, stroke);
      }
    }

    // 描画中ストロークをリセットして描き直す (undo 等で消えたものは捨てる)
    const alive = new Set(
      strokes.filter((s) => !s.done).map((s) => s.strokeId),
    );
    for (const id of this.actives.keys()) {
      if (!alive.has(id)) this.actives.delete(id);
    }
    for (const stroke of strokes) {
      if (!stroke.done) {
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
}
