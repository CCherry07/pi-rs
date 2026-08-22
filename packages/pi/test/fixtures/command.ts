import type { PiExtensionApi } from "../../src/extension-api.js";

export default function registerBridgeSmokeCommand(pi: PiExtensionApi) {
  pi.registerCommand("bridge-smoke", {
    description: "Verify that a Pi TypeScript command crossed the NAPI bridge",
    async handler() {
      // A handled command completes without requiring a provider. If this
      // callback is not registered and invoked, the CLI tries to submit the
      // slash command to the model and the smoke test fails.
    },
  });
}
