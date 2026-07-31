// overlay 側の CanvasItem 状態機械。Rust のローカルハブと同じ規則で
// 状態を更新し、レンダラに必要な再描画の種類を返す。

import type {
  CanvasItem,
  ServerToOverlayMessage,
  ShapeItem,
  StampItem,
  Stroke,
} from "~/protocol";
import { MAX_ITEMS, MAX_STROKE_POINTS, MAX_TOTAL_POINTS } from "~/protocol";

function canvasItemId(item: CanvasItem): string {
  return item.kind === "stroke" ? item.strokeId : item.itemId;
}

// preview: active canvas の図形だけ更新 / bake: 確定 stroke の焼き込み
export type RenderEffect =
  | { kind: "none" }
  | { kind: "active"; stroke: Stroke }
  | { kind: "bake"; stroke: Stroke }
  | { kind: "cancel"; strokeId: string }
  | { kind: "preview" }
  | { kind: "rebuild" };

export class OverlayState {
  items: CanvasItem[] = [];
  fadeAfterMs: number | null = null;
  rev = 0;

  /** v1 のテスト・呼び出し側向けの読み取り互換ビュー。 */
  get strokes(): Stroke[] {
    return this.items
      .filter(
        (item): item is Extract<CanvasItem, { kind: "stroke" }> =>
          item.kind === "stroke",
      )
      .map(({ kind: _kind, ...stroke }) => stroke);
  }

  apply(msg: ServerToOverlayMessage): RenderEffect {
    switch (msg.type) {
      case "snapshot": {
        this.items =
          msg.items !== undefined
            ? msg.items
            : msg.strokes.map((stroke) => ({ kind: "stroke", ...stroke }));
        this.fadeAfterMs = msg.fadeAfterMs;
        this.rev = msg.rev;
        return { kind: "rebuild" };
      }
      case "stroke_begin": {
        if (this.items.some((item) => canvasItemId(item) === msg.strokeId)) {
          return { kind: "none" };
        }
        const item: Extract<CanvasItem, { kind: "stroke" }> = {
          kind: "stroke",
          strokeId: msg.strokeId,
          brush: msg.brush,
          pts: [],
          done: false,
          endedAt: null,
        };
        this.items.push(item);
        return this.trim()
          ? { kind: "rebuild" }
          : { kind: "active", stroke: item };
      }
      case "stroke_points": {
        const stroke = this.findActiveStroke(msg.strokeId);
        if (!stroke) return { kind: "none" };
        const available = Math.max(0, MAX_STROKE_POINTS - stroke.pts.length);
        stroke.pts.push(...msg.pts.slice(0, available));
        return { kind: "active", stroke };
      }
      case "stroke_end": {
        const stroke = this.findActiveStroke(msg.strokeId);
        if (!stroke) return { kind: "none" };
        stroke.done = true;
        stroke.endedAt = msg.endedAt;
        return { kind: "bake", stroke };
      }
      case "stroke_cancel": {
        const index = this.items.findIndex(
          (item) =>
            item.kind === "stroke" &&
            item.strokeId === msg.strokeId &&
            !item.done,
        );
        if (index === -1) return { kind: "none" };
        const [item] = this.items.splice(index, 1);
        return item?.kind === "stroke" && item.brush.tool === "eraser"
          ? { kind: "rebuild" }
          : { kind: "cancel", strokeId: msg.strokeId };
      }
      case "shape_begin": {
        if (
          this.items.some((item) => canvasItemId(item) === msg.shape.itemId)
        ) {
          return { kind: "none" };
        }
        this.items.push({ kind: "shape", ...msg.shape });
        return this.trim() ? { kind: "rebuild" } : { kind: "preview" };
      }
      case "shape_update": {
        const shape = this.findActiveShape(msg.itemId);
        if (!shape) return { kind: "none" };
        shape.end = msg.end;
        return { kind: "preview" };
      }
      case "shape_end": {
        const shape = this.findActiveShape(msg.itemId);
        if (!shape) return { kind: "none" };
        shape.done = true;
        shape.endedAt = msg.endedAt;
        return { kind: "rebuild" };
      }
      case "shape_cancel": {
        const index = this.items.findIndex(
          (item) =>
            item.kind === "shape" && item.itemId === msg.itemId && !item.done,
        );
        if (index === -1) return { kind: "none" };
        this.items.splice(index, 1);
        return { kind: "preview" };
      }
      case "stamp_add": {
        if (
          this.items.some((item) => canvasItemId(item) === msg.stamp.itemId)
        ) {
          return { kind: "none" };
        }
        const stamp: StampItem = { ...msg.stamp, done: true };
        this.items.push({ kind: "stamp", ...stamp });
        this.trim();
        return { kind: "rebuild" };
      }
      case "undo": {
        let index = -1;
        for (let i = this.items.length - 1; i >= 0; i--) {
          if (this.items[i]?.done) {
            index = i;
            break;
          }
        }
        if (index === -1) return { kind: "none" };
        this.items.splice(index, 1);
        return { kind: "rebuild" };
      }
      case "clear": {
        if (this.items.length === 0) return { kind: "none" };
        this.items = [];
        return { kind: "rebuild" };
      }
      case "pong":
        return { kind: "none" };
      default:
        // JSON は実行時には将来版の type を含み得る。
        return { kind: "none" };
    }
  }

  activeItems(): CanvasItem[] {
    return this.items.filter((item) => !item.done);
  }

  doneItems(): CanvasItem[] {
    return this.items.filter((item) => item.done);
  }

  activeStrokes(): Stroke[] {
    return this.strokes.filter((stroke) => !stroke.done);
  }

  doneStrokes(): Stroke[] {
    return this.strokes.filter((stroke) => stroke.done);
  }

  private findActiveStroke(
    strokeId: string,
  ): Extract<CanvasItem, { kind: "stroke" }> | null {
    const item = this.items.find(
      (candidate) =>
        candidate.kind === "stroke" &&
        candidate.strokeId === strokeId &&
        !candidate.done,
    );
    return item?.kind === "stroke" ? item : null;
  }

  private findActiveShape(
    itemId: string,
  ): (ShapeItem & { kind: "shape" }) | null {
    const item = this.items.find(
      (candidate) =>
        candidate.kind === "shape" &&
        candidate.itemId === itemId &&
        !candidate.done,
    );
    return item?.kind === "shape" ? item : null;
  }

  // ローカルハブと同じトリム規則。確定項目を捨てたら true (要 rebuild)。
  private trim(): boolean {
    let dropped = false;
    while (this.items.length > MAX_ITEMS) {
      if (!this.dropOldestDone()) break;
      dropped = true;
    }
    let total = this.items.reduce(
      (sum, item) => sum + (item.kind === "stroke" ? item.pts.length : 0),
      0,
    );
    while (total > MAX_TOTAL_POINTS) {
      const item = this.dropOldestDone();
      if (!item) break;
      if (item.kind === "stroke") total -= item.pts.length;
      dropped = true;
    }
    return dropped;
  }

  private dropOldestDone(): CanvasItem | null {
    const index = this.items.findIndex((item) => item.done);
    if (index === -1) return null;
    return this.items.splice(index, 1)[0] ?? null;
  }
}
