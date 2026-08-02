// Rust 側 `painter/src/protocol.rs` と JSON 表現を揃えるローカル WS 型。

export const PROTOCOL_VERSION = 6;
export const MAX_ITEMS = 500;
export const MAX_STROKES = MAX_ITEMS;
export const MAX_TOTAL_POINTS = 200_000;
export const MAX_STROKE_POINTS = 10_000;
export const MAX_POINTS_PER_MESSAGE = 512;

// v6 appends normalized Windows tilt to the complete v5 tuple prefix.
export type StrokePoint = [
  u: number,
  v: number,
  pressure: number,
  dt: number,
  tiltX: number,
  tiltY: number,
];
export type Tool = "pen" | "marker" | "eraser";

export interface Brush {
  tool: Tool;
  color: string;
  opacity: number;
  widthN: number;
  pressureWidth: boolean;
  pressureMin: number;
  tiltWidth: boolean;
  tiltMaxScale: number;
}

export interface Stroke {
  strokeId: string;
  brush: Brush;
  pts: StrokePoint[];
  done: boolean;
  endedAt: number | null;
}

export type Position = [u: number, v: number];
export type ShapeKind = "line" | "arrow" | "rectangle" | "ellipse";

export interface LineStyle {
  color: string;
  opacity: number;
  widthN: number;
}

export interface ShapeItem {
  itemId: string;
  shape: ShapeKind;
  style: LineStyle;
  start: Position;
  end: Position;
  done: boolean;
  endedAt: number | null;
}

export interface StampItem {
  itemId: string;
  stampId: string;
  center: Position;
  widthN: number;
  heightN: number;
  opacity: number;
  done: boolean;
  endedAt: number | null;
}

export type CanvasItem =
  | ({ kind: "stroke" } & Stroke)
  | ({ kind: "shape" } & ShapeItem)
  | ({ kind: "stamp" } & StampItem);

export type PaintEvent =
  | { type: "stroke_begin"; strokeId: string; brush: Brush }
  | { type: "stroke_points"; strokeId: string; pts: StrokePoint[] }
  | { type: "stroke_end"; strokeId: string; endedAt: number }
  | { type: "stroke_cancel"; strokeId: string }
  | { type: "shape_begin"; shape: ShapeItem }
  | { type: "shape_update"; itemId: string; end: Position }
  | { type: "shape_end"; itemId: string; endedAt: number }
  | { type: "shape_cancel"; itemId: string }
  | { type: "stamp_add"; stamp: StampItem }
  | { type: "stamp_move_preview"; itemId: string; center: Position }
  | { type: "stamp_move"; itemId: string; center: Position }
  | { type: "undo" }
  | { type: "redo"; item: CanvasItem }
  | { type: "clear" };

export type RevisionedPaintEvent = PaintEvent & { rev: number };

export type ServerToOverlayMessage =
  | RevisionedPaintEvent
  | {
      type: "snapshot";
      protocolVersion: number;
      rev: number;
      fadeAfterMs: number | null;
      items: CanvasItem[];
    }
  | { type: "pong"; t: number };
