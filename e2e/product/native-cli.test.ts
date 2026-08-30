import assert from "node:assert/strict";
import test from "node:test";

import { runProductScenario, type JsonObject } from "./harness.js";

test("runs the standalone CLI through the product NDJSON seam", async () => {
  const result = await runProductScenario({
    adapter: "native-cli",
    input: "Reply with exactly: native product e2e passed",
    providerTurns: [{ text: "native product e2e passed" }],
  });

  assert.equal(result.providerRequests.length, 1);
  assert.ok(hasProductEvent(result.events, "agent_start"));
  assert.match(result.stdout, /native product e2e passed/);
  assert.ok(result.sessionLog, "the first completed assistant message must persist the session");
  assert.match(result.sessionLog, /native product e2e passed/);

  const messages = arrayField(result.providerRequests[0], "messages");
  assert.equal(
    stringField(messages.at(-1), "content"),
    "Reply with exactly: native product e2e passed",
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

function record(value: unknown): JsonObject {
  assert.ok(isRecord(value), "value must be a JSON object");
  return value;
}

function isRecord(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
