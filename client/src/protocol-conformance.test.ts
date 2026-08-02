import { describe, expect, test } from "bun:test";
import type {
  Brush,
  CanvasItem,
  LineStyle,
  ServerToOverlayMessage,
  ShapeKind,
  StampItem,
  StrokePoint,
  Tool,
} from "~/protocol";
import {
  MAX_ITEMS,
  MAX_POINTS_PER_MESSAGE,
  MAX_STROKE_POINTS,
  MAX_TOTAL_POINTS,
  PROTOCOL_VERSION,
} from "~/protocol";
import { OverlayState } from "./Overlay/state";

type MessageType = ServerToOverlayMessage["type"];
type MessageOf<Type extends MessageType> = Extract<
  ServerToOverlayMessage,
  { type: Type }
>;
type Equal<Left, Right> = [Left] extends [Right]
  ? [Right] extends [Left]
    ? true
    : false
  : false;
type Assert<Condition extends true> = Condition;

const SERVER_MESSAGE_TYPES = [
  "snapshot",
  "pong",
  "stroke_begin",
  "stroke_points",
  "stroke_end",
  "stroke_cancel",
  "shape_begin",
  "shape_update",
  "shape_end",
  "shape_cancel",
  "stamp_add",
  "stamp_move_preview",
  "stamp_move",
  "undo",
  "redo",
  "clear",
] as const satisfies readonly MessageType[];

const MESSAGE_FIELDS = {
  snapshot: ["fadeAfterMs", "items", "protocolVersion", "rev", "type"],
  pong: ["t", "type"],
  stroke_begin: ["brush", "rev", "strokeId", "type"],
  stroke_points: ["pts", "rev", "strokeId", "type"],
  stroke_end: ["endedAt", "rev", "strokeId", "type"],
  stroke_cancel: ["rev", "strokeId", "type"],
  shape_begin: ["rev", "shape", "type"],
  shape_update: ["end", "itemId", "rev", "type"],
  shape_end: ["endedAt", "itemId", "rev", "type"],
  shape_cancel: ["itemId", "rev", "type"],
  stamp_add: ["rev", "stamp", "type"],
  stamp_move_preview: ["center", "itemId", "rev", "type"],
  stamp_move: ["center", "itemId", "rev", "type"],
  undo: ["rev", "type"],
  redo: ["item", "rev", "type"],
  clear: ["rev", "type"],
} as const satisfies Record<MessageType, readonly string[]>;

const OBJECT_FIELDS = {
  brush: [
    "color",
    "opacity",
    "pressureMin",
    "pressureWidth",
    "tiltMaxScale",
    "tiltWidth",
    "tool",
    "widthN",
  ],
  strokeItem: ["brush", "done", "endedAt", "kind", "pts", "strokeId"],
  lineStyle: ["color", "opacity", "widthN"],
  shapeItem: [
    "done",
    "end",
    "endedAt",
    "itemId",
    "kind",
    "shape",
    "start",
    "style",
  ],
  stampItem: [
    "center",
    "done",
    "endedAt",
    "heightN",
    "itemId",
    "kind",
    "opacity",
    "stampId",
    "widthN",
  ],
} as const;

const TOOLS = ["pen", "marker", "eraser"] as const satisfies readonly Tool[];
const SHAPE_KINDS = [
  "line",
  "arrow",
  "rectangle",
  "ellipse",
] as const satisfies readonly ShapeKind[];
const CANVAS_KINDS = [
  "stroke",
  "shape",
  "stamp",
] as const satisfies readonly CanvasItem["kind"][];

