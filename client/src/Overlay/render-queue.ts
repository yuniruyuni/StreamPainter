import type { RenderEffect } from "./state";

const MAX_PENDING_EFFECTS = 128;

export type QueuedRenderEffect = Exclude<
  RenderEffect,
  { kind: "none" } | { kind: "resync" }
>;

/**
 * requestAnimationFrameが止まってもメモリを増やし続けない描画効果キュー。
 * rebuildはflush時点の最新stateを使うため、それ以前・以後の増分をすべて包含する。
 */
export class RenderQueue {
  private effects: QueuedRenderEffect[] = [];

  enqueue(effect: QueuedRenderEffect): void {
    if (this.effects[0]?.kind === "rebuild") return;
    if (effect.kind === "rebuild") {
      this.effects = [effect];
      return;
    }

    const last = this.effects.at(-1);
    if (
      effect.kind === "active" &&
      last?.kind === "active" &&
      last.stroke.strokeId === effect.stroke.strokeId
    ) {
      this.effects[this.effects.length - 1] = effect;
      return;
    }
    if (effect.kind === "preview" && last?.kind === "preview") return;

    this.effects.push(effect);
    if (this.effects.length > MAX_PENDING_EFFECTS) {
      this.effects = [{ kind: "rebuild" }];
    }
  }

  drain(): QueuedRenderEffect[] {
    return this.effects.splice(0, this.effects.length);
  }

  clear(): void {
    this.effects = [];
  }
}
