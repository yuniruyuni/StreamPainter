import { describe, expect, test } from "bun:test";
import type { Brush, CanvasItem, Stroke } from "~/protocol";
import { RenderQueue } from "./render-queue";

const brush: Brush = {
  tool: "pen",
  color: "#ffffff",
  opacity: 1,
  widthN: 0.005,
  pressureWidth: false,
};

function stroke(id: string, pointCount: number): Stroke {
  return {
    strokeId: id,
    brush,
    pts: Array.from({ length: pointCount }, (_, index) => [
      index / 100,
      0,
      1,
      index,
    ]),
    done: false,
    endedAt: null,
  };
}

function stamp(index: number): CanvasItem {
  return {
    kind: "stamp",
    itemId: `item-${index}`,
    stampId: "stamp-1",
    center: [0.5, 0.5],
    widthN: 0.1,
    heightN: 0.1,
    opacity: 1,
    done: true,
    endedAt: index,
  };
}

describe("RenderQueue", () => {
  test("同じstrokeの連続更新は最新1件へ集約する", () => {
    const queue = new RenderQueue();
    queue.enqueue({ kind: "active", stroke: stroke("s1", 1) });
    queue.enqueue({ kind: "active", stroke: stroke("s1", 2) });
    queue.enqueue({ kind: "active", stroke: stroke("s1", 3) });

    const effects = queue.drain();
    expect(effects).toHaveLength(1);
    expect(effects[0]?.kind).toBe("active");
    if (effects[0]?.kind === "active") {
      expect(effects[0].stroke.pts).toHaveLength(3);
    }
  });

  test("確定操作との順序は保持する", () => {
    const queue = new RenderQueue();
    const active = stroke("s1", 2);
    queue.enqueue({ kind: "active", stroke: active });
    queue.enqueue({ kind: "bake", stroke: { ...active, done: true } });
    queue.enqueue({ kind: "preview" });
    queue.enqueue({ kind: "preview" });

    expect(queue.drain().map((effect) => effect.kind)).toEqual([
      "active",
      "bake",
      "preview",
    ]);
  });

  test("rebuildは待機中の増分を包含する", () => {
    const queue = new RenderQueue();
    queue.enqueue({ kind: "active", stroke: stroke("s1", 1) });
    queue.enqueue({ kind: "rebuild" });
    queue.enqueue({ kind: "bake_item", item: stamp(1) });

    expect(queue.drain()).toEqual([{ kind: "rebuild" }]);
  });

  test("上限を超えた待機効果はrebuild 1件へ畳み込む", () => {
    const queue = new RenderQueue();
    for (let index = 0; index < 200; index++) {
      queue.enqueue({ kind: "bake_item", item: stamp(index) });
    }
    expect(queue.drain()).toEqual([{ kind: "rebuild" }]);
  });
});
