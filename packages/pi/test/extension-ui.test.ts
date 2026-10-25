import test from "node:test";
import assert from "node:assert/strict";
import { ExtensionUI } from "../src/extension-ui.js";
import { uiNodeSchema, type UiRegion, type UiInteraction, type UiComponentEvent } from "../src/extension-ui-frame.js";
import type { NativeExtensionContext } from "../src/native-binding.js";

const snapshot = { columns: 80, rows: 24, editorText: "draft", toolsExpanded: false };
function frontend({ autoWake = true } = {}) {
  let currentSnapshot: Record<string, unknown> = { ...snapshot };
  const frames: { owner: string; revision: number; regions: UiRegion[]; shortcuts: { key: string }[] }[] = [];
  const actions: Record<string, any>[] = [];
  let pending: ((value: string) => void) | undefined;
  const client: NativeExtensionContext = {
    query: () => JSON.stringify(currentSnapshot),
    notify: (raw) => {
      const operation = JSON.parse(raw);
      actions.push(operation);
      if (autoWake && operation.command?.channel === "surface" && operation.command.command.type === "wake") event({ type: "refresh", snapshot: currentSnapshot });
      if (operation.command?.channel === "surface" && operation.command.command.type === "close") event({ type: "closed" });
    },
    request: (raw) => { frames.push(JSON.parse(raw).frame); return new Promise((resolve) => { pending = resolve; }); },
  };
  function event(value: unknown) { const resolve = pending; pending = undefined; resolve?.(JSON.stringify(value)); }
  function interaction(value: UiInteraction, nodeId = "value", regionId = frames.at(-1)!.regions.at(-1)!.id, revision = frames.at(-1)!.revision) {
    event({ type: "interaction", regionId, nodeId, revision, event: value, snapshot: currentSnapshot });
  }
  function refresh(values: Record<string, unknown> = {}) { currentSnapshot = { ...currentSnapshot, ...values }; event({ type: "refresh", snapshot: currentSnapshot }); }
  return { client, frames, actions, event, interaction, refresh };
}
const tick = () => new Promise((resolve) => setImmediate(resolve));

test("standard dialogs publish semantic trees and receive Rust selection results", async () => {
  const host = new ExtensionUI(); const app = frontend(); host.bind(app.client);
  const result = host.select("环境", ["测试", "生产"]);
  await tick();
  const region = app.frames.at(-1)!.regions[0]!;
  assert.equal(region.placement, "overlay");
  assert.equal(region.overlay?.width, 60);
  assert.deepEqual(region.root, { id: "dialog", type: "panel", title: "环境", child: { id: "body", type: "column", gap: 1, children: [
    { id: "value", type: "select", options: ["测试", "生产"], initialSelected: 0 },
    { id: "hint", type: "text", text: "↑↓ move · enter select · esc cancel", tone: "muted" },
  ] } });
  assert.equal("lines" in region, false);
  app.interaction({ type: "submit", value: 1 });
  assert.equal(await result, "生产");
  await tick(); assert.deepEqual(app.frames.at(-1)!.regions, []);
  host.retire();
});

test("rich multi-selection uses the same semantic tree exchange", async () => {
  const host = new ExtensionUI(); const app = frontend(); host.bind(app.client);
  const request = {
    title: "Skills",
    options: [{ label: "Alpha", searchText: "alpha", detailLines: [], readOnly: false, sortValues: [["alpha"]] }],
    actions: [{ id: "delete", key: "d", label: "delete", enabled: true }],
    categories: [] as [string, string][], sortModes: [["Name", false]] as [string, boolean][],
    initiallySelected: [], initialQuery: "", initialActiveCategories: [], initialSortMode: 0, summaryLines: [],
  };
  const result = host.multiSelect(request);
  await tick();
  assert.deepEqual(app.frames.at(-1)!.regions[0]!.root, { id: "multi-select", type: "multiSelect", request });
  app.interaction({
    type: "submit",
    value: { selected: [0], actionId: "delete", query: "a", activeCategories: [], sortMode: 0 },
  }, "multi-select");
  assert.deepEqual(await result, { selected: [0], actionId: "delete", query: "a", activeCategories: [], sortMode: 0 });
  host.retire();
});

