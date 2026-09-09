import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import test from "node:test";

const testDirectory = dirname(fileURLToPath(import.meta.url));
const launcher = join(testDirectory, "../dist/bin/pi.js");
const execFileAsync = promisify(execFile);

// Run outside node:test's worker: its stdin pipe stays open while a test is running,
// so invoking the native CLI in-process would wait forever for piped input to end.
async function runNativeSmoke(arguments_: string[], root: string) {
  const env: NodeJS.ProcessEnv = {};
  for (const name of [
    "PATH", "SystemRoot", "SYSTEMROOT", "WINDIR", "ComSpec", "COMSPEC", "PATHEXT",
    "TMP", "TEMP", "TMPDIR", "LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH",
  ]) {
    if (process.env[name] !== undefined) env[name] = process.env[name];
  }
  if (process.env.PI_RS_NATIVE_BINDING) {
    env.PI_RS_NATIVE_BINDING = resolve(process.env.PI_RS_NATIVE_BINDING);
  }
  Object.assign(env, {
    HOME: root,
    USERPROFILE: root,
    XDG_CONFIG_HOME: join(root, "config"),
    PI_OFFLINE: "1",
    // Handled commands need no model call. Route accidental provider use locally;
    // completion markers below detect it even when the CLI reports a zero exit code.
    OPENAI_API_KEY: "unused-native-smoke-key",
    OPENAI_BASE_URL: "http://127.0.0.1:9/v1",
  });
  const run = execFileAsync(process.execPath, [launcher, ...arguments_], {
    cwd: root,
    env,
    timeout: 15_000,
    killSignal: "SIGKILL",
    maxBuffer: 1024 * 1024,
  });
  // EOF is part of the fixture, independent of the test runner's own stdin.
  run.child.stdin?.end();
  return run;
}

test("runs a TypeScript command through Node, NAPI, and the Rust session runtime", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "pi-rs-napi-command-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const agentDirectory = join(root, "agent");
  await mkdir(agentDirectory);
  const extension = join(testDirectory, "fixtures", "command.ts");

  const { stdout } = await runNativeSmoke([
    "--cwd", root,
    "--agent-dir", agentDirectory,
    "--no-extensions",
    "--extension", extension,
    "--no-approve",
    "--print", "/bridge-smoke",
  ], root);
  assert.match(stdout, /bridge-smoke: session replacement and reload verified/);
});

test("loads a settings extension through PackageManager discovery and NAPI", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "pi-rs-napi-settings-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const agentDirectory = join(root, "agent");
  const extension = join(agentDirectory, "configured/settings-command.js");
  await mkdir(dirname(extension), { recursive: true });
  await writeFile(
    extension,
    `
      export default function (pi) {
        pi.registerCommand("settings-bridge-smoke", {
          description: "Verify settings-driven extension discovery",
          async handler() {
            process.stdout.write("settings-bridge-smoke: command verified\\n");
          }
        });
      }
    `,
  );
  await writeFile(
    join(agentDirectory, "settings.json"),
    JSON.stringify({ extensions: ["./configured/settings-command.js"] }),
  );

  const { stdout } = await runNativeSmoke([
    "--cwd", root,
    "--agent-dir", agentDirectory,
    "--no-approve",
    "--print", "/settings-bridge-smoke",
  ], root);
  assert.match(stdout, /settings-bridge-smoke: command verified/);
});

test("runs a PackageManager command through the Node/NAPI launcher", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "pi-rs-napi-package-command-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const agentDirectory = join(root, "agent");
  const projectDirectory = join(root, "project");
  const extension = join(projectDirectory, "extension");
  await mkdir(extension, { recursive: true });
  await mkdir(agentDirectory, { recursive: true });
  await writeFile(join(extension, "index.ts"), "export default function () {}\n");
  await writeFile(
    join(agentDirectory, "settings.json"),
    JSON.stringify({ packages: [extension] }),
  );

  const { stdout } = await runNativeSmoke([
    "--cwd", projectDirectory,
    "--agent-dir", agentDirectory,
    "--no-approve",
    "list",
  ], root);
  assert.ok(stdout.includes(extension), "the configured local package must be listed");
});
