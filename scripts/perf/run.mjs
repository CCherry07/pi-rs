#!/usr/bin/env node
import { access, readFile, writeFile, mkdir, mkdtemp, rm, stat } from "node:fs/promises";
import { createReadStream } from "node:fs";
import { createHash } from "node:crypto";
import { spawn } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import os from "node:os";
import { HELP, parseOptions, statistics, validateTiming } from "./core.mjs";
import { cancelChildren, isolatedEnv, measureMemory, measureRpc, runProcess } from "./process.mjs";
import { writeReports } from "./report.mjs";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(scriptDir, "../..");
const exists = async path => access(path).then(() => true, () => false);
const sha = text => createHash("sha256").update(text).digest("hex");
async function fingerprint(path) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  const info = await stat(path);
  return { path, sha256: hash.digest("hex"), bytes: info.size, modifiedAt: info.mtime.toISOString() };
}
async function commandText(command, args, cwd = root) {
  return (await runProcess(command, args, { cwd, timeoutMs: 30000 })).stdout.trim();
}
async function revision(directory) {
  try {
    const head = await commandText("git", ["rev-parse", "HEAD"], directory);
    const status = await commandText("git", ["status", "--porcelain"], directory);
    const diff = await commandText("git", ["diff", "HEAD", "--"], directory);
    return { head, dirty: Boolean(status), status, trackedDiffSha256: sha(diff) };
  } catch { return { head: null, dirty: null }; }
}
function progress(message) { console.log("[perf] " + message); }
function display(item) {
  const s = statistics(item.samples);
  const value = item.unit === "bytes" ? (s.p50 / 1048576).toFixed(2) + " MiB"
    : s.p50 < 1 ? (s.p50 * 1000).toFixed(3) + " us" : s.p50.toFixed(3) + " ms";
  progress(item.backend + " " + item.name + (item.size ? " / " + item.size : "") +
    " / round " + item.round + ": P50 " + value + " (n=" + s.n + ")");
}
async function openBrowser(path) {
  const command = process.platform === "darwin" ? "open" : "xdg-open";
  await new Promise((resolve, reject) => {
    const child = spawn(command, [path], { stdio: "ignore" });
    child.once("error", reject);
    child.once("exit", code => code === 0 ? resolve() : reject(new Error(command + " exited " + code)));
  });
}

