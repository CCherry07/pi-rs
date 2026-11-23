import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ExtensionHost } from "../src/extension-host.js";
import { parseGenerationManifest } from "../src/extension-protocol.js";

test("hook snapshots carry the real extension path and thrown errors retain their raw message", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "pi-rpc-error-source-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const path = join(root, "fixture.ts");
  await writeFile(path, `export default function (pi: any) { pi.on("agent_start", () => { throw new Error("fixture boom"); }); }`);
  const host = new ExtensionHost();
  const manifest = parseGenerationManifest(await host.dispatch(JSON.stringify({
    type: "prepareGeneration",
    request: { projectTrusted: false, extensionPaths: [path], mode: "rpc", cwd: root, agentDir: join(root, "agent") },
  })));
  t.after(() => host.dispatch(JSON.stringify({ type: "retireGeneration", generationId: manifest.generationId })));
  const snapshot = JSON.parse(await host.dispatch(JSON.stringify({ type: "snapshotHooks", generationId: manifest.generationId, hook: "agent_start" })));
  assert.equal(snapshot.callbacks.length, 1);
  assert.equal(snapshot.callbacks[0].sourcePath, path);
  const result = JSON.parse(await host.dispatch(JSON.stringify({
    type: "invokeHookBatch",
    invocation: { invocationId: "failure", generationId: manifest.generationId, hook: "agent_start",
      callbacks: snapshot.callbacks.map((callback: { callbackId: string; pluginId: string }) => ({ callbackId: callback.callbackId, context: { pluginId: callback.pluginId } })),
      event: { type: "agent_start" },
    },
  })));
  assert.deepEqual(result.errors, [{ callbackId: snapshot.callbacks[0].callbackId, message: "fixture boom" }]);
});
