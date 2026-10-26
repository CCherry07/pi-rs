import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { Input, Text, stripTerminalSequences, type OverlayHandle } from "@earendil-works/pi-tui";

import { ExtensionHost } from "../src/extension-host.js";
import { parseGenerationManifest } from "../src/extension-protocol.js";
import { ExtensionUI } from "../src/extension-ui.js";
import { ExtensionTranscript } from "../src/extension-ui-transcript.js";
import { getAgentDir } from "../src/extension-ui-paths.js";
import type { NativeExtensionContext } from "../src/native-binding.js";
import type { UiSnapshot } from "../src/extension-ui-frame.js";

const snapshot = { columns: 80, rows: 24, editorText: "", toolsExpanded: false, transcript: [], transcriptWidth: 72, areas: [], regions: [] as UiSnapshot["regions"] };
const tick = () => new Promise<void>((resolve) => setImmediate(resolve));

test("Pi widgets accept lines and persistent factories, resize and dispose on replacement", async () => {
  const ui = new ExtensionUI();
  const app = frontend();
  ui.bind(app.client);
  let factories = 0;
  let disposed = 0;
  let redraw: (() => void) | undefined;
  let value = "first";
  try {
    Reflect.apply(ui.setWidget, ui, ["lines", ["one", "two"]]);
    Reflect.apply(ui.setWidget, ui, ["tasks", (tui: any, theme: any) => {
      factories++;
      redraw = () => tui.requestRender();
      return { render: (width: number) => [theme.fg("accent", `${value}:${width}`)], invalidate() {}, dispose() { disposed++; } };
    }, { placement: "belowEditor" }]);
    await tick();
    assert.deepEqual(app.frames.at(-1).regions[0].root.lines, ["one", "two"]);
    assert.equal(app.frames.at(-1).regions[1].placement, "belowEditor");
    assert.match(app.frames.at(-1).regions[1].root.lines[0], /first:80/);
    value = "updated";
    redraw?.();
    await tick();
    app.resize(42);
    await tick();
    assert.equal(factories, 1);
    assert.match(app.frames.at(-1).regions[1].root.lines[0], /updated:42/);
    ui.setWidget("tasks", { id: "replacement", type: "text", text: "replacement" });
    await tick();
    assert.equal(disposed, 1);
    ui.setWidget("lines", undefined);
    await tick();
    assert.equal(app.frames.at(-1).regions.length, 1);
  } finally { ui.retire(); }
  assert.equal(disposed, 1);
});

test("Pi header and footer factories share status data and clean up independently", async () => {
  const ui = new ExtensionUI();
  const app = frontend();
  ui.bind({ ...app.client, query(raw) {
    if (JSON.parse(raw).type === "availableModels") return JSON.stringify([{ provider: "one" }, { provider: "one" }, { provider: "two" }]);
    return app.client.query(raw);
  } });
  const disposed: string[] = [];
  try {
    ui.setStatus("tasks", "2 open");
    Reflect.apply(ui.setHeader, ui, [() => ({ render: () => ["header"], invalidate() {}, dispose() { disposed.push("header"); } })]);
    Reflect.apply(ui.setFooter, ui, [(_tui: unknown, _theme: unknown, data: any) => ({
      render: () => [`${data.getExtensionStatuses().get("tasks")}:${data.getAvailableProviderCount()}`],
      invalidate() {}, dispose() { disposed.push("footer"); },
    })]);
    await tick();
    const regions = app.frames.at(-1).regions;
    assert.deepEqual(regions.map((region: any) => region.id), ["header", "footer"]);
    assert.deepEqual(regions[1].root.lines, ["2 open:2"]);
    ui.setStatus("tasks", "3 open");
    await tick();
    assert.deepEqual(app.frames.at(-1).regions[1].root.lines, ["3 open:2"]);
    ui.setFooter(undefined);
    await tick();
    assert.deepEqual(disposed, ["footer"]);
    assert.ok(app.frames.at(-1).regions.some((region: any) => region.id === "_statuses"));
  } finally { ui.retire(); }
  assert.deepEqual(disposed, ["footer", "header"]);
});

