import { spawn, execFile } from "node:child_process";
import { performance } from "node:perf_hooks";
import { createInterface } from "node:readline";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const active = new Set();

export function cancelChildren() {
  for (const cancel of active) cancel();
}

// All children own a dedicated POSIX process group so timeouts also stop their workers.
export function runProcess(command, args, options = {}) {
  return new Promise((resolve, reject) => {
    const started = performance.now();
    const child = spawn(command, args, {
      cwd: options.cwd, env: options.env, detached: process.platform !== "win32",
      stdio: ["pipe", "pipe", "pipe"],
    });
    let stdout = "", stderr = "", failure, intentional = false, escalation;
    let pending = Promise.resolve();
    const signal = name => {
      if (!child.pid) return;
      try {
        if (process.platform === "win32") {
          if (child.exitCode === null && child.signalCode === null) child.kill(name);
        }
        else process.kill(-child.pid, name);
      } catch (error) { if (error.code !== "ESRCH") failure ??= error; }
    };
    const kill = () => {
      signal("SIGTERM");
      escalation ??= setTimeout(() => signal("SIGKILL"), 500);
    };
    const cancel = () => { failure ??= new Error("Benchmark cancelled"); kill(); };
    active.add(cancel);
    const timeout = setTimeout(() => {
      failure ??= new Error("Timed out after " + options.timeoutMs + " ms: " + command);
      kill();
    }, options.timeoutMs ?? 120000);
    const control = {
      pid: child.pid,
      elapsed: () => performance.now() - started,
      write: text => child.stdin.write(text),
      stop: () => { intentional = true; kill(); },
    };
    child.stdin.on("error", error => { failure ??= error; kill(); });
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    const capture = (field, data) => {
      if (field === "stdout") stdout += data; else stderr += data;
      if (options.inheritOutput || (field === "stderr" && options.stderrOutput))
        (field === "stdout" ? process.stdout : process.stderr).write(data);
      if (stdout.length + stderr.length > (options.maxOutput ?? 8 * 1024 * 1024)) {
        failure ??= new Error("Child output exceeded limit: " + command);
        kill();
      }
    };
    child.stdout.on("data", data => capture("stdout", data));
    child.stderr.on("data", data => capture("stderr", data));
    const reader = createInterface({ input: child.stdout });
    reader.on("line", line => {
      pending = pending.then(() => options.onLine?.(line, control))
        .catch(error => { failure ??= error; kill(); });
    });
    child.once("error", error => { failure ??= error; });
    child.once("close", async (code, exitSignal) => {
      const durationMs = performance.now() - started;
      await pending;
      clearTimeout(timeout);
      clearTimeout(escalation);
      if (failure || intentional) signal("SIGKILL");
      reader.close();
      active.delete(cancel);
      if (failure) {
        failure.stderr = stderr;
        reject(failure);
      } else if (code !== 0 && !intentional) {
        reject(new Error(command + " exited " + (code ?? exitSignal) + "\n" + stderr.slice(-4000)));
      } else {
        resolve({ stdout, stderr, durationMs, code, signal: exitSignal });
      }
    });
    try {
      options.onStart?.(control);
      if (!options.keepStdin) child.stdin.end();
    } catch (error) { failure ??= error; kill(); }
  });
}

export async function readRss(pid) {
  const { stdout } = await execFileAsync("ps", ["-o", "rss=", "-p", String(pid)], {
    timeout: 5000, maxBuffer: 1024,
  });
  const text = stdout.trim();
  if (!/^\d+$/.test(text)) throw new Error("Unable to read RSS for benchmark process " + pid);
  return Number(text) * 1024; // macOS and Linux ps both report KiB.
}

export async function measureMemory(command, args, options) {
  const checkpoints = {};
  const expected = ["baseline", "loaded"];
  await runProcess(command, args, {
    ...options, keepStdin: true,
    onLine: async (line, child) => {
      const item = JSON.parse(line);
      if (item.phase !== expected[Object.keys(checkpoints).length])
        throw new Error("Invalid memory checkpoint sequence");
      checkpoints[item.phase] = { rssBytes: await readRss(child.pid), runtime: item.memory ?? null };
      child.write("next\n");
    },
  });
  if (!checkpoints.baseline || !checkpoints.loaded) throw new Error("Memory worker exited before both checkpoints");
  return {
    ...checkpoints,
    deltaBytes: checkpoints.loaded.rssBytes - checkpoints.baseline.rssBytes,
  };
}

export async function measureRpc(command, args, options, withRss = false) {
  let response;
  await runProcess(command, [...args, "--mode", "rpc"], {
    ...options, keepStdin: true,
    onStart: child => child.write(JSON.stringify({ id: "perf-get-state", type: "get_state" }) + "\n"),
    onLine: async (line, child) => {
      let message;
      try { message = JSON.parse(line); } catch { return; }
      if (response || message.type !== "response" || message.id !== "perf-get-state") return;
      if (message.success !== true) throw new Error("RPC get_state was unsuccessful");
      response = { ms: child.elapsed() };
      if (withRss) {
        await new Promise(resolve => setTimeout(resolve, 250));
        response.rssBytes = await readRss(child.pid);
      }
      child.stop();
    },
  });
  if (!response) throw new Error("RPC process exited before a successful get_state response");
  return response;
}

// Benchmark children get no provider tokens or user configuration from the parent environment.
export function isolatedEnv(directory, backend, extra = {}) {
  const env = Object.fromEntries(["PATH", "SystemRoot", "WINDIR"].filter(k => process.env[k]).map(k => [k, process.env[k]]));
  return {
    ...env, LANG: "en_US.UTF-8", TERM: "xterm-256color", NO_COLOR: "1",
    TMPDIR: directory, XDG_CONFIG_HOME: directory + "/config", XDG_CACHE_HOME: directory + "/cache",
    PI_AGENT_DIR: directory + "/" + backend + "-agent",
    PI_CODING_AGENT_DIR: directory + "/" + backend + "-agent",
    PI_CODING_AGENT_SESSION_DIR: directory + "/" + backend + "-sessions",
    ...extra,
  };
}
