import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { test } from "node:test";
import { collectDefinitions, refreshInventory, validateMappings } from "./check-tool-test-parity.mjs";

const entry = {
  id: "tools.test.ts::read > reads text",
  disposition: "covered",
  rustTests: [{ path: "plugins/tools/pi-plugin-read/tests/parity.rs", name: "reads_text" }],
};
const inventory = (cases = [entry]) => ({ schemaVersion: 1, cases });

test("requires a mapping or a documented boundary for every definition", () => {
  assert.deepEqual(validateMappings(inventory(), () => "#[tokio::test]\nasync fn reads_text() {}"), []);
  assert.match(validateMappings(inventory([{ ...entry, disposition: "pending" }]), () => "").join(), /Unmapped/);
  assert.match(validateMappings(inventory([{ ...entry, rustTests: [] }]), () => "").join(), /Unmapped/);
  assert.match(validateMappings(inventory([{ ...entry, disposition: "not-applicable", rustTests: [] }]), () => "").join(), /rationale/);
  assert.deepEqual(validateMappings(inventory([{
    ...entry, disposition: "not-applicable", rustTests: [], reason: "A JS-only adapter interface, not a Rust API.",
  }]), () => ""), []);
});

test("a helper, missing test or duplicate mapping cannot pass as executable coverage", () => {
  assert.match(validateMappings(inventory(), () => "fn reads_text() {}").join(), /Missing executable/);
  assert.match(validateMappings(inventory(), () => "#[test]\nfn reads_text_different() {}").join(), /Missing executable/);
  assert.match(validateMappings(inventory(), () => { throw new Error("missing file"); }).join(), /Cannot read/);
  assert.match(validateMappings(inventory([entry, entry]), () => "#[test]\nfn reads_text() {}").join(), /Duplicate/);
  assert.match(validateMappings(inventory([{
    ...entry, rustTests: [{ path: "../elsewhere.rs", name: "reads_text" }],
  }]), () => "").join(), /Invalid/);
});

test("accepts platform-gated and asynchronous tests without counting helper names", () => {
  for (const attributes of ['#[test]\n#[ignore]', '#[ignore]\n#[test]']) {
    assert.match(validateMappings(inventory(), () => `${attributes}\nfn reads_text() {}`).join(), /Missing executable/);
  }
  assert.deepEqual(validateMappings(inventory(), () =>
    '#[test]\n#[cfg(unix)]\nfn reads_text() {}'), []);
  assert.deepEqual(validateMappings(inventory(), () =>
    '#[tokio::test(flavor = "multi_thread")]\nasync fn reads_text() {}'), []);
});

test("a changed shared fixture invalidates mappings even if the test body is unchanged", () => {
  const definition = { id: entry.id, file: "tools.test.ts", definitionHash: "body" };
  const previous = {
    ...inventory([{ ...entry, ...definition }]),
    files: [{ file: "tools.test.ts", sha256: "old-fixture" }],
  };
  const retained = refreshInventory(previous, [definition], previous.files, "revision");
  assert.equal(retained.cases[0].disposition, "covered");
  const changed = refreshInventory(previous, [definition], [{ file: "tools.test.ts", sha256: "new-fixture" }], "revision");
  assert.equal(changed.cases[0].disposition, "pending");
  assert.deepEqual(changed.cases[0].rustTests, []);
  const added = refreshInventory(previous, [{ ...definition, id: "new case" }], previous.files, "revision");
  assert.equal(added.cases[0].disposition, "pending");
});

test("oracle parser records definitions, nested suites, each and conditional calls without executing them", () => {
  // Use the Node host's normal test dependency: this test needs no legacy
  // checkout, Vitest, provider credentials or execution of the source fixture.
  const require = createRequire(new URL("../packages/pi/package.json", import.meta.url));
  const ts = require("typescript");
  const definitions = collectDefinitions(ts, `
    describe("outer", () => {
      it("simple", () => { throw new Error("must not run"); });
      describe.skipIf(true)("platform", () => {
        it.each([1, 2])("row %s", async () => {});
        it.skipIf(false)(\`shell \${name}\`, () => {});
      });
      // it("not a test", () => {});
      const example = 'it("not registered", () => {})';
    });
  `, "fixture.test.ts");
  assert.deepEqual(definitions.map(({ title, suites }) => ({ title, suites })), [
    { title: "simple", suites: ["outer"] },
    { title: "row %s", suites: ["outer", "platform"] },
    { title: "shell ${name}", suites: ["outer", "platform"] },
  ]);
  assert.equal(new Set(definitions.map(({ id }) => id)).size, 3);
  assert(definitions.every(({ definitionHash, line }) => definitionHash.length === 64 && line > 0));
  assert.throws(() => collectDefinitions(ts, 'it("new case", sharedTestBody);', "fixture.test.ts"), /Unsupported/);
});