test("a broken Pi slot disposes itself without retiring unrelated widgets", async () => {
  const ui = new ExtensionUI();
  const app = frontend();
  ui.bind(app.client);
  let fail = false;
  let disposed = 0;
  try {
    Reflect.apply(ui.setWidget, ui, ["broken", () => ({
      render() { if (fail) throw new Error("slot failed"); return ["working"]; },
      invalidate() {}, dispose() { disposed++; throw new Error("dispose failed"); },
    })]);
    ui.setWidget("healthy", { id: "healthy", type: "text", text: "still here" });
    await tick();
    fail = true;
    app.resize(40);
    await tick();
    assert.equal(disposed, 1);
    assert.deepEqual(app.frames.at(-1).regions.map((region: any) => region.id), ["widget:healthy"]);
    ui.setStatus("alive", "yes");
  } finally { ui.retire(); }
});

function frontend() {
  const frames: any[] = [];
  const commands: any[] = [];
  let current = { ...snapshot };
  let pending: ((value: string) => void) | undefined;
  const event = (value: unknown) => { const resolve = pending; pending = undefined; resolve?.(JSON.stringify(value)); };
  const client: NativeExtensionContext = {
    query: () => JSON.stringify(current),
    notify(raw) {
      const { command } = JSON.parse(raw);
      commands.push(command);
      if (command?.channel === "surface" && command.command.type === "wake") event({ type: "refresh", snapshot: current });
      if (command?.channel === "surface" && command.command.type === "close") event({ type: "closed" });
    },
    request(raw) {
      frames.push(JSON.parse(raw).frame);
      return new Promise((resolve) => { pending = resolve; });
    },
  };
  return {
    client, frames, commands,
    regions(regions: UiSnapshot["regions"]) { current = { ...current, regions }; event({ type: "refresh", snapshot: current }); },
    resize(columns: number) { current = { ...current, columns }; event({ type: "refresh", snapshot: current }); },
    input(data: string) {
      const frame = frames.at(-1);
      const region = frame.regions.at(-1);
      event({ type: "interaction", revision: frame.revision, regionId: region.id, nodeId: region.root.id, event: { type: "customInput", data }, snapshot: current });
    },
  };
}

test("dynamic overlay layout and hide/show retain the component and deferred result", async () => {
  const ui = new ExtensionUI();
  const app = frontend();
  ui.bind(app.client);
  let handle!: OverlayHandle;
  let done!: (value: number) => void;
  let factories = 0;
  let layouts = 0;
  let disposed = 0;
  let renders = 0;
  const input: string[] = [];
  const result = ui.custom<number>((_tui, _theme, _keys, finish) => {
    factories++;
    done = finish;
    return { render: width => { renders++; return [`width:${width}`]; }, invalidate() {}, handleInput: data => { input.push(data); }, dispose() { disposed++; } };
  }, {
    overlay: true,
    overlayOptions: () => { layouts++; return { width: "50%", minWidth: 30, anchor: "right-center", row: "25%", margin: { right: 2 }, visible: width => width >= 100 }; },
    onHandle: value => { handle = value; },
  });
  try {
    await tick();
    assert.equal(app.frames.at(-1).regions[0].overlay.hidden, true);
    assert.equal(renders, 0);
    app.resize(120);
    await tick();
    const overlay = app.frames.at(-1).regions[0].overlay;
    assert.equal(overlay.width, 60);
    assert.equal(overlay.anchor, "right-center");
    assert.deepEqual(overlay.row, { type: "percent", value: 2500 });
    assert.equal(overlay.hidden, false);
    handle.setHidden(true);
    await tick();
    const hiddenRenders = renders;
    app.input("ignored");
    await tick();
    assert.deepEqual(input, []);
    assert.equal(renders, hiddenRenders);
    handle.setHidden(false);
    await tick();
    app.input("accepted");
    await tick();
    assert.deepEqual(input, ["accepted"]);
    assert.equal(factories, 1);
    assert.ok(layouts > 2);
    handle.hide();
    await tick();
    assert.equal(app.frames.at(-1).regions.length, 0);
    assert.equal(disposed, 0);
    let completed = false;
    void result.then(() => { completed = true; });
    await tick();
    assert.equal(completed, false);
    done(7);
    assert.equal(await result, 7);
    assert.equal(disposed, 1);
  } finally { ui.retire(); }
});

