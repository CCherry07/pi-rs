import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { Input, Text, stripTerminalSequences } from "@earendil-works/pi-tui";

import { ExtensionHost } from "../src/extension-host.js";
import { parseGenerationManifest } from "../src/extension-protocol.js";
import { ExtensionUI } from "../src/extension-ui.js";
import { ExtensionTranscript } from "../src/extension-ui-transcript.js";
import { getAgentDir } from "../src/extension-ui-paths.js";
import type { NativeExtensionContext } from "../src/native-binding.js";

const snapshot = { columns: 80, rows: 24, editorText: "", toolsExpanded: false, transcript: [], transcriptWidth: 72, areas: [] };
const tick = () => new Promise<void>((resolve) => setImmediate(resolve));

function frontend() {
  const frames: any[] = [];
  let current = { ...snapshot };
  let pending: ((value: string) => void) | undefined;
  const event = (value: unknown) => { const resolve = pending; pending = undefined; resolve?.(JSON.stringify(value)); };
  const client: NativeExtensionContext = {
    query: () => JSON.stringify(current),
    notify(raw) {
      const { command } = JSON.parse(raw);
      if (command?.channel === "surface" && command.command.type === "wake") event({ type: "refresh", snapshot: current });
      if (command?.channel === "surface" && command.command.type === "close") event({ type: "closed" });
    },
    request(raw) {
      frames.push(JSON.parse(raw).frame);
      return new Promise((resolve) => { pending = resolve; });
    },
  };
  return {
    client, frames,
    resize(columns: number) { current = { ...current, columns }; event({ type: "refresh", snapshot: current }); },
    input(data: string) {
      const frame = frames.at(-1);
      const region = frame.regions.at(-1);
      event({ type: "interaction", revision: frame.revision, regionId: region.id, nodeId: region.root.id, event: { type: "customInput", data }, snapshot: current });
    },
  };
}

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
