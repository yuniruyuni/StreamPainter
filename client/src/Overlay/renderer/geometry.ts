// ストローク幾何の純ロジック (docs/protocol.md)。
// painter (Rust/Direct2D) と同一の見た目になるよう、この仕様はプロトコルと一致させる。
//
// 中点法二次ベジェ: moveTo(P0) → quadTo(P1, mid(P1,P2)) → quadTo(P2, mid(P2,P3)) → …
// を「制御点 P[j] を中心とするセグメント」単位に分解して扱う。
//   セグメント j (1 <= j <= n-2):
//     from = (j == 1 ? P0 : mid(P[j-1], P[j])), ctrl = P[j], to = mid(P[j], P[j+1])
//   末尾 (描画確定時のみ): mid(P[n-2], P[n-1]) → P[n-1] の直線
// セグメント j は P[j+1] が到着した時点で形が確定する (以降変化しない) ため、
// 描画中は確定済みセグメントだけを増分描画し、末尾は stroke_end 時の全描画で足す。

import type { Brush, StrokePoint } from "~/protocol";

export interface Vec2 {
  x: number;
  y: number;
}

export interface Segment {
  from: Vec2;
  ctrl: Vec2;
  to: Vec2;
  width: number; // px
}

// 筆圧 → 線幅。セグメント幅は制御点の筆圧で決める
export function strokeWidth(
  brush: Brush,
  pressure: number,
  tiltX: number,
  tiltY: number,
  canvasHeight: number,
): number {
  const base = brush.widthN * canvasHeight;
  const safePressure = Number.isFinite(pressure)
    ? Math.min(1, Math.max(0, pressure))
    : 1;
  const minimum = Number.isFinite(brush.pressureMin)
    ? Math.min(1, Math.max(0.05, brush.pressureMin))
    : 1;
  const pressureScale = brush.pressureWidth
    ? minimum + (1 - minimum) * safePressure
    : 1;
  const safeTiltX = Number.isFinite(tiltX)
    ? Math.min(1, Math.max(-1, tiltX))
    : 0;
  const safeTiltY = Number.isFinite(tiltY)
    ? Math.min(1, Math.max(-1, tiltY))
    : 0;
  const maxTiltScale = Number.isFinite(brush.tiltMaxScale)
    ? Math.min(4, Math.max(1, brush.tiltMaxScale))
    : 1;
  const tiltMagnitude = Math.min(1, Math.hypot(safeTiltX, safeTiltY));
  const tiltScale = brush.tiltWidth
    ? 1 + (maxTiltScale - 1) * tiltMagnitude
    : 1;
  return base * pressureScale * tiltScale;
}

function pos(pt: StrokePoint, w: number, h: number): Vec2 {
  return { x: pt[0] * w, y: pt[1] * h };
}

function mid(a: Vec2, b: Vec2): Vec2 {
  return { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
}

// n 点のうち形が確定しているセグメント数 (j = 1..n-2)
export function stableSegmentCount(pointCount: number): number {
  return Math.max(0, pointCount - 2);
}

// fromSegment (1-origin) 以降の確定済みセグメントを返す
export function stableSegments(
  pts: readonly StrokePoint[],
  canvasWidth: number,
  canvasHeight: number,
  brush: Brush,
  fromSegment = 1,
): Segment[] {
  const segments: Segment[] = [];
  for (let j = Math.max(1, fromSegment); j <= pts.length - 2; j++) {
    const prev = pts[j - 1] as StrokePoint;
    const curr = pts[j] as StrokePoint;
    const next = pts[j + 1] as StrokePoint;
    const prevPos = pos(prev, canvasWidth, canvasHeight);
    const currPos = pos(curr, canvasWidth, canvasHeight);
    const nextPos = pos(next, canvasWidth, canvasHeight);
    segments.push({
      from: j === 1 ? prevPos : mid(prevPos, currPos),
      ctrl: currPos,
      to: mid(currPos, nextPos),
      width: strokeWidth(brush, curr[2], curr[4], curr[5], canvasHeight),
    });
  }
  return segments;
}

// 末尾セグメント (確定時のみ描画)。2 点未満は tail なし (dot は別扱い)
export function tailSegment(
  pts: readonly StrokePoint[],
  canvasWidth: number,
  canvasHeight: number,
  brush: Brush,
): Segment | null {
  const n = pts.length;
  if (n < 2) return null;
  const last = pts[n - 1] as StrokePoint;
  const prev = pts[n - 2] as StrokePoint;
  const lastPos = pos(last, canvasWidth, canvasHeight);
  const prevPos = pos(prev, canvasWidth, canvasHeight);
  const from = n === 2 ? prevPos : mid(prevPos, lastPos);
  return {
    from,
    ctrl: lastPos,
    to: lastPos,
    width: strokeWidth(brush, last[2], last[4], last[5], canvasHeight),
  };
}

// 1 点ストローク: round cap の点
export function dot(
  pts: readonly StrokePoint[],
  canvasWidth: number,
  canvasHeight: number,
  brush: Brush,
): { center: Vec2; radius: number } | null {
  if (pts.length !== 1) return null;
  const pt = pts[0] as StrokePoint;
  return {
    center: pos(pt, canvasWidth, canvasHeight),
    radius: strokeWidth(brush, pt[2], pt[4], pt[5], canvasHeight) / 2,
  };
}

// ストローク全体のセグメント列 (確定描画用)
export function fullSegments(
  pts: readonly StrokePoint[],
  canvasWidth: number,
  canvasHeight: number,
  brush: Brush,
): Segment[] {
  const segments = stableSegments(pts, canvasWidth, canvasHeight, brush);
  const tail = tailSegment(pts, canvasWidth, canvasHeight, brush);
  if (tail) segments.push(tail);
  return segments;
}
