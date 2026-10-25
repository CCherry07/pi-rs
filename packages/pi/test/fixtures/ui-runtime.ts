import { writeFileSync, renameSync, existsSync } from "node:fs";
import { setTimeout as delay } from "node:timers/promises";
import { Box, Container, Input, SettingsList, Text, type OverlayHandle, type TuiMouseEvent } from "@earendil-works/pi-tui";
import { getSettingsListTheme } from "../../src/pi-component-adapter.js";
import type { PiExtensionApi, PiExtensionContext } from "../../src/extension-api.js";
import type { UiNode } from "../../src/extension-ui-frame.js";
const text = (id: string, value: string): UiNode => ({ id, type: "text", text: value });
const redPixelPng = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

/** End-to-end fixture for semantic controls and adapted Pi components on the same surface. */
export default function (pi: PiExtensionApi) {
  const evidence: Record<string, any> = { toolResults: [], slotFactories: {}, slotDisposals: {}, widgetWidths: [] };
  let overlayHandle: OverlayHandle | undefined;
  let overlayRedraw: (() => void) | undefined;
  let overlayAtTop = false;
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
    async execute(_id, input, signal, update) {
      update?.({ content: [{ type: "text", text: "partial" }], details: { phase: "partial", input } });
      const deadline = Date.now() + 20_000;
      while (!existsSync(process.env.UI_ACCEPTANCE_TOOL_CONTINUE!)) {
        signal?.throwIfAborted();
        if (Date.now() >= deadline) throw new Error("PTY did not acknowledge partial tool output");
        await delay(20);
      }
      return { content: [{ type: "text", text: "complete" }], details: { phase: "complete", input } };
    },
  });
  pi.registerShortcut("ctrl+k", { description: "PTY shortcut acceptance", handler: (ctx: PiExtensionContext) => {
    evidence.shortcut = true; ctx.ui.setWidget("shortcut", text("shortcut", "UI_PTY_SHORTCUT")); save();
  } });
  pi.registerShortcut("ctrl+g", { description: "Toggle acceptance overlay", handler: (ctx: PiExtensionContext) => {
    overlayHandle?.setHidden(!overlayHandle.isHidden());
    evidence.overlayHidden = overlayHandle?.isHidden();
    evidence.overlayEditor = ctx.ui.getEditorText(); save();
  } });
  pi.registerShortcut("ctrl+l", { description: "Focus acceptance overlay", handler: () => { overlayHandle?.focus(); } });
  pi.registerShortcut("ctrl+y", { description: "Release acceptance overlay", handler: () => { overlayHandle?.unfocus({ target: null }); } });
  pi.registerShortcut("ctrl+o", { description: "Move acceptance overlay", handler: () => { overlayAtTop = !overlayAtTop; overlayRedraw?.(); } });
  pi.on("session_start", async (_event, ctx) => {
    if (!ctx.hasUI) throw new Error("interactive UI missing");
    ctx.ui.setWorkingMessage("UI_PTY_WORKING"); ctx.ui.setHiddenThinkingLabel("UI_PTY_HIDDEN_THINKING");
    ctx.ui.setStatus("acceptance", "ready");
    ctx.ui.setHeader((_tui, theme) => {
      evidence.slotFactories.header = (evidence.slotFactories.header ?? 0) + 1;
      return { render: () => [theme.fg("accent", "UI_PTY_HEADER")], invalidate() {}, dispose() { evidence.slotDisposals.header = (evidence.slotDisposals.header ?? 0) + 1; save(); } };
    });
    ctx.ui.setFooter((tui, theme, data) => {
      evidence.slotFactories.footer = (evidence.slotFactories.footer ?? 0) + 1;
      const unsubscribe = data.onBranchChange(() => { evidence.branchChanges = (evidence.branchChanges ?? 0) + 1; tui.requestRender(); });
      return {
        render() {
          evidence.footerBranch = data.getGitBranch();
          evidence.footerStatuses = [...data.getExtensionStatuses()];
          evidence.footerProviderCount = data.getAvailableProviderCount(); save();
          return [theme.fg("muted", `UI_PTY_FOOTER ${evidence.footerBranch}`)];
        },
        invalidate() {}, dispose() { unsubscribe(); evidence.slotDisposals.footer = (evidence.slotDisposals.footer ?? 0) + 1; save(); },
      };
    });
    ctx.ui.setTitle("UI_PTY_TITLE");
    evidence.startup = await ctx.ui.custom({ id: "startup", type: "column", children: [text("title", "UI_PTY_STARTUP"), { id: "input", type: "input" }] });
    ctx.ui.setWidget("media", { id: "media", type: "row", gap: 1, children: [
      { id: "link", type: "link", text: "UI_PTY_LINK", url: "https://example.com/pi-ui" },
      { id: "image", type: "image", data: redPixelPng, mimeType: "image/png", alt: "UI_PTY_IMAGE", width: 2, height: 1 },
    ] });
    ctx.ui.setWidget("lines", ["UI_PTY_STRING_WIDGET"], { placement: "belowEditor" });
    ctx.ui.setWidget("acceptance", (_tui, theme) => {
      evidence.slotFactories.widget = (evidence.slotFactories.widget ?? 0) + 1;
      return {
        render(width) { evidence.widgetWidths.push(width); save(); return [theme.fg("success", "UI_PTY_MAIN_READY")]; },
        handleMouse(event) {
          if (event.type === "click") { evidence.widgetMouse = (evidence.widgetMouse ?? 0) + 1; save(); }
          return { handled: true, render: false };
        },
        invalidate() {}, dispose() { evidence.slotDisposals.widget = (evidence.slotDisposals.widget ?? 0) + 1; save(); },
      };
    });
    save();
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
    clearWidgets(ctx, ["acceptance", "interaction", "shortcut", "external-editor", "lines"]);
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
  pi.registerCommand("ui-custom", { description: "Pi component input over the shared surface", handler: async (_args, ctx) => {
    clearWidgets(ctx, ["completion-done"]);
    evidence.customWidths = [];
    evidence.customDisposals = 0;
    evidence.custom = await ctx.ui.custom<string | undefined>((tui, theme, _keys, done) => {
      const container = new Container();
      const input = new Input();
      input.onSubmit = done;
      input.onEscape = () => done(undefined);
      container.addChild(new Text(theme.fg("accent", "UI_PTY_CUSTOM"), 0, 0));
      container.addChild(input);
      tui.setFocus(input);
      return {
        render(width) { evidence.customWidths.push(width); save(); return container.render(width); },
        invalidate() { container.invalidate(); },
        dispose() { evidence.customDisposals++; save(); },
      };
    });
    save();
    ctx.ui.setWidget("custom-done", text("done", "UI_PTY_CUSTOM_DONE"));
  } });
  pi.registerCommand("ui-working", { description: "Working and tool presentation", handler: (_args, ctx) => {
    clearWidgets(ctx, ["update-done", "completion-done", "custom-done", "controlled-done", "mouse-done"]);
    ctx.ui.setProgress(true); ctx.ui.setProgramStatus({ state: "working", text: "UI_PTY_PROGRAM_WORKING", progress: 25 });
    evidence.workingRequested = true; save(); pi.sendUserMessage("run the ui_tool with value streamed");
  } });
  pi.registerCommand("ui-mouse", { description: "Mouse callbacks through nested Pi components", handler: async (_args, ctx) => {
    clearWidgets(ctx, ["controlled-done"]);
    evidence.mouseEvents = [];
    evidence.mouseDraft = await ctx.ui.custom<string>((_tui, _theme, _keys, done) => {
      const box = new Box(2, 1);
      box.addChild(new Text("UI_PTY_MOUSE", 0, 0));
      class MouseSettings extends SettingsList {
        override handleMouse(event: TuiMouseEvent) {
          evidence.mouseEvents.push(event.type); save();
          return super.handleMouse(event);
        }
      }
      box.addChild(new MouseSettings([{ id: "mode", label: "Mouse mode", currentValue: "one", values: ["one", "two"] }], 3,
        getSettingsListTheme(), (_id, value) => { evidence.mouseSetting = value; save(); }, () => done("cancelled")));
      box.addChild({
        render: () => ["UI_PTY_DRAG"], invalidate() {},
        handleMouse(event) {
          evidence.mouseDrag = { type: event.type, x: event.x, y: event.y }; save();
          return { handled: true, capture: event.type === "press", render: false };
        },
      });
      class MouseInput extends Input {
        override handleMouse(event: TuiMouseEvent) {
          // Force a real bridge round trip before queued typing can use the requested focus.
          if (event.type === "press") Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 60);
          return super.handleMouse(event);
        }
      }
      const input = new MouseInput();
      input.setValue("abcd"); input.onSubmit = done;
      box.addChild(input);
      return box;
    }, { overlay: true, overlayOptions: { width: 50 } });
    save(); ctx.ui.setWidget("mouse-done", ["UI_PTY_MOUSE_DONE"]);
  } });
  pi.registerCommand("ui-overlay-controls", { description: "Pi overlay positioning, visibility and focus", handler: async (_args, ctx) => {
    clearWidgets(ctx, ["custom-done"]);
    evidence.overlayWidths = [];
    evidence.overlayFactories = 0;
    evidence.overlayDisposals = 0;
    evidence.controlled = await ctx.ui.custom<string>((tui, _theme, _keys, done) => {
      evidence.overlayFactories++;
      const input = new Input();
      input.onSubmit = done;
      overlayRedraw = () => tui.requestRender();
      return {
        get focused() { return input.focused; }, set focused(value) { input.focused = value; },
        handleInput(data) { input.handleInput(data); },
        render(width) {
          evidence.overlayWidths.push(width);
          evidence.overlayState = { bounds: overlayHandle?.getBounds(), focused: overlayHandle?.isFocused(), inputFocused: input.focused, value: input.getValue() };
          save(); return ["UI_PTY_CONTROLLED", ...input.render(width)];
        },
        invalidate() { input.invalidate(); }, dispose() { evidence.overlayDisposals++; save(); },
      };
    }, {
      overlay: true,
      overlayOptions: () => ({ width: "50%", anchor: overlayAtTop ? "top-left" : "bottom-right", margin: { top: 1, right: 2, bottom: 1, left: 2 }, nonCapturing: true, visible: width => width >= 60 }),
      onHandle(handle) { overlayHandle = handle; },
    });
    overlayHandle = undefined; overlayRedraw = undefined; save();
    ctx.ui.setEditorText("");
    ctx.ui.setWidget("controlled-done", ["UI_PTY_CONTROLLED_DONE"]);
  } });
  pi.registerCommand("ui-finish", { description: "Finish PTY acceptance", handler: (_args, ctx) => {
    ctx.ui.setProgress(false); ctx.ui.setProgramStatus({ state: "clear" });
    ctx.ui.setHeader(undefined); ctx.ui.setFooter(undefined);
    evidence.finished = true; save(); ctx.shutdown();
  } });
}
