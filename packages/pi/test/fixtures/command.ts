import type { PiExtensionApi } from "../../src/extension-api.js";

export default function registerBridgeSmokeCommand(pi: PiExtensionApi) {
  pi.registerCommand("bridge-smoke", {
    description: "Verify that a Pi TypeScript command crossed the NAPI bridge",
    async handler(_arguments, context) {
      // A handled command completes without requiring a provider. If this
      // callback is not registered and invoked, the CLI tries to submit the
      // slash command to the model and the smoke test fails.
      if (context.hasUI || context.isProjectTrusted()) {
        throw new Error("native context exposed incorrect product capabilities");
      }
      if (!context.cwd || !context.sessionManager.getSessionId()) {
        throw new Error("native context is missing session identity");
      }
      if (context.sessionManager.getSessionFile() !== undefined) {
        throw new Error("an unanswered session must remain unmaterialized");
      }
      if (!context.getSystemPrompt()) {
        throw new Error("native context is missing the effective system prompt");
      }
      await context.waitForIdle();

      const previousSessionId = context.sessionManager.getSessionId();
      let replacementSessionId: string | undefined;
      const replacement = await context.newSession({
        setup: (manager) => {
          manager.appendCustomEntry("bridge-setup", { ready: true });
        },
        withSession: async (next) => {
          replacementSessionId = next.sessionManager.getSessionId();
          await next.sendMessage(
            { customType: "bridge-context", content: "ready", display: false },
            { triggerTurn: false },
          );
          const entries = next.sessionManager.getEntries() as Array<Record<string, unknown>>;
          if (!entries.some((entry) => entry.customType === "bridge-setup")) {
            throw new Error("native setup did not mutate the replacement session");
          }
          if (!entries.some((entry) => entry.customType === "bridge-context")) {
            throw new Error("native replacement sendMessage did not persist context");
          }
        },
      });
      if (replacement.cancelled || !replacementSessionId || replacementSessionId === previousSessionId) {
        throw new Error("native newSession did not bind the replacement session");
      }

      await context.reload();
      if (context.sessionManager.getSessionId() !== replacementSessionId) {
        throw new Error("native reload changed the logical session identity");
      }
    },
  });
}
