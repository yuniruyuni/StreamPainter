import type { CanvasItem } from "~/protocol";
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
    if (effect.kind === "rebuild") {
      this.effects = [effect];
      return;
    }
    if (this.effects[0]?.kind === "rebuild") {
      // stamp previewはitemsの状態だけでは「移動中の別レイヤー」を復元できない。
      if (effect.kind === "stamp_preview" || effect.kind === "item_preview") {
        this.effects = [{ kind: "rebuild" }, { ...effect, rebuildBaked: true }];
      }
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
    if (
      effect.kind === "stamp_preview" &&
      last?.kind === "stamp_preview" &&
      last.stamp.itemId === effect.stamp.itemId
    ) {
      this.effects[this.effects.length - 1] = {
        ...effect,
        rebuildBaked: last.rebuildBaked || effect.rebuildBaked,
      };
      return;
    }
    if (
      effect.kind === "item_preview" &&
      last?.kind === "item_preview" &&
      itemId(last.item) === itemId(effect.item)
    ) {
      this.effects[this.effects.length - 1] = {
        ...effect,
        rebuildBaked: last.rebuildBaked || effect.rebuildBaked,
      };
      return;
    }

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

function itemId(item: CanvasItem): string {
  return item.kind === "stroke" ? item.strokeId : item.itemId;
}
