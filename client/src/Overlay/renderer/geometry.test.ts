import { describe, expect, test } from "bun:test";
import type { Brush, StrokePoint } from "~/protocol";
import {
  dot,
  fullSegments,
  stableSegmentCount,
  stableSegments,
  strokeWidth,
  tailSegment,
} from "./geometry";

const brush: Brush = {
  tool: "pen",
  color: "#ff4d6d",
  opacity: 1,
  widthN: 0.01,
  pressureWidth: true,
};

const pts: StrokePoint[] = [
  [0.0, 0.0, 0.5, 0],
  [0.1, 0.0, 0.5, 16],
  [0.2, 0.0, 0.5, 32],
  [0.3, 0.0, 0.5, 48],
];

describe("strokeWidth", () => {
  test("筆圧が線幅に反映される", () => {
    // w(p) = widthN * height * (0.35 + 0.65p)
    expect(strokeWidth(brush, 1, 1000)).toBeCloseTo(10);
    expect(strokeWidth(brush, 0, 1000)).toBeCloseTo(3.5);
    expect(strokeWidth(brush, 0.5, 1000)).toBeCloseTo(6.75);
  });

  test("pressureWidth: false なら常に基準幅", () => {
    const fixed = { ...brush, pressureWidth: false };
    expect(strokeWidth(fixed, 0, 1000)).toBeCloseTo(10);
    expect(strokeWidth(fixed, 1, 1000)).toBeCloseTo(10);
  });
});

describe("stableSegments", () => {
  test("n 点で n-2 個の確定セグメント", () => {
    expect(stableSegmentCount(4)).toBe(2);
    expect(stableSegments(pts, 1000, 1000, brush)).toHaveLength(2);
    expect(stableSegmentCount(2)).toBe(0);
    expect(stableSegments(pts.slice(0, 2), 1000, 1000, brush)).toHaveLength(0);
  });

  test("最初のセグメントは P0 から始まり、以降は中点から", () => {
    const [s1, s2] = stableSegments(pts, 1000, 1000, brush);
    expect(s1?.from).toEqual({ x: 0, y: 0 }); // P0
    expect(s1?.ctrl).toEqual({ x: 100, y: 0 }); // P1
    expect(s1?.to).toEqual({ x: 150, y: 0 }); // mid(P1,P2)
    expect(s2?.from).toEqual({ x: 150, y: 0 }); // mid(P1,P2) — 連続
    expect(s2?.to).toEqual({ x: 250, y: 0 }); // mid(P2,P3)
  });

  test("fromSegment 以降の増分が全体描画と一致する", () => {
    const all = stableSegments(pts, 1000, 1000, brush);
    const first = stableSegments(pts.slice(0, 3), 1000, 1000, brush);
    const rest = stableSegments(pts, 1000, 1000, brush, first.length + 1);
    expect([...first, ...rest]).toEqual(all);
  });

  test("10,000点でも1点更新は新規1segmentだけで全体幾何と一致する", () => {
    const source: StrokePoint[] = Array.from({ length: 10_000 }, (_, index) => [
      index / 9_999,
      ((index * 37) % 997) / 996,
      (index % 101) / 100,
      index * 0.25,
    ]);
    const received: StrokePoint[] = [];
    const incremental: ReturnType<typeof stableSegments> = [];
    let nextSegment = 1;

    for (const point of source) {
      received.push(point);
      const added = stableSegments(received, 3_840, 2_160, brush, nextSegment);
      expect(added.length).toBeLessThanOrEqual(1);
      nextSegment += added.length;
      incremental.push(...added);
    }

    expect(incremental).toEqual(stableSegments(source, 3_840, 2_160, brush));
    expect(nextSegment).toBe(stableSegmentCount(source.length) + 1);
  });
});

describe("tailSegment / dot / fullSegments", () => {
  test("末尾セグメントは最後の中点から終点まで", () => {
    const tail = tailSegment(pts, 1000, 1000, brush);
    expect(tail?.from).toEqual({ x: 250, y: 0 }); // mid(P2,P3)
    expect(tail?.to).toEqual({ x: 300, y: 0 }); // P3
  });

  test("2 点なら P0 → P1 の直線", () => {
    const tail = tailSegment(pts.slice(0, 2), 1000, 1000, brush);
    expect(tail?.from).toEqual({ x: 0, y: 0 });
    expect(tail?.to).toEqual({ x: 100, y: 0 });
  });

  test("1 点は dot になる", () => {
    expect(tailSegment(pts.slice(0, 1), 1000, 1000, brush)).toBeNull();
    const d = dot(pts.slice(0, 1), 1000, 1000, brush);
    expect(d?.center).toEqual({ x: 0, y: 0 });
    expect(d?.radius).toBeCloseTo(strokeWidth(brush, 0.5, 1000) / 2);
  });

  test("fullSegments = 確定分 + 末尾", () => {
    expect(fullSegments(pts, 1000, 1000, brush)).toHaveLength(3);
  });
});