test("overlay handles read frontend bounds and route focus changes to Rust", async () => {
  const ui = new ExtensionUI();
  const app = frontend();
  ui.bind(app.client);
  let handle!: OverlayHandle;
  const input = new Input();
  const result = ui.custom(() => input, { overlay: true, overlayOptions: { nonCapturing: true }, onHandle: value => { handle = value; } });
  try {
    await tick();
    const region = app.frames.at(-1).regions[0];
    assert.equal(region.overlay.nonCapturing, true);
    assert.equal(handle.isFocused(), false);
    assert.equal(input.focused, false);
    handle.focus();
    assert.deepEqual(app.commands.at(-1), { channel: "surface", command: { type: "focus", regionId: region.id, nodeId: "custom-content" } });
    const bounds = { row: 2, col: 3, width: 40, height: 1 };
    app.regions([{ owner: ui.owner, regionId: region.id, focused: true, bounds }]);
    await tick();
    assert.equal(handle.isFocused(), true);
    assert.equal(input.focused, true);
    assert.deepEqual(handle.getBounds(), bounds);
    handle.unfocus({ target: null });
    assert.deepEqual(app.commands.at(-1), { channel: "surface", command: { type: "unfocus", regionId: region.id, target: { type: "app" } } });
    app.regions([{ owner: ui.owner, regionId: region.id, focused: false, bounds }]);
    await tick();
    assert.equal(input.focused, false);
    app.regions([{ owner: ui.owner, regionId: region.id, focused: true, bounds }]);
    await tick();
    assert.equal(input.focused, true);
    handle.setHidden(true);
    await tick();
    assert.equal(handle.isFocused(), false);
    assert.equal(input.focused, false);
    assert.equal(handle.getBounds(), undefined);
  } finally { ui.retire(); }
  assert.equal(await result, undefined);
});

test("a component finishing during its first mounted render leaves no live region", async () => {
  const ui = new ExtensionUI();
  const app = frontend();
  ui.bind(app.client);
  let renders = 0;
  let disposed = 0;
  try {
    const result = ui.custom<string>((_tui, _theme, _keys, done) => ({
      render() { if (++renders === 2) done("finished"); return ["temporary"]; },
      invalidate() {}, dispose() { disposed++; },
    }));
    assert.equal(await result, "finished");
    await tick();
    assert.equal(disposed, 1);
    assert.deepEqual(app.frames.at(-1).regions, []);
  } finally { ui.retire(); }
});

test("explicit overlay focus release sends the source and destination together", async () => {
  const ui = new ExtensionUI();
  const app = frontend();
  ui.bind(app.client);
  const target = new Input();
  const editor = ui.custom(() => target);
  let handle!: OverlayHandle;
  const overlay = ui.custom(() => new Input(), { overlay: true, onHandle: value => { handle = value; } });
  try {
    await tick();
    const regions = app.frames.at(-1).regions;
    handle.unfocus({ target });
    assert.deepEqual(app.commands.at(-1), { channel: "surface", command: {
      type: "unfocus", regionId: regions[1].id, target: { type: "region", regionId: regions[0].id },
    } });
  } finally { ui.retire(); }
  assert.deepEqual(await Promise.all([editor, overlay]), [undefined, undefined]);
});

