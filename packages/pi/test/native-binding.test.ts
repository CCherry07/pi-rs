import assert from "node:assert/strict";
import test from "node:test";

import { missingNativeBindingMessage } from "../src/native-binding.js";
import { resolveNativeTarget } from "../src/native-target.js";

test("missing native bindings provide an exact npm repair command", () => {
  const message = missingNativeBindingMessage(
    resolveNativeTarget("darwin", "arm64"),
    "@pi-rs/cli-darwin-arm64",
    "0.3.0",
    ["/install/pi-napi.darwin-arm64.node"],
    "Cannot find module '@pi-rs/cli-darwin-arm64'",
  );

  assert.match(
    message,
    /npm install --global @pi-rs\/cli@0\.3\.0 @pi-rs\/cli-darwin-arm64@0\.3\.0 --registry=https:\/\/registry\.npmjs\.org --@pi-rs:registry=https:\/\/registry\.npmjs\.org/,
  );
  assert.match(
    message,
    /npx --yes --package=@pi-rs\/cli@0\.3\.0 --package=@pi-rs\/cli-darwin-arm64@0\.3\.0 --registry=https:\/\/registry\.npmjs\.org --@pi-rs:registry=https:\/\/registry\.npmjs\.org -- pi/,
  );
  assert.match(message, /npm may have skipped the optional component/);
  assert.match(message, /private registry/);
});
