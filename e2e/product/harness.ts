import { spawn } from "node:child_process";
import { access, cp, mkdir, mkdtemp, readFile, rm } from "node:fs/promises";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export type JsonObject = Record<string, unknown>;

export type ProviderTurn =
  | { text: string }
  | {
      toolCalls: Array<{
        id: string;
        name: string;
        arguments: JsonObject;
      }>;
    };

export interface ProductScenario {
  adapter: "native-cli" | "node-napi";
  input: string;
  providerTurns: ProviderTurn[];
  projectFixture?: string;
  extensions?: string[];
}

export interface ProductRun {
  events: JsonObject[];
  providerRequests: JsonObject[];
  sessionLog: string | null;
  stderr: string;
  stdout: string;
}

interface ProcessResult {
  code: number | null;
  stderr: string;
  stdout: string;
}

const workspaceDirectory = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "../..",
);
const nodeLauncher = join(workspaceDirectory, "packages/pi/dist/bin/pi.js");
const processTimeoutMs = 30_000;
const excludedFixtureEntries = new Set([
  ".agents",
  "dist",
  "node_modules",
  "target",
]);

/**
 * Runs one deterministic scenario through a real product process.
 *
 * The Interface deliberately exposes product intent and observable evidence only.
 * Temporary state, credential isolation, provider transport, process lifetime, and
 * NDJSON decoding stay behind this seam.
 */
export async function runProductScenario(
  scenario: ProductScenario,
): Promise<ProductRun> {
  if (scenario.adapter === "native-cli" && scenario.extensions?.length) {
    throw new Error("the native-cli adapter cannot load JavaScript extensions");
  }

  const root = await mkdtemp(join(tmpdir(), "pi-rs-product-e2e-"));
  const projectPath = join(root, "project");
  const agentDirectory = join(root, "agent");
  const homeDirectory = join(root, "home");
  const sessionPath = join(root, "session.jsonl");
  const provider = new ScriptedOpenAiServer(scenario.providerTurns);

  if (scenario.projectFixture) {
    await cp(resolve(scenario.projectFixture), projectPath, {
      recursive: true,
      filter: (source) => !excludedFixtureEntry(source),
    });
  } else {
    await mkdir(projectPath, { recursive: true });
  }
  await Promise.all([
    mkdir(agentDirectory, { recursive: true }),
    mkdir(homeDirectory, { recursive: true }),
  ]);

  try {
    await provider.start();
    const product = await resolveProductAdapter(scenario.adapter);
    const arguments_ = [
      ...product.arguments,
      "--cwd",
      projectPath,
      "--agent-dir",
      agentDirectory,
      "--session",
      sessionPath,
      "--no-approve",
      "--no-extensions",
      ...(scenario.extensions ?? []).flatMap((extension) => [
        "--extension",
        isAbsolute(extension) ? extension : resolve(projectPath, extension),
      ]),
      "--provider",
      "openai-compatible",
      "--model",
      "e2e-model",
      "--base-url",
      provider.baseUrl,
      "--json",
      scenario.input,
    ];
    const result = await runProcess(product.command, arguments_, {
      cwd: projectPath,
      env: isolatedEnvironment(homeDirectory),
    });

    if (result.code !== 0) {
      throw new Error(
        `product exited with ${String(result.code)}\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}`,
      );
    }
    provider.assertHealthy();

    const events = parseNdjson(result.stdout);
    const sessionLog = await readOptionalFile(sessionPath);
    return {
      events,
      providerRequests: provider.requests,
      sessionLog,
      stderr: result.stderr,
      stdout: result.stdout,
    };
  } finally {
    await provider.close();
    await rm(root, { recursive: true, force: true });
  }
}

class ScriptedOpenAiServer {
  readonly #server = createServer((request, response) => {
    void this.#handle(request, response).catch((error: unknown) => {
      const failure = error instanceof Error ? error : new Error(String(error));
      this.#failures.push(failure);
      if (!response.headersSent) {
        response.writeHead(500, { "Content-Type": "application/json" });
      }
      response.end(JSON.stringify({ error: failure.message }));
    });
  });
  readonly #turns: ProviderTurn[];
  readonly #failures: Error[] = [];
  #baseUrl: string | null = null;
  readonly requests: JsonObject[] = [];

  constructor(turns: ProviderTurn[]) {
    this.#turns = structuredClone(turns);
  }

  get baseUrl(): string {
    if (this.#baseUrl === null) {
      throw new Error("scripted provider has not started");
    }
    return this.#baseUrl;
  }

  async start(): Promise<void> {
    await new Promise<void>((resolvePromise, reject) => {
      const fail = (error: Error) => reject(error);
      this.#server.once("error", fail);
      this.#server.listen(0, "127.0.0.1", () => {
        this.#server.off("error", fail);
        resolvePromise();
      });
    });
    const address = this.#server.address();
    if (!isAddressInfo(address)) {
      throw new Error("scripted provider did not bind a TCP address");
    }
    this.#baseUrl = `http://127.0.0.1:${String(address.port)}/v1`;
  }

  async close(): Promise<void> {
    if (!this.#server.listening) return;
    await new Promise<void>((resolvePromise, reject) => {
      this.#server.close((error) => {
        if (error) reject(error);
        else resolvePromise();
      });
    });
  }

  assertHealthy(): void {
    if (this.#failures.length > 0) {
      throw new Error(
        `scripted provider failed: ${this.#failures.map((error) => error.message).join("; ")}`,
      );
    }
    if (this.#turns.length > 0) {
      throw new Error(
        `product left ${String(this.#turns.length)} scripted provider turn(s) unused`,
      );
    }
  }

  async #handle(
    request: IncomingMessage,
    response: ServerResponse,
  ): Promise<void> {
    if (request.method !== "POST") {
      response.writeHead(405, { Allow: "POST" });
      response.end();
      return;
    }

    const body = parseObject(await readRequestBody(request), "provider request");
    this.requests.push(body);
    const turn = this.#turns.shift();
    if (turn === undefined) {
      throw new Error("product made more provider requests than the scenario declared");
    }

    response.writeHead(200, {
      "Cache-Control": "no-cache",
      "Content-Type": "text/event-stream",
    });
    response.end(`${openAiSse(turn)}data: [DONE]\n\n`);
  }
}