test("loads real pi-tui components from both supported Pi package namespaces", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "pi-custom-ui-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  for (const scope of ["@earendil-works", "@mariozechner"]) {
    const path = join(root, `${scope.slice(1)}.ts`);
    await writeFile(path, `
      import { Text, matchesKey, visibleWidth } from "${scope}/pi-tui";
      export default (pi: any) => pi.registerTool({
        name: "todo", description: "todo", parameters: { type: "object" },
        execute: () => ({ content: [{ type: "text", text: JSON.stringify({
          lines: new Text("你好", 0, 0).render(8), width: visibleWidth("你好"), escape: matchesKey("\\x1b", "escape")
        }) }] })
      });
    `);
    const host = new ExtensionHost();
    const manifest = parseGenerationManifest(await host.dispatch(JSON.stringify({ type: "prepareGeneration", request: { projectTrusted: true, extensionPaths: [path], mode: "print" } })));
    const tool = manifest.agentPlugins[0]?.tools[0];
    assert.ok(tool);
    const result = JSON.parse(await host.dispatch(JSON.stringify({ type: "invoke", invocation: { invocationId: "todo", generationId: manifest.generationId, callbackId: tool.callbackId, kind: "tool", payload: { input: {}, context: { toolCallId: "todo-1" } } } })));
    const value = JSON.parse(result.content[0].text);
    assert.equal(value.width, 4);
    assert.equal(value.escape, true);
    assert.match(value.lines.join("\n"), /你好/);
    await host.dispatch(JSON.stringify({ type: "retireGeneration", generationId: manifest.generationId }));
  }
});

test("SettingsList themes load through both Pi SDK namespaces and support command interaction", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "pi-settings-theme-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  for (const scope of ["@earendil-works", "@mariozechner"]) {
    await t.test(scope, async () => {
      const path = join(root, `${scope.slice(1)}.ts`);
      await writeFile(path, `
        import { getSettingsListTheme } from "${scope}/pi-coding-agent";
        import { SettingsList } from "${scope}/pi-tui";
        export default (pi: any) => pi.registerCommand("tasks", {
          handler: () => {
            const changes: unknown[] = [];
            let cancelled = false;
            const list = new SettingsList([
              { id: "scope", label: "Task storage", description: "Choose where tasks are stored", currentValue: "session", values: ["session", "memory"] },
              { id: "collapse", label: "Collapse completed tasks", currentValue: "off", values: ["off", "on"] },
            ], 2, getSettingsListTheme(), (id, value) => changes.push([id, value]), () => { cancelled = true; });
            const before = list.render(60);
            list.handleInput("\\r");
            const after = list.render(60);
            list.handleInput("\\x1b");
            return { action: "transform", text: JSON.stringify({ before, after, changes, cancelled }) };
          },
        });
      `);
      const host = new ExtensionHost();
      const manifest = parseGenerationManifest(await host.dispatch(JSON.stringify({
        type: "prepareGeneration",
        request: { projectTrusted: true, extensionPaths: [path], mode: "print" },
      })));
      try {
        const command = manifest.agentPlugins[0]?.commands[0];
        assert.ok(command);
        const result = JSON.parse(await host.dispatch(JSON.stringify({
          type: "invoke",
          invocation: { invocationId: "settings", generationId: manifest.generationId, callbackId: command.callbackId, kind: "command", payload: { arguments: "", context: {} } },
        })));
        const value = JSON.parse(result.text);
        const before = stripTerminalSequences(value.before.join("\n"));
        assert.match(before, /→ Task storage\s+session/);
        assert.match(before, /Collapse completed tasks\s+off/);
        assert.match(before, /Choose where tasks are stored/);
        assert.match(stripTerminalSequences(value.after.join("\n")), /→ Task storage\s+memory/);
        assert.deepEqual(value.changes, [["scope", "memory"]]);
        assert.equal(value.cancelled, true);
      } finally {
        await host.dispatch(JSON.stringify({ type: "retireGeneration", generationId: manifest.generationId }));
      }
    });
  }
});

