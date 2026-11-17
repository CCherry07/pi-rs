import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const launcher = resolve(dirname(fileURLToPath(import.meta.url)), "../dist/bin/pi.js");

for (const kind of ["Node", "standalone"] as const) {
  test(`${kind} auth check preserves readiness exit codes and avoids session startup`, {
    timeout: 60_000,
    skip: kind === "standalone" && !process.env.PI_RS_STANDALONE_BINARY,
  }, async t => {
    const root = await mkdtemp(join(tmpdir(), "pi-auth-check-"));
    t.after(() => rm(root, { recursive: true, force: true }));
    const agent = join(root, "agent");
    await mkdir(agent);
    await writeFile(join(agent, "settings.json"), JSON.stringify({
      defaultProvider: "auth-fixture", defaultModel: "fixture-model",
    }));
    await writeFile(join(agent, "models.json"), JSON.stringify({ providers: {
      "auth-fixture": { api: "openai-completions", baseUrl: "http://127.0.0.1:1",
        apiKey: "fixture-secret", models: [{ id: "fixture-model" }] },
    } }));
    const extension = join(root, "must-not-load.ts");
    await writeFile(extension, 'throw new Error("auth check loaded an extension");');
    const env: NodeJS.ProcessEnv = { HOME: root, USERPROFILE: root, PI_OFFLINE: "1" };
    for (const key of ["PATH", "SystemRoot", "WINDIR", "ComSpec", "PATHEXT", "TMP", "TEMP", "TMPDIR"]) {
      if (process.env[key] !== undefined) env[key] = process.env[key];
    }
    if (process.env.PI_RS_NATIVE_BINDING) env.PI_RS_NATIVE_BINDING = resolve(process.env.PI_RS_NATIVE_BINDING);
    const executable = kind === "Node" ? process.execPath : resolve(process.env.PI_RS_STANDALONE_BINARY!);
    const prefix = kind === "Node" ? [launcher] : [];
    async function check(args: string[], code: number, status: string, extraEnv: NodeJS.ProcessEnv = {}) {
      const child = spawn(executable, [...prefix, "--agent-dir", agent, "--cwd", root,
        "--extension", extension, "auth", "check", "--json", ...args],
      { cwd: root, env: { ...env, ...extraEnv }, stdio: ["ignore", "pipe", "pipe"], timeout: 30_000 });
      let stdout = "";
      let stderr = "";
      child.stdout.on("data", chunk => { stdout += chunk.toString(); });
      child.stderr.on("data", chunk => { stderr += chunk.toString(); });
      const exit = await new Promise<number | null>((done, reject) => {
        child.once("error", reject);
        child.once("close", done);
      });
      assert.equal(exit, code, `${kind} auth check ${args.join(" ")}: ${stderr || stdout || "process timed out or terminated"}`);
      const result = JSON.parse(stdout);
      assert.equal(result.status, status);
      assert.equal(result.credentials, undefined);
      assert.ok(!stdout.includes("fixture-secret") && !stderr.includes("fixture-secret"));
      assert.equal(stderr, "");
      return result;
    }
    assert.equal((await check([], 0, "ready")).provider, "auth-fixture");
    await check(["--provider", "missing-fixture"], 1, "not_ready");
    await check(["--model", "missing-fixture/unknown"], 2, "invalid");
    await check(["--provider", "google"], 1, "not_ready", { GOOGLE_API_KEY: "fixture-secret" });
    await check(["--provider", "openai-codex"], 1, "not_ready", { OPENAI_CODEX_TOKEN: "fixture-secret" });
    for (const provider of ["cloudflare-workers-ai", "cloudflare-ai-gateway"]) {
      await check(["--provider", provider], 1, "not_ready", { CLOUDFLARE_API_KEY: "fixture-secret" });
      await check(["--provider", provider], 0, "ready", {
        CLOUDFLARE_API_KEY: "fixture-secret", CLOUDFLARE_ACCOUNT_ID: "fixture-account", CLOUDFLARE_GATEWAY_ID: "fixture-gateway",
      });
    }
    await writeFile(join(agent, "models.json"), "fixture-secret-invalid-json");
    await check([], 2, "invalid");
    assert.deepEqual((await readdir(agent)).sort(), ["models.json", "settings.json"]);
  });
}
