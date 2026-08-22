import assert from "node:assert/strict";
import { spawn, type SpawnOptions } from "node:child_process";
import { once } from "node:events";
import { mkdtemp } from "node:fs/promises";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

import { z } from "zod";

import { isRecord, parseJson } from "../src/extension-protocol.js";

const testDirectory = dirname(fileURLToPath(import.meta.url));
const providerRequestSchema = z.looseObject({
  messages: z.array(z.looseObject({ content: z.string() })),
  tools: z.array(
    z.looseObject({
      function: z.looseObject({ name: z.string() }),
    }),
  ),
});
type ProviderRequest = z.infer<typeof providerRequestSchema>;

function parseProviderRequest(raw: string): ProviderRequest {
  return providerRequestSchema.parse(parseJson(raw, "provider request"));
}

function isAddressInfo(value: string | AddressInfo | null): value is AddressInfo {
  return value !== null && typeof value !== "string";
}

function isAgentStartEvent(value: unknown): boolean {
  return isRecord(value) && isRecord(value.event) && value.event.type === "agent_start";
}

const workspaceDirectory = resolve(testDirectory, "../../..");
const launcher = join(workspaceDirectory, "packages/pi/dist/bin/pi.js");

interface ProcessResult {
  code: number | null;
  stdout: string;
  stderr: string;
}

function runProcess(
  command: string,
  arguments_: string[],
  options: SpawnOptions & { stdio: ["ignore", "pipe", "pipe"] },
): Promise<ProcessResult> {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(command, arguments_, options);
    const stdoutStream = child.stdout;
    const stderrStream = child.stderr;
    assert.ok(stdoutStream);
    assert.ok(stderrStream);
    let stdout = "";
    let stderr = "";
    stdoutStream.setEncoding("utf8");
    stderrStream.setEncoding("utf8");
    stdoutStream.on("data", (chunk: string) => {
      stdout += chunk;
    });
    stderrStream.on("data", (chunk: string) => {
      stderr += chunk;
    });
    child.once("error", reject);
    child.once("close", (code) => resolvePromise({ code, stdout, stderr }));
  });
}

test("runs the frontend-app extension through NAPI", async () => {
  const agentDirectory = await mkdtemp(join(tmpdir(), "pi-rs-frontend-agent-"));
  const projectDirectory = join(workspaceDirectory, "e2e/projects/frontend-app");
  const extension = join(projectDirectory, ".pi/extensions/frontend-napi.ts");
  const requests: ProviderRequest[] = [];
  const provider = createServer(async (request, response) => {
    let body = "";
    request.setEncoding("utf8");
    for await (const chunk of request) body += chunk;
    requests.push(parseProviderRequest(body));
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    response.end(
      `data: ${JSON.stringify({
        id: "frontend-napi-e2e",
        object: "chat.completion.chunk",
        created: 0,
        model: "e2e-model",
        choices: [
          {
            index: 0,
            delta: { content: "Frontend NAPI command reached the agent." },
            finish_reason: "stop",
          },
        ],
        usage: { prompt_tokens: 10, completion_tokens: 6, total_tokens: 16 },
      })}\n\ndata: [DONE]\n\n`,
    );
  });
  provider.listen(0, "127.0.0.1");
  await once(provider, "listening");
  const address = provider.address();
  assert.ok(isAddressInfo(address));

  try {
    const result = await runProcess(
      process.execPath,
      [
        launcher,
        "--cwd",
        projectDirectory,
        "--agent-dir",
        agentDirectory,
        // The frontend fixture also contains a platform-specific native plugin
        // lock. Load only the exact JS source so this test is portable.
        "--no-approve",
        "--no-extensions",
        "--extension",
        extension,
        "--provider",
        "openai-compatible",
        "--model",
        "e2e-model",
        "--base-url",
        `http://127.0.0.1:${address.port}/v1`,
        "--json",
        "/frontend-napi-smoke",
      ],
      {
        cwd: workspaceDirectory,
        env: { ...process.env, OPENAI_API_KEY: "" },
        stdio: ["ignore", "pipe", "pipe"],
      },
    );
    assert.equal(result.code, 0, result.stderr);

    const events = result.stdout
      .trim()
      .split("\n")
      .filter(Boolean)
      .map((line) => parseJson(line, "product event"));
    assert.ok(
      events.some(isAgentStartEvent),
      `agent_start was not emitted:\n${result.stdout}`,
    );
    assert.equal(requests.length, 1);
    const providerRequest = requests[0];
    assert.ok(providerRequest);
    assert.equal(
      providerRequest.messages.at(-1)?.content,
      "Use frontend_project_checks to inspect this project's required verification workflow, then summarize it.",
    );
    assert.ok(
      providerRequest.tools.some(
        (tool) => tool.function.name === "frontend_project_checks",
      ),
    );
  } finally {
    await new Promise((resolveClose) => provider.close(resolveClose));
  }
});
