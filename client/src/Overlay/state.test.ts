import { describe, expect, test } from "bun:test";
import type { Brush, Stroke } from "~/protocol";
import { MAX_STROKES } from "~/protocol";
import { OverlayState } from "./state";

const brush: Brush = {
  tool: "pen",
  color: "#ff4d6d",
  opacity: 1,
  widthN: 0.005,
  pressureWidth: true,
};

function doneStroke(id: string): Stroke {
  return {
    strokeId: id,
    brush,
    pts: [[0.1, 0.1, 0.5, 0]],
    done: true,
    endedAt: 100,
  };
}

describe("OverlayState", () => {
  test("snapshot は状態を置換し rebuild を返す", () => {
    const state = new OverlayState();
    const effect = state.apply({
      type: "snapshot",
      rev: 3,
      fadeAfterMs: null,
      strokes: [doneStroke("s1")],
    });
    expect(effect.kind).toBe("rebuild");
    expect(state.strokes).toHaveLength(1);
    expect(state.rev).toBe(3);
  });

  test("begin → points → end のライフサイクル", () => {
    const state = new OverlayState();
    const begin = state.apply({ type: "stroke_begin", strokeId: "s1", brush });
    expect(begin.kind).toBe("active");

    const points = state.apply({
      type: "stroke_points",
      strokeId: "s1",
      pts: [
        [0.1, 0.1, 0.5, 0],
        [0.2, 0.2, 0.6, 16],
      ],
    });
    expect(points.kind).toBe("active");
    expect(state.activeStrokes()[0]?.pts).toHaveLength(2);

    const end = state.apply({
      type: "stroke_end",
      strokeId: "s1",
      endedAt: 1234,
    });
    expect(end.kind).toBe("bake");
    expect(state.doneStrokes()).toHaveLength(1);
    expect(state.doneStrokes()[0]?.endedAt).toBe(1234);
  });

  test("未知の strokeId への points は none", () => {
    const state = new OverlayState();
    const effect = state.apply({
      type: "stroke_points",
      strokeId: "unknown",
      pts: [[0, 0, 0.5, 0]],
    });
    expect(effect.kind).toBe("none");
  });

  test("undo は最後の確定ストロークを削除し rebuild", () => {
    const state = new OverlayState();
    state.apply({
      type: "snapshot",
      rev: 1,
      fadeAfterMs: null,
      strokes: [doneStroke("s1"), doneStroke("s2")],
    });
    const effect = state.apply({ type: "undo" });
    expect(effect.kind).toBe("rebuild");
    expect(state.strokes.map((s) => s.strokeId)).toEqual(["s1"]);
  });

  test("確定ストロークがなければ undo は none", () => {
    const state = new OverlayState();
    expect(state.apply({ type: "undo" }).kind).toBe("none");
  });

  test("clear は全消去して rebuild", () => {
    const state = new OverlayState();
    state.apply({
      type: "snapshot",
      rev: 1,
      fadeAfterMs: null,
      strokes: [doneStroke("s1")],
    });
    expect(state.apply({ type: "clear" }).kind).toBe("rebuild");
    expect(state.strokes).toHaveLength(0);
  });

  test("eraser の cancel は rebuild になる", () => {
    const state = new OverlayState();
    state.apply({
      type: "stroke_begin",
      strokeId: "e1",
      brush: { ...brush, tool: "eraser" },
    });
    expect(state.apply({ type: "stroke_cancel", strokeId: "e1" }).kind).toBe(
      "rebuild",
    );
  });

  test("pen の cancel は active layer の破棄を指示する", () => {
    const state = new OverlayState();
    state.apply({ type: "stroke_begin", strokeId: "s1", brush });
    expect(state.apply({ type: "stroke_cancel", strokeId: "s1" })).toEqual({
      kind: "cancel",
      strokeId: "s1",
    });
  });

  test("ローカルハブと同じ規則でトリムする (本数上限)", () => {
    const state = new OverlayState();
    state.apply({
      type: "snapshot",
      rev: 1,
      fadeAfterMs: null,
      strokes: Array.from({ length: MAX_STROKES }, (_, i) =>
        doneStroke(`s${i}`),
      ),
    });
    const effect = state.apply({
      type: "stroke_begin",
      strokeId: "new",
      brush,
    });
    expect(effect.kind).toBe("rebuild"); // 古い確定ストロークが落ちた
    expect(state.strokes).toHaveLength(MAX_STROKES);
    expect(state.strokes.some((s) => s.strokeId === "s0")).toBe(false);
  });
});
