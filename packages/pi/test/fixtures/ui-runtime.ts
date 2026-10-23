import { writeFileSync, renameSync } from "node:fs";
import { setTimeout as delay } from "node:timers/promises";
import type { PiExtensionApi, PiExtensionContext } from "../../src/extension-api.js";
import type { UiNode } from "../../src/extension-ui-frame.js";
const text = (id: string, value: string): UiNode => ({ id, type: "text", text: value });
const redPixelPng = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

/** End-to-end fixture: JavaScript only supplies nodes and consumes semantic events. */
export default function (pi: PiExtensionApi) {
  const evidence: Record<string, any> = { toolResults: [] };
  const save = () => {
    const path = process.env.UI_ACCEPTANCE_OUTPUT!;
    writeFileSync(`${path}.tmp`, JSON.stringify(evidence)); renameSync(`${path}.tmp`, path);
  };
  const clearWidgets = (ctx: PiExtensionContext, keys: string[]) => { for (const key of keys) ctx.ui.setWidget(key, undefined); };
  pi.registerMessageRenderer("ui-acceptance", (message: any, options: any) => {
    evidence.messageDetails = message.details; evidence.messageOutputPad = options.outputPad; save();
    return text("message", "UI_PTY_RENDERED_MESSAGE");
  });
  pi.registerEntryRenderer("ui-entry", (entry: any) => {
    evidence.entryId = entry.id; evidence.entryData = entry.data; save(); return text("entry", "UI_PTY_RENDERED_ENTRY");
  });
  pi.registerMarkdownTransformer((value: string) => value.replace("UI_PTY_MARKDOWN_BEFORE", "UI_PTY_MARKDOWN_AFTER"));
  pi.registerToolRenderer((name, next) => { if (name === "ui_tool") evidence.toolResolver = true; return next(); });
  pi.registerTool({
    name: "ui_tool", label: "UI tool", description: "Exercise semantic tool presentation in the Rust terminal.", parameters: { type: "object", properties: { value: { type: "string" } }, required: ["value"] },
    renderCall(args, context) {
      evidence.toolCall = { args, argsComplete: context.argsComplete, executionStarted: context.executionStarted, outputPad: context.outputPad, showImages: context.showImages }; save();
      return text("call", "UI_PTY_TOOL_CALL");
    },
    renderResult(result, context) {
      evidence.toolResults.push({ details: result.details, isPartial: context.isPartial, durationMs: context.durationMs, sameArgs: context.args.value === "streamed" }); save();
      return text("result", context.isPartial ? "UI_PTY_TOOL_PARTIAL" : "UI_PTY_TOOL_RESULT");
    },
    async execute(_id, input, _signal, update) {
      update?.({ content: [{ type: "text", text: "partial" }], details: { phase: "partial", input } }); await delay(80);
      return { content: [{ type: "text", text: "complete" }], details: { phase: "complete", input } };
    },
  });
  pi.registerShortcut("ctrl+k", { description: "PTY shortcut acceptance", handler: (ctx: PiExtensionContext) => {
    evidence.shortcut = true; ctx.ui.setWidget("shortcut", text("shortcut", "UI_PTY_SHORTCUT")); save();
  } });
  pi.on("session_start", async (_event, ctx) => {
    if (!ctx.hasUI) throw new Error("interactive UI missing");
    ctx.ui.setWorkingMessage("UI_PTY_WORKING"); ctx.ui.setHiddenThinkingLabel("UI_PTY_HIDDEN_THINKING");
    ctx.ui.setStatus("acceptance", "ready"); ctx.ui.setHeader(text("header", "UI_PTY_HEADER")); ctx.ui.setFooter(text("footer", "UI_PTY_FOOTER"));
    ctx.ui.setTitle("UI_PTY_TITLE");
    evidence.startup = await ctx.ui.custom({ id: "startup", type: "column", children: [text("title", "UI_PTY_STARTUP"), { id: "input", type: "input" }] });
    ctx.ui.setWidget("media", { id: "media", type: "row", gap: 1, children: [
      { id: "link", type: "link", text: "UI_PTY_LINK", url: "https://example.com/pi-ui" },
      { id: "image", type: "image", data: redPixelPng, mimeType: "image/png", alt: "UI_PTY_IMAGE", width: 2, height: 1 },
    ] });
    save(); ctx.ui.setWidget("acceptance", text("ready", "UI_PTY_MAIN_READY"));
  });
  pi.registerCommand("ui-smoke", { description: "Declarative UI PTY acceptance", handler: async (_args, ctx) => {
    evidence.commandEntered = true; save();
    try {
      evidence.selection = await ctx.ui.select("UI_PTY_SELECT", ["alpha", "beta"]);
      evidence.confirmed = await ctx.ui.confirm("UI_PTY_CONFIRM", "Continue?");
      evidence.input = await ctx.ui.input("UI_PTY_INPUT", "type here");
      evidence.editor = await ctx.ui.editor("UI_PTY_EDITOR", "before external editor");
      evidence.externalEditor = await ctx.ui.hostOperation({ type: "externalEditor", text: evidence.editor ?? "" });
      save(); ctx.ui.setWidget("external-editor", text("external-editor", "UI_PTY_EXTERNAL_DONE"));
      pi.sendMessage({ customType: "ui-acceptance", content: "source message", display: true, details: { retained: 42 } });
      pi.appendEntry("ui-entry", { retained: 43 }); await ctx.ui.hostOperation({ type: "clipboard", text: "UI_PTY_CLIPBOARD" });
      evidence.overlay = await ctx.ui.custom({ id: "overlay", type: "panel", title: "UI_PTY_OVERLAY", child: { id: "input", type: "input" } }, { overlay: { width: 40, maxHeight: 10 } });
      evidence.interactionDone = true; ctx.ui.setWidget("interaction", text("done", "UI_PTY_INTERACTION_DONE")); save();
    } catch (error) { evidence.error = String(error); save(); throw error; }
  } });
  pi.registerCommand("ui-update", { description: "Update a mounted descriptor", handler: async (_args, ctx) => {
    clearWidgets(ctx, ["acceptance", "interaction", "shortcut", "external-editor"]);
    const mounted = ctx.ui.mount({ id: "button", type: "button", text: "UI_PTY_UPDATE_BEFORE" }, { id: "updatable", onEvent: (event) => {
      if (event.event.type === "activate") { evidence.activated = true; mounted.unmount(); ctx.ui.setWidget("update-done", text("done", "UI_PTY_UPDATE_DONE")); save(); }
    } });
    mounted.update({ id: "button", type: "button", text: "UI_PTY_UPDATE_AFTER" });
  } });
  pi.registerCommand("ui-complete", { description: "Accept Rust-owned input completions", handler: async (_args, ctx) => {
    clearWidgets(ctx, ["update-done"]);
    evidence.completion = await ctx.ui.custom({ id: "completion-dialog", type: "column", children: [
      text("title", "UI_PTY_COMPLETE"),
      { id: "value", type: "input", initialValue: "he", completions: [{ value: "hello from Rust", label: "hello from Rust", description: "UI_PTY_COMPLETION" }] },
    ] });
    ctx.ui.setWidget("completion-done", text("completion-done", "UI_PTY_COMPLETE_DONE")); save();
  } });
  pi.registerCommand("ui-working", { description: "Working and tool presentation", handler: (_args, ctx) => {
    clearWidgets(ctx, ["update-done", "completion-done"]);
    ctx.ui.setProgress(true); ctx.ui.setProgramStatus({ state: "working", text: "UI_PTY_PROGRAM_WORKING", progress: 25 });
    evidence.workingRequested = true; save(); pi.sendUserMessage("run the ui_tool with value streamed");
  } });
  pi.registerCommand("ui-finish", { description: "Finish PTY acceptance", handler: (_args, ctx) => {
    ctx.ui.setProgress(false); ctx.ui.setProgramStatus({ state: "clear" });
    evidence.finished = true; save(); ctx.shutdown();
  } });
}
