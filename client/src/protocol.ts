// Rust 側 `painter/src/protocol.rs` と JSON 表現を揃えるローカル WS 型。

export const PROTOCOL_VERSION = 8;
export const MIN_COMPATIBLE_PROTOCOL_VERSION = 6;
export const MAX_ITEMS = 500;
export const MAX_STROKES = MAX_ITEMS;
export const MAX_TOTAL_POINTS = 200_000;
export const MAX_STROKE_POINTS = 10_000;
export const MAX_POINTS_PER_MESSAGE = 512;
export const MAX_LAYERS = 8;
export const DEFAULT_LAYER_ID = "default";

export interface CanvasLayer {
  layerId: string;
  name: string;
}

export const DEFAULT_LAYER: CanvasLayer = {
  layerId: DEFAULT_LAYER_ID,
  name: "レイヤー 1",
};

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
  layerId: string;
  brush: Brush;
  pts: StrokePoint[];
  done: boolean;
  endedAt: number | null;
}

export type Position = [u: number, v: number];
export type ShapeKind = "line" | "arrow" | "rectangle" | "ellipse";

export interface ItemTransform {
  center: Position;
  widthN: number;
  heightN: number;
  /** Canvas上の時計回りradian。 */
  rotation: number;
}

export interface LineStyle {
  color: string;
  opacity: number;
  widthN: number;
}

export interface ShapeItem {
  itemId: string;
  layerId: string;
  shape: ShapeKind;
  style: LineStyle;
  start: Position;
  end: Position;
  /** v6 snapshotでは未定義。start/endをfallbackとして使う。 */
  transform?: ItemTransform;
  done: boolean;
  endedAt: number | null;
}

export interface StampItem {
  itemId: string;
  layerId: string;
  stampId: string;
  center: Position;
  widthN: number;
  heightN: number;
  /** v6 snapshotでは未定義（0 radianとして扱う）。 */
  rotation?: number;
  opacity: number;
  done: boolean;
  endedAt: number | null;
}

export type CanvasItem =
  | ({ kind: "stroke" } & Stroke)
  | ({ kind: "shape" } & ShapeItem)
  | ({ kind: "stamp" } & StampItem);

export type PaintEvent =
  | { type: "stroke_begin"; strokeId: string; layerId: string; brush: Brush }
  | { type: "stroke_points"; strokeId: string; pts: StrokePoint[] }
  | { type: "stroke_end"; strokeId: string; endedAt: number }
  | { type: "stroke_cancel"; strokeId: string }
  | { type: "shape_begin"; shape: ShapeItem }
  | { type: "shape_update"; itemId: string; end: Position }
  | {
      type: "shape_end";
      itemId: string;
      endedAt: number;
      transform?: ItemTransform;
    }
  | { type: "shape_cancel"; itemId: string }
  | { type: "stamp_add"; stamp: StampItem }
  | { type: "stamp_move_preview"; itemId: string; center: Position }
  | { type: "stamp_move"; itemId: string; center: Position }
  | {
      type: "item_transform_preview";
      itemId: string;
      transform: ItemTransform;
    }
  | {
      type: "item_transform_commit";
      itemId: string;
      transform: ItemTransform;
    }
  | { type: "layer_add"; layer: CanvasLayer }
  | { type: "layer_delete"; layerId: string }
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
      /** v6/v7 snapshotでは未定義で、既定レイヤーへ移行する。 */
      layers?: CanvasLayer[];
      items: CanvasItem[];
    }
  | { type: "pong"; t: number };