test("custom components render in the editor, resize, receive input and dispose on done", async () => {
  const ui = new ExtensionUI();
  const app = frontend();
  ui.bind(app.client);
  const inputs: string[] = [];
  let disposed = 0;
  const result = Reflect.apply(ui.custom, ui, [(_tui: unknown, theme: any, _keys: unknown, done: (value: string) => void) => ({
    render: (width: number) => [theme.fg("accent", `Todos ${width}`)],
    invalidate() {},
    handleInput(data: string) { inputs.push(data); if (data === "\x1b") done("closed"); },
    dispose() { disposed++; },
  })]) as Promise<unknown>;
  void result.catch(() => {});
  try {
    await tick();
    const node = app.frames.at(-1)?.regions[0]?.root;
    assert.equal(node?.type, "custom");
    assert.equal(app.frames.at(-1)?.regions[0]?.placement, "editor");
    assert.match(node.lines.join("\n"), /Todos 80/);
    app.resize(60);
    await tick();
    assert.match(app.frames.at(-1).regions[0].root.lines.join("\n"), /Todos 60/);
    app.input("\x1b");
    assert.equal(await result, "closed");
    await tick();
    assert.deepEqual(inputs, ["\x1b"]);
    assert.deepEqual(app.frames.at(-1).regions, []);
    assert.equal(disposed, 1);
  } finally { ui.retire(); }
});

test("Pi tool renderers receive their original arguments and rerender on width changes", () => {
  const transcript = new ExtensionTranscript(() => {});
  transcript.registerTool("todo", {
    renderCall(args: any, theme: any, context: any) {
      assert.equal(context.toolCallId, "todo-1");
      return { render: (width: number) => [theme.fg("accent", `todo ${args.action} ${width}`)], invalidate() {} };
    },
    renderResult(result: any, options: any, theme: any, context: any) {
      assert.equal(options.expanded, false);
      assert.equal(context.toolCallId, "todo-1");
      return { render: () => [theme.fg("success", result.content[0].text)], invalidate() {} };
    },
  });
  const view = { ...snapshot, isRunning: false, transcript: [{ index: 0, fingerprint: "same", payload: { kind: "tool", name: "todo", toolCallId: "todo-1", args: { action: "list" }, result: { content: [{ type: "text", text: "No todos" }] } } }] };
  const first = transcript.render(view)[0]?.root;
  assert.match(JSON.stringify(first), /todo list 72/);
  assert.match(JSON.stringify(first), /No todos/);
  assert.match(JSON.stringify(transcript.render({ ...view, transcriptWidth: 40 })[0]?.root), /todo list 40/);
  transcript.dispose();
});

test("a cancelled asynchronous custom factory disposes its late component without mounting", async (t) => {
  for (const reason of ["abort", "timeout", "retire"] as const) {
    await t.test(reason, async () => {
      const ui = new ExtensionUI();
      const app = frontend();
      ui.bind(app.client);
      const controller = new AbortController();
      let complete!: (component: Text & { dispose(): void }) => void;
      const created = new Promise<Text & { dispose(): void }>(resolve => { complete = resolve; });
      let disposed = 0;
      const result = ui.custom(() => created, { signal: controller.signal, ...(reason === "timeout" ? { timeout: 1 } : {}) });
      if (reason === "abort") controller.abort();
      if (reason === "retire") ui.retire();
      assert.equal(await result, undefined);
      complete(Object.assign(new Text("late", 0, 0), { dispose() { disposed++; } }));
      await tick();
      ui.retire();
      assert.equal(disposed, 1);
      assert.ok(app.frames.every(frame => frame.regions.length === 0));
    });
  }
});

test("synchronous completion and throwing disposal settle once and release the region", async () => {
  const ui = new ExtensionUI();
  const app = frontend();
  ui.bind(app.client);
  let disposed = 0;
  const completed = ui.custom<string>((_tui, _theme, _keys, done) => {
    done("ready");
    return Object.assign(new Text("late", 0, 0), { dispose() { disposed++; } });
  });
  assert.equal(await completed, "ready");
  await tick();
  assert.equal(disposed, 1);
  let close!: () => void;
  const result = ui.custom<void>((_tui, _theme, _keys, done) => {
    close = done;
    return Object.assign(new Text("open", 0, 0), { dispose() { disposed++; throw new Error("dispose failed"); } });
  });
  const rejected = assert.rejects(result, /dispose failed/);
  await tick();
  close();
  await rejected;
  await tick();
  assert.deepEqual(app.frames.at(-1).regions, []);
  ui.retire();
  assert.equal(disposed, 2);
});

