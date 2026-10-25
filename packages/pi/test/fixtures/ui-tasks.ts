import { writeFileSync } from "node:fs";
import type { PiExtensionApi } from "../../src/extension-api.js";

/** Runs beside the original installed pi-tasks package; all state stays in the PTY temp directory. */
export default function (pi: PiExtensionApi) {
  pi.on("session_start", (_event, ctx) => {
    ctx.ui.setWidget("acceptance", ["UI_PTY_TASKS_READY"]);
  });
  pi.on("tool_result", event => {
    writeFileSync(process.env.UI_ACCEPTANCE_OUTPUT!, JSON.stringify(event));
  });
  pi.registerCommand("ui-create-task", { description: "Create a fixture task", handler: () => {
    pi.sendUserMessage("Create a task named UI_PTY_ORIGINAL_TASK with the TaskCreate tool.");
  } });
  pi.registerCommand("ui-finish", { description: "Finish task widget acceptance", handler: (_args, ctx) => { ctx.shutdown(); } });
}
