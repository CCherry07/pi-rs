import assert from "node:assert/strict";
import test from "node:test";

import { PiEvalNodeHost, type NativeBinding } from "../src/index.js";

test("eval app forwards arguments through the JavaScript extension host binding", async () => {
  let received: string[] | undefined;
  const binding: NativeBinding = {
    async runPi() {
      throw new Error("unexpected product CLI invocation");
    },
    async runPiEval(arguments_) {
      received = arguments_;
    },
  };
  await new PiEvalNodeHost({
    arguments: ["run", "coding/js-extension"],
    nativeBinding: binding,
  }).run();
  assert.deepEqual(received, ["run", "coding/js-extension"]);
});