type MissingMessageField = {
  [Type in MessageType]: Exclude<
    keyof MessageOf<Type> & string,
    (typeof MESSAGE_FIELDS)[Type][number]
  >;
}[MessageType];
type ExtraMessageField = {
  [Type in MessageType]: Exclude<
    (typeof MESSAGE_FIELDS)[Type][number],
    keyof MessageOf<Type> & string
  >;
}[MessageType];
type ObjectTypes = {
  brush: Brush;
  strokeItem: Extract<CanvasItem, { kind: "stroke" }>;
  lineStyle: LineStyle;
  shapeItem: Extract<CanvasItem, { kind: "shape" }>;
  stampItem: Extract<CanvasItem, { kind: "stamp" }>;
};
type MissingObjectField = {
  [Name in keyof ObjectTypes]: Exclude<
    keyof ObjectTypes[Name] & string,
    (typeof OBJECT_FIELDS)[Name][number]
  >;
}[keyof ObjectTypes];
type ExtraObjectField = {
  [Name in keyof ObjectTypes]: Exclude<
    (typeof OBJECT_FIELDS)[Name][number],
    keyof ObjectTypes[Name] & string
  >;
}[keyof ObjectTypes];

const typeCoverage: Assert<
  Equal<(typeof SERVER_MESSAGE_TYPES)[number], MessageType>
> = true;
const messageFieldCoverage: Assert<
  Equal<MissingMessageField | ExtraMessageField, never>
> = true;
const objectFieldCoverage: Assert<
  Equal<MissingObjectField | ExtraObjectField, never>
> = true;
const toolCoverage: Assert<Equal<(typeof TOOLS)[number], Tool>> = true;
const shapeKindCoverage: Assert<
  Equal<(typeof SHAPE_KINDS)[number], ShapeKind>
> = true;
const canvasKindCoverage: Assert<
  Equal<(typeof CANVAS_KINDS)[number], CanvasItem["kind"]>
> = true;

interface ExpectedState {
  rev: number;
  items: CanvasItem[];
}

interface ProtocolEventCase {
  name: string;
  initial: unknown;
  message: unknown;
  expected: ExpectedState;
}

interface RevisionCase extends ProtocolEventCase {
  expectedEffect: "resync";
}

interface TrimInitial {
  kind: "done_stamps" | "done_strokes" | "active_stroke";
  revision: number;
  count?: number;
  pointsPerItem?: number;
  idPrefix?: string;
  id?: string;
  points?: number;
}

interface StateSummary {
  rev: number;
  itemIds: string[];
  pointCounts: number[];
  totalPoints: number;
}

interface TrimCase {
  name: string;
  initial: TrimInitial;
  messages: unknown[];
  expected: StateSummary;
}

interface ProtocolFixture {
  fixtureVersion: number;
  protocolVersion: number;
  limits: {
    maxItems: number;
    maxTotalPoints: number;
    maxStrokePoints: number;
    maxPointsPerMessage: number;
  };
  serverMessageTypes: string[];
  messageFields: Record<string, string[]>;
  objectFields: Record<string, string[]>;
  enumValues: {
    tools: string[];
    shapeKinds: string[];
    canvasKinds: string[];
  };
  controlMessages: unknown[];
  clientMessages: unknown[];
  eventCases: ProtocolEventCase[];
  revisionCases: RevisionCase[];
  trimCases: TrimCase[];
}

const fixtureUrl = new URL(
  "../../protocol-fixtures/canonical.json",
  import.meta.url,
);
const fixture = JSON.parse(
  await Bun.file(fixtureUrl).text(),
) as ProtocolFixture;
const serverMessageTypeSet = new Set<string>(SERVER_MESSAGE_TYPES);

type JsonObject = Record<string, unknown>;

function expectObject(value: unknown, label: string): JsonObject {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError(`${label} must be an object`);
  }
  return value as JsonObject;
}

function expectExactFields(
  value: JsonObject,
  expected: readonly string[],
  label: string,
): void {
  const actual = Object.keys(value).sort();
  const sortedExpected = [...expected].sort();
  if (
    actual.length !== sortedExpected.length ||
    actual.some((field, index) => field !== sortedExpected[index])
  ) {
    throw new TypeError(
      `${label} fields must be ${sortedExpected.join(", ")}; got ${actual.join(", ")}`,
    );
  }
}

function expectString(value: unknown, label: string): string {
  if (typeof value !== "string") {
    throw new TypeError(`${label} must be a string`);
  }
  return value;
}

function expectFiniteNumber(value: unknown, label: string): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new TypeError(`${label} must be a finite number`);
  }
  return value;
}

