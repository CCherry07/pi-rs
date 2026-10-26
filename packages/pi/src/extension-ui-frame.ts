import { z } from "zod";

export type UiTone = "default" | "accent" | "muted" | "success" | "warning" | "error";
export interface UiCompletion { value: string; label: string; description?: string }
export interface UiMultiSelectOption {
  label: string; searchText: string; category?: string; detailLines: string[]; readOnly: boolean; sortValues: string[][];
}
export interface UiMultiSelectAction { id: string; key: string; label: string; enabled: boolean; confirmation?: string }
export interface UiMultiSelectRequest {
  title: string; options: UiMultiSelectOption[]; actions: UiMultiSelectAction[]; categories: [string, string][];
  sortModes: [string, boolean][]; initiallySelected: number[]; initialQuery: string;
  initialActiveCategories: string[]; initialSortMode: number; summaryLines: string[];
}
export interface UiMultiSelectResponse {
  selected: number[]; actionId: string; query: string; activeCategories: string[]; sortMode: number;
}
export type UiNode = { id: string } & (
  | { type: "text"; text: string; tone?: UiTone }
  | { type: "custom"; lines: string[]; interactive?: boolean; mouse?: boolean; instanceId?: string; keyRelease?: boolean; cursor?: [number, number] | null }
  | { type: "markdown"; text: string }
  | { type: "link"; text: string; url: string }
  | { type: "column" | "row"; children: UiNode[]; gap?: number }
  | { type: "panel"; title?: string; child: UiNode }
  | { type: "scroll"; child: UiNode; height: number }
  | { type: "spacer"; height?: number }
  | { type: "select"; options: string[]; initialSelected?: number }
  | { type: "multiSelect"; request: UiMultiSelectRequest }
  | { type: "input"; initialValue?: string; placeholder?: string; completions?: UiCompletion[] }
  | { type: "editor"; initialValue?: string; placeholder?: string; height?: number; completions?: UiCompletion[] }
  | { type: "button"; text: string }
  | { type: "progress"; text: string }
  | { type: "image"; data: string; mimeType: string; alt: string; width: number; height: number }
);
const id = z.string().min(1);
const cells = z.number().int().min(0).max(65535);
const completions = z.array(z.strictObject({ value: z.string(), label: z.string(), description: z.string().optional() })).default([]);
const multiSelectOption = z.strictObject({
  label: z.string(), searchText: z.string(), category: z.string().optional(), detailLines: z.array(z.string()).default([]),
  readOnly: z.boolean().default(false), sortValues: z.array(z.array(z.string())).default([]),
});
const multiSelectAction = z.strictObject({
  id: z.string(), key: z.string().length(1), label: z.string(), enabled: z.boolean().default(true), confirmation: z.string().optional(),
});
export const uiMultiSelectRequestSchema: z.ZodType<UiMultiSelectRequest> = z.strictObject({
  title: z.string(), options: z.array(multiSelectOption), actions: z.array(multiSelectAction),
  categories: z.array(z.tuple([z.string(), z.string()])), sortModes: z.array(z.tuple([z.string(), z.boolean()])),
  initiallySelected: z.array(z.number().int().nonnegative()).default([]), initialQuery: z.string().default(""),
  initialActiveCategories: z.array(z.string()).default([]), initialSortMode: z.number().int().nonnegative().default(0),
  summaryLines: z.array(z.string()).default([]),
}).superRefine((request, context) => {
  const categories = new Set<string>();
  for (const [id] of request.categories) {
    if (!id.trim() || categories.has(id)) context.addIssue({ code: "custom", message: `Invalid or duplicate multi-select category: ${id}` });
    categories.add(id);
  }
  if (request.initialActiveCategories.some((category) => !categories.has(category))) context.addIssue({ code: "custom", message: "Multi-select initial category is not registered" });
  if (request.options.some((option) => option.category !== undefined && !categories.has(option.category))) context.addIssue({ code: "custom", message: "Multi-select option category is not registered" });
  if (request.initiallySelected.some((index) => index >= request.options.length)) context.addIssue({ code: "custom", message: "Multi-select initial selection is out of range" });
  if ((request.sortModes.length === 0 && request.initialSortMode !== 0) || (request.sortModes.length > 0 && request.initialSortMode >= request.sortModes.length)) context.addIssue({ code: "custom", message: "Multi-select initial sort mode is out of range" });
  const actionIds = new Set<string>(); const actionKeys = new Set<string>();
  for (const action of request.actions) {
    if (!action.id.trim() || actionIds.has(action.id)) context.addIssue({ code: "custom", message: `Invalid or duplicate multi-select action: ${action.id}` });
    if (actionKeys.has(action.key)) context.addIssue({ code: "custom", message: `Duplicate multi-select action key: ${action.key}` });
    if ([" ", "/", "a", "n", "s", "f"].includes(action.key)) context.addIssue({ code: "custom", message: `Multi-select action key is reserved by the control: ${action.key}` });
    actionIds.add(action.id); actionKeys.add(action.key);
  }
});
export const uiMultiSelectResponseSchema: z.ZodType<UiMultiSelectResponse> = z.strictObject({
  selected: z.array(z.number().int().nonnegative()), actionId: z.string(), query: z.string(),
  activeCategories: z.array(z.string()), sortMode: z.number().int().nonnegative(),
});
const node: z.ZodType<UiNode> = z.lazy(() => z.discriminatedUnion("type", [
  z.strictObject({ id, type: z.literal("text"), text: z.string(), tone: z.enum(["default", "accent", "muted", "success", "warning", "error"]).default("default") }),
  z.strictObject({ id, type: z.literal("custom"), lines: z.array(z.string()), interactive: z.boolean().default(false), mouse: z.boolean().default(false), instanceId: z.string().default(""), keyRelease: z.boolean().default(false), cursor: z.tuple([cells, cells]).nullish() }),
  z.strictObject({ id, type: z.literal("markdown"), text: z.string() }),
  z.strictObject({ id, type: z.literal("link"), text: z.string(), url: z.string() }),
  z.strictObject({ id, type: z.literal("column"), children: z.array(node), gap: cells.default(0) }),
  z.strictObject({ id, type: z.literal("row"), children: z.array(node), gap: cells.default(0) }),
  z.strictObject({ id, type: z.literal("panel"), title: z.string().default(""), child: node }),
  z.strictObject({ id, type: z.literal("scroll"), child: node, height: cells }),
  z.strictObject({ id, type: z.literal("spacer"), height: cells.default(1) }),
  z.strictObject({ id, type: z.literal("select"), options: z.array(z.string()), initialSelected: z.number().int().nonnegative().default(0) }),
  z.strictObject({ id, type: z.literal("multiSelect"), request: uiMultiSelectRequestSchema }),
  z.strictObject({ id, type: z.literal("input"), initialValue: z.string().default(""), placeholder: z.string().default(""), completions }),
  z.strictObject({ id, type: z.literal("editor"), initialValue: z.string().default(""), placeholder: z.string().default(""), height: cells.default(6), completions }),
  z.strictObject({ id, type: z.literal("button"), text: z.string() }),
  z.strictObject({ id, type: z.literal("progress"), text: z.string() }),
  z.strictObject({ id, type: z.literal("image"), data: z.string(), mimeType: z.string(), alt: z.string(), width: cells, height: cells }),
]));
export const uiNodeSchema = node.superRefine((root, context) => {
  const ids = new Set<string>();
  const visit = (value: UiNode) => {
    if (ids.has(value.id)) context.addIssue({ code: "custom", message: `Duplicate UI node id: ${value.id}` });
    ids.add(value.id);
    if (value.type === "column" || value.type === "row") value.children.forEach(visit);
    else if (value.type === "panel" || value.type === "scroll") visit(value.child);
  };
  visit(root);
});
export type Placement = "header" | "aboveEditor" | "belowEditor" | "footer" | "editor" | "overlay";
export type UiOverlayPosition = { type: "cells" | "percent"; value: number };
export type UiOverlayAnchor = "top-left" | "top-center" | "top-right" | "left-center" | "center" | "right-center" | "bottom-left" | "bottom-center" | "bottom-right";
export interface UiOverlayOptions {
  width?: number; maxHeight?: number; modal?: boolean; anchor?: UiOverlayAnchor;
  offsetX?: number; offsetY?: number; row?: UiOverlayPosition; col?: UiOverlayPosition;
  margin?: { top: number; right: number; bottom: number; left: number };
  hidden?: boolean; nonCapturing?: boolean;
}
export interface UiRegion { id: string; placement: Placement; root: UiNode; overlay?: UiOverlayOptions }
export const uiSnapshotSchema = z.object({
  columns: z.number(), rows: z.number(), editorText: z.string(), toolsExpanded: z.boolean(),
  isRunning: z.boolean().default(false),
  transcript: z.array(z.object({ index: z.number().int().nonnegative(), fingerprint: z.string(), payload: z.record(z.string(), z.unknown()) })).default([]),
  transcriptWidth: z.number().default(0),
  areas: z.array(z.object({ placement: z.enum(["header", "aboveEditor", "belowEditor", "footer", "editor", "overlay"]), row: z.number(), column: z.number(), width: z.number(), height: z.number() })).default([]),
  regions: z.array(z.object({ owner: z.string(), regionId: z.string(), focused: z.boolean(), bounds: z.object({ row: cells, col: cells, width: cells, height: cells }).nullable() })).default([]),
});
export type UiSnapshot = z.infer<typeof uiSnapshotSchema>;
export const uiMouseEventSchema = z.strictObject({
  type: z.enum(["press", "release", "move", "drag", "click", "wheel"]),
  button: z.enum(["left", "middle", "right", "none"]),
  x: z.number().int().min(-2147483648).max(2147483647), y: z.number().int().min(-2147483648).max(2147483647),
  screenX: cells, screenY: cells, width: cells, height: cells,
  shift: z.boolean(), alt: z.boolean(), ctrl: z.boolean(),
  wheelDelta: z.number().int().min(-2147483648).max(2147483647).nullish(),
  clickCount: z.number().int().min(0).max(255).nullish(),
});
export type UiMouseEvent = z.infer<typeof uiMouseEventSchema>;
export const uiMouseResultSchema = z.strictObject({
  handled: z.boolean().default(false), capture: z.boolean().default(false), focus: z.boolean().default(false),
  render: z.boolean().nullish(), target: z.number().int().nonnegative().nullish(), cancelled: z.boolean().default(false),
});
export type UiMouseResult = z.infer<typeof uiMouseResultSchema>;
export const uiInteractionSchema = z.discriminatedUnion("type", [
  z.strictObject({ type: z.literal("change"), value: z.unknown() }),
  z.strictObject({ type: z.literal("submit"), value: z.unknown() }),
  z.strictObject({ type: z.literal("activate") }),
  z.strictObject({ type: z.literal("cancel") }),
  z.strictObject({ type: z.literal("shortcut"), key: z.string() }),
  z.strictObject({ type: z.literal("customInput"), data: z.string() }),
  z.strictObject({ type: z.literal("customMouse"), id: z.number().int().nonnegative(), instanceId: z.string(), target: z.number().int().nonnegative().nullish(), event: uiMouseEventSchema }),
]);
export type UiInteraction = z.infer<typeof uiInteractionSchema>;
export const uiEventSchema = z.discriminatedUnion("type", [
  z.object({ type: z.literal("hostResult"), id: z.number(), result: z.string().nullable(), error: z.string().nullable(), snapshot: uiSnapshotSchema }),
  z.object({ type: z.literal("refresh"), snapshot: uiSnapshotSchema }),
  z.object({ type: z.literal("interaction"), regionId: z.string(), nodeId: z.string(), revision: z.number().int().nonnegative(), event: uiInteractionSchema, snapshot: uiSnapshotSchema }),
  z.object({ type: z.literal("closed") }),
]);
export type UiEvent = z.infer<typeof uiEventSchema>;
export type UiComponentEvent = Extract<UiEvent, { type: "interaction" }>;

