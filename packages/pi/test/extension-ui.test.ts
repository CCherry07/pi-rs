import test from "node:test";
import assert from "node:assert/strict";
import { ExtensionUI } from "../src/extension-ui.js";
import { componentRegion } from "../src/extension-ui-frame.js";
import { CURSOR_MARKER } from "@pi-rs/pi-tui-runtime";
import type { NativeExtensionContext } from "../src/native-binding.js";

const snapshot = {
  columns: 80,
  rows: 24,
  editorText: "draft",
  toolsExpanded: false,
};
function frontend() {
  const frames: {
    owner: string;
    regions: ReturnType<typeof componentRegion>[];
    captureInput: boolean;
  }[] = [];
  const actions: Record<string, unknown>[] = [];
  let pending: ((value: string) => void) | undefined;
  const client: NativeExtensionContext = {
    query: () => JSON.stringify(snapshot),
    notify: (raw) => {
      const operation = JSON.parse(raw);
      actions.push(operation);
      if (operation.action?.type === "wake")
        event({ type: "refresh", snapshot });
      if (operation.action?.type === "close") event({ type: "closed" });
    },
    request: (raw) => {
      const operation = JSON.parse(raw);
      frames.push(operation.frame);
      return new Promise((resolve) => {
        pending = resolve;
      });
    },
  };
  function event(value: unknown) {
    const resolve = pending;
    pending = undefined;
    resolve?.(JSON.stringify(value));
  }
  return { client, frames, actions, event };
}
const tick = () => new Promise((resolve) => setImmediate(resolve));

test("custom component receives input, resolves result, restores editor and disposes exactly once", async () => {
  const host = new ExtensionUI();
  const app = frontend();
  host.bind(app.client);
  let disposed = 0;
  const result = host.custom(
    (_tui, theme, _keys, done) => ({
      render: () => [theme.fg("accent", "中文") + CURSOR_MARKER + " value"],
      invalidate() {},
      handleInput: (data) => done(data),
      dispose: () => {
        disposed++;
      },
    }),
    { overlay: true },
  );
  await tick();
  const frame = app.frames.at(-1)!;
  assert.equal(frame.captureInput, true);
  assert.deepEqual(frame.regions[0]?.cursor, [0, 4]);
  app.event({ type: "input", data: "界", sequence: 1, snapshot });
  assert.equal(await result, "界");
  await tick();
  assert.equal(disposed, 1);
  assert.equal(app.frames.at(-1)?.captureInput, false);
  host.retire();
  assert.equal(disposed, 1);
});

test("timed and aborted dialogs settle and remove their component", async () => {
  const host = new ExtensionUI();
  const app = frontend();
  host.bind(app.client);
  const result = host.confirm("Continue?", "Message", { timeout: 10 });
  assert.equal(await result, false);
  await tick();
  assert.equal(app.frames.at(-1)?.regions.length, 0);
  const controller = new AbortController();
  const selection = host.select("Pick", ["a", "b"], {
    signal: controller.signal,
  });
  await tick();
  controller.abort();
  assert.equal(await selection, undefined);
  host.retire();
});

test("generation retirement settles nested components and disposes widgets", async () => {
  const host = new ExtensionUI();
  const app = frontend();
  host.bind(app.client);
  let disposed = 0;
  host.setWidget("test", () => ({
    render: () => ["widget"],
    invalidate() {},
    dispose() {
      disposed++;
    },
  }));
  const first = host.custom(() => ({
    render: () => ["first"],
    invalidate() {},
    dispose() {
      disposed++;
    },
  }));
  const second = host.custom(() => ({
    render: () => ["second"],
    invalidate() {},
    dispose() {
      disposed++;
    },
  }));
  await tick();
  host.retire();
  assert.equal(await first, undefined);
  assert.equal(await second, undefined);
  assert.equal(disposed, 3);
});

test("frame decoder preserves RGB, Unicode and cursor but drops terminal controls", () => {
  const region = componentRegion(
    ["\x1b[1;38;2;10;20;30m字" + CURSOR_MARKER + "\x1b[0mX\x1b]0;bad\x07"],
    "editor",
    10,
  );
  assert.deepEqual(region.cursor, [0, 2]);
  assert.equal(region.lines[0]?.map((s) => s.text).join(""), "字X");
  assert.deepEqual(region.lines[0]?.[0]?.foreground, [10, 20, 30]);
  assert.equal(region.lines[0]?.[0]?.bold, true);
  assert.equal(region.lines[0]?.[1]?.bold, false);
});

test('retirement settles a custom factory that is still awaiting', async () => {
  const host = new ExtensionUI(); const app = frontend(); host.bind(app.client)
  let finish: ((component: { render(): string[]; invalidate(): void; dispose(): void }) => void) | undefined
  let disposed = 0
  const dialog = host.custom(() => new Promise(resolve => { finish = resolve }))
  await tick(); host.retire()
  assert.equal(await dialog, undefined)
  finish?.({ render: () => [], invalidate() {}, dispose() { disposed++ } })
  await tick(); assert.equal(disposed, 1)
})

test('invocation cancellation closes a dialog without retiring persistent widgets', async () => {
  const host = new ExtensionUI(); const app = frontend(); host.bind(app.client)
  const controller = new AbortController()
  const ui = host.forContext(controller.signal) as Pick<ExtensionUI, 'input'>
  host.setWidget('keep', ['persistent'])
  const result = ui.input('Cancel me'); await tick(); controller.abort()
  assert.equal(await result, undefined); await tick()
  assert.equal(app.frames.at(-1)?.regions[0]?.lines[0]?.[0]?.text, 'persistent')
  host.retire()
})

test('renderer failure closes surfaces and resolves pending custom result', async () => {
  const host = new ExtensionUI(); const app = frontend(); host.bind(app.client)
  const result = host.custom(() => ({ render() { throw new Error('broken renderer') }, invalidate() {} }))
  assert.equal(await result, undefined)
  assert.ok(app.actions.some(action => action.type === 'uiNotify' && String(action.message).includes('broken renderer')))
})

test('editor text replacement is visible synchronously before the frontend handles the action', () => {
  const host = new ExtensionUI(); const app = frontend(); host.bind(app.client)
  host.setEditorText('new draft')
  assert.equal(host.getEditorText(), 'new draft')
  host.retire()
})
