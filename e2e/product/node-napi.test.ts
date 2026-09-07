import assert from "node:assert/strict";
import { dirname, join, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { runProductScenario, type JsonObject } from "./harness.js";

const testDirectory = dirname(fileURLToPath(import.meta.url));
const workspaceDirectory = resolve(testDirectory, "../..");
const projectDirectory = join(workspaceDirectory, "e2e/projects/frontend-app");
const extension = ".pi/extensions/frontend-napi.ts";

test("runs a TypeScript extension through Node, NAPI, Rust, and a tool loop", async () => {
  const result = await runProductScenario({
    adapter: "node-napi",
    projectFixture: projectDirectory,
    extensions: [extension],
    input: "/frontend-napi-smoke",
    providerTurns: [
      {
        toolCalls: [
          {
            id: "frontend-checks-1",
            name: "frontend_project_checks",
            arguments: {},
          },
        ],
      },
      { text: "Frontend NAPI product e2e passed." },
    ],
  });

  assert.equal(result.providerRequests.length, 2);
  assert.ok(hasProductEvent(result.events, "agent_start"));
  assert.match(result.stdout, /Frontend NAPI product e2e passed/);

  const firstRequest = result.providerRequests[0];
  const firstMessages = arrayField(firstRequest, "messages");
  assert.equal(
    stringField(firstMessages.at(-1), "content"),
    "Use frontend_project_checks to inspect this project's required verification workflow, then summarize it.",
  );
  assert.ok(
    arrayField(firstRequest, "tools").some(
      (tool) =>
        stringField(recordField(record(tool), "function"), "name") ===
        "frontend_project_checks",
    ),
  );

  const secondMessages = arrayField(result.providerRequests[1], "messages");
  const toolResult = secondMessages
    .map(record)
    .find((message) => message.role === "tool");
  assert.ok(toolResult, "the extension tool result must reach the next provider request");
  assert.match(stringField(toolResult, "content") ?? "", /npm run lint/);
  assert.ok(result.sessionLog);
  assert.match(result.sessionLog, /Frontend NAPI product e2e passed/);
});

test("accepts BotMux launch arguments and persists its agent_settled marker", async () => {
  const sessionId = "018f4f7c-7f4a-7000-8000-botmux000001";
  const result = await runProductScenario({
    adapter: "node-napi",
    projectFixture: projectDirectory,
    extensions: [".pi/extensions/botmux-turn-boundary.ts"],
    input: "@botmux-prompt.md",
    providerTurns: [{ text: "BotMux-compatible response." }],
    sessionId,
    sessionName: "BotMux compatibility",
  });

  assert.ok(result.sessionPath?.endsWith(`_${sessionId}.jsonl`));
  assert.ok(result.sessionLog);
  const mutations = result.sessionLog
    .trim()
    .split("\n")
    .map((line) => JSON.parse(line) as JsonObject);
  assert.equal(mutations[0]?.id, sessionId);
  assert.ok(
    mutations.some(
      (mutation) =>
        mutation.kind === "entry" &&
        mutation.type === "thinking_level_change" &&
        mutation.thinkingLevel === "medium",
    ),
    "new product sessions must use Pi's medium thinking default",
  );
  assert.ok(
    mutations.some(
      (mutation) =>
        mutation.kind === "fact" &&
        mutation.fact === "name" &&
        mutation.name === "BotMux compatibility",
    ),
  );
  const settled = mutations.find(
    (mutation) =>
      mutation.kind === "entry" &&
      mutation.customType === "botmux-turn-settled",
  );
  assert.ok(settled, "agent_settled must append BotMux's durable turn marker");
  assert.equal(typeof settled.timestamp, "string");
  assert.match(String(settled.timestamp), /^\d{4}-\d{2}-\d{2}T/);

  const firstMessages = arrayField(result.providerRequests[0], "messages");
  assert.match(
    stringField(firstMessages.at(-1), "content") ?? "",
    /<file name=".*botmux-prompt\.md">\n# BotMux task/,
  );
});

function hasProductEvent(events: JsonObject[], type: string): boolean {
  return events.some((entry) => stringField(entry, "type") === type);
}

function arrayField(value: unknown, field: string): unknown[] {
  const candidate = record(value)[field];
  assert.ok(Array.isArray(candidate), `${field} must be an array`);
  return candidate;
}

function stringField(value: unknown, field: string): string | undefined {
  if (!isRecord(value)) return undefined;
  const candidate = value[field];
  return typeof candidate === "string" ? candidate : undefined;
}

function recordField(value: JsonObject, field: string): JsonObject | undefined {
  const candidate = value[field];
  return isRecord(candidate) ? candidate : undefined;
}

function record(value: unknown): JsonObject {
  assert.ok(isRecord(value), "value must be a JSON object");
  return value;
}

function isRecord(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