async function main() {
  const config = parseOptions(process.argv.slice(2), root);
  if (config.help) { console.log(HELP); return; }
  if (!["darwin", "linux"].includes(process.platform))
    throw new Error("The RSS collector currently supports macOS and Linux.");
  const [major, minor] = process.versions.node.split(".").map(Number);
  if (major < 22 || (major === 22 && minor < 19)) throw new Error("Node >=22.19 is required.");
  await mkdir(config.output, { recursive: true });
  const runDirectory = await mkdtemp(join(config.output, new Date().toISOString().replace(/[:.]/g, "-") + "-"));
  const runId = runDirectory.slice(config.output.length + 1);
  const latest = join(config.output, "latest.html");
  const scratch = await mkdtemp(join(os.tmpdir(), "pi-perf-"));
  const backends = config.backend === "both" ? ["rust", "ts"] : [config.backend];
  const sessionEnabled = config.suite !== "cli", cliEnabled = config.suite !== "session";
  let cancelled = false;
  const interrupt = () => { cancelled = true; cancelChildren(); };
  process.on("SIGINT", interrupt); process.on("SIGTERM", interrupt);
  const result = {
    schemaVersion: 1, benchmarkVersion: 1, runId, status: "running",
    startedAt: new Date().toISOString(), finishedAt: null, config,
    environment: {
      platform: process.platform, arch: process.arch, osRelease: os.release(),
      cpu: os.cpus()[0]?.model ?? "unknown", logicalCpus: os.cpus().length,
      memoryBytes: os.totalmem(), node: process.version,
    },
    implementations: {}, metrics: [], load: [], errors: [],
  };
  const save = (publishLatest = false) => writeReports(result, runDirectory, publishLatest ? latest : null);
  const checkCancelled = () => { if (cancelled) throw new Error("Benchmark cancelled"); };
  const options = backend => ({
    cwd: join(scratch, "cwd"),
    env: isolatedEnv(scratch, backend, backend === "ts" ? {
      TSX_TSCONFIG_PATH: join(config.tsRoot, "packages/agent/benchmark/tsconfig.json"),
    } : {}),
    timeoutMs: config.timeoutMs,
  });
  let rustCli, rustWorker, tsCli, tsLoader;
  try {
    await save();
    progress("结果目录: " + runDirectory);
    await mkdir(join(scratch, "cwd"), { recursive: true });
    for (const backend of backends) await mkdir(join(scratch, backend + "-agent"), { recursive: true });
    // Preflight before any measurement. Builds are deliberately outside the timed phases.
    if (backends.includes("ts")) {
      const packagePath = join(config.tsRoot, "packages/coding-agent/package.json");
      if (!await exists(packagePath))
        throw new Error("Pi oracle checkout missing. Place it at legacy/pi or use --ts-root PATH.");
      if (sessionEnabled) {
        if (!await exists(join(config.tsRoot, "node_modules/tsx/package.json")))
          throw new Error("TS dependencies missing. Run npm ci --prefix " + JSON.stringify(config.tsRoot));
        const loaderPackage = JSON.parse(await readFile(join(config.tsRoot, "node_modules/tsx/package.json"), "utf8"));
        tsLoader = join(config.tsRoot, "node_modules/tsx", loaderPackage.exports["."]);
      }
      if (config.buildTs) {
        progress("构建 TypeScript Pi（上游 build:offline）…");
        await runProcess("npm", ["run", "build:offline"], {
          cwd: config.tsRoot, timeoutMs: 3_600_000, inheritOutput: true,
        });
        checkCancelled();
      }
      tsCli = config.tsCliEntry ?? join(config.tsRoot, "packages/coding-agent/dist",
        config.tsCli === "bundled" ? "bundle/cli.js" : "cli.js");
      if (cliEnabled && !await exists(tsCli))
        throw new Error("Compiled TS CLI missing: " + tsCli + ". Use --build-ts after installing dependencies, or --suite session.");
      result.implementations.ts = {
        ...(await revision(config.tsRoot)),
        version: JSON.parse(await readFile(packagePath, "utf8")).version,
        cliFlavor: config.tsCliEntry ? "custom" : config.tsCli,
        cliExistingBuild: !config.buildTs,
        cliArtifact: cliEnabled ? await fingerprint(tsCli) : null,
        sessionWorker: await fingerprint(join(scriptDir, "ts-session.mjs")),
        sessionExecution: "tsx + source export condition, local oracle benchmark helpers",
      };
      if (cliEnabled) progress("TS CLI: " + tsCli + (config.buildTs ? "（本次构建）" : "（现有构建，记录入口指纹）"));
    }
    if (backends.includes("rust")) {
      result.environment.rustc = await commandText("rustc", ["--version"]);
      const metadata = JSON.parse(await commandText("cargo", ["metadata", "--format-version", "1", "--no-deps"]));
      const manifest = join(metadata.target_directory, "perf", "rust-build.json");
      if (!config.skipBuild) {
        progress("构建 Rust release …");
        const args = ["build", "--locked", "--release", "--message-format=json-render-diagnostics"];
        if (sessionEnabled) args.push("-p", "pi-session", "--example", "perf_session");
        if (cliEnabled) args.push("-p", "pi-cli", "--bin", "pi");
        await runProcess("cargo", args, {
          cwd: root, timeoutMs: 3_600_000, stderrOutput: true,
          onLine(line) {
            let item; try { item = JSON.parse(line); } catch { return; }
            if (item.reason !== "compiler-artifact" || !item.executable) return;
            if (item.target.name === "pi") rustCli = item.executable;
            if (item.target.name === "perf_session") rustWorker = item.executable;
          },
        });
        checkCancelled();
        let previous = {};
        try { previous = JSON.parse(await readFile(manifest, "utf8")); } catch { /* first build */ }
        await mkdir(dirname(manifest), { recursive: true });
        await writeFile(manifest, JSON.stringify({ ...previous,
          ...(rustCli ? { cli: rustCli } : {}), ...(rustWorker ? { worker: rustWorker } : {}),
        }, null, 2));
      } else {
        let previous = {};
        try { previous = JSON.parse(await readFile(manifest, "utf8")); } catch { /* conventional target layout */ }
        const profile = join(metadata.target_directory, process.env.CARGO_BUILD_TARGET ?? "", "release");
        rustCli = previous.cli ?? join(profile, "pi");
        rustWorker = previous.worker ?? join(profile, "examples/perf_session");
      }
      if ((sessionEnabled && (!rustWorker || !await exists(rustWorker))) ||
          (cliEnabled && (!rustCli || !await exists(rustCli))))
        throw new Error("Rust benchmark executable missing. Run without --skip-build.");
      result.implementations.rust = {
        ...(await revision(root)), profile: "release", reusedBuild: config.skipBuild,
        cliArtifact: cliEnabled ? await fingerprint(rustCli) : null,
        sessionArtifact: sessionEnabled ? await fingerprint(rustWorker) : null,
        sessionWorker: await fingerprint(join(root, "crates/pi-session/examples/perf_session.rs")),
      };
    }
    await save();
    const record = async item => { result.metrics.push(item); display(item); await save(); };
    const workerCommand = (backend, mode, size) => {
      const params = [mode, size, config.samples, config.warmup, config.batch, config.payloadBytes].map(String);
      return backend === "rust" ? [rustWorker, ...params] :
        [process.execPath, "--conditions=source", "--expose-gc", "--import", tsLoader,
          join(scriptDir, "ts-session.mjs"), config.tsRoot, ...params];
    };
    if (sessionEnabled) {
      for (let round = 1; round <= config.rounds; round++) {
        const order = round % 2 ? backends : [...backends].reverse();
        const jobs = [...config.sizes.map(size => ["read", size]),
          ...config.forkSizes.map(size => ["fork", size]), ["catalog", config.catalogSize]];
        for (const [mode, size] of jobs) {
          for (const backend of order) {
            checkCancelled();
            progress("运行 " + backend + " " + mode + " / " + size + " / round " + round);
            result.load.push({ phase: mode, backend, round, size, at: new Date().toISOString(), loadavg: os.loadavg() });
            const [command, ...args] = workerCommand(backend, mode, size);
            const output = await runProcess(command, args, options(backend));
            const items = output.stdout.trim().split("\n").map(line => validateTiming(JSON.parse(line), config.samples, mode));
            const expected = { read: 3, fork: 1, catalog: 2 }[mode];
            if (items.length !== expected || new Set(items.map(item => item.name)).size !== expected)
              throw new Error("Worker returned missing/duplicate metrics: " + backend + " " + mode);
            for (const item of items)
              await record({ ...item, backend, round, size: item.name === "create_session" ? 0 : size });
          }
        }
        for (const size of config.sizes) for (const backend of order) {
          checkCancelled();
          const [command, ...args] = workerCommand(backend, "memory", size);
          const memory = await measureMemory(command, args, options(backend));
          await record({ backend, round, size, name: "loaded_rss", unit: "bytes", batch: 1,
            samples: [memory.deltaBytes], checkpoints: memory });
        }
      }
    }
    if (cliEnabled) {
      for (const scenario of ["version", "help", "rpc"]) {
        const timing = {}, rss = {};
        for (const backend of backends) {
          timing[backend] = { backend, name: "cli_" + scenario, size: 0, round: 1, batch: 1, unit: "ms", samples: [] };
          result.metrics.push(timing[backend]);
          if (scenario === "rpc") {
            rss[backend] = { backend, name: "rpc_rss", size: 0, round: 1, batch: 1, unit: "bytes", samples: [] };
            result.metrics.push(rss[backend]);
          }
        }
        progress("CLI " + scenario + " / " + config.cliSamples + " samples per implementation");
        for (let sample = -config.cliWarmup; sample < config.cliSamples; sample++) {
          for (const backend of sample % 2 === 0 ? backends : [...backends].reverse()) {
            checkCancelled();
            const command = backend === "rust" ? rustCli : process.execPath;
            const args = backend === "rust" ? [] : [tsCli];
            const opts = { ...options(backend), timeoutMs: Math.min(config.timeoutMs, 15000) };
            const measured = scenario === "rpc"
              ? await measureRpc(command, args, opts, sample >= 0 && sample < Math.min(5, config.cliSamples))
              : await runProcess(command, [...args, "--" + scenario], opts);
            if (scenario !== "rpc" && !measured.stdout.trim()) throw new Error("Empty CLI output: " + backend);
            if (sample >= 0) {
              timing[backend].samples.push(scenario === "rpc" ? measured.ms : measured.durationMs);
              if (measured.rssBytes !== undefined) rss[backend].samples.push(measured.rssBytes);
            }
          }
          if (sample >= 0 && ((sample + 1) % 10 === 0 || sample === config.cliSamples - 1)) {
            progress("CLI " + scenario + ": " + (sample + 1) + "/" + config.cliSamples);
            await save();
          }
        }
        for (const backend of backends) { display(timing[backend]); if (rss[backend]) display(rss[backend]); }
      }
    }
    result.status = "complete";
  } catch (error) {
    result.status = cancelled ? "cancelled" : "failed";
    result.errors.push(error.message);
    process.exitCode = cancelled ? 130 : 1;
    console.error("[perf] " + error.message);
  } finally {
    cancelChildren();
    result.finishedAt = new Date().toISOString();
    await save(true);
    process.removeListener("SIGINT", interrupt); process.removeListener("SIGTERM", interrupt);
    await rm(scratch, { recursive: true, force: true });
    progress("看板: " + join(runDirectory, "index.html"));
    progress("最新: " + latest);
    progress("原始数据: " + join(runDirectory, "results.json"));
  }
  if (config.open) {
    try { await openBrowser(join(runDirectory, "index.html")); }
    catch (error) { console.error("[perf] 无法自动打开浏览器，可手动打开 HTML: " + error.message); }
  }
}

main().catch(error => { console.error("[perf] " + error.message); process.exitCode = 1; });