test("mount updates are revisions and reject late interactions from the old document", async () => {
  const host = new ExtensionUI(); const app = frontend(); host.bind(app.client);
  const events: string[] = [];
  const mount = host.mount({ id: "button", type: "button", text: "before" }, { id: "widget", onEvent: (event) => events.push(event.event.type) });
  await tick(); const previous = app.frames.at(-1)!.revision;
  mount.update({ id: "button", type: "button", text: "after" });
  await tick();
  app.interaction({ type: "activate" }, "button", mount.id, previous);
  await tick(); assert.deepEqual(events, []);
  app.interaction({ type: "activate" }, "button", mount.id);
  await tick(); assert.deepEqual(events, ["activate"]);
  mount.unmount(); await tick(); assert.deepEqual(app.frames.at(-1)!.regions, []);
  assert.throws(() => mount.update({ id: "button", type: "button", text: "late" }), /unmounted/);
  host.retire();
});

test("typing and submit survive descriptor updates while an exchange is pending", async () => {
  const host = new ExtensionUI(); const app = frontend({ autoWake: false }); host.bind(app.client);
  const events: string[] = [];
  const mount = host.mount({ id: "value", type: "input" }, { onEvent: (event) => {
    events.push(event.event.type);
    if (event.event.type === "change") mount.update({ id: "value", type: "input", completions: [{ value: "hello", label: "Hello" }] });
  } });
  await tick();
  app.interaction({ type: "change", value: "hel" }, "value", mount.id);
  host.setStatus("connection", "Updated before the callback runs");
  await tick(); assert.deepEqual(events, ["change"]);
  const revision = app.frames.at(-1)!.revision;
  app.interaction({ type: "submit", value: "hello" }, "value", mount.id, revision);
  host.setWidget("unrelated", { id: "other", type: "text", text: "Updated during input" });
  await tick(); assert.deepEqual(events, ["change", "submit"]);
  assert.ok(app.frames.at(-1)!.revision > revision);
  host.retire();
});

test("removed or replaced nodes cannot receive interactions from an in-flight document", async () => {
  const host = new ExtensionUI(); const app = frontend({ autoWake: false }); host.bind(app.client);
  const events: string[] = [];
  const onEvent = (event: UiComponentEvent) => { events.push(event.event.type); };
  const mount = host.mount({ id: "value", type: "button", text: "old" }, { id: "widget", onEvent });
  await tick();
  app.interaction({ type: "activate" }, "value", mount.id);
  mount.update({ id: "value", type: "input" });
  await tick(); assert.deepEqual(events, []);
  app.interaction({ type: "submit", value: "old" }, "value", mount.id);
  mount.update({ id: "different", type: "input" });
  mount.update({ id: "value", type: "input" });
  await tick(); assert.deepEqual(events, []);
  app.interaction({ type: "submit", value: "old" }, "value", mount.id);
  mount.unmount();
  host.mount({ id: "value", type: "input" }, { id: "widget", onEvent });
  await tick(); assert.deepEqual(events, []);
  host.retire();
});

test("frame revisions change for transcript content and remain stable for identical refreshes", async () => {
  const host = new ExtensionUI(); const app = frontend(); host.bind(app.client);
  host.transcript.registerMessage("sample", () => ({ id: "message", type: "text", text: "Rendered by Rust" }));
  await tick(); const before = app.frames.at(-1)!.revision;
  app.refresh({ transcript: [{ index: 0, fingerprint: "first", payload: { kind: "customMessage", entry: { customType: "sample" } } }] });
  await tick(); const rendered = app.frames.at(-1)!.revision;
  assert.ok(rendered > before);
  app.refresh(); await tick(); assert.equal(app.frames.at(-1)!.revision, rendered);
  host.retire();
});

test("completion descriptors and supported theme actions retain their native semantics", async () => {
  const host = new ExtensionUI(); const app = frontend(); host.bind(app.client);
  const completions = [{ value: "hello", label: "Hello", description: "Greeting" }];
  host.mount({ id: "value", type: "editor", completions });
  await tick();
  assert.deepEqual(app.frames.at(-1)!.regions[0]!.root, { id: "value", type: "editor", initialValue: "", placeholder: "", height: 6, completions });
  host.setTheme("dark");
  assert.deepEqual(app.actions.at(-1), {
    type: "uiCommand",
    owner: host.owner,
    command: { channel: "product", command: { type: "theme", name: "dark" } },
  });
  assert.throws(() => Reflect.apply(host.setTheme, host, ["unknown"]), /Unknown UI theme/);
  host.retire();
});