function expectUnsignedInteger(value: unknown, label: string): number {
  const number = expectFiniteNumber(value, label);
  if (!Number.isSafeInteger(number) || number < 0) {
    throw new TypeError(`${label} must be a non-negative safe integer`);
  }
  return number;
}

function expectBoolean(value: unknown, label: string): boolean {
  if (typeof value !== "boolean") {
    throw new TypeError(`${label} must be a boolean`);
  }
  return value;
}

function expectNullableNumber(value: unknown, label: string): void {
  if (value !== null) expectFiniteNumber(value, label);
}

function expectArray(value: unknown, label: string): unknown[] {
  if (!Array.isArray(value)) {
    throw new TypeError(`${label} must be an array`);
  }
  return value;
}

function expectNumberTuple(
  value: unknown,
  length: number,
  label: string,
): void {
  const tuple = expectArray(value, label);
  if (tuple.length !== length) {
    throw new TypeError(`${label} must contain exactly ${length} numbers`);
  }
  tuple.forEach((part, index) => {
    expectFiniteNumber(part, `${label}[${index}]`);
  });
}

function expectEnum(
  value: unknown,
  allowed: readonly string[],
  label: string,
): void {
  const member = expectString(value, label);
  if (!allowed.includes(member)) {
    throw new TypeError(`${label} must be one of ${allowed.join(", ")}`);
  }
}

function validateBrush(value: unknown, label: string): void {
  const brush = expectObject(value, label);
  expectExactFields(brush, OBJECT_FIELDS.brush, label);
  expectEnum(brush.tool, TOOLS, `${label}.tool`);
  expectString(brush.color, `${label}.color`);
  expectFiniteNumber(brush.opacity, `${label}.opacity`);
  expectFiniteNumber(brush.widthN, `${label}.widthN`);
  expectBoolean(brush.pressureWidth, `${label}.pressureWidth`);
  expectFiniteNumber(brush.pressureMin, `${label}.pressureMin`);
  expectBoolean(brush.tiltWidth, `${label}.tiltWidth`);
  expectFiniteNumber(brush.tiltMaxScale, `${label}.tiltMaxScale`);
}

function validateLineStyle(value: unknown, label: string): void {
  const style = expectObject(value, label);
  expectExactFields(style, OBJECT_FIELDS.lineStyle, label);
  expectString(style.color, `${label}.color`);
  expectFiniteNumber(style.opacity, `${label}.opacity`);
  expectFiniteNumber(style.widthN, `${label}.widthN`);
}

function validateStrokeItem(value: unknown, label: string): void {
  const stroke = expectObject(value, label);
  expectExactFields(stroke, OBJECT_FIELDS.strokeItem, label);
  expectEnum(stroke.kind, ["stroke"], `${label}.kind`);
  expectString(stroke.strokeId, `${label}.strokeId`);
  validateBrush(stroke.brush, `${label}.brush`);
  expectArray(stroke.pts, `${label}.pts`).forEach((point, index) => {
    expectNumberTuple(point, 6, `${label}.pts[${index}]`);
  });
  expectBoolean(stroke.done, `${label}.done`);
  expectNullableNumber(stroke.endedAt, `${label}.endedAt`);
}

function validateShapeItem(
  value: unknown,
  label: string,
  canvasItem: boolean,
): void {
  const shape = expectObject(value, label);
  const fields = canvasItem
    ? OBJECT_FIELDS.shapeItem
    : OBJECT_FIELDS.shapeItem.filter((field) => field !== "kind");
  expectExactFields(shape, fields, label);
  if (canvasItem) expectEnum(shape.kind, ["shape"], `${label}.kind`);
  expectString(shape.itemId, `${label}.itemId`);
  expectEnum(shape.shape, SHAPE_KINDS, `${label}.shape`);
  validateLineStyle(shape.style, `${label}.style`);
  expectNumberTuple(shape.start, 2, `${label}.start`);
  expectNumberTuple(shape.end, 2, `${label}.end`);
  expectBoolean(shape.done, `${label}.done`);
  expectNullableNumber(shape.endedAt, `${label}.endedAt`);
}

