import assert from "node:assert/strict";
import test from "node:test";
import { Box, Input, SettingsList, type TuiMouseEvent, type OverlayHandle } from "@earendil-works/pi-tui";
import { PiComponentAdapter, getSettingsListTheme } from "../src/pi-component-adapter.js";
import { ExtensionUI } from "../src/extension-ui.js";
import { uiCommandSchema, uiSnapshotSchema, type UiRegion, type UiMouseResult } from "../src/extension-ui-frame.js";
import type { NativeExtensionContext } from "../src/native-binding.js";

const mouse = (overrides: Partial<TuiMouseEvent> = {}): TuiMouseEvent => ({
  type: "press", button: "left", x: 3, y: 1, screenX: 13, screenY: 6,
  width: 20, height: 5, shift: false, alt: false, ctrl: false, ...overrides,
});

test("mouse focus selects the real nested input and subsequent keys edit it", () => {
  const adapter = new PiComponentAdapter("nested");
  const box = new Box(2, 1);
  const input = new Input();
  input.setValue("abcd");
  box.addChild(input);
  adapter.setComponent(box);
  const node = adapter.render(20, true);
  assert.equal(node.type, "custom");
  if (node.type !== "custom") throw new Error("expected a custom component");
  assert.equal(node.mouse, true);
  const result = adapter.handleMouse(mouse({ x: 5, screenX: 15 }));
  assert.equal(result.handled, true);
  assert.equal(result.focus, true);
  adapter.handleInput("X");
  assert.equal(input.getValue(), "aXbcd");
  adapter.dispose();
});

test("capture keeps a nested target outside its box and replacement invalidates it", () => {
  const adapter = new PiComponentAdapter("drag");
  const seen: TuiMouseEvent[] = [];
  const box = new Box(2, 1);
  box.addChild({ render: () => ["drag here"], invalidate() {}, handleMouse(event) {
    seen.push(event);
    return { capture: event.type === "press", handled: true };
  } });
  adapter.setComponent(box);
  adapter.render(20, true);
  const press = adapter.handleMouse(mouse());
  assert.equal(press.capture, true);
  assert.equal(typeof press.target, "number");
  adapter.handleMouse(mouse({ type: "drag", x: -3, y: -2, screenX: 7, screenY: 3 }), press.target);
  assert.deepEqual(seen.map(({ x, y }) => [x, y]), [[1, 0], [-5, -3]]);
  adapter.setComponent(new Box());
  assert.equal(adapter.handleMouse(mouse({ type: "release" }), press.target).cancelled, true);
  adapter.dispose();
});

test("real SettingsList mouse press and click activate one value change", () => {
  const adapter = new PiComponentAdapter("settings");
  const changed: string[] = [];
  const list = new SettingsList([{ id: "mode", label: "Mode", currentValue: "one", values: ["one", "two"] }], 5,
    getSettingsListTheme(), (_id, value) => changed.push(value), () => {});
  adapter.setComponent(list);
  adapter.render(30, true);
  const press = adapter.handleMouse(mouse({ x: 0, y: 0, width: 30 }));
  adapter.handleMouse(mouse({ type: "release", x: 0, y: 0, width: 30 }), press.target);
  assert.deepEqual(changed, []);
  adapter.handleMouse(mouse({ type: "click", x: 0, y: 0, width: 30, clickCount: 1 }), press.target);
  assert.deepEqual(changed, ["two"]);
  adapter.dispose();
});

function frontend() {
  const snapshot = uiSnapshotSchema.parse({ columns: 80, rows: 24, editorText: "", toolsExpanded: false });
  type Frame = { owner: string; revision: number; regions: UiRegion[] };
  let frame!: Frame;
  let pending: ((value: string) => void) | undefined;
  const replies = new Map<number, UiMouseResult>();
  const event = (value: unknown) => { const resolve = pending; pending = undefined; resolve?.(JSON.stringify(value)); };
  const client: NativeExtensionContext = {
    query: () => JSON.stringify(snapshot),
    request(raw) { frame = JSON.parse(raw).frame; return new Promise(resolve => { pending = resolve; }); },
    notify(raw) {
      const value = JSON.parse(raw);
      if (!value.command) return;
      const command = uiCommandSchema.parse(value.command);
      if (command.channel !== "surface") return;
      if (command.command.type === "mouseResult") replies.set(command.command.id, command.command.result);
      if (command.command.type === "wake") event({ type: "refresh", snapshot });
      if (command.command.type === "close") event({ type: "closed" });
    },
  };
  return {
    client, replies, get frame() { return frame; },
    mouse(id: number, region = frame.regions[0]!, eventFrame = frame) {
      if (region.root.type !== "custom") throw new Error("expected custom region");
      event({ type: "interaction", revision: eventFrame.revision, regionId: region.id, nodeId: region.root.id,
        event: { type: "customMouse", id, instanceId: region.root.instanceId, target: null, event: mouse() }, snapshot });
    },
  };
}
const tick = () => new Promise<void>(resolve => setImmediate(resolve));

