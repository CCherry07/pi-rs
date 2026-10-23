import { z } from "zod";

export type UiTone = "default" | "accent" | "muted" | "success" | "warning" | "error";
export interface UiCompletion { value: string; label: string; description?: string }
export type UiNode = { id: string } & (
  | { type: "text"; text: string; tone?: UiTone }
  | { type: "markdown"; text: string }
  | { type: "link"; text: string; url: string }
  | { type: "column" | "row"; children: UiNode[]; gap?: number }
  | { type: "panel"; title?: string; child: UiNode }
  | { type: "scroll"; child: UiNode; height: number }
  | { type: "spacer"; height?: number }
  | { type: "select"; options: string[]; initialSelected?: number }
  | { type: "input"; initialValue?: string; placeholder?: string; completions?: UiCompletion[] }
  | { type: "editor"; initialValue?: string; placeholder?: string; height?: number; completions?: UiCompletion[] }
  | { type: "button"; text: string }
  | { type: "progress"; text: string }
  | { type: "image"; data: string; mimeType: string; alt: string; width: number; height: number }
);
const id = z.string().min(1);
const cells = z.number().int().min(0).max(65535);
const completions = z.array(z.strictObject({ value: z.string(), label: z.string(), description: z.string().optional() })).default([]);
const node: z.ZodType<UiNode> = z.lazy(() => z.discriminatedUnion("type", [
  z.strictObject({ id, type: z.literal("text"), text: z.string(), tone: z.enum(["default", "accent", "muted", "success", "warning", "error"]).default("default") }),
  z.strictObject({ id, type: z.literal("markdown"), text: z.string() }),
  z.strictObject({ id, type: z.literal("link"), text: z.string(), url: z.string() }),
  z.strictObject({ id, type: z.literal("column"), children: z.array(node), gap: cells.default(0) }),
  z.strictObject({ id, type: z.literal("row"), children: z.array(node), gap: cells.default(0) }),
  z.strictObject({ id, type: z.literal("panel"), title: z.string().default(""), child: node }),
  z.strictObject({ id, type: z.literal("scroll"), child: node, height: cells }),
  z.strictObject({ id, type: z.literal("spacer"), height: cells.default(1) }),
  z.strictObject({ id, type: z.literal("select"), options: z.array(z.string()), initialSelected: z.number().int().nonnegative().default(0) }),
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
export interface UiRegion { id: string; placement: Placement; root: UiNode; overlay?: { width?: number; maxHeight?: number; modal?: boolean } }
export const uiSnapshotSchema = z.object({
  columns: z.number(), rows: z.number(), editorText: z.string(), toolsExpanded: z.boolean(),
  isRunning: z.boolean().default(false),
  transcript: z.array(z.object({ index: z.number().int().nonnegative(), fingerprint: z.string(), payload: z.record(z.string(), z.unknown()) })).default([]),
  transcriptWidth: z.number().default(0),
  areas: z.array(z.object({ placement: z.enum(["header", "aboveEditor", "belowEditor", "footer", "editor", "overlay"]), row: z.number(), column: z.number(), width: z.number(), height: z.number() })).default([]),
});
export type UiSnapshot = z.infer<typeof uiSnapshotSchema>;
export const uiInteractionSchema = z.discriminatedUnion("type", [
  z.strictObject({ type: z.literal("change"), value: z.unknown() }),
  z.strictObject({ type: z.literal("submit"), value: z.unknown() }),
  z.strictObject({ type: z.literal("activate") }),
  z.strictObject({ type: z.literal("cancel") }),
  z.strictObject({ type: z.literal("shortcut"), key: z.string() }),
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