function validateStampItem(
  value: unknown,
  label: string,
  canvasItem: boolean,
): void {
  const stamp = expectObject(value, label);
  const fields = canvasItem
    ? OBJECT_FIELDS.stampItem
    : OBJECT_FIELDS.stampItem.filter((field) => field !== "kind");
  expectExactFields(stamp, fields, label);
  if (canvasItem) expectEnum(stamp.kind, ["stamp"], `${label}.kind`);
  expectString(stamp.itemId, `${label}.itemId`);
  expectString(stamp.stampId, `${label}.stampId`);
  expectNumberTuple(stamp.center, 2, `${label}.center`);
  expectFiniteNumber(stamp.widthN, `${label}.widthN`);
  expectFiniteNumber(stamp.heightN, `${label}.heightN`);
  expectFiniteNumber(stamp.opacity, `${label}.opacity`);
  expectBoolean(stamp.done, `${label}.done`);
  expectNullableNumber(stamp.endedAt, `${label}.endedAt`);
}

function validateCanvasItem(value: unknown, label: string): void {
  const item = expectObject(value, label);
  switch (expectString(item.kind, `${label}.kind`)) {
    case "stroke":
      validateStrokeItem(item, label);
      return;
    case "shape":
      validateShapeItem(item, label, true);
      return;
    case "stamp":
      validateStampItem(item, label, true);
      return;
    default:
      throw new TypeError(`${label}.kind is unknown`);
  }
}

function decodeServerMessage(value: unknown): ServerToOverlayMessage {
  const decoded: unknown = JSON.parse(JSON.stringify(value));
  const message = expectObject(decoded, "message");
  const messageType = expectString(message.type, "message.type");
  if (!serverMessageTypeSet.has(messageType)) {
    throw new Error("fixture contains an unknown server message");
  }
  const type = messageType as MessageType;
  expectExactFields(message, MESSAGE_FIELDS[type], type);

  if (type !== "snapshot" && type !== "pong") {
    expectUnsignedInteger(message.rev, `${type}.rev`);
  }

  switch (type) {
    case "snapshot":
      expectUnsignedInteger(
        message.protocolVersion,
        "snapshot.protocolVersion",
      );
      expectUnsignedInteger(message.rev, "snapshot.rev");
      expectNullableNumber(message.fadeAfterMs, "snapshot.fadeAfterMs");
      expectArray(message.items, "snapshot.items").forEach((item, index) => {
        validateCanvasItem(item, `snapshot.items[${index}]`);
      });
      break;
    case "pong":
      expectFiniteNumber(message.t, "pong.t");
      break;
    case "stroke_begin":
      expectString(message.strokeId, "stroke_begin.strokeId");
      validateBrush(message.brush, "stroke_begin.brush");
      break;
    case "stroke_points":
      expectString(message.strokeId, "stroke_points.strokeId");
      expectArray(message.pts, "stroke_points.pts").forEach((point, index) => {
        expectNumberTuple(point, 6, `stroke_points.pts[${index}]`);
      });
      break;
    case "stroke_end":
      expectString(message.strokeId, "stroke_end.strokeId");
      expectFiniteNumber(message.endedAt, "stroke_end.endedAt");
      break;
    case "stroke_cancel":
      expectString(message.strokeId, "stroke_cancel.strokeId");
      break;
    case "shape_begin":
      validateShapeItem(message.shape, "shape_begin.shape", false);
      break;
    case "shape_update":
      expectString(message.itemId, "shape_update.itemId");
      expectNumberTuple(message.end, 2, "shape_update.end");
      break;
    case "shape_end":
      expectString(message.itemId, "shape_end.itemId");
      expectFiniteNumber(message.endedAt, "shape_end.endedAt");
      break;
    case "shape_cancel":
      expectString(message.itemId, "shape_cancel.itemId");
      break;
    case "stamp_add":
      validateStampItem(message.stamp, "stamp_add.stamp", false);
      break;
    case "stamp_move_preview":
      expectString(message.itemId, "stamp_move_preview.itemId");
      expectNumberTuple(message.center, 2, "stamp_move_preview.center");
      break;
    case "stamp_move":
      expectString(message.itemId, "stamp_move.itemId");
      expectNumberTuple(message.center, 2, "stamp_move.center");
      break;
    case "undo":
    case "clear":
      break;
    case "redo":
      validateCanvasItem(message.item, "redo.item");
      break;
    default: {
      const unsupported: never = type;
      throw new TypeError(`missing decoder for ${unsupported}`);
    }
  }

  return decoded as ServerToOverlayMessage;
}

