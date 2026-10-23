import test from "node:test";
import assert from "node:assert/strict";
import { ExtensionTranscript, type ToolRenderContext } from "../src/extension-ui-transcript.js";
import { uiSnapshotSchema, type UiNode } from "../src/extension-ui-frame.js";
const snapshot = (payload: Record<string, unknown>, fingerprint = "a") => uiSnapshotSchema.parse({ columns: 80, rows: 24, editorText: "", toolsExpanded: false, transcriptWidth: 60, transcript: [{ index: 0, fingerprint, payload }] });
const plain = (node: UiNode): string => node.type === "column" || node.type === "row" ? node.children.map(plain).join("\n") : "text" in node ? node.text : "";

test("tool presentation retains details and state while Rust receives only nodes", () => {
  const host = new ExtensionTranscript(() => {}, 0, false);
  const order: string[] = []; let context: ToolRenderContext | undefined;
  host.registerTool("read", {
    renderCall(_args: unknown, ctx: ToolRenderContext) { (ctx.state as Record<string, unknown>).retained = 7; return { id: "call", type: "text", text: "read" }; },
    renderResult(result: Record<string, unknown>, ctx: ToolRenderContext) { context = ctx; assert.equal((ctx.state as Record<string, unknown>).retained, 7); return { id: "result", type: "text", text: JSON.stringify(result.details) }; },
  });
  host.registerResolver((name: string, next: () => unknown) => { order.push(name); return next(); });
  const payload = { kind: "tool", name: "read", toolCallId: "call-1", args: { path: "a" }, result: { content: [], details: { kept: 7 } }, executionStarted: true, argsComplete: false, isPartial: true, cwd: "/tmp" };
  assert.match(plain(host.render(snapshot(payload))[0]!.root), /"kept":7/);
  assert.deepEqual(order, ["read"]); assert.equal(context?.isPartial, true); assert.equal(context?.outputPad, 0); assert.equal(context?.showImages, false);
  host.render(snapshot({ ...payload, argsComplete: true, isPartial: false, result: { ...payload.result, durationMs: 12 } }, "b"));
  assert.equal(context?.durationMs, 12); assert.equal(context?.argsComplete, true); host.dispose();
});

test("custom messages and Markdown transformations describe content without terminal width", () => {
  const host = new ExtensionTranscript(() => {});
  host.registerMessage("notice", (entry: Record<string, unknown>) => ({ id: "message", type: "markdown", text: String(entry.content) }));
  assert.equal(plain(host.render(snapshot({ kind: "customMessage", entry: { customType: "notice", content: "visible" } }))[0]!.root), "visible");
  host.registerMarkdown((value: string, context: Record<string, unknown>) => { assert.equal("availableWidth" in context, false); return value.replace("before", "after"); });
  host.setHiddenThinkingLabel("Reasoning hidden");
  assert.match(plain(host.render(snapshot({ kind: "assistant", text: "before", thinking: "reasoning", streaming: true }, "b"))[0]!.root), /Reasoning hidden\nafter/); host.dispose();
});

test("retired presentation callbacks cannot trigger another generation render", () => {
  let wakes = 0; const host = new ExtensionTranscript(() => { wakes++; }); let context: ToolRenderContext | undefined;
  host.registerTool("late", { renderCall: (_args: unknown, value: ToolRenderContext) => { context = value; return { id: "late", type: "text", text: "late" }; } });
  const value = snapshot({ kind: "tool", name: "late", toolCallId: "1", args: {} }); host.render(value); host.dispose(); const before = wakes;
  context?.invalidate(); assert.equal(wakes, before); assert.deepEqual(host.render(value), []);
});
