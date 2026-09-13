import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createServer, type ServerResponse } from "node:http";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import test from "node:test";

const testDirectory = dirname(fileURLToPath(import.meta.url));
const launcher = join(testDirectory, "../dist/bin/pi.js");
const evalLauncher = join(testDirectory, "../dist/bin/pi-eval.js");
const execFileAsync = promisify(execFile);

// Run outside node:test's worker: its stdin pipe stays open while a test is running,
// so invoking the native CLI in-process would wait forever for piped input to end.
async function runNativeSmoke(arguments_: string[], root: string) {
  return runNativeLauncher(launcher, arguments_, root);
}

async function runNativeLauncher(entry: string, arguments_: string[], root: string) {
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
  const run = execFileAsync(process.execPath, [entry, ...arguments_], {
    cwd: root,
    env,
    timeout: 30_000,
    killSignal: "SIGKILL",
    maxBuffer: 1024 * 1024,
  });
  // EOF is part of the fixture, independent of the test runner's own stdin.
  run.child.stdin?.end();
  return run;
}

test("exposes the eval runner through Node and NAPI", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "pi-rs-napi-eval-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const { stdout } = await runNativeLauncher(evalLauncher, ["--help"], root);
  assert.match(stdout, /Run model-backed pi-rs evaluations/);
});

test("eval runner creates reloads and invokes a TypeScript extension", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "pi-rs-napi-extension-eval-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const agentDirectory = join(root, "agent");
  const artifactDirectory = join(root, "artifacts");
  await mkdir(agentDirectory, { recursive: true });
  const extensionSource = `
    import { defineTool } from "@earendil-works/pi-coding-agent";
    export default function (pi) {
      pi.registerTool(defineTool({
        name: "hello",
        label: "Hello",
        description: "Return a greeting",
        parameters: {
          type: "object",
          properties: { name: { type: "string" } },
          required: ["name"],
          additionalProperties: false
        },
        async execute(_id, params) {
          return { content: [{ type: "text", text: "Hello, " + params.name + "!" }] };
        }
      }));
    }
  `;
  const server = createServer((request, response) => {
    let body = "";
    request.setEncoding("utf8");
    request.on("data", (chunk) => { body += chunk; });
    request.on("end", () => {
      const payload = JSON.parse(body) as { messages: Array<Record<string, unknown>> };
      const last = payload.messages.at(-1) ?? {};
      const content = typeof last.content === "string" ? last.content : "";
      if (last.role === "user" && content.includes("Create a Pi TypeScript extension")) {
        sendToolCall(response, "write-call", "write", {
          path: ".pi/extensions/hello.ts",
          content: extensionSource,
        });
      } else if (last.role === "tool" && last.tool_call_id === "write-call") {
        sendText(response, "Extension created.");
      } else if (last.role === "user" && content.includes("Use the hello tool")) {
        sendToolCall(response, "hello-call", "hello", { name: "Bob" });
      } else if (last.role === "tool" && last.tool_call_id === "hello-call") {
        sendText(response, "Hello, Bob!");
      } else {
        response.writeHead(422, { "content-type": "text/plain" });
        response.end(`Unexpected eval request: ${body}`);
      }
    });
  });
  await new Promise<void>((resolvePromise, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolvePromise);
  });
  t.after(() => server.close());
  const address = server.address();
  assert.ok(address && typeof address !== "string");

  const { stdout } = await runNativeLauncher(evalLauncher, [
    "run", "coding/js-extension",
    "--variant", "default-system-prompt",
    "--provider", "openai-compatible",
    "--model", "gpt-4o-mini",
    "--base-url", `http://127.0.0.1:${address.port}/v1`,
    "--agent-dir", agentDirectory,
    "--artifact-dir", artifactDirectory,
  ], root);
  assert.match(stdout, /grader=generated_tool_workflow score=1\.000 passed=true/);
});

function sendToolCall(
  response: ServerResponse,
  id: string,
  name: string,
  arguments_: Record<string, unknown>,
): void {
  sendEvents(response, [{
    id: "chatcmpl-eval",
    object: "chat.completion.chunk",
    created: 0,
    model: "gpt-4o-mini",
    choices: [{
      index: 0,
      delta: {
        role: "assistant",
        tool_calls: [{
          index: 0,
          id,
          type: "function",
          function: { name, arguments: JSON.stringify(arguments_) },
        }],
      },
      finish_reason: "tool_calls",
    }],
    usage: { prompt_tokens: 8, completion_tokens: 4, total_tokens: 12 },
  }]);
}

function sendText(response: ServerResponse, text: string): void {
  sendEvents(response, [{
    id: "chatcmpl-eval",
    object: "chat.completion.chunk",
    created: 0,
    model: "gpt-4o-mini",
    choices: [{
      index: 0,
      delta: { role: "assistant", content: text },
      finish_reason: "stop",
    }],
    usage: { prompt_tokens: 8, completion_tokens: 4, total_tokens: 12 },
  }]);
}

function sendEvents(response: ServerResponse, events: unknown[]): void {
  response.writeHead(200, {
    "content-type": "text/event-stream",
    "cache-control": "no-cache",
  });
  for (const event of events) response.write(`data: ${JSON.stringify(event)}\n\n`);
  response.end("data: [DONE]\n\n");
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