function itemId(item: CanvasItem): string {
  return item.kind === "stroke" ? item.strokeId : item.itemId;
}

function pointCount(item: CanvasItem): number {
  return item.kind === "stroke" ? item.pts.length : 0;
}

function stateSummary(state: OverlayState): StateSummary {
  const pointCounts = state.items.map(pointCount);
  return {
    rev: state.rev,
    itemIds: state.items.map(itemId),
    pointCounts,
    totalPoints: pointCounts.reduce((sum, count) => sum + count, 0),
  };
}

const fixtureBrush: Brush = {
  tool: "pen",
  color: "#4455aa",
  opacity: 0.8,
  widthN: 0.0075,
  pressureWidth: true,
  pressureMin: 0.2,
  tiltWidth: false,
  tiltMaxScale: 1,
};
const fixtureStamp = (id: string): StampItem => ({
  itemId: id,
  stampId: "fixture-stamp",
  center: [0.45, 0.55],
  widthN: 0.1,
  heightN: 0.2,
  opacity: 0.9,
  done: true,
  endedAt: 1_700_000_000_300,
});

function fixturePoints(count: number): StrokePoint[] {
  return Array.from(
    { length: count },
    (_, index) => [0.1, 0.2, 0.5, index, 0, 0] as StrokePoint,
  );
}

function paddedId(prefix: string, index: number): string {
  return `${prefix}${index.toString().padStart(3, "0")}`;
}

function buildTrimItems(initial: TrimInitial): CanvasItem[] {
  switch (initial.kind) {
    case "done_stamps": {
      if (initial.count === undefined || initial.idPrefix === undefined) {
        throw new Error("done_stamps fixture is incomplete");
      }
      return Array.from({ length: initial.count }, (_, index) => ({
        kind: "stamp" as const,
        ...fixtureStamp(paddedId(initial.idPrefix as string, index)),
      }));
    }
    case "done_strokes": {
      if (
        initial.count === undefined ||
        initial.pointsPerItem === undefined ||
        initial.idPrefix === undefined
      ) {
        throw new Error("done_strokes fixture is incomplete");
      }
      return Array.from({ length: initial.count }, (_, index) => ({
        kind: "stroke" as const,
        strokeId: paddedId(initial.idPrefix as string, index),
        brush: fixtureBrush,
        pts: fixturePoints(initial.pointsPerItem as number),
        done: true,
        endedAt: 1_700_000_000_100,
      }));
    }
    case "active_stroke": {
      if (initial.id === undefined || initial.points === undefined) {
        throw new Error("active_stroke fixture is incomplete");
      }
      return [
        {
          kind: "stroke",
          strokeId: initial.id,
          brush: { ...fixtureBrush, tool: "marker" },
          pts: fixturePoints(initial.points),
          done: false,
          endedAt: null,
        },
      ];
    }
  }
}

function mutableFieldMap(
  fields: Record<string, readonly string[]>,
): Record<string, string[]> {
  return Object.fromEntries(
    Object.entries(fields).map(([name, names]) => [name, [...names]]),
  );
}

