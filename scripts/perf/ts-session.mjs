// TS oracle worker. Imports are resolved from --ts-root, never from a user's global Pi.
import assert from "node:assert/strict";
import { performance } from "node:perf_hooks";
import { createInterface } from "node:readline";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const [tsRoot, mode, sizeArg, samplesArg, warmupArg, batchArg, payloadArg] = process.argv.slice(2);
const [size, sampleCount, warmup, batch, payload] = [sizeArg, samplesArg, warmupArg, batchArg, payloadArg].map(Number);
assert([size, sampleCount, batch].every(n => Number.isSafeInteger(n) && n > 0));
assert(Number.isSafeInteger(warmup) && warmup >= 0 && Number.isSafeInteger(payload) && payload >= 32);
const load = path => import(pathToFileURL(resolve(tsRoot, path)).href);
const { MemoryStorage, MemorySessionRepo } = await load("packages/agent/src/harness/session/memory.ts");
const { BACKGROUND_CONTEXT: context } = await load("packages/agent/src/harness/context.ts");
const { seedStorageBenchmark, seedSessionRepoForkBenchmark, seedSessionRepoCatalogBenchmark } =
  await load("packages/agent/src/harness/session/testing/index.ts");
const id = index => `benchmark-entry-${String(index).padStart(8, "0")}`;
const lookups = Math.min(100, size);
const dataset = {
  name: `synthetic linear branch: ${size}, ${payload}-byte payloads`,
  entryCount: size, payloadBytes: payload, tipId: id(size - 1),
  lookupIds: Array.from({ length: lookups }, (_, i) => id(Math.floor(i * (size - 1) / Math.max(1, lookups - 1)))),
};
const now = () => 1_700_000_000_000;
function emit(name, batch, samples) {
  console.log(JSON.stringify({ name, batch, samples, unit: "ms" }));
}
async function measure(name, batch, expected, run) {
  assert.equal(await run(), expected, name + ": incorrect fixture/result");
  let consumed = 0;
  for (let warm = 0; warm < warmup; warm++) for (let i = 0; i < batch; i++) consumed += await run();
  const samples = [];
  for (let sample = 0; sample < sampleCount; sample++) {
    const start = performance.now();
    for (let i = 0; i < batch; i++) consumed += await run();
    samples.push((performance.now() - start) / batch);
  }
  assert(Number.isFinite(consumed));
  emit(name, batch, samples);
}

if (mode === "memory") {
  assert.equal(typeof globalThis.gc, "function", "memory worker requires --expose-gc");
  const reader = createInterface({ input: process.stdin });
  const lines = reader[Symbol.asyncIterator]();
  const storage = new MemoryStorage({ now });
  const checkpoint = async phase => {
    for (let i = 0; i < 3; i++) globalThis.gc();
    console.log(JSON.stringify({ phase, memory: process.memoryUsage() }));
    assert.equal((await lines.next()).done, false, "checkpoint acknowledgement missing");
  };
  try {
    await checkpoint("baseline");
    await seedStorageBenchmark(storage, dataset);
    assert.equal((await storage.getStats(context)).messageCount, size);
    await checkpoint("loaded");
    await storage.close(context);
  } finally { reader.close(); }
} else if (mode === "read") {
  const storage = new MemoryStorage({ now });
  await seedStorageBenchmark(storage, dataset);
  await measure("get100", batch, lookups, async () =>
    (await storage.getEntries([...dataset.lookupIds], context)).size);
  await measure("latest50", batch, Math.min(50, size), async () =>
    (await storage.scanEntries({ order: "desc", limit: 50 }, context)).length);
  await measure("full_branch", 1, size, async () =>
    (await storage.scanBranchStructure({ start: dataset.tipId, order: "newestFirst" }, context)).length);
  await storage.close(context);
} else if (mode === "fork") {
  const samples = [];
  for (let iteration = 0; iteration < warmup + sampleCount; iteration++) {
    const repo = new MemorySessionRepo({ now });
    try {
      const source = await seedSessionRepoForkBenchmark(repo, dataset);
      const start = performance.now();
      const fork = await repo.fork(source, {
        id: "benchmark-session-00000001", scope: "branch", branch: "main",
      }, context);
      const elapsed = performance.now() - start;
      assert.equal(fork.metadata.id, "benchmark-session-00000001");
      assert.equal(fork.metadata.parentSessionId, source.id);
      assert.equal((await fork.getStats(context)).messageCount, size);
      if (iteration >= warmup) samples.push(elapsed);
    } finally { await repo.close(); }
  }
  emit("fork", 1, samples);
} else if (mode === "catalog") {
  {
    const repo = new MemorySessionRepo({ now });
    await seedSessionRepoCatalogBenchmark(repo, { name: "synthetic catalog", sessionCount: size });
    await measure("list_sessions", 1, size, async () => (await repo.list(undefined, context)).length);
    await repo.close();
  }
  const samples = [];
  for (let iteration = 0; iteration < warmup + sampleCount; iteration++) {
    const repo = new MemorySessionRepo({ now });
    try {
      const options = { id: "benchmark-session-00000000" };
      const start = performance.now();
      const session = await repo.create(options, context);
      const elapsed = performance.now() - start;
      assert.equal(session.metadata.id, options.id);
      if (iteration >= warmup) samples.push(elapsed);
    } finally { await repo.close(); }
  }
  emit("create_session", 1, samples);
} else {
  throw new Error("mode must be read, fork, catalog or memory");
}
