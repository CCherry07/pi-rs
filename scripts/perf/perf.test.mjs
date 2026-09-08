import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readdir, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { HELP, parseOptions, statistics, summarize, toCsv, embeddedJson, validateTiming } from "./core.mjs";
import { isolatedEnv, measureMemory, measureRpc, runProcess } from "./process.mjs";
import { renderDashboard } from "./report.mjs";

test("options reject invalid workloads and quick defaults remain overridable", () => {
  const options = parseOptions(["--quick", "--rounds", "2", "--sizes", "10000,1000,1000"], "/repo");
  assert.equal(options.samples, 5);
  assert.equal(options.rounds, 2);
  assert.deepEqual(options.sizes, [1000, 10000]);
  assert.equal(options.tsRoot, "/repo/legacy/pi");
  assert.equal(parseOptions(["--samples=9"], "/repo").samples, 9);
  for (const args of [["--samples", "0"], ["--samples", "NaN"], ["--rounds", "1.5"],
    ["--sizes", "1,"], ["--suite", "unknown"], ["--payload-bytes", "1"], ["--wat"]])
    assert.throws(() => parseOptions(args, "/repo"));
  assert.deepEqual(parseOptions(["--help"], "/repo"), { help: true });
  assert.match(HELP, /--open/);
});

test("nearest-rank statistics preserve samples and missing results", () => {
  const values = Array.from({ length: 20 }, (_, i) => 20 - i);
  const result = statistics(values);
  assert.equal(result.p50, 10);
  assert.equal(result.p95, 19);
  assert.equal(result.mean, 10.5);
  assert.equal(values[0], 20);
  assert.equal(statistics([]), null);
  assert.throws(() => statistics([NaN]));
  const rows = summarize([
    { name: "fork", size: 1000, unit: "ms", backend: "rust", round: 1, batch: 1, samples: [1, 2] },
    { name: "fork", size: 1000, unit: "ms", backend: "rust", round: 2, batch: 1, samples: [3, 4] },
    { name: "fork", size: 1000, unit: "ms", backend: "ts", round: 1, batch: 1, samples: [] },
    { name: "loaded_rss", size: 1000, unit: "bytes", backend: "rust", round: 1, batch: 1, samples: [-1024] },
  ]);
  assert.equal(rows[0].statistics.n, 4);
  assert.equal(rows[0].rounds.length, 2);
  assert.equal(rows[1].statistics, null);
  assert.equal(rows[2].statistics.mean, -1024);
  assert.match(toCsv(rows), /"fork","1000","rust","ms","4"/);
});

test("worker sample validation rejects incomplete, non-finite and mislabeled output", () => {
  const item = { name: "get100", unit: "ms", batch: 1000, samples: [1, 2] };
  assert.equal(validateTiming(item, 2, "read"), item);
  for (const value of [null, { ...item, samples: [1] }, { ...item, samples: [NaN, 1] },
    { ...item, samples: [-1, 2] }, { ...item, name: "fork" }, { ...item, unit: "bytes" }])
    assert.throws(() => validateTiming(value, 2, "read"));
});

test("dashboard is self-contained and safely embeds metadata", async () => {
  const attack = "</script><img src=x onerror=alert(1)>";
  assert(!embeddedJson({ attack }).includes("<"));
  assert.equal(JSON.parse(embeddedJson({ attack })).attack, attack);
  const html = await renderDashboard({ status: "failed", errors: [attack], metrics: [], summary: [] });
  assert(!html.includes(attack));
  assert(!html.includes("__PERF_DATA__"));
  assert(!/<script[^>]+src=/.test(html));
  assert(!/<link[^>]+href=/.test(html));
  const data = html.match(/<script id="perf-data" type="application\/json">([\s\S]*?)<\/script>/)[1];
  assert.equal(JSON.parse(data).errors[0], attack);
});

test("benchmark environment forwards no provider tokens or NODE_OPTIONS", () => {
  const env = isolatedEnv("/tmp/perf-test", "rust");
  assert.equal(env.PI_AGENT_DIR, "/tmp/perf-test/rust-agent");
  assert.equal(env.OPENAI_API_KEY, undefined);
  assert.equal(env.NODE_OPTIONS, undefined);
  assert.equal(env.ANTHROPIC_AUTH_TOKEN, undefined);
});

test("child spawn failures and hard timeouts reject without orphaning work", async () => {
  await assert.rejects(runProcess("/missing/perf-executable", [], { timeoutMs: 1000 }), /ENOENT/);
  await assert.rejects(runProcess(process.execPath,
    ["-e", "process.on('SIGTERM',()=>{});setInterval(()=>{},1000)"],
    { timeoutMs: 150 }), /Timed out/);
});

test("RPC keeps stdin open, matches response identity and rejects EOF/failure", async () => {
  const good = "process.stdin.once('data',b=>{const m=JSON.parse(b);process.stdout.write(JSON.stringify({type:'response',id:'unrelated',success:true})+'\\n');setTimeout(()=>process.stdout.write(JSON.stringify({type:'response',id:m.id,success:true})+'\\n'),20)});setInterval(()=>{},1000);";
  const measured = await measureRpc(process.execPath, ["-e", good, "--"], { timeoutMs: 3000 });
  assert(measured.ms > 0);
  await assert.rejects(measureRpc(process.execPath, ["-e", "process.exit(0)", "--"], { timeoutMs: 3000 }), /before a successful/);
  const bad = "process.stdin.once('data',b=>console.log(JSON.stringify({type:'response',id:JSON.parse(b).id,success:false})))";
  await assert.rejects(measureRpc(process.execPath, ["-e", bad, "--"], { timeoutMs: 3000 }), /unsuccessful/);
});

test("memory collection requires both acknowledged checkpoints", async () => {
  const worker = "let phase=0;console.log(JSON.stringify({phase:'baseline'}));process.stdin.on('data',()=>{if(phase++===0)console.log(JSON.stringify({phase:'loaded'}));else process.exit(0)});";
  const memory = await measureMemory(process.execPath, ["-e", worker], { timeoutMs: 3000 });
  assert(memory.baseline.rssBytes > 0);
  assert(memory.loaded.rssBytes > 0);
  assert(Number.isFinite(memory.deltaBytes));
  await assert.rejects(measureMemory(process.execPath, ["-e", "process.exit(0)"], { timeoutMs: 3000 }), /before both/);
});

test("failed preflight still exports an explicit failed dashboard and raw results", async () => {
  const output = await mkdtemp(join(tmpdir(), "pi-perf-test-"));
  try {
    await assert.rejects(runProcess(process.execPath, [
      fileURLToPath(new URL("./run.mjs", import.meta.url)),
      "--quick", "--backend", "ts", "--ts-root", join(output, "missing-oracle"),
      "--output", output,
    ], { timeoutMs: 10000 }), /exited 1/);
    const entries = await readdir(output, { withFileTypes: true });
    const run = entries.find(entry => entry.isDirectory());
    assert(run);
    const result = JSON.parse(await readFile(join(output, run.name, "results.json"), "utf8"));
    assert.equal(result.status, "failed");
    assert.equal(result.metrics.length, 0);
    assert.match(result.errors[0], /oracle checkout missing/);
    assert.match(await readFile(join(output, "latest.html"), "utf8"), /Pi 性能看板/);
  } finally {
    await rm(output, { recursive: true, force: true });
  }
});