describe("Rust / TypeScript protocol conformance", () => {
  test("version、limits、variant、JSON fieldが一致する", () => {
    expect(typeCoverage).toBe(true);
    expect(messageFieldCoverage).toBe(true);
    expect(objectFieldCoverage).toBe(true);
    expect(toolCoverage).toBe(true);
    expect(shapeKindCoverage).toBe(true);
    expect(canvasKindCoverage).toBe(true);

    expect(fixture.fixtureVersion).toBe(1);
    expect(fixture.protocolVersion).toBe(PROTOCOL_VERSION);
    expect(fixture.limits).toEqual({
      maxItems: MAX_ITEMS,
      maxPointsPerMessage: MAX_POINTS_PER_MESSAGE,
      maxStrokePoints: MAX_STROKE_POINTS,
      maxTotalPoints: MAX_TOTAL_POINTS,
    });
    expect(fixture.serverMessageTypes).toEqual([...SERVER_MESSAGE_TYPES]);
    expect(fixture.messageFields).toEqual(mutableFieldMap(MESSAGE_FIELDS));
    expect(fixture.objectFields).toEqual(mutableFieldMap(OBJECT_FIELDS));
    expect(fixture.enumValues).toEqual({
      tools: [...TOOLS],
      shapeKinds: [...SHAPE_KINDS],
      canvasKinds: [...CANVAS_KINDS],
    });
    expect(fixture.eventCases.map((eventCase) => eventCase.name)).toEqual(
      SERVER_MESSAGE_TYPES.filter(
        (type) => type !== "snapshot" && type !== "pong",
      ),
    );
  });

  test("field型と固定長tupleが壊れたmessageを拒否する", () => {
    expect(() =>
      decodeServerMessage({
        type: "stroke_begin",
        rev: 41,
        strokeId: "invalid-brush",
        brush: {
          tool: "pen",
          color: "#4455aa",
          opacity: 0.8,
          widthN: "0.0075",
          pressureWidth: true,
          pressureMin: 0.2,
          tiltWidth: false,
          tiltMaxScale: 1,
        },
      }),
    ).toThrow("stroke_begin.brush.widthN must be a finite number");

    expect(() =>
      decodeServerMessage({
        type: "stroke_points",
        rev: 42,
        strokeId: "invalid-point",
        pts: [[0.1, 0.2, 0.5]],
      }),
    ).toThrow("stroke_points.pts[0] must contain exactly 6 numbers");
  });

  test("Rustが生成した全control messageをdecodeできる", () => {
    const state = new OverlayState();
    for (const raw of fixture.controlMessages) {
      const message = decodeServerMessage(raw);
      const effect = state.apply(message);
      if (message.type === "snapshot") {
        expect(effect.kind).toBe("rebuild");
        expect(state.rev).toBe(message.rev);
        expect(state.items).toEqual(message.items);
        expect(state.fadeAfterMs).toBe(message.fadeAfterMs);
      } else {
        expect(message.type).toBe("pong");
        expect(effect.kind).toBe("none");
      }
    }
    expect(fixture.clientMessages).toEqual([
      { type: "ping", t: 1_700_000_001_500 },
    ]);
  });

  for (const eventCase of fixture.eventCases) {
    test(`${eventCase.name} をdecode / applyしてRust状態と一致する`, () => {
      const state = new OverlayState();
      expect(state.apply(decodeServerMessage(eventCase.initial)).kind).toBe(
        "rebuild",
      );
      expect(state.apply(decodeServerMessage(eventCase.message)).kind).not.toBe(
        "resync",
      );
      expect({ rev: state.rev, items: state.items }).toEqual(
        eventCase.expected,
      );
    });
  }

  for (const revisionCase of fixture.revisionCases) {
    test(`${revisionCase.name} は共有fixtureどおり再同期する`, () => {
      const state = new OverlayState();
      expect(state.apply(decodeServerMessage(revisionCase.initial)).kind).toBe(
        "rebuild",
      );
      expect(state.apply(decodeServerMessage(revisionCase.message)).kind).toBe(
        revisionCase.expectedEffect,
      );
      expect({ rev: state.rev, items: state.items }).toEqual(
        revisionCase.expected,
      );
    });
  }

  for (const trimCase of fixture.trimCases) {
    test(`${trimCase.name} のtrim結果がRustと一致する`, () => {
      const state = new OverlayState();
      const initialItems = buildTrimItems(trimCase.initial);
      expect(
        state.apply({
          type: "snapshot",
          protocolVersion: fixture.protocolVersion,
          rev: trimCase.initial.revision,
          fadeAfterMs: null,
          items: initialItems,
        }).kind,
      ).toBe("rebuild");
      for (const raw of trimCase.messages) {
        expect(state.apply(decodeServerMessage(raw)).kind).not.toBe("resync");
      }
      expect(stateSummary(state)).toEqual(trimCase.expected);
    });
  }
});