const uiSurfaceCommandSchema = z.discriminatedUnion("type", [
  z.strictObject({ type: z.literal("wake") }),
  z.strictObject({ type: z.literal("close") }),
  z.strictObject({ type: z.literal("focus"), regionId: z.string(), nodeId: z.string() }),
  z.strictObject({ type: z.literal("unfocus"), regionId: z.string(), target: z.discriminatedUnion("type", [
    z.strictObject({ type: z.literal("app") }),
    z.strictObject({ type: z.literal("region"), regionId: z.string() }),
  ]).nullish() }),
  z.strictObject({ type: z.literal("mouseResult"), id: z.number().int().nonnegative(), result: uiMouseResultSchema }),
]);
const uiProductCommandSchema = z.discriminatedUnion("type", [
  z.strictObject({ type: z.literal("appAction"), action: z.enum(["clear", "interrupt", "exit", "paste", "copy", "followUp", "dequeue", "newSession", "tree", "fork", "resume"]) }),
  z.strictObject({ type: z.literal("editorText"), text: z.string(), append: z.boolean() }),
  z.strictObject({ type: z.literal("toolsExpanded"), expanded: z.boolean() }),
  z.strictObject({ type: z.literal("title"), title: z.string() }),
  z.strictObject({ type: z.literal("theme"), name: z.string() }),
  z.strictObject({ type: z.literal("progress"), active: z.boolean() }),
  z.strictObject({ type: z.literal("programStatus"), state: z.string(), text: z.string().nullish(), progress: z.number().int().min(0).max(255).nullish() }),
  z.strictObject({ type: z.literal("submit"), text: z.string() }),
]);
const uiHostOperationSchema = z.discriminatedUnion("type", [
  z.strictObject({ type: z.literal("clipboard"), text: z.string() }),
  z.strictObject({ type: z.literal("externalEditor"), text: z.string(), command: z.string().nullish() }),
  z.strictObject({ type: z.literal("suspend") }),
]);
export const uiCommandSchema = z.discriminatedUnion("channel", [
  z.strictObject({ channel: z.literal("surface"), command: uiSurfaceCommandSchema }),
  z.strictObject({ channel: z.literal("product"), command: uiProductCommandSchema }),
  z.strictObject({ channel: z.literal("host"), command: z.strictObject({ id: z.number().int().nonnegative(), operation: uiHostOperationSchema }) }),
]);
export type UiHostOperation = z.infer<typeof uiHostOperationSchema>;
export type UiCommand = z.infer<typeof uiCommandSchema>;
