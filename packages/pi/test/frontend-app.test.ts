import assert from "node:assert/strict";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

import { ExtensionHost } from "../src/extension-host.js";
import {
  isRecord,
  parseGenerationManifest,
  parseJson,
} from "../src/extension-protocol.js";

const testDirectory = dirname(fileURLToPath(import.meta.url));
const workspaceDirectory = resolve(testDirectory, "../../..");
const projectDirectory = join(workspaceDirectory, "e2e/projects/frontend-app");

test("discovers the frontend-app TypeScript extension and invokes its callbacks", async () => {
  const host = new ExtensionHost();
  const manifest = parseGenerationManifest(
    await host.dispatch(
      JSON.stringify({
        type: "prepareGeneration",
        request: {
          cwd: projectDirectory,
          agentDir: join(projectDirectory, ".test-agent"),
          projectTrusted: true,
          explicitPaths: [],
          discoverExtensions: true,
          mode: "print",
        },
      }),
    ),
  );

  const plugin = manifest.agentPlugins.find((candidate) =>
    candidate.commands.some((command) => command.name === "frontend-napi-smoke"),
  );
  assert.ok(plugin, "frontend-app extension was not discovered");
  const tool = plugin.tools[0];
  assert.ok(tool);
  assert.equal(tool.name, "frontend_project_checks");
  assert.equal(plugin.hooks[0]?.name, "before_agent_start");
  assert.equal(manifest.providerPlugins[0]?.hooks[0]?.name, "before_provider_request");
  assert.equal(manifest.sessionPlugins[0]?.hooks[0]?.name, "session_start");

  const command = plugin.commands.find(
    (candidate) => candidate.name === "frontend-napi-smoke",
  );
  assert.ok(command);
  const commandResult = parseJson(
    await host.dispatch(
      JSON.stringify({
        type: "invoke",
        invocation: {
          invocationId: "frontend-command-1",
          generationId: manifest.generationId,
          callbackId: command.callbackId,
          kind: "command",
          payload: {
            context: { cwd: projectDirectory },
            arguments: "",
          },
        },
      }),
    ),
  );
  assert.deepEqual(commandResult, {
    action: "transform",
    text: "Use frontend_project_checks to inspect this project's required verification workflow, then summarize it.",
  });

  const result = parseJson(
    await host.dispatch(
      JSON.stringify({
        type: "invoke",
        invocation: {
          invocationId: "frontend-tool-1",
          generationId: manifest.generationId,
          callbackId: tool.callbackId,
          kind: "tool",
          payload: {
            context: { cwd: projectDirectory, toolCallId: "frontend-tool-call-1" },
            input: {},
          },
        },
      }),
    ),
  );
  assert.ok(isRecord(result));
  const content = result.content;
  assert.ok(Array.isArray(content));
  const firstBlock = content[0];
  assert.ok(isRecord(firstBlock));
  const text = firstBlock.text;
  if (typeof text !== "string") throw new Error("tool result text must be a string");
  assert.match(text, /npm run lint/);
  assert.match(text, /npm run build/);

  await host.dispatch(
    JSON.stringify({ type: "retireGeneration", generationId: manifest.generationId }),
  );
});
