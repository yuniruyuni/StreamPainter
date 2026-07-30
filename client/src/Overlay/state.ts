// overlay 側のストローク状態機械。Rust のローカルハブと同じ規則で
// 状態を更新し、レンダラに必要な再描画の種類を返す (docs/protocol.md)。

import type { ServerToOverlayMessage, Stroke } from "~/protocol";
import { MAX_STROKE_POINTS, MAX_STROKES, MAX_TOTAL_POINTS } from "~/protocol";

// none: 再描画不要 / active: 描画中レイヤーのみ / bake: 確定 1 本の焼き込み / rebuild: 全再構築
export type RenderEffect =
  | { kind: "none" }
  | { kind: "active"; stroke: Stroke }
  | { kind: "bake"; stroke: Stroke }
  | { kind: "cancel"; strokeId: string }
  | { kind: "rebuild" };

export class OverlayState {
  strokes: Stroke[] = [];
  fadeAfterMs: number | null = null;
  rev = 0;

  apply(msg: ServerToOverlayMessage): RenderEffect {
    switch (msg.type) {
      case "snapshot": {
        this.strokes = msg.strokes;
        this.fadeAfterMs = msg.fadeAfterMs;
        this.rev = msg.rev;
        return { kind: "rebuild" };
      }
      case "stroke_begin": {
        if (this.strokes.some((s) => s.strokeId === msg.strokeId)) {
          return { kind: "none" };
        }
        const stroke: Stroke = {
          strokeId: msg.strokeId,
          brush: msg.brush,
          pts: [],
          done: false,
          endedAt: null,
        };
        this.strokes.push(stroke);
        return this.trim() ? { kind: "rebuild" } : { kind: "active", stroke };
      }
      case "stroke_points": {
        const stroke = this.findActive(msg.strokeId);
        if (!stroke) return { kind: "none" };
        const available = Math.max(0, MAX_STROKE_POINTS - stroke.pts.length);
        stroke.pts.push(...msg.pts.slice(0, available));
        return { kind: "active", stroke };
      }
      case "stroke_end": {
        const stroke = this.findActive(msg.strokeId);
        if (!stroke) return { kind: "none" };
        stroke.done = true;
        stroke.endedAt = msg.endedAt;
        return { kind: "bake", stroke };
      }
      case "stroke_cancel": {
        const index = this.strokes.findIndex(
          (s) => s.strokeId === msg.strokeId && !s.done,
        );
        if (index === -1) return { kind: "none" };
        const [stroke] = this.strokes.splice(index, 1);
        // 消しゴムは baked に直接作用しているため取り消しに全再構築が要る
        return stroke?.brush.tool === "eraser"
          ? { kind: "rebuild" }
          : { kind: "cancel", strokeId: msg.strokeId };
      }
      case "undo": {
        let index = -1;
        for (let i = this.strokes.length - 1; i >= 0; i--) {
          if (this.strokes[i]?.done) {
            index = i;
            break;
          }
        }
        if (index === -1) return { kind: "none" };
        this.strokes.splice(index, 1);
        return { kind: "rebuild" };
      }
      case "clear": {
        if (this.strokes.length === 0) return { kind: "none" };
        this.strokes = [];
        return { kind: "rebuild" };
      }
      case "pong":
        return { kind: "none" };
    }
  }

  activeStrokes(): Stroke[] {
    return this.strokes.filter((s) => !s.done);
  }

  doneStrokes(): Stroke[] {
    return this.strokes.filter((s) => s.done);
  }

  private findActive(strokeId: string): Stroke | null {
    return this.strokes.find((s) => s.strokeId === strokeId && !s.done) ?? null;
  }

  // ローカルハブと同じトリム規則。確定ストロークを捨てたら true (要 rebuild)
  private trim(): boolean {
    let dropped = false;
    while (this.strokes.length > MAX_STROKES) {
      if (!this.dropOldestDone()) break;
      dropped = true;
    }
    let total = this.strokes.reduce((sum, s) => sum + s.pts.length, 0);
    while (total > MAX_TOTAL_POINTS) {
      const stroke = this.dropOldestDone();
      if (!stroke) break;
      total -= stroke.pts.length;
      dropped = true;
    }
    return dropped;
  }

  private dropOldestDone(): Stroke | null {
    const index = this.strokes.findIndex((s) => s.done);
    if (index === -1) return null;
    return this.strokes.splice(index, 1)[0] ?? null;
  }
}
