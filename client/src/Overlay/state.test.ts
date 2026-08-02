import { describe, expect, test } from "bun:test";
import type {
  Brush,
  CanvasItem,
  PaintEvent,
  RevisionedPaintEvent,
  ServerToOverlayMessage,
  Stroke,
} from "~/protocol";
import {
  MAX_STROKES,
  MIN_COMPATIBLE_PROTOCOL_VERSION,
  PROTOCOL_VERSION,
} from "~/protocol";
import { OverlayState } from "./state";

const brush: Brush = {
  tool: "pen",
  color: "#ff4d6d",
  opacity: 1,
  widthN: 0.005,
  pressureWidth: true,
  pressureMin: 0.2,
  tiltWidth: false,
  tiltMaxScale: 1,
};

function doneStroke(id: string): Stroke {
  return {
    strokeId: id,
    brush,
    pts: [[0.1, 0.1, 0.5, 0, 0, 0]],
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

  test("v6の筆圧snapshotとtransformなしeventを安全に移行する", () => {
    const state = new OverlayState();
    const activeShape: CanvasItem = {
      kind: "shape",
      itemId: "shape-v6",
      shape: "rectangle",
      style: { color: "#fff", opacity: 1, widthN: 0.01 },
      start: [0.2, 0.3],
      end: [0.6, 0.7],
      done: false,
      endedAt: null,
    };
    const stamp: CanvasItem = {
      kind: "stamp",
      itemId: "stamp-v6",
      stampId: "asset",
      center: [0.5, 0.5],
      widthN: 0.1,
      heightN: 0.2,
      opacity: 1,
      done: true,
      endedAt: 2,
    };

    expect(
      state.apply({
        type: "snapshot",
        protocolVersion: MIN_COMPATIBLE_PROTOCOL_VERSION,
        rev: 6,
        fadeAfterMs: null,
        items: [strokeItem("stroke-v6"), activeShape, stamp],
      }).kind,
    ).toBe("rebuild");
    expect(state.strokes[0]?.pts[0]).toEqual([0.1, 0.1, 0.5, 0, 0, 0]);
    expect(state.items[2]).not.toHaveProperty("rotation");

    expect(
      state.apply({
        type: "shape_end",
        rev: 7,
        itemId: "shape-v6",
        endedAt: 3,
      }).kind,
    ).toBe("bake_item");
    expect(state.items[1]).toMatchObject({ done: true, endedAt: 3 });
    expect(state.items[1]).not.toHaveProperty("transform");
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
        [0.1, 0.1, 0.5, 0, 0, 0],
        [0.2, 0.2, 0.6, 16, 0.2, -0.1],
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
      pts: [[0, 0, 0.5, 0, 0, 0]],
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

  test("snapshot と undo はストローク・スタンプ共通の描画順を使う", () => {
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

  test("shape/stamp transform previewを集約しcommitで永続状態へ戻す", () => {
    const shape: CanvasItem = {
      kind: "shape",
      itemId: "shape-1",
      shape: "rectangle",
      style: { color: "#fff", opacity: 1, widthN: 0.01 },
      start: [0.2, 0.2],
      end: [0.4, 0.4],
      done: true,
      endedAt: 1,
    };
    const state = synchronizedState([shape]);
    const firstTransform = {
      center: [0.5, 0.5] as [number, number],
      widthN: 0.3,
      heightN: 0.2,
      rotation: 0.25,
    };
    const first = applyNext(state, {
      type: "item_transform_preview",
      itemId: "shape-1",
      transform: firstTransform,
    });
    expect(first).toEqual({
      kind: "item_preview",
      item: state.items[0],
      rebuildBaked: true,
    });

    const committedTransform = {
      ...firstTransform,
      center: [0.7, 0.6] as [number, number],
      rotation: 0.5,
    };
    const second = applyNext(state, {
      type: "item_transform_preview",
      itemId: "shape-1",
      transform: committedTransform,
    });
    expect(second.kind).toBe("item_preview");
    if (second.kind === "item_preview") {
      expect(second.rebuildBaked).toBe(false);
    }
    expect(
      applyNext(state, {
        type: "item_transform_commit",
        itemId: "shape-1",
        transform: committedTransform,
      }).kind,
    ).toBe("rebuild");
    expect(state.items[0]).toMatchObject({ transform: committedTransform });
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

    expect(
      state.apply({
        type: "snapshot",
        protocolVersion: MIN_COMPATIBLE_PROTOCOL_VERSION - 1,
        rev: 1,
        fadeAfterMs: null,
        items: [],
      }).kind,
    ).toBe("resync");
  });

  test("snapshot protocolVersionはnumberのsafe integer v6/v7だけを受理する", () => {
    const invalidVersions: unknown[] = [
      undefined,
      String(PROTOCOL_VERSION),
      { value: PROTOCOL_VERSION },
      Number.NaN,
      Number.POSITIVE_INFINITY,
      PROTOCOL_VERSION + 0.5,
      Number.MAX_SAFE_INTEGER + 1,
    ];
    for (const protocolVersion of invalidVersions) {
      const state = new OverlayState();
      const raw = {
        type: "snapshot",
        ...(protocolVersion === undefined ? {} : { protocolVersion }),
        rev: 1,
        fadeAfterMs: null,
        items: [],
      };
      expect(state.apply(raw as unknown as ServerToOverlayMessage).kind).toBe(
        "resync",
      );
      expect(state.items).toEqual([]);
      expect(state.rev).toBe(0);
    }

    for (const protocolVersion of [
      MIN_COMPATIBLE_PROTOCOL_VERSION,
      PROTOCOL_VERSION,
    ]) {
      const state = new OverlayState();
      expect(
        state.apply({
          type: "snapshot",
          protocolVersion,
          rev: 1,
          fadeAfterMs: null,
          items: [],
        }).kind,
      ).toBe("rebuild");
    }
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
