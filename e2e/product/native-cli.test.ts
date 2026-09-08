import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
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

test("sends an image-only @file through the real CLI and persists it", async () => {
  const fixture = await mkdtemp(join(tmpdir(), "pi-media-e2e-"));
  const data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAX+XDSwAAAABJRU5ErkJggg==";
  try {
    // Deliberately use a non-image suffix and spaces: content determines the MIME.
    await writeFile(join(fixture, "screen shot.bin"), Buffer.from(data, "base64"));
    const result = await runProductScenario({
      adapter: "native-cli",
      input: "@screen shot.bin",
      projectFixture: fixture,
      providerTurns: [{ text: "image received" }],
    });
    assert.equal(result.providerRequests.length, 1);
    const messages = arrayField(result.providerRequests[0], "messages");
    const content = arrayField(messages.at(-1), "content");
    const image = content.find((block) => stringField(block, "type") === "image_url");
    assert.equal(stringField(record(image).image_url, "url"), `data:image/png;base64,${data}`);
    assert.ok(result.sessionLog?.includes(data));
    assert.match(result.stdout, /image received/);
  } finally {
    await rm(fixture, { recursive: true, force: true });
  }
});

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