test("scroll containers validate nested nodes and route child interactions", async () => {
  const host = new ExtensionUI(); const app = frontend(); host.bind(app.client);
  const events: string[] = [];
  const root = { id: "viewport", type: "scroll" as const, height: 5, child: { id: "button", type: "button" as const, text: "Submit" } };
  const mount = host.mount(root, { onEvent: (event) => events.push(event.event.type) });
  await tick();
  assert.deepEqual(app.frames.at(-1)!.regions[0]!.root, root);
  app.interaction({ type: "activate" }, "button", mount.id);
  await tick(); assert.deepEqual(events, ["activate"]);
  assert.throws(() => uiNodeSchema.parse({ ...root, child: { ...root.child, id: "viewport" } }), /Duplicate UI node id/);
  assert.throws(() => uiNodeSchema.parse({ ...root, height: -1 }), /Too small/);
  host.retire();
});

test("abort and retirement close pending dialogs and release their mounts", async () => {
  const host = new ExtensionUI(); const app = frontend(); host.bind(app.client);
  const signal = new AbortController();
  const result = host.input("name", "请输入", { signal: signal.signal });
  await tick(); signal.abort(); assert.equal(await result, undefined);
  const pending = host.editor("edit", "值"); await tick(); host.retire();
  assert.equal(await pending, undefined);
  assert.throws(() => host.mount({ id: "late", type: "text", text: "late" }), /retired/);
});

test("invocation cancellation applies to descriptor custom dialogs", async () => {
  const host = new ExtensionUI(); const app = frontend(); host.bind(app.client);
  const controller = new AbortController();
  const ui = host.forContext(controller.signal) as ExtensionUI;
  const result = ui.custom({ id: "value", type: "input" });
  await tick(); controller.abort(); assert.equal(await result, undefined); host.retire();
});

test("mounted UI outlives its command while explicit mount cancellation still works", async () => {
  const host = new ExtensionUI(); const app = frontend(); host.bind(app.client);
  const invocation = new AbortController();
  const ui = host.forContext(invocation.signal) as ExtensionUI;
  let activated = false;
  const mount = ui.mount({ id: "button", type: "button", text: "persist" }, { onEvent: () => { activated = true; } });
  await tick(); invocation.abort(); await tick();
  assert.equal(app.frames.at(-1)!.regions.length, 1);
  app.interaction({ type: "activate" }, "button", mount.id);
  await tick(); assert.equal(activated, true);
  mount.unmount();
  const lifetime = new AbortController();
  ui.mount({ id: "scoped", type: "text", text: "scoped" }, { signal: lifetime.signal });
  await tick(); lifetime.abort(); await tick();
  assert.equal(app.frames.at(-1)!.regions.length, 0);
  host.retire();
});

test("shortcuts are semantic host events and never require JavaScript key decoding", async () => {
  const host = new ExtensionUI(); const app = frontend(); host.bind(app.client);
  let invoked = 0; host.registerShortcut("ctrl+k", async () => { invoked++; });
  await tick(); assert.deepEqual(app.frames.at(-1)!.shortcuts, [{ key: "ctrl+k" }]);
  app.interaction({ type: "shortcut", key: "ctrl+k" }, "", "");
  await tick(); assert.equal(invoked, 1); host.retire();
});

test("physical clipboard requests are executed by the native frontend", async () => {
  const host = new ExtensionUI(); const app = frontend(); host.bind(app.client);
  const pending = host.hostOperation({ type: "clipboard", text: "你好" });
  await tick(); const operation = app.actions.find((value) => value.command?.channel === "host")!;
  assert.deepEqual(operation.command.command.operation, { type: "clipboard", text: "你好" });
  app.event({ type: "hostResult", id: operation.command.command.id, result: null, error: null, snapshot });
  assert.equal(await pending, undefined); host.retire();
});

test("malformed host operations are rejected before native notification", async () => {
  const host = new ExtensionUI(); const app = frontend(); host.bind(app.client);
  await assert.rejects(Reflect.apply(host.hostOperation, host, [{ type: "clipboard" }]), /Invalid/);
  assert.equal(app.actions.some((value) => value.command?.channel === "host"), false);
  host.retire();
});

test("duplicate IDs and legacy render functions cannot cross the descriptor boundary", async () => {
  assert.throws(() => uiNodeSchema.parse({ id: "root", type: "column", children: [{ id: "duplicate", type: "text", text: "a" }, { id: "duplicate", type: "input" }] }), /Duplicate UI node id/);
  assert.throws(() => uiNodeSchema.parse({ render: () => ["legacy"], invalidate() {} }), /Invalid/);
  const host = new ExtensionUI();
  await assert.rejects(host.custom({ id: "invalid", type: "column", children: [{ id: "invalid", type: "input" }] }), /Duplicate/);
  host.retire();
});
