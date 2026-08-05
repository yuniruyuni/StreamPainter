import { useEffect, useRef } from "react";
import { connectOverlay } from "./connection";
import { OverlayDisconnectGuard } from "./disconnect-guard";
import { RenderQueue } from "./render-queue";
import { OverlayLayers } from "./renderer/layers";
import { OverlayState } from "./state";

// OBS ブラウザソースとして読み込まれる描画表示ページ (docs/webapp.md)。
// 完全透明背景・UI なし。React はマウントと接続ライフサイクルのみ担当し、
// 描画は canvas 直叩きで行う (再レンダリングを発生させない)。
export const OverlayApp: React.FC = () => {
  const bakedRef = useRef<HTMLCanvasElement>(null);
  const activeRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const baked = bakedRef.current;
    const active = activeRef.current;
    if (!baked || !active) return;

    const state = new OverlayState();
    const layers = new OverlayLayers(baked, active);
    let scheduled = false;
    let animationFrame: number | null = null;
    const pending = new RenderQueue();

    function cancelScheduledRender() {
      if (animationFrame !== null) cancelAnimationFrame(animationFrame);
      animationFrame = null;
      scheduled = false;
      pending.clear();
    }

    function size() {
      cancelScheduledRender();
      const dpr = window.devicePixelRatio || 1;
      layers.resize(
        Math.round(window.innerWidth * dpr),
        Math.round(window.innerHeight * dpr),
        state.items,
        state.layers,
      );
    }
    size();

    // 受信は上限付き・集約可能なキューに溜め、1フレームでまとめて反映する。
    function schedule(effect: Parameters<RenderQueue["enqueue"]>[0]) {
      pending.enqueue(effect);
      if (scheduled) return;
      scheduled = true;
      animationFrame = requestAnimationFrame(() => {
        scheduled = false;
        animationFrame = null;
        for (const queued of pending.drain()) {
          switch (queued.kind) {
            case "active":
              layers.beginActive(queued.stroke);
              layers.appendActive(queued.stroke);
              break;
            case "bake":
              layers.bake(queued.stroke);
              break;
            case "bake_item":
              layers.bakeItem(queued.item);
              break;
            case "cancel":
              layers.cancelActive(queued.strokeId);
              break;
            case "preview":
              break;
            case "stamp_preview":
              layers.previewStamp(queued.stamp, queued.rebuildBaked);
              break;
            case "item_preview":
              layers.previewItem(queued.item, queued.rebuildBaked);
              break;
            case "rebuild":
              layers.rebuild(state.items, state.layers);
              break;
          }
        }
        layers.renderActive();
      });
    }

    const disconnectGuard = new OverlayDisconnectGuard(() => {
      cancelScheduledRender();
      state.reset();
      layers.clear();
    });
    const conn = connectOverlay(
      `ws://${location.host}/ws`,
      (msg) => {
        const effect = state.apply(msg);
        layers.setDocument(state.items, state.layers);
        switch (effect.kind) {
          case "none":
            return true;
          case "active":
            schedule(effect);
            return true;
          case "bake":
            schedule(effect);
            return true;
          case "bake_item":
            schedule(effect);
            return true;
          case "cancel":
            schedule(effect);
            return true;
          case "preview":
            schedule(effect);
            return true;
          case "stamp_preview":
            layers.prepareItemPreview({ kind: "stamp", ...effect.stamp });
            schedule(effect);
            return true;
          case "item_preview":
            layers.prepareItemPreview(effect.item);
            schedule(effect);
            return true;
          case "rebuild":
            layers.prepareRebuild();
            schedule(effect);
            return true;
          case "resync":
            cancelScheduledRender();
            return false;
        }
      },
      (status) => disconnectGuard.update(status),
    );

    window.addEventListener("resize", size);
    return () => {
      window.removeEventListener("resize", size);
      cancelScheduledRender();
      disconnectGuard.dispose();
      conn.close();
      layers.dispose();
    };
  }, []);

  const canvasStyle: React.CSSProperties = {
    position: "fixed",
    inset: 0,
    width: "100vw",
    height: "100vh",
  };

  return (
    <>
      <canvas ref={bakedRef} style={canvasStyle} />
      <canvas ref={activeRef} style={canvasStyle} />
    </>
  );
};
