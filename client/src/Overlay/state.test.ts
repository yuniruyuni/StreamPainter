import { describe, expect, test } from "bun:test";
import type {
  Brush,
  CanvasItem,
  PaintEvent,
  RevisionedPaintEvent,
  Stroke,
} from "~/protocol";
import { MAX_STROKES, PROTOCOL_VERSION } from "~/protocol";
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

function strokeItem(id: string): CanvasItem {
  return { kind: "stroke", ...doneStroke(id) };
}

function synchronizedState(items: CanvasItem[] = [], rev = 0): OverlayState {
  const state = new OverlayState();
  expect(
    state.apply({
      type: "snapshot",
      protocolVersion: PROTOCOL_VERSION,
      rev,
      fadeAfterMs: null,
      items,
    }).kind,
  ).toBe("rebuild");
  return state;
}

function applyNext(state: OverlayState, event: PaintEvent) {
  return state.apply({
    ...event,
    rev: state.rev + 1,
  } as RevisionedPaintEvent);
}

describe("OverlayState", () => {
  test("snapshot は状態を置換し rebuild を返す", () => {
    const state = synchronizedState([strokeItem("s1")], 3);
    expect(state.strokes).toHaveLength(1);
    expect(state.rev).toBe(3);
  });

  test("resetは履歴と同期状態を破棄し、次のsnapshotから復元する", () => {
    const state = synchronizedState([strokeItem("old")], 3);
    state.reset();
    expect(state.items).toEqual([]);
    expect(state.fadeAfterMs).toBeNull();
    expect(state.rev).toBe(0);
    expect(
      state.apply({ type: "clear", rev: 4 } as RevisionedPaintEvent).kind,
    ).toBe("resync");

    expect(
      state.apply({
        type: "snapshot",
        protocolVersion: PROTOCOL_VERSION,
        rev: 8,
        fadeAfterMs: 5_000,
        items: [strokeItem("new")],
      }).kind,
    ).toBe("rebuild");
    expect(state.items).toEqual([strokeItem("new")]);
    expect(state.fadeAfterMs).toBe(5_000);
    expect(state.rev).toBe(8);
  });

  test("begin → points → end のライフサイクル", () => {
    const state = synchronizedState();
    const begin = applyNext(state, {
      type: "stroke_begin",
      strokeId: "s1",
      brush,
    });
    expect(begin.kind).toBe("active");

    const points = applyNext(state, {
      type: "stroke_points",
      strokeId: "s1",
      pts: [
        [0.1, 0.1, 0.5, 0],
        [0.2, 0.2, 0.6, 16],
      ],
    });
    expect(points.kind).toBe("active");
    expect(state.activeStrokes()[0]?.pts).toHaveLength(2);

    const end = applyNext(state, {
      type: "stroke_end",
      strokeId: "s1",
      endedAt: 1234,
    });
    expect(end.kind).toBe("bake");
    expect(state.doneStrokes()).toHaveLength(1);
    expect(state.doneStrokes()[0]?.endedAt).toBe(1234);
  });

  test("未知の strokeId への points は none", () => {
    const state = synchronizedState();
    const effect = applyNext(state, {
      type: "stroke_points",
      strokeId: "unknown",
      pts: [[0, 0, 0.5, 0]],
    });
    expect(effect.kind).toBe("none");
    expect(state.rev).toBe(1);
  });

  test("図形は preview 更新後に確定される", () => {
    const state = synchronizedState();
    const shape = {
      itemId: "shape-1",
      shape: "arrow" as const,
      style: { color: "#ffffff", opacity: 1, widthN: 0.005 },
      start: [0.1, 0.2] as [number, number],
      end: [0.1, 0.2] as [number, number],
      done: false,
      endedAt: null,
    };
    expect(applyNext(state, { type: "shape_begin", shape }).kind).toBe(
      "preview",
    );
    expect(
      applyNext(state, {
        type: "shape_update",
        itemId: "shape-1",
        end: [0.8, 0.7],
      }).kind,
    ).toBe("preview");
    expect(state.items[0]).toMatchObject({ end: [0.8, 0.7], done: false });
    expect(
      applyNext(state, {
        type: "shape_end",
        itemId: "shape-1",
        endedAt: 123,
      }).kind,
    ).toBe("bake_item");
    expect(state.items[0]).toMatchObject({ done: true, endedAt: 123 });
  });

  test("v5 snapshot と undo はストローク・スタンプ共通の描画順を使う", () => {
    const state = synchronizedState(
      [
        strokeItem("s1"),
        {
          kind: "stamp",
          itemId: "stamp-item-1",
          stampId: "stamp-1",
          center: [0.5, 0.5],
          widthN: 0.1,
          heightN: 0.2,
          opacity: 1,
          done: true,
          endedAt: 200,
        },
      ],
      4,
    );
    expect(state.items).toHaveLength(2);
    expect(state.strokes.map((stroke) => stroke.strokeId)).toEqual(["s1"]);
    expect(applyNext(state, { type: "undo" }).kind).toBe("rebuild");
    expect(state.items).toHaveLength(1);
    expect(state.items[0]?.kind).toBe("stroke");
  });

  test("連続する stamp_move_preview は初回だけbaked再構築し確定時に戻す", () => {
    const state = synchronizedState([
      {
        kind: "stamp",
        itemId: "stamp-item-1",
        stampId: "stamp-1",
        center: [0.2, 0.3],
        widthN: 0.1,
        heightN: 0.2,
        opacity: 1,
        done: true,
        endedAt: 200,
      },
      strokeItem("s1"),
    ]);

    const first = applyNext(state, {
      type: "stamp_move_preview",
      itemId: "stamp-item-1",
      center: [0.5, 0.45],
    });
    expect(first.kind).toBe("stamp_preview");
    if (first.kind === "stamp_preview") expect(first.rebuildBaked).toBe(true);

    const second = applyNext(state, {
      type: "stamp_move_preview",
      itemId: "stamp-item-1",
      center: [0.75, 0.6],
    });
    expect(second.kind).toBe("stamp_preview");
    if (second.kind === "stamp_preview") {
      expect(second.rebuildBaked).toBe(false);
      expect(second.stamp.center).toEqual([0.75, 0.6]);
    }

    expect(
      applyNext(state, {
        type: "stamp_move",
        itemId: "stamp-item-1",
        center: [0.8, 0.65],
      }).kind,
    ).toBe("rebuild");
    expect(state.items[0]).toMatchObject({ center: [0.8, 0.65] });
    expect(state.items[1]).toEqual(strokeItem("s1"));
  });

  test("未知の stamp_move は状態を変えない", () => {
    const state = synchronizedState([strokeItem("s1")]);
    expect(
      applyNext(state, {
        type: "stamp_move",
        itemId: "missing",
        center: [0.75, 0.6],
      }).kind,
    ).toBe("none");
    expect(state.items).toEqual([strokeItem("s1")]);
  });

  test("undo は最後の確定ストロークを削除し rebuild", () => {
    const state = synchronizedState([strokeItem("s1"), strokeItem("s2")], 1);
    const effect = applyNext(state, { type: "undo" });
    expect(effect.kind).toBe("rebuild");
    expect(state.strokes.map((stroke) => stroke.strokeId)).toEqual(["s1"]);
  });

  test("確定ストロークがなければ undo は none", () => {
    const state = synchronizedState();
    expect(applyNext(state, { type: "undo" }).kind).toBe("none");
  });

  test("redo は確定項目を末尾へ戻して焼き込みを要求する", () => {
    const state = synchronizedState([strokeItem("s1")], 1);
    applyNext(state, { type: "undo" });
    const item = strokeItem("s1");
    expect(applyNext(state, { type: "redo", item })).toEqual({
      kind: "bake_item",
      item,
    });
    expect(state.items).toEqual([item]);
  });

  test("clear は全消去して rebuild", () => {
    const state = synchronizedState([strokeItem("s1")], 1);
    expect(applyNext(state, { type: "clear" }).kind).toBe("rebuild");
    expect(state.strokes).toHaveLength(0);
  });

  test("eraser の cancel は rebuild になる", () => {
    const state = synchronizedState();
    applyNext(state, {
      type: "stroke_begin",
      strokeId: "e1",
      brush: { ...brush, tool: "eraser" },
    });
    expect(
      applyNext(state, { type: "stroke_cancel", strokeId: "e1" }).kind,
    ).toBe("rebuild");
  });

  test("pen の cancel は active layer の破棄を指示する", () => {
    const state = synchronizedState();
    applyNext(state, { type: "stroke_begin", strokeId: "s1", brush });
    expect(applyNext(state, { type: "stroke_cancel", strokeId: "s1" })).toEqual(
      {
        kind: "cancel",
        strokeId: "s1",
      },
    );
  });

  test("ローカルハブと同じ規則でトリムする (本数上限)", () => {
    const state = synchronizedState(
      Array.from({ length: MAX_STROKES }, (_, index) =>
        strokeItem(`s${index}`),
      ),
      1,
    );
    const effect = applyNext(state, {
      type: "stroke_begin",
      strokeId: "new",
      brush,
    });
    expect(effect.kind).toBe("rebuild");
    expect(state.strokes).toHaveLength(MAX_STROKES);
    expect(state.strokes.some((stroke) => stroke.strokeId === "s0")).toBe(
      false,
    );
  });

  test("未対応protocol versionは再同期を要求する", () => {
    const state = new OverlayState();
    expect(
      state.apply({
        type: "snapshot",
        protocolVersion: PROTOCOL_VERSION + 1,
        rev: 1,
        fadeAfterMs: null,
        items: [],
      }).kind,
    ).toBe("resync");
  });

  test("増分revisionの欠落は再同期を要求する", () => {
    const state = synchronizedState([], 10);
    expect(
      state.apply({
        type: "clear",
        rev: 12,
      }).kind,
    ).toBe("resync");
    expect(state.rev).toBe(10);
  });
});