test("a custom render failure closes only its own region", async () => {
  const ui = new ExtensionUI();
  const app = frontend();
  ui.bind(app.client);
  ui.setWidget("retained", { id: "widget", type: "text", text: "retained" });
  let disposed = 0;
  let broken = false;
  const result = ui.custom(() => ({
    render() { if (broken) throw new Error("render failed"); return ["open"]; },
    invalidate() {},
    dispose() { disposed++; },
  }));
  const rejected = assert.rejects(result, /render failed/);
  await tick();
  broken = true;
  app.resize(60);
  await rejected;
  await tick();
  assert.deepEqual(app.frames.at(-1).regions.map((region: any) => region.id), ["widget:retained"]);
  assert.doesNotThrow(() => ui.setStatus("alive", "ready"));
  ui.retire();
  assert.equal(disposed, 1);
});

test("a real pi-tui Input receives Unicode paste, cursor movement and submit", async () => {
  const ui = new ExtensionUI();
  const app = frontend();
  ui.bind(app.client);
  const result = ui.custom<string>((_tui, _theme, _keys, done) => {
    const input = new Input();
    input.onSubmit = done;
    return input;
  }, { overlay: true, overlayOptions: { width: "50%" } });
  try {
    await tick();
    assert.equal(app.frames.at(-1).regions[0].placement, "overlay");
    assert.equal(app.frames.at(-1).regions[0].overlay.width, 40);
    app.input("\x1b[200~你好\x1b[201~");
    await tick();
    app.input("\x1b[D");
    await tick();
    app.input("啊");
    await tick();
    const node = app.frames.at(-1).regions[0].root;
    assert.match(stripTerminalSequences(node.lines.join("\n")), /你啊好/);
    assert.ok(node.cursor);
    app.input("\r");
    assert.equal(await result, "你啊好");
  } finally { ui.retire(); }
});

test("tool call and result slots retain separate lastComponent instances and release them", () => {
  const ui = new ExtensionTranscript(() => {});
  let created = 0;
  let disposed = 0;
  const retained = new Map<string, Text>();
  const render = (slot: string, value: any, context: any) => {
    assert.equal(context.lastComponent, retained.get(slot));
    let component = context.lastComponent as Text | undefined;
    if (!component) {
      created++;
      component = Object.assign(new Text("", 0, 0), { dispose() { disposed++; } });
      retained.set(slot, component);
    }
    component.setText(`${slot}: ${value}`);
    return component;
  };
  ui.registerTool("test", {
    renderCall(args: any, _theme: unknown, context: any) { return render("call", args.value, context); },
    renderResult(result: any, _options: unknown, _theme: unknown, context: any) { return render("result", result.value, context); },
  });
  const view = { ...snapshot, isRunning: false, transcript: [{ index: 0, fingerprint: "1", payload: { kind: "tool", toolCallId: "1", name: "test", args: { value: "one" }, result: { value: "partial" } } }] };
  ui.render(view);
  ui.render({ ...view, transcriptWidth: 20 });
  const final = ui.render({ ...view, transcript: [{ ...view.transcript[0]!, fingerprint: "2", payload: { ...view.transcript[0]!.payload, result: { value: "final" } } }] });
  assert.match(JSON.stringify(final), /result: final/);
  assert.equal(created, 2);
  assert.equal(disposed, 0);
  ui.render({ ...view, transcript: [] });
  assert.equal(disposed, 2);
  ui.dispose();
  assert.equal(disposed, 2);
});

test("retirement disposes transcript components in their owning agent context", () => {
  const agentDir = join(tmpdir(), "custom-component-agent");
  const ui = new ExtensionUI({ agentDir });
  const disposedIn: string[] = [];
  ui.transcript.registerTool("scoped", {
    renderCall() {
      return Object.assign(new Text("scoped", 0, 0), { dispose() { disposedIn.push(getAgentDir()); } });
    },
  });
  ui.transcript.render({ ...snapshot, isRunning: false, transcript: [
    { index: 0, fingerprint: "scoped", payload: { kind: "tool", name: "scoped", toolCallId: "scoped", args: {} } },
  ] });
  ui.retire();
  ui.retire();
  assert.deepEqual(disposedIn, [agentDir]);
});
