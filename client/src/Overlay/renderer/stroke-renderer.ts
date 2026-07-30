// Canvas 2D へのストローク描画 (docs/protocol.md)。
// 幾何は geometry.ts の純ロジックに委譲し、ここでは Canvas API 呼び出しのみ行う。

import type { Brush, Stroke } from "~/protocol";
import { dot, fullSegments, type Segment, stableSegments } from "./geometry";

export function drawSegments(
  ctx: CanvasRenderingContext2D,
  segments: Segment[],
  brush: Brush,
): void {
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  // 不透明度はストローク単位でレイヤー合成時に適用するため、ここでは常に不透明で描く
  ctx.strokeStyle = brush.tool === "eraser" ? "#000000" : brush.color;
  for (const seg of segments) {
    ctx.lineWidth = seg.width;
    ctx.beginPath();
    ctx.moveTo(seg.from.x, seg.from.y);
    ctx.quadraticCurveTo(seg.ctrl.x, seg.ctrl.y, seg.to.x, seg.to.y);
    ctx.stroke();
  }
}

// ストローク全体を不透明で描く (確定描画 / rebuild 用)
export function drawFullStroke(
  ctx: CanvasRenderingContext2D,
  stroke: Stroke,
  width: number,
  height: number,
): void {
  const d = dot(stroke.pts, width, height, stroke.brush);
  if (d) {
    ctx.fillStyle =
      stroke.brush.tool === "eraser" ? "#000000" : stroke.brush.color;
    ctx.beginPath();
    ctx.arc(d.center.x, d.center.y, d.radius, 0, Math.PI * 2);
    ctx.fill();
    return;
  }
  drawSegments(
    ctx,
    fullSegments(stroke.pts, width, height, stroke.brush),
    stroke.brush,
  );
}

// 描画中ストロークの増分描画: fromSegment 以降の確定済みセグメントを描き足す。
// 戻り値は次回の fromSegment (= 描画済みセグメント数 + 1)
export function drawStableIncrement(
  ctx: CanvasRenderingContext2D,
  stroke: Stroke,
  width: number,
  height: number,
  fromSegment: number,
): number {
  const segments = stableSegments(
    stroke.pts,
    width,
    height,
    stroke.brush,
    fromSegment,
  );
  drawSegments(ctx, segments, stroke.brush);
  return fromSegment + segments.length;
}
