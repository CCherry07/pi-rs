import { parseArgs } from "node:util";
import { resolve } from "node:path";

export const HELP = `Usage: ./scripts/bench-perf [options]
Build Rust release binaries, benchmark against local TypeScript Pi, and generate an offline dashboard.

  --quick                 1 round, 5 samples, 2 warmups, 5 CLI samples
  --suite all|session|cli  Default: all
  --backend both|rust|ts   Default: both
  --rounds N              Independent session/memory rounds (default 3)
  --samples N             Measured session samples per round (default 30)
  --warmup N              Session warmup samples (default 10)
  --batch N               Operations per micro-read sample (default 1000)
  --cli-samples N         Measured processes per CLI scenario (default 50)
  --cli-warmup N          Warmup processes per CLI scenario (default 5)
  --sizes N,N,...         Read/memory entry counts (default 1000,10000,100000)
  --fork-sizes N,N,...    Fork entry counts (default 1000,10000)
  --catalog-size N        Session catalog size (default 10000)
  --payload-bytes N       Per-message ASCII text bytes, >=32 (default 256)
  --ts-root PATH          Pi oracle checkout (default legacy/pi)
  --ts-cli bundled|unbundled  Existing TS CLI build to measure (default bundled)
  --ts-cli-entry PATH     Explicit compiled TS CLI entry; overrides --ts-cli
  --build-ts              Run upstream npm run build:offline before benchmarking
  --skip-build            Reuse Rust binaries; their fingerprints are recorded
  --output PATH           Results parent directory (default target/perf)
  --timeout-ms N          Worker timeout (default 120000; CLI capped at 15000)
  --open                  Open the generated self-contained HTML dashboard
  --help                  Show this help

Each run creates <output>/<timestamp-id>/{index.html,results.json,summary.csv}.
<output>/latest.html points to the most recent report (also usable without a server).
Requires Node >=22.19, Rust toolchain, macOS/Linux. TS requires a local Pi checkout,
npm dependencies, and an existing compiled CLI for CLI tests. No provider requests.
`;

export function parseOptions(args, root) {
  const strings = ["suite", "backend", "rounds", "samples", "warmup", "batch", "cli-samples",
    "cli-warmup", "sizes", "fork-sizes", "catalog-size", "payload-bytes", "ts-root", "ts-cli",
    "ts-cli-entry", "output", "timeout-ms"];
  const booleans = ["quick", "build-ts", "skip-build", "open", "help"];
  const { values } = parseArgs({
    args, strict: true, allowPositionals: false,
    options: Object.fromEntries([...strings.map(key => [key, { type: "string" }]),
      ...booleans.map(key => [key, { type: "boolean" }])]),
  });
  if (values.help) return { help: true };
  const integer = (key, fallback, min = 1, max = 1_000_000) => {
    const raw = values[key] ?? String(fallback);
    if (!/^\d+$/.test(raw)) throw new Error("--" + key + " must be an integer");
    const number = Number(raw);
    if (!Number.isSafeInteger(number) || number < min || number > max)
      throw new Error("--" + key + " must be between " + min + " and " + max);
    return number;
  };
  const choice = (key, fallback, allowed) => {
    const value = values[key] ?? fallback;
    if (!allowed.includes(value)) throw new Error("--" + key + " must be " + allowed.join("|"));
    return value;
  };
  const sizes = (key, fallback) => {
    const parts = (values[key] ?? fallback).split(",");
    if (parts.some(p => !/^\d+$/.test(p) || Number(p) < 1 || Number(p) > 1_000_000))
      throw new Error("--" + key + " requires comma-separated entry counts in 1..1000000");
    return [...new Set(parts.map(Number))].sort((a, b) => a - b);
  };
  const quick = Boolean(values.quick);
  return {
    suite: choice("suite", "all", ["all", "session", "cli"]),
    backend: choice("backend", "both", ["both", "rust", "ts"]),
    rounds: integer("rounds", quick ? 1 : 3, 1, 30),
    samples: integer("samples", quick ? 5 : 30, 1, 1000),
    warmup: integer("warmup", quick ? 2 : 10, 0, 1000),
    batch: integer("batch", quick ? 50 : 1000),
    cliSamples: integer("cli-samples", quick ? 5 : 50, 1, 1000),
    cliWarmup: integer("cli-warmup", quick ? 1 : 5, 0, 1000),
    sizes: sizes("sizes", "1000,10000,100000"),
    forkSizes: sizes("fork-sizes", "1000,10000"),
    catalogSize: integer("catalog-size", 10000),
    payloadBytes: integer("payload-bytes", 256, 32, 16384),
    tsRoot: resolve(root, values["ts-root"] ?? "legacy/pi"),
    tsCli: choice("ts-cli", "bundled", ["bundled", "unbundled"]),
    tsCliEntry: values["ts-cli-entry"] ? resolve(root, values["ts-cli-entry"]) : null,
    output: resolve(root, values.output ?? "target/perf"),
    timeoutMs: integer("timeout-ms", 120000, 100, 3_600_000),
    skipBuild: Boolean(values["skip-build"]), buildTs: Boolean(values["build-ts"]),
    open: Boolean(values.open), quick,
  };
}