test("widget/header/footer share mouse dispatch, honor render false and reject stale mounts", async () => {
  for (const placement of ["widget", "header", "footer"] as const) {
    const ui = new ExtensionUI();
    const app = frontend();
    ui.bind(app.client);
    let clicks = 0;
    let renders = 0;
    const factory = () => ({ render: () => { renders++; return ["click"]; }, invalidate() {}, handleMouse() { clicks++; return { handled: true, render: false }; } });
    const mount = () => {
      if (placement === "widget") ui.setWidget("mouse", factory);
      else if (placement === "header") ui.setHeader(factory);
      else ui.setFooter(factory);
    };
    try {
      mount();
      await tick();
      const oldFrame = app.frame;
      const region = oldFrame.regions[0]!;
      if (region.root.type !== "custom") throw new Error("expected custom region");
      assert.equal(region.root.mouse, true);
      assert.equal(region.root.interactive, false);
      const before = renders;
      app.mouse(1);
      await tick();
      assert.equal(clicks, 1);
      assert.equal(app.replies.get(1)?.handled, true);
      assert.equal(renders, before);
      mount();
      await tick();
      app.mouse(2, region, oldFrame);
      await tick();
      assert.equal(app.replies.get(2)?.cancelled, true);
      assert.equal(clicks, 1);
    } finally { ui.retire(); }
  }
});

test("a throwing mouse callback cancels its dialog and replies without retiring healthy mounts", async () => {
  const ui = new ExtensionUI();
  const app = frontend();
  ui.bind(app.client);
  try {
    ui.setWidget("healthy", ["still here"]);
    const result = ui.custom(() => ({ render: () => ["broken"], invalidate() {}, handleMouse() { throw new Error("mouse failed"); } }))
      .then(() => undefined, error => error);
    await tick();
    app.mouse(3, app.frame.regions.at(-1)!);
    assert.match(String(await result), /mouse failed/);
    await tick();
    assert.equal(app.replies.get(3)?.cancelled, true);
    assert.deepEqual(app.frame.regions.map(region => region.id), ["widget:healthy"]);
    ui.setStatus("live", "still active");
  } finally { ui.retire(); }
});

test("hiding an overlay before a queued mouse callback runs cancels the callback immediately", async () => {
  const ui = new ExtensionUI();
  const app = frontend();
  ui.bind(app.client);
  let handle!: OverlayHandle;
  let calls = 0;
  const result = ui.custom(() => ({ render: () => ["hide"], invalidate() {}, handleMouse() { calls++; return { focus: true }; } }),
    { overlay: true, onHandle: value => { handle = value; } });
  try {
    await tick();
    app.mouse(4);
    handle.setHidden(true);
    await tick();
    assert.equal(calls, 0);
    assert.equal(app.replies.get(4)?.cancelled, true);
  } finally { ui.retire(); await result; }
});

test("a failing mouse-only widget is removed from the next published frame", async () => {
  const ui = new ExtensionUI();
  const app = frontend();
  ui.bind(app.client);
  try {
    ui.setWidget("healthy", ["healthy"]);
    ui.setWidget("broken", () => ({ render: () => ["broken"], invalidate() {}, handleMouse() { throw new Error("broken mouse"); } }));
    await tick();
    app.mouse(5, app.frame.regions.at(-1)!);
    await tick();
    assert.equal(app.replies.get(5)?.cancelled, true);
    assert.deepEqual(app.frame.regions.map(region => region.id), ["widget:healthy"]);
  } finally { ui.retire(); }
});
