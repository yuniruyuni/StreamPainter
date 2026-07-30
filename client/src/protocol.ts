// Rust 側 `painter/src/protocol.rs` と JSON 表現を揃えるローカル WS 型。

export const MAX_STROKES = 500;
export const MAX_TOTAL_POINTS = 200_000;
export const MAX_STROKE_POINTS = 10_000;
export const MAX_POINTS_PER_MESSAGE = 512;

export type StrokePoint = [u: number, v: number, pressure: number, dt: number];
export type Tool = "pen" | "marker" | "eraser";

export interface Brush {
  tool: Tool;
  color: string;
  opacity: number;
  widthN: number;
  pressureWidth: boolean;
}

export interface Stroke {
  strokeId: string;
  brush: Brush;
  pts: StrokePoint[];
  done: boolean;
  endedAt: number | null;
}

export type PaintEvent =
  | { type: "stroke_begin"; strokeId: string; brush: Brush }
  | { type: "stroke_points"; strokeId: string; pts: StrokePoint[] }
  | { type: "stroke_end"; strokeId: string; endedAt: number }
  | { type: "stroke_cancel"; strokeId: string }
  | { type: "undo" }
  | { type: "clear" };

export type ServerToOverlayMessage =
  | PaintEvent
  | {
      type: "snapshot";
      rev: number;
      fadeAfterMs: number | null;
      strokes: Stroke[];
    }
  | { type: "pong"; t: number };