export function statistics(samples) {
  if (!Array.isArray(samples) || samples.length === 0) return null;
  if (samples.some(value => !Number.isFinite(value))) throw new Error("Non-finite benchmark sample");
  const sorted = [...samples].sort((a, b) => a - b);
  return {
    n: sorted.length,
    mean: sorted.reduce((sum, value) => sum + value, 0) / sorted.length,
    p50: sorted[Math.ceil(sorted.length * 0.50) - 1],
    p95: sorted[Math.ceil(sorted.length * 0.95) - 1],
    min: sorted[0], max: sorted.at(-1),
  };
}

export const DEFINITIONS = {
  get100: ["会话读取", "分散读取至多 100 条", "Rust: 至多 100 次单条调用并复制完整记录；TS: 1 次批量调用并返回引用。"],
  latest50: ["会话读取", "读取最新 50 条", "Rust 返回完整记录副本；TS 返回记录引用。"],
  full_branch: ["会话读取", "完整分支查询", "Rust 返回含 payload 的完整记录；TS 只返回结构元数据。输出不等价，不能作为等量工作的语言速度比较。"],
  fork: ["会话分支", "Fork 当前分支", "每次使用新建的等量源会话，准备与验证不计时。TS 源会话包含上游基准的配置和状态。"],
  list_sessions: ["会话目录", "列出全部会话", "内存目录查询：Rust 返回拥有所有权的元数据，TS 返回元数据引用。"],
  create_session: ["会话目录", "创建空会话", "每次使用独立的空仓库；只测内存会话创建，不含持久化。"],
  loaded_rss: ["内存", "已加载会话 RSS 增量", "同一进程加载前后用 ps 读取 RSS，TS 在两次读数前各强制 GC 3 次。这不是峰值 RSS，也不是精确堆分配量。"],
  cli_version: ["CLI 启动", "--version", "从创建进程到正常退出。包含动态库 / Node 模块加载；不清除系统文件缓存。"],
  cli_help: ["CLI 启动", "--help", "从创建进程到正常退出。使用临时工作目录与独立 agent 配置。"],
  cli_rpc: ["CLI 启动", "RPC get_state 就绪", "从创建进程到收到匹配 ID 的成功响应。stdin 保持打开；没有模型请求。"],
  rpc_rss: ["内存", "RPC 就绪后总 RSS", "收到 get_state 后等待 250 ms，使用 ps 读取总 RSS。最多 5 次；包含进程运行时，不减空进程基线。"],
};

export function validateTiming(item, expectedSamples, mode) {
  const allowed = { read: ["get100", "latest50", "full_branch"], fork: ["fork"],
    catalog: ["list_sessions", "create_session"] }[mode];
  if (!item || !allowed?.includes(item.name) || item.unit !== "ms" ||
      !Number.isInteger(item.batch) || item.batch < 1 ||
      !Array.isArray(item.samples) || item.samples.length !== expectedSamples ||
      item.samples.some(value => !Number.isFinite(value) || value < 0)) {
    throw new Error("Invalid or incomplete benchmark output for " + mode);
  }
  return item;
}

export function summarize(metrics) {
  const groups = new Map();
  for (const metric of metrics) {
    const key = JSON.stringify([metric.name, metric.size, metric.unit, metric.backend]);
    if (!groups.has(key)) groups.set(key, { name: metric.name, size: metric.size,
      unit: metric.unit, backend: metric.backend, samples: [], rounds: [], batches: [] });
    const group = groups.get(key);
    group.samples.push(...metric.samples);
    group.rounds.push({ round: metric.round, statistics: statistics(metric.samples) });
    group.batches.push(metric.batch);
  }
  return [...groups.values()].map(group => ({ ...group, statistics: statistics(group.samples) }));
}

export function toCsv(rows) {
  const columns = ["name", "size", "backend", "unit", "n", "mean", "p50", "p95", "min", "max"];
  const cell = value => '"' + String(value ?? "").replaceAll('"', '""') + '"';
  return [columns.join(","), ...rows.map(row => {
    const flat = { ...row, ...row.statistics };
    return columns.map(key => cell(flat[key])).join(",");
  })].join("\n") + "\n";
}

export function escapeHtml(text) {
  return String(text).replace(/[&<>"']/g, char =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[char]);
}

export function embeddedJson(value) {
  return JSON.stringify(value).replace(/</g, "\\u003c").replace(/\u2028/g, "\\u2028").replace(/\u2029/g, "\\u2029");
}
