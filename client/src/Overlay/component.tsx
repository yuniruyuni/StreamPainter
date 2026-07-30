import { useEffect, useRef } from "react";
import { connectOverlay } from "./connection";
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

    function size() {
      const dpr = window.devicePixelRatio || 1;
      layers.resize(
        Math.round(window.innerWidth * dpr),
        Math.round(window.innerHeight * dpr),
        state.strokes,
      );
    }
    size();

    // 受信はキューに溜め、requestAnimationFrame でまとめて反映する
    let scheduled = false;
    const pending: (() => void)[] = [];
    function schedule(task: () => void) {
      pending.push(task);
      if (scheduled) return;
      scheduled = true;
      requestAnimationFrame(() => {
        scheduled = false;
        const tasks = pending.splice(0, pending.length);
        for (const t of tasks) t();
        layers.renderActive();
      });
    }

    const conn = connectOverlay(`ws://${location.host}/ws`, (msg) => {
      const effect = state.apply(msg);
      switch (effect.kind) {
        case "none":
          return;
        case "active":
          schedule(() => {
            layers.beginActive(effect.stroke);
            layers.appendActive(effect.stroke);
          });
          return;
        case "bake":
          schedule(() => layers.bake(effect.stroke));
          return;
        case "cancel":
          schedule(() => layers.cancelActive(effect.strokeId));
          return;
        case "rebuild":
          schedule(() => layers.rebuild(state.strokes));
          return;
      }
    });

    window.addEventListener("resize", size);
    return () => {
      window.removeEventListener("resize", size);
      conn.close();
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
