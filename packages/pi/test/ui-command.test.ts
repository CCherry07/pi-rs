import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { z } from "zod";
import { uiCommandSchema } from "../src/extension-ui-frame.js";

test("commands match the shared Rust/JavaScript wire corpus", () => {
  const entry = z.object({ name: z.string(), value: z.unknown() });
  const fixtures = z.object({ valid: z.array(entry), invalid: z.array(entry) }).parse(JSON.parse(
    readFileSync(new URL("../../../crates/pi-plugin/tests/fixtures/ui-commands.json", import.meta.url), "utf8"),
  ));
  for (const { name, value } of fixtures.valid) {
    assert.deepEqual(uiCommandSchema.parse(value), value, name);
  }
  for (const { name, value } of fixtures.invalid) {
    assert.equal(uiCommandSchema.safeParse(value).success, false, name);
  }
});