function openAiSse(turn: ProviderTurn): string {
  const choice =
    "text" in turn
      ? {
          index: 0,
          delta: { content: turn.text },
          finish_reason: "stop",
        }
      : {
          index: 0,
          delta: {
            tool_calls: turn.toolCalls.map((call, index) => ({
              index,
              id: call.id,
              type: "function",
              function: {
                name: call.name,
                arguments: JSON.stringify(call.arguments),
              },
            })),
          },
          finish_reason: "tool_calls",
        };
  const chunk = {
    id: "pi-rs-product-e2e",
    object: "chat.completion.chunk",
    created: 0,
    model: "e2e-model",
    choices: [choice],
    usage: { prompt_tokens: 10, completion_tokens: 6, total_tokens: 16 },
  };
  return `data: ${JSON.stringify(chunk)}\n\n`;
}

async function resolveProductAdapter(
  adapter: ProductScenario["adapter"],
): Promise<{ command: string; arguments: string[] }> {
  if (adapter === "node-napi") {
    await requireFile(nodeLauncher, "run `npm --prefix packages/pi run build`");
    return { command: process.execPath, arguments: [nodeLauncher] };
  }

  const configured = process.env.PI_E2E_NATIVE_BIN;
  const targetSetting = process.env.CARGO_TARGET_DIR;
  const targetDirectory = targetSetting
    ? resolve(targetSetting)
    : join(workspaceDirectory, "target");
  const binary = configured
    ? resolve(configured)
    : join(
        targetDirectory,
        "debug",
        process.platform === "win32" ? "pi.exe" : "pi",
      );
  await requireFile(binary, "run `cargo build -p pi-cli`");
  return { command: binary, arguments: [] };
}

function isolatedEnvironment(homeDirectory: string): NodeJS.ProcessEnv {
  const environment = { ...process.env };
  for (const name of [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_OAUTH_TOKEN",
    "GEMINI_API_KEY",
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "PI_AGENT_DIR",
    "PI_PLUGIN_REGISTRY",
    "XAI_API_KEY",
  ]) {
    delete environment[name];
  }
  environment.HOME = homeDirectory;
  environment.USERPROFILE = homeDirectory;
  environment.PI_OFFLINE = "1";
  environment.NO_PROXY = "127.0.0.1,localhost";
  environment.no_proxy = "127.0.0.1,localhost";
  return environment;
}

async function runProcess(
  command: string,
  arguments_: string[],
  options: { cwd: string; env: NodeJS.ProcessEnv },
): Promise<ProcessResult> {
  return await new Promise<ProcessResult>((resolvePromise, reject) => {
    const child = spawn(command, arguments_, {
      cwd: options.cwd,
      env: options.env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    if (child.stdout === null || child.stderr === null) {
      reject(new Error("product process did not expose stdout and stderr"));
      return;
    }

    let stdout = "";
    let stderr = "";
    let timedOut = false;
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk: string) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk: string) => {
      stderr += chunk;
    });
    const timeout = setTimeout(() => {
      timedOut = true;
      child.kill();
    }, processTimeoutMs);
    child.once("error", (error) => {
      clearTimeout(timeout);
      reject(error);
    });
    child.once("close", (code) => {
      clearTimeout(timeout);
      if (timedOut) {
        reject(
          new Error(
            `product exceeded ${String(processTimeoutMs)}ms\nstdout:\n${stdout}\nstderr:\n${stderr}`,
          ),
        );
      } else {
        resolvePromise({ code, stdout, stderr });
      }
    });
  });
}

function parseNdjson(output: string): JsonObject[] {
  return output
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line, index) => parseObject(line, `NDJSON line ${String(index + 1)}`));
}

function parseObject(raw: string, label: string): JsonObject {
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch (error) {
    throw new Error(`${label} is not valid JSON: ${String(error)}\n${raw}`);
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`${label} must be a JSON object`);
  }
  return value as JsonObject;
}

async function readRequestBody(request: IncomingMessage): Promise<string> {
  let body = "";
  request.setEncoding("utf8");
  for await (const chunk of request) body += String(chunk);
  return body;
}

async function readOptionalFile(path: string): Promise<string | null> {
  try {
    return await readFile(path, "utf8");
  } catch (error) {
    if (isNodeError(error) && error.code === "ENOENT") return null;
    throw error;
  }
}

async function requireFile(path: string, remedy: string): Promise<void> {
  try {
    await access(path);
  } catch {
    throw new Error(`required product artifact is missing: ${path}; ${remedy}`);
  }
}

function excludedFixtureEntry(path: string): boolean {
  return excludedFixtureEntries.has(basename(path));
}

function isAddressInfo(value: string | AddressInfo | null): value is AddressInfo {
  return value !== null && typeof value !== "string";
}

function isNodeError(value: unknown): value is NodeJS.ErrnoException {
  return value instanceof Error && "code" in value;
}
