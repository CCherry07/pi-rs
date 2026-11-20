import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { ExtensionUI } from "../src/extension-ui.js";

const oracle: { commit: string; cases: { method: string; parameters: unknown[]; resultType: string; result?: unknown }[] } = JSON.parse(
  readFileSync(new URL("./fixtures/rpc-ui-oracle.json", import.meta.url), "utf8"),
);

test("RPC context exposes legacy terminal-inactive methods without frontend effects", async () => {
  const host = new ExtensionUI();
  const effects: string[] = [];
  host.bind({
    query: (raw) => { assert.equal(JSON.parse(raw).type, "mode"); return JSON.stringify("rpc"); },
    notify: (raw) => { effects.push(raw); },
    request: async (raw) => { effects.push(raw); throw new Error("Inactive RPC method requested a terminal"); },
  });
  const ui = host.forContext(new AbortController().signal);
  try {
    const actual = [];
    for (const item of oracle.cases) {
      const method = Reflect.get(ui, item.method);
      assert.equal(typeof method, "function", `Missing RPC UI method ${item.method}`);
      const result = await Reflect.apply(method, ui, item.parameters);
      actual.push(typeof result === "function"
        ? { method: item.method, parameters: item.parameters, resultType: "function", unsubscribeResult: result(), repeatedResult: result() }
        : { method: item.method, parameters: item.parameters, resultType: typeof result, result });
    }
    assert.deepEqual(JSON.parse(JSON.stringify(actual)), oracle.cases);
    assert.deepEqual(effects, []);
  } finally {
    host.retire();
  }
});
