import type { PiExtensionApi } from "../../../../../packages/pi/src/extension-api.js";
import { defineTool } from "@earendil-works/pi-coding-agent";

interface BeforeAgentStartEvent extends Record<string, unknown> {
  systemPrompt: string;
}

const checks = ["npm run lint", "npm run build"];
const reviewPaths = ["src/", "public/", "index.html", "package.json"];

export default function frontendNapiExtension(pi: PiExtensionApi) {
  pi.registerTool(
    defineTool({
      name: "frontend_project_checks",
      label: "Frontend project checks",
      description: "Return the required verification commands and review scope for this fixture",
      parameters: {
        type: "object",
        properties: {},
        additionalProperties: false,
      },
      promptSnippet: "Inspect the frontend fixture's required checks and review scope",
      promptGuidelines: [
        "Use frontend_project_checks before reporting frontend work complete.",
      ],
      executionMode: "parallel",
      async execute() {
        return {
          content: [
            {
              type: "text",
              text: [
                "Required checks:",
                ...checks.map((check) => `- ${check}`),
                "Review paths:",
                ...reviewPaths.map((path) => `- ${path}`),
              ].join("\n"),
            },
          ],
          details: { checks, reviewPaths },
        };
      },
    }),
  );

  pi.registerCommand("frontend-napi-smoke", {
    description: "Ask the agent to inspect the frontend fixture's verification workflow",
    async handler() {
      return {
        action: "transform",
        text: "Use frontend_project_checks to inspect this project's required verification workflow, then summarize it.",
      };
    },
  });

  pi.on<BeforeAgentStartEvent>("before_agent_start", (event) => ({
    systemPrompt: `${event.systemPrompt}\n\nFrontend TypeScript extension:\nRun ${checks.join(
      " and ",
    )} before reporting frontend work complete.`,
  }));

  // Register no-op hooks in the other two narrow lifecycles so this fixture
  // verifies that one Pi source is partitioned without a PluginBundle.
  pi.on("before_provider_request", () => undefined);
  pi.on("session_start", () => undefined);
}
