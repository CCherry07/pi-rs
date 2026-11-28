import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { access, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { createServer, type ServerResponse } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { StringDecoder } from "node:string_decoder";
import { fileURLToPath } from "node:url";
import test, { type TestContext } from "node:test";

const launcher = resolve(dirname(fileURLToPath(import.meta.url)), "../dist/bin/pi.js");
type RecordValue = Record<string, any>;
type FixtureTool = boolean | { name: string; input: RecordValue };

async function exists(path: string) {
  return access(path).then(() => true, () => false);
}

function stream(response: ServerResponse, tool: FixtureTool) {
  const call = typeof tool === "object" ? tool : { name: "write", input: { path: "approved.txt", content: "allowed" } };
  response.writeHead(200, { "content-type": "text/event-stream" });
  response.end(`data: ${JSON.stringify({
    id: "fixture-response", object: "chat.completion.chunk", created: 0, model: "gpt-4o-mini",
    choices: [{ index: 0, delta: tool ? {
      role: "assistant", tool_calls: [{ index: 0, id: "write-fixture", type: "function",
        function: { name: call.name, arguments: JSON.stringify(call.input) } }],
    } : { role: "assistant", content: "fixture finished" }, finish_reason: tool ? "tool_calls" : "stop" }],
    usage: { prompt_tokens: 8, completion_tokens: 4, total_tokens: 12 },
  })}\n\ndata: [DONE]\n\n`);
}

async function fixture(t: TestContext, tool: FixtureTool = false, respond = stream) {
  const root = await mkdtemp(join(tmpdir(), "pi-rpc-interop-"));
  const agent = join(root, "agent");
  await mkdir(agent);
  const requests: RecordValue[] = [];
  const children: Array<() => Promise<void>> = [];
  const server = createServer((request, response) => {
    let body = "";
    request.setEncoding("utf8");
    request.on("data", chunk => { body += chunk; });
    request.on("end", () => {
      requests.push(JSON.parse(body));
      respond(response, requests.length === 1 ? tool : false);
    });
  });
  await new Promise<void>((done, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", done);
  });
  t.after(async () => {
    await Promise.all(children.map(cleanup => cleanup()));
    server.closeAllConnections();
    await new Promise<void>(done => server.close(() => done()));
    await rm(root, { recursive: true, force: true });
  });
  const address = server.address();
  assert.ok(address && typeof address !== "string");
  const baseUrl = `http://127.0.0.1:${address.port}/v1`;
  const env: NodeJS.ProcessEnv = {
    HOME: root, USERPROFILE: root, XDG_CONFIG_HOME: join(root, "config"), PI_OFFLINE: "1",
    OPENAI_API_KEY: "fixture-only", OPENAI_BASE_URL: baseUrl,
  };
  for (const key of ["PATH", "SystemRoot", "WINDIR", "ComSpec", "PATHEXT", "TMP", "TEMP", "TMPDIR"]) {
    if (process.env[key] !== undefined) env[key] = process.env[key];
  }
  if (process.env.PI_RS_NATIVE_BINDING) env.PI_RS_NATIVE_BINDING = resolve(process.env.PI_RS_NATIVE_BINDING);

  function start(extra: string[] = [], extraEnv: NodeJS.ProcessEnv = {}) {
    const child = spawn(process.execPath, [launcher, "--mode", "rpc", "--no-extensions", "--no-approve",
      "--cwd", root, "--agent-dir", agent, "--provider", "openai-compatible", "--model", "gpt-4o-mini", ...extra],
    { cwd: root, env: { ...env, ...extraEnv }, stdio: "pipe" });
    const records: RecordValue[] = [];
    let errorOutput = "";
    let parseError: Error | undefined;
    let exited = false;
    let serial = 0;
    const waiters = new Set<() => void>();
    const wake = () => { for (const waiter of waiters) waiter(); };
    child.stderr.on("data", bytes => { errorOutput += bytes.toString(); });
    const decoder = new StringDecoder("utf8");
    let outputBuffer = "";
    child.stdout.on("data", bytes => {
      outputBuffer += decoder.write(bytes);
      for (let newline = outputBuffer.indexOf("\n"); newline !== -1; newline = outputBuffer.indexOf("\n")) {
        const line = outputBuffer.slice(0, newline);
        outputBuffer = outputBuffer.slice(newline + 1);
        try { records.push(JSON.parse(line)); } catch { parseError = new Error(`Non-JSON RPC output: ${line}`); }
      }
      wake();
    });
    child.on("error", error => { parseError = error; wake(); });
    const closed = new Promise<number | null>(done => child.on("close", code => {
      exited = true; wake(); done(code);
    }));
    children.push(async () => {
      if (!exited) child.kill("SIGKILL");
      await closed;
    });
    function send(value: RecordValue) { child.stdin.write(JSON.stringify(value) + "\n"); }
    async function wait(predicate: (record: RecordValue) => boolean, from = 0): Promise<RecordValue> {
      return new Promise((done, reject) => {
        const timer = setTimeout(() => finish(new Error(`RPC response timed out: ${errorOutput}`)), 10_000);
        const finish = (error?: Error, record?: RecordValue) => {
          clearTimeout(timer); waiters.delete(check);
          if (error) reject(error); else done(record!);
        };
        const check = () => {
          const record = records.slice(from).find(predicate);
          if (parseError) finish(parseError);
          else if (record) finish(undefined, record);
          else if (exited) finish(new Error(`RPC exited before response: ${errorOutput}`));
        };
        waiters.add(check); check();
      });
    }
    async function request(type: string, fields: RecordValue = {}) {
      const id = `test-${++serial}`;
      send({ type, id, ...fields });
      const answer = await wait(record => record.type === "response" && record.id === id)
        .catch(error => { throw new Error(`RPC ${type}: ${String(error)}`); });
      assert.equal(answer.success, true, JSON.stringify(answer));
      return answer.data;
    }
    async function waitForExit(expectedCode = 0) {
      assert.equal(await closed, expectedCode, errorOutput);
    }
    return { records, wait, request, send, waitForExit,
      diagnostics: () => errorOutput,
      sendRaw: (value: string) => child.stdin.write(value),
      signal: (signal: NodeJS.Signals) => child.kill(signal),
      endInput: (value?: string) => child.stdin.end(value),
      async close(batch: RecordValue[] = []) {
      child.stdin.end(batch.map(value => JSON.stringify(value) + "\n").join(""));
      await waitForExit();
    } };
  }
  return { root, agent, baseUrl, requests, start };
}

test("RPC model discovery flushes batched query responses before stdin EOF", { timeout: 10_000 }, async t => {
  const f = await fixture(t);
  const rpc = f.start(["--no-session"]);
  const batch = Array.from({ length: 5 }, (_, index) => [
    { id: `state-${index}`, type: "get_state" },
    { id: `models-${index}`, type: "get_available_models" },
  ]).flat();
  await rpc.close(batch);
  const responses = rpc.records.filter(record => record.type === "response");
  const receivedIds = responses.map(record => record.id);
  assert.equal(responses.length, batch.length, `Received response IDs: ${JSON.stringify(receivedIds)}`);
  for (const command of batch) {
    const response = responses.find(record => record.id === command.id);
    assert.ok(response, `Missing response for ${command.id}: ${JSON.stringify(receivedIds)}`);
    assert.equal(response.success, true, JSON.stringify(response));
    if (command.type === "get_state") {
      assert.equal(response.data.sessionFile, undefined);
    } else {
      assert.ok(response.data.models.some((model: RecordValue) => model.id === "gpt-4o-mini"),
        `Fixture model missing: ${JSON.stringify(response.data.models.map((model: RecordValue) => model.id))}`);
    }
  }
  assert.equal(f.requests.length, 0, "model discovery must not send a provider request");
  assert.ok(!(await readdir(f.agent, { recursive: true })).some(file => file.endsWith(".jsonl")),
    "model discovery must leave no resume entry");
});

test("RPC disable flags preserve explicit runtime MCP registration across Node/NAPI reload", { timeout: 15_000 }, async t => {
  const f = await fixture(t);
  const extension = join(f.root, "runtime-mcp.ts");
  await writeFile(extension, `export default function (pi) {
    pi.on("mcp_servers_change", (event, ctx) => {
      ctx.ui.notify("mcp servers: " + event.servers.map(server => server.name).join(","));
    });
    pi.registerCommand("runtime-mcp-add", {
      handler: () => pi.registerMcpServer("late", { command: "unused", enabled: false })
    });
    pi.registerCommand("runtime-mcp-remove", {
      handler: () => pi.unregisterMcpServer("late")
    });
  }`);
  const rpc = f.start(["--no-session", "--no-mcp", "--extension", extension]);

  async function waitForMcp(present: boolean) {
    for (let attempt = 0; attempt < 100; attempt++) {
      const commands = (await rpc.request("get_commands")).commands
        .map((command: RecordValue) => command.name);
      if (commands.includes("mcp") === present) return;
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    assert.fail(`MCP command did not become ${present ? "available" : "unavailable"}: ${rpc.diagnostics()}`);
  }

  assert.deepEqual(await rpc.request("prompt", { message: "/runtime-mcp-add" }), { disposition: "handled" });
  await rpc.wait(record => record.method === "notify" && record.message === "mcp servers: late");
  await waitForMcp(true);
  assert.deepEqual(await rpc.request("prompt", { message: "/runtime-mcp-remove" }), { disposition: "handled" });
  await rpc.wait(record => record.method === "notify" && record.message === "mcp servers: ");
  await waitForMcp(false);
  await rpc.close();
});

test("RPC EOF aborts an active replacement-generation MCP observer", { timeout: 15_000 }, async t => {
  const f = await fixture(t);
  const extension = join(f.root, "runtime-mcp-close.ts");
  await writeFile(extension, `export default function (pi) {
    pi.on("mcp_servers_change", async (_event, ctx) => {
      ctx.ui.notify("blocking MCP observer started");
      await new Promise(resolve => {
        if (ctx.signal.aborted) resolve();
        else ctx.signal.addEventListener("abort", resolve, { once: true });
      });
      try { ctx.ui.notify("blocking MCP observer escaped retirement"); } catch {}
    });
    pi.registerCommand("runtime-mcp-block", {
      handler: () => pi.registerMcpServer("late", { command: "unused", enabled: false })
    });
  }`);
  const rpc = f.start(["--no-session", "--extension", extension]);

  assert.deepEqual(await rpc.request("prompt", { message: "/runtime-mcp-block" }), { disposition: "handled" });
  await rpc.wait(record => record.method === "notify" && record.message === "blocking MCP observer started");
  await rpc.close();
  assert.equal(
    rpc.records.some(record => record.message === "blocking MCP observer escaped retirement"),
    false,
  );
});

test("RPC new_session waits for an active replacement MCP observer then publishes a fresh scope", { timeout: 15_000 }, async t => {
  const f = await fixture(t);
  const extension = join(f.root, "runtime-mcp-new-session.ts");
  await writeFile(extension, `export default function (pi) {
    pi.on("mcp_servers_change", async (_event, ctx) => {
      ctx.ui.notify("replacement MCP observer started");
      await ctx.ui.confirm("replacement MCP observer", "Allow replacement to finish?");
      ctx.ui.notify("replacement MCP observer released");
    });
    pi.registerCommand("runtime-mcp-interleave", {
      handler: () => pi.registerMcpServer("late", { command: "unused", enabled: false })
    });
  }`);
  const rpc = f.start(["--no-session", "--extension", extension]);

  rpc.send({ id: "mcp-reload", type: "prompt", message: "/runtime-mcp-interleave" });
  const question = await rpc.wait(record => record.method === "confirm"
    && record.title === "replacement MCP observer");
  rpc.send({ id: "fresh-session", type: "new_session" });
  await new Promise(resolve => setTimeout(resolve, 100));
  assert.equal(rpc.records.some(record => record.type === "response" && record.id === "fresh-session"), false,
    "new_session must wait behind the replacement lifecycle gate");

  rpc.send({ type: "extension_ui_response", id: question.id, confirmed: true });
  const released = await rpc.wait(record => record.method === "notify"
    && record.message === "replacement MCP observer released");
  const reload = await rpc.wait(record => record.type === "response" && record.id === "mcp-reload");
  const fresh = await rpc.wait(record => record.type === "response" && record.id === "fresh-session");
  assert.equal(reload.success, true, JSON.stringify(reload));
  assert.equal(fresh.success, true, JSON.stringify(fresh));
  assert.ok(rpc.records.indexOf(released) < rpc.records.indexOf(fresh),
    "the queued replacement must publish only after the active observer settles");
  const commands = (await rpc.request("get_commands")).commands.map((command: RecordValue) => command.name);
  assert.equal(commands.includes("mcp"), false,
    "a fresh session scope must not inherit the previous session's runtime MCP overlay");
  await rpc.close();
});

test("RPC snapshot queries survive stdin EOF while an extension command is pending", { timeout: 10_000 }, async t => {
  const f = await fixture(t);
  const extension = join(f.root, "pending-command.ts");
  await writeFile(extension, `export default function (pi) {
    pi.registerCommand("rpc-pending", { handler: async (_args, ctx) => {
      ctx.ui.notify("pending command started");
      await new Promise(() => {});
    } });
  }`);
  const rpc = f.start(["--no-session", "--extension", extension]);
  rpc.send({ id: "pending", type: "prompt", message: "/rpc-pending" });
  await rpc.wait(record => record.method === "notify" && record.message === "pending command started");
  const batch = [
    { id: "state", type: "get_state" },
    { id: "models", type: "get_available_models" },
  ];
  await rpc.close(batch);
  const responses = rpc.records.filter(record => record.type === "response");
  for (const command of batch) {
    const response = responses.find(record => record.id === command.id);
    assert.ok(response, `Missing response for ${command.id}: ${JSON.stringify(responses.map(record => record.id))}`);
    assert.equal(response.success, true, JSON.stringify(response));
  }
  assert.equal(f.requests.length, 0, "the pending extension command must not start a model request");
  assert.ok(!(await readdir(f.agent, { recursive: true })).some(file => file.endsWith(".jsonl")),
    "a disconnected in-memory session must leave no resume entry");
});

test("RPC large command catalogs preserve complete output and subsequent streamed replies", { timeout: 30_000 }, async t => {
  const f = await fixture(t);
  const extension = join(f.root, "large-catalog.ts");
  const count = 1500;
  await writeFile(extension, `export default function (pi) {
    const description = "Deterministic catalog fixture. ".repeat(30);
    for (let index = 0; index < ${count}; index++) {
      pi.registerCommand("catalog-" + index, { description, handler: async () => {} });
    }
  }`);
  const rpc = f.start(["--extension", extension]);
  const state = await rpc.request("get_state");
  const catalog = await rpc.request("get_commands");
  assert.equal(catalog.commands.filter((command: RecordValue) => command.name.startsWith("catalog-")).length, count);
  assert.ok(Buffer.byteLength(JSON.stringify(catalog)) > 1024 * 1024, "the catalog must exceed ordinary pipe buffers");

  const start = rpc.records.length;
  assert.equal((await rpc.request("prompt", { message: "hi" })).disposition, "started");
  await rpc.wait(record => record.type === "agent_settled", start);
  const events = rpc.records.slice(start);
  const deltas = events.filter(record => record.type === "message_update" && record.assistantMessageEvent?.type === "text_delta");
  assert.equal(deltas.map(record => record.assistantMessageEvent.delta).join(""), "fixture finished");
  assert.ok(events.some(record => record.type === "message_update" && record.assistantMessageEvent?.type === "text_end"));
  assert.ok(events.some(record => record.type === "message_end" && record.message?.role === "assistant"));
  assert.ok(events.some(record => record.type === "agent_end"));
  assert.equal((await rpc.request("get_last_assistant_text")).text, "fixture finished");
  const journal = (await readFile(state.sessionFile, "utf8")).trim().split("\n").map(line => JSON.parse(line));
  const assistant = journal.find(entry => entry.type === "message" && entry.message?.role === "assistant");
  assert.ok(assistant?.message.content.some((block: RecordValue) => block.type === "text" && block.text === "fixture finished"));
  assert.equal(f.requests.length, 1, "catalog discovery must not make a model request");
  await rpc.close();
});

test("RPC --skill loads repeated directory and file paths and expands skill commands", { timeout: 40_000 }, async t => {
  const f = await fixture(t);
  const directory = join(f.root, "injected skills", "rpc-directory");
  const standalone = join(f.root, "rpc-file.md");
  await mkdir(directory, { recursive: true });
  await writeFile(join(directory, "SKILL.md"), "---\nname: rpc-directory\ndescription: Injected directory skill\n---\nFollow the injected directory instructions.\n");
  await writeFile(standalone, "---\nname: rpc-file\ndescription: Injected file skill\n---\nFollow the injected standalone instructions.\n");
  for (const [path, name] of [
    [join(f.agent, "skills", "rpc-default", "SKILL.md"), "rpc-default"],
    [join(f.agent, "configured", "SKILL.md"), "rpc-configured"],
  ] as const) {
    await mkdir(dirname(path), { recursive: true });
    await writeFile(path, `---\nname: ${name}\ndescription: ${name} fixture\n---\nFixture instructions.\n`);
  }
  await writeFile(join(f.agent, "settings.json"), JSON.stringify({ skills: ["configured"] }));
  const rpc = f.start(["--no-session", "--skill", "injected skills", `--skill=${standalone}`]);
  const commands = (await rpc.request("get_commands")).commands.map((command: RecordValue) => command.name);
  for (const name of ["rpc-directory", "rpc-file", "rpc-default", "rpc-configured"]) {
    assert.ok(commands.includes(`skill:${name}`), JSON.stringify(commands));
  }
  await rpc.request("prompt", { message: "/skill:rpc-directory Check the fixture" });
  await rpc.wait(record => record.type === "agent_settled");
  assert.equal(f.requests.length, 1);
  const request = f.requests[0];
  assert.ok(request);
  const userMessage = request.messages.find((message: RecordValue) => message.role === "user");
  assert.ok(userMessage, JSON.stringify(request.messages));
  assert.match(JSON.stringify(userMessage.content), /Follow the injected directory instructions\./);
  assert.match(JSON.stringify(userMessage.content), /Check the fixture/);
  await rpc.request("new_session");
  const replacement = (await rpc.request("get_commands")).commands.map((command: RecordValue) => command.name);
  assert.ok(replacement.includes("skill:rpc-directory"), JSON.stringify(replacement));
  assert.ok(replacement.includes("skill:rpc-file"), JSON.stringify(replacement));
  await rpc.close();
});

test("RPC loads and executes the upstream Sikemux MCP extension", {
  timeout: 40_000, skip: !process.env.PI_RS_SIKEMUX_EXTENSION_PATH,
}, async t => {
  const f = await fixture(t, { name: "mcp__sikemux-tools__fixture_echo", input: { text: "MCP fixture result" } });
  const mcp = join(f.root, "mcp.mjs");
  await writeFile(mcp, `import { createInterface } from "node:readline";
    createInterface({ input: process.stdin }).on("line", line => {
      const request = JSON.parse(line);
      if (request.id === undefined) return;
      let result;
      if (request.method === "initialize") result = { protocolVersion: "2025-03-26", capabilities: { tools: {} }, serverInfo: { name: "fixture", version: "1" } };
      else if (request.method === "tools/list") result = { tools: [{ name: "fixture_echo", description: "Echo fixture text.", inputSchema: { type: "object", properties: { text: { type: "string" } }, required: ["text"] } }] };
      else if (request.method === "tools/call") result = { content: [{ type: "text", text: request.params.arguments.text }] };
      process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: request.id, result }) + "\\n");
    });`);
  const rpc = f.start(["--extension", resolve(process.env.PI_RS_SIKEMUX_EXTENSION_PATH!)], {
    SIKEMUX_PI_PERMISSIONS: "normal",
    SIKEMUX_TOOLS_MCP_COMMAND: process.execPath,
    SIKEMUX_TOOLS_MCP_ARGS: JSON.stringify([mcp]),
  });
  await rpc.request("prompt", { message: "use the MCP fixture tool" });
  await rpc.wait(record => record.type === "agent_settled");
  const result = rpc.records.find(record => record.type === "tool_execution_end");
  assert.equal(result?.isError, false, JSON.stringify(result));
  assert.ok(result?.result?.content.some((block: RecordValue) => block.text === "MCP fixture result"), JSON.stringify(result));
  assert.ok(!rpc.records.some(record => record.type === "extension_ui_request" && record.method === "confirm"),
    "Sikemux MCP tools use their own authorization rather than Pi's ordinary tool prompt");
  await rpc.close();
});

test("RPC exposes a stable planned session path before saving and resumes it after a response", { timeout: 40_000 }, async t => {
  const f = await fixture(t);
  const rpc = f.start();
  const before = await rpc.request("get_state");
  assert.equal(typeof before.sessionFile, "string", "desktop clients need the intended path before the first prompt");
  assert.equal(await exists(before.sessionFile), false, "reporting the path must not materialize history");
  assert.equal((await rpc.request("prompt", { message: "remember this fixture" })).disposition, "started");
  await rpc.wait(record => record.type === "agent_settled");
  const saved = await rpc.request("get_state");
  assert.equal(saved.sessionFile, before.sessionFile);
  assert.equal(await exists(saved.sessionFile), true);
  await rpc.close();
  const resumed = f.start(["--session", saved.sessionFile]);
  assert.equal((await resumed.request("get_state")).sessionId, before.sessionId);
  const history = await resumed.request("get_entries");
  assert.ok(history.entries.some((entry: RecordValue) => entry.message?.role === "user"));
  assert.ok(history.entries.some((entry: RecordValue) => entry.message?.role === "assistant"));
  await resumed.close();
});

test("RPC --no-session stays ephemeral after a response and new_session", { timeout: 40_000 }, async t => {
  const f = await fixture(t);
  const existing = join(f.root, "existing.jsonl");
  const original = "existing history must not be opened or modified\n";
  await writeFile(existing, original);
  const rpc = f.start(["--no-session", "--session", existing]);
  assert.equal((await rpc.request("get_state")).sessionFile, undefined);
  assert.ok((await rpc.request("get_available_models")).models.length > 0);
  await rpc.request("prompt", { message: "ephemeral fixture" });
  await rpc.wait(record => record.type === "agent_settled");
  assert.equal((await rpc.request("get_state")).sessionFile, undefined);
  await rpc.request("new_session");
  assert.equal((await rpc.request("get_state")).sessionFile, undefined);
  await rpc.close();
  const files = await readdir(f.agent, { recursive: true });
  assert.ok(!files.some(file => file.endsWith(".jsonl")), "ephemeral runs must leave no resume entry");
  assert.equal(await readFile(existing, "utf8"), original);
});

test("RPC prompt acknowledges handled commands and reports preflight errors", { timeout: 30_000 }, async t => {
  const f = await fixture(t);
  const extension = join(f.root, "commands.ts");
  await writeFile(extension, `export default function (pi) {
    pi.registerCommand("rpc-handled", { handler: async (_args, ctx) => { ctx.ui.notify("fixture notification", "warning"); } });
    pi.registerCommand("rpc-failed", { handler: async () => { throw new Error("fixture preflight failure"); } });
  }`);
  const rpc = f.start(["--extension", extension]);
  const answer = await rpc.request("prompt", { message: "/rpc-handled" });
  assert.equal(answer?.disposition, "handled", "handled commands do not emit agent_settled");
  const notice = await rpc.wait(record => record.type === "extension_ui_request" && record.method === "notify");
  assert.equal(notice.message, "fixture notification");
  assert.equal(notice.notifyType, "warning");
  rpc.send({ type: "prompt", id: "failing-preflight", message: "/rpc-failed" });
  const failed = await rpc.wait(record => record.type === "response" && record.id === "failing-preflight");
  assert.equal(failed.success, false);
  assert.match(failed.error, /fixture preflight failure/);
  assert.equal(f.requests.length, 0);
  await rpc.close();
});

test("RPC answers startup confirmation while ordinary commands wait for activation", { timeout: 30_000 }, async t => {
  const f = await fixture(t);
  const extension = join(f.root, "startup.ts");
  await writeFile(extension, `export default function (pi) {
    pi.on("session_start", async (_event, ctx) => {
      if (!ctx.hasUI) throw new Error("startup confirmation requires UI");
      await ctx.ui.confirm("startup fixture", "Continue startup?");
    });
  }`);
  const rpc = f.start(["--extension", extension]);
  rpc.send({ type: "get_state", id: "queued-before-startup" });
  const question = await rpc.wait(record => record.type === "extension_ui_request" && record.method === "confirm");
  assert.equal(question.title, "startup fixture");
  assert.ok(!rpc.records.some(record => record.id === "queued-before-startup"));
  rpc.send({ type: "extension_ui_response", id: question.id, confirmed: true });
  const state = await rpc.wait(record => record.type === "response" && record.id === "queued-before-startup");
  assert.equal(state.success, true);
  await rpc.close();
});

test("RPC startup notifications and text dialogs arrive before session activation", { timeout: 30_000 }, async t => {
  const f = await fixture(t);
  const extension = join(f.root, "startup-dialogs.ts");
  await writeFile(extension, `export default function (pi) {
    pi.on("session_start", async (_event, ctx) => {
      ctx.ui.notify("before startup dialogs", "warning");
      const selected = await ctx.ui.select("startup selection", ["first", "second"], { timeout: 5000 });
      const input = await ctx.ui.input("startup input", "type here", { timeout: 5000 });
      const edited = await ctx.ui.editor("startup editor", "line one\\nline two");
      ctx.ui.notify(JSON.stringify({ selected, input, edited }));
    });
  }`);
  const rpc = f.start(["--extension", extension]);
  rpc.send({ type: "get_state", id: "startup-state" });
  const notice = await rpc.wait(record => record.method === "notify" && record.message === "before startup dialogs");
  assert.equal(notice.notifyType, "warning");
  const select = await rpc.wait(record => record.method === "select");
  assert.deepEqual(select.options, ["first", "second"]);
  assert.equal(select.timeout, 5000);
  assert.ok(rpc.records.indexOf(notice) < rpc.records.indexOf(select));
  assert.ok(!rpc.records.some(record => record.id === "startup-state"), "startup hooks must finish before commands run");
  rpc.send({ type: "extension_ui_response", id: "unknown-startup-dialog", value: "first" });
  rpc.send({ type: "extension_ui_response", id: select.id, value: "second" });
  const input = await rpc.wait(record => record.method === "input");
  assert.equal(input.placeholder, "type here");
  assert.equal(input.timeout, 5000);
  rpc.send({ type: "extension_ui_response", id: input.id, value: "" });
  const editor = await rpc.wait(record => record.method === "editor");
  assert.equal(editor.prefill, "line one\nline two");
  assert.equal(editor.timeout, undefined, "legacy editor requests have no timeout field");
  rpc.send({ type: "extension_ui_response", id: editor.id, value: "edited\n文本" });
  const result = await rpc.wait(record => record.method === "notify" && record.message.startsWith("{\"selected\""));
  assert.deepEqual(JSON.parse(result.message), { selected: "second", input: "", edited: "edited\n文本" });
  assert.equal((await rpc.wait(record => record.type === "response" && record.id === "startup-state")).success, true);
  await rpc.close();
  assert.equal(rpc.records.filter(record => record.method === "notify" && record.message === "before startup dialogs").length, 1);
  assert.equal(rpc.records.filter(record => record.method === "notify" && record.message === result.message).length, 1);
});

async function dialogFixture(t: TestContext) {
  const f = await fixture(t);
  const extension = join(f.root, "dialogs.ts");
  await writeFile(extension, `export default function (pi) {
    pi.registerCommand("rpc-dialog", { handler: async (args, ctx) => {
      const [method, scenario] = args.split(" ");
      const opts = { timeout: scenario === "timeout" ? 500 : 5000 };
      let value;
      if (method === "select") value = await ctx.ui.select(method + "/" + scenario, ["one", "two"], opts);
      else if (method === "input") value = await ctx.ui.input(method + "/" + scenario, "placeholder", opts);
      else if (method === "editor") value = await ctx.ui.editor(method + "/" + scenario, "prefill");
      else value = await ctx.ui.confirm(method + "/" + scenario, "confirm message", opts);
      ctx.ui.notify(JSON.stringify({ method, scenario, value: value ?? null }));
    } });
  }`);
  const rpc = f.start(["--extension", extension]);
  await rpc.request("get_state");
  return rpc;
}

test("RPC text dialog dismissal and abort preserve correlation for subsequent questions", { timeout: 40_000 }, async t => {
  const rpc = await dialogFixture(t);
  let oldId: string | undefined;
  for (const method of ["select", "input", "editor"] as const) {
    for (const scenario of ["dismiss", "abort"] as const) {
      const id = `${method}-${scenario}`;
      const from = rpc.records.length;
      rpc.send({ type: "prompt", id, message: `/rpc-dialog ${method} ${scenario}` });
      const question = await rpc.wait(record => record.method === method && record.title === `${method}/${scenario}`, from);
      if (oldId) rpc.send({ type: "extension_ui_response", id: oldId, value: "late answer" });
      rpc.send({ type: "extension_ui_response", id: "unknown-dialog", value: "unrelated" });
      await rpc.request("get_state");
      assert.ok(!rpc.records.some(record => record.type === "response" && record.id === id), "old and unknown responses cannot answer the active dialog");
      if (scenario === "abort") await rpc.request("abort");
      else rpc.send({ type: "extension_ui_response", id: question.id, cancelled: true, value: "ignored" });
      const notice = await rpc.wait(record => record.method === "notify" && record.message.includes(`\"scenario\":\"${scenario}\"`), from);
      assert.deepEqual(JSON.parse(notice.message), { method, scenario, value: null });
      const completed = await rpc.wait(record => record.type === "response" && record.id === id, from);
      assert.equal(completed.success, true, JSON.stringify(completed));
      oldId = question.id;
    }
  }
  await rpc.close();
});

test("RPC select, input, and confirm expose deadlines and resolve timed-out dialogs", { timeout: 30_000 }, async t => {
  const rpc = await dialogFixture(t);
  for (const method of ["select", "input", "confirm"] as const) {
    const id = `timeout-${method}`;
    const from = rpc.records.length;
    rpc.send({ type: "prompt", id, message: `/rpc-dialog ${method} timeout` });
    const question = await rpc.wait(record => record.method === method && record.title === `${method}/timeout`, from);
    assert.equal(question.timeout, 500, "desktop clients need the same deadline as Pi's dialog promise");
    const notice = await rpc.wait(record => record.method === "notify" && record.message.startsWith(`{\"method\":\"${method}\",\"scenario\":\"timeout\"`), from);
    assert.deepEqual(JSON.parse(notice.message), { method, scenario: "timeout", value: method === "confirm" ? false : null });
    const completed = await rpc.wait(record => record.type === "response" && record.id === id, from);
    assert.equal(completed.success, true, JSON.stringify(completed));
    rpc.send({ type: "extension_ui_response", id: question.id, value: "late", confirmed: true });
  }
  await rpc.request("get_state");
  await rpc.close();
});

for (const method of ["select", "input", "editor"] as const) {
  test(`RPC startup ${method} releases its waiter on EOF`, { timeout: 30_000 }, async t => {
    const f = await fixture(t);
    const extension = join(f.root, "startup-disconnect.ts");
    const resultPath = join(f.root, "dialog-result.json");
    await writeFile(extension, `import { writeFile } from "node:fs/promises";
      export default function (pi) {
        pi.on("session_start", async (_event, ctx) => {
          const value = await ctx.ui.${method}("disconnect dialog", ${method === "select" ? '["one", "two"]' : '"initial text"'});
          await writeFile(${JSON.stringify(resultPath)}, JSON.stringify({ value: value ?? null }));
        });
      }`);
    const rpc = f.start(["--extension", extension]);
    await rpc.wait(record => record.method === method);
    await rpc.close();
    assert.deepEqual(JSON.parse(await readFile(resultPath, "utf8")), { value: null });
  });
}

test("RPC notices arrive exactly once across startup, reload, new_session, and shutdown", { timeout: 30_000 }, async t => {
  const f = await fixture(t);
  const extension = join(f.root, "lifecycle-notices.ts");
  await writeFile(extension, `export default function (pi) {
    pi.on("session_start", (event, ctx) => ctx.ui.notify("lifecycle start: " + event.reason));
    pi.on("session_shutdown", (event, ctx) => ctx.ui.notify("lifecycle shutdown: " + event.reason));
    pi.registerCommand("rpc-reload", { handler: async (_args, ctx) => {
      ctx.ui.notify("before reload", "warning");
      await ctx.reload();
      ctx.ui.notify("after reload", "warning");
      const answer = await ctx.ui.input("after reload input", "fresh generation");
      ctx.ui.notify("reloaded answer: " + answer);
    } });
    pi.registerCommand("rpc-notice", { handler: async (_args, ctx) => ctx.ui.notify("ordinary notice", "error") });
  }`);
  const rpc = f.start(["--extension", extension]);
  await rpc.request("get_state");
  await rpc.wait(record => record.method === "notify" && record.message === "lifecycle start: startup");
  rpc.send({ type: "prompt", id: "reload-command", message: "/rpc-reload" });
  const input = await rpc.wait(record => record.method === "input" && record.title === "after reload input");
  assert.equal(input.placeholder, "fresh generation");
  rpc.send({ type: "extension_ui_response", id: input.id, value: "ready" });
  assert.equal((await rpc.wait(record => record.type === "response" && record.id === "reload-command")).success, true);
  await rpc.request("new_session");
  await rpc.request("prompt", { message: "/rpc-notice" });
  await rpc.close();
  const notices = rpc.records.filter(record => record.method === "notify");
  assert.deepEqual(notices.filter(record => record.message.startsWith("lifecycle ")).map(record => record.message), [
    "lifecycle start: startup",
    "lifecycle shutdown: reload",
    "lifecycle start: reload",
    "lifecycle shutdown: new",
    "lifecycle start: new",
    "lifecycle shutdown: quit",
  ]);
  for (const message of ["before reload", "after reload", "reloaded answer: ready", "ordinary notice"]) {
    assert.equal(notices.filter(record => record.message === message).length, 1, message);
  }
  assert.equal(notices.find(record => record.message === "ordinary notice")?.notifyType, "error");
});

test("RPC extension-requested shutdown flushes the last lifecycle notification", { timeout: 30_000 }, async t => {
  const f = await fixture(t);
  const extension = join(f.root, "requested-shutdown.ts");
  await writeFile(extension, `export default function (pi) {
    pi.on("session_shutdown", (event, ctx) => ctx.ui.notify("last shutdown notice: " + event.reason, "warning"));
    pi.registerCommand("rpc-shutdown", { handler: async (_args, ctx) => {
      ctx.ui.notify("shutdown requested");
      ctx.shutdown();
      await new Promise(resolve => setTimeout(resolve, 30));
      ctx.ui.notify("shutdown command finished");
    } });
  }`);
  const rpc = f.start(["--extension", extension]);
  await rpc.request("get_state");
  rpc.send({ type: "prompt", id: "shutdown-command", message: "/rpc-shutdown" });
  await rpc.waitForExit();
  const responseIndex = rpc.records.findIndex(record => record.type === "response" && record.id === "shutdown-command");
  assert.ok(responseIndex >= 0, "the requesting command must receive its response before exit");
  assert.deepEqual(rpc.records[responseIndex]!.data, { disposition: "handled" });
  assert.ok(rpc.records.findIndex(record => record.message === "shutdown command finished") < responseIndex);
  assert.ok(responseIndex < rpc.records.findIndex(record => record.message === "last shutdown notice: quit"));
  const notices = rpc.records.filter(record => record.method === "notify"
    && ["shutdown requested", "last shutdown notice: quit"].includes(record.message));
  assert.deepEqual(notices.map(record => record.message), ["shutdown requested", "last shutdown notice: quit"]);
  assert.equal(notices.at(-1)?.notifyType, "warning");
});

test("RPC forwards extension display notifications and keeps extension logs on stderr", { timeout: 30_000 }, async t => {
  const f = await fixture(t);
  const extension = join(f.root, "display-notifications.ts");
  await writeFile(extension, `export default function (pi) {
    pi.on("session_start", (_event, ctx) => {
      console.log("extension console fixture");
      process.stdout.write("extension stdout fixture\\n");
      ctx.ui.setStatus("build", "running");
      ctx.ui.setStatus("build", "");
      ctx.ui.setStatus("build", undefined);
      ctx.ui.setWidget("progress", ["line one", "文本"], { placement: "belowEditor" });
      ctx.ui.setWidget("progress", undefined);
      ctx.ui.setTitle("RPC fixture");
      ctx.ui.setEditorText("first");
      ctx.ui.pasteToEditor("replacement");
      ctx.ui.notify("default notification");
    });
  }`);
  const rpc = f.start(["--extension", extension]);
  await rpc.request("get_state");
  const records = rpc.records.filter(record => record.type === "extension_ui_request")
    .map(({ id: _id, type: _type, ...record }) => record);
  assert.deepEqual(records, [
    { method: "setStatus", statusKey: "build", statusText: "running" },
    { method: "setStatus", statusKey: "build", statusText: "" },
    { method: "setStatus", statusKey: "build" },
    { method: "setWidget", widgetKey: "progress", widgetLines: ["line one", "文本"], widgetPlacement: "belowEditor" },
    { method: "setWidget", widgetKey: "progress" },
    { method: "setTitle", title: "RPC fixture" },
    { method: "set_editor_text", text: "first" },
    { method: "set_editor_text", text: "replacement" },
    { method: "notify", message: "default notification" },
  ]);
  await rpc.close();
  assert.match(rpc.diagnostics(), /extension console fixture/);
  assert.match(rpc.diagnostics(), /extension stdout fixture/);
});

test("RPC input hooks and extension commands run before streaming admission checks", { timeout: 40_000 }, async t => {
  const f = await fixture(t, true);
  const extension = join(f.root, "streaming-input.ts");
  await writeFile(extension, `export default function (pi) {
    pi.on("input", (event, ctx) => {
      ctx.ui.notify(JSON.stringify({ text: event.text, source: event.source }));
      if (event.text === "handled input") return { action: "handled" };
    });
    pi.registerCommand("during-run", { handler: (_args, ctx) => ctx.ui.notify("command handled") });
    pi.on("tool_call", async (_event, ctx) => {
      await ctx.ui.confirm("hold tool", "Continue?", { signal: ctx.signal });
      return { block: true, reason: "fixture only" };
    });
    pi.on("agent_settled", (event, ctx) => ctx.ui.notify(JSON.stringify({ settled: true, aborted: event.aborted })));
  }`);
  const rpc = f.start(["--extension", extension]);
  assert.deepEqual(await rpc.request("prompt", { message: "start fixture" }), { disposition: "started" });
  await rpc.wait(record => record.method === "confirm" && record.title === "hold tool");
  assert.deepEqual(await rpc.request("prompt", { message: "/during-run" }), { disposition: "handled" });
  assert.deepEqual(await rpc.request("prompt", { message: "handled input" }), { disposition: "handled" });
  assert.deepEqual(await rpc.request("steer", { message: "steering fixture" }), { disposition: "queued" });
  assert.deepEqual(await rpc.request("follow_up", { message: "follow-up fixture" }), { disposition: "queued" });
  assert.deepEqual(await rpc.request("clear_queue"), { steering: ["steering fixture"], followUp: ["follow-up fixture"] });
  await rpc.request("abort");
  assert.equal((await rpc.request("get_state")).isStreaming, false, "abort acknowledges an idle session");
  assert.equal((await rpc.wait(record => record.type === "agent_settled")).aborted, true);
  assert.deepEqual(JSON.parse((await rpc.wait(record => record.method === "notify" && record.message.includes('"settled"'))).message),
    { settled: true, aborted: true });
  const inputs = rpc.records.filter(record => record.method === "notify" && record.message.startsWith('{"text"'))
    .map(record => JSON.parse(record.message));
  assert.deepEqual(inputs.map(record => record.text), ["start fixture", "handled input", "steering fixture", "follow-up fixture"]);
  assert.ok(inputs.every(record => record.source === "rpc"));
  await rpc.close();
});

test("RPC queues idle user input and rejects extension commands in explicit queues", { timeout: 30_000 }, async t => {
  const f = await fixture(t);
  const extension = join(f.root, "idle-queue.ts");
  await writeFile(extension, `export default function (pi) {
    pi.registerCommand("queue-command", { handler: (_args, ctx) => ctx.ui.notify("must not execute") });
  }`);
  const rpc = f.start(["--no-session", "--extension", extension]);
  assert.deepEqual(await rpc.request("steer", { message: "next steering" }), { disposition: "queued" });
  assert.deepEqual(await rpc.request("follow_up", { message: "next follow-up" }), { disposition: "queued" });
  assert.equal((await rpc.request("get_state")).pendingMessageCount, 2);
  rpc.send({ id: "queued-command", type: "steer", message: "/queue-command" });
  assert.equal((await rpc.wait(record => record.id === "queued-command")).success, false);
  assert.deepEqual(await rpc.request("clear_queue"), { steering: ["next steering"], followUp: ["next follow-up"] });
  assert.equal(f.requests.length, 0);
  assert.ok(!rpc.records.some(record => record.message === "must not execute"));
  await rpc.close();
});

for (const signal of ["SIGTERM", "SIGHUP"] as const) {
  test(`RPC ${signal} waits for shutdown hooks`, { timeout: 30_000, skip: process.platform === "win32" }, async t => {
    const f = await fixture(t);
    const marker = join(f.root, "signal-cleanup.txt");
    const extension = join(f.root, "signal-cleanup.ts");
    await writeFile(extension, `import { writeFile } from "node:fs/promises";
      export default function (pi) {
        pi.on("session_shutdown", async () => {
          await new Promise(resolve => setTimeout(resolve, 50));
          await writeFile(${JSON.stringify(marker)}, "finished");
        });
      }`);
    const rpc = f.start(["--extension", extension]);
    await rpc.request("get_state");
    rpc.signal(signal);
    await rpc.waitForExit(signal === "SIGTERM" ? 143 : 129);
    assert.equal(await readFile(marker, "utf8"), "finished");
  });
}

test("RPC EOF and one following SIGTERM finish the same shutdown", { timeout: 30_000, skip: process.platform === "win32" }, async t => {
  const f = await fixture(t);
  const marker = join(f.root, "eof-cleanup.txt");
  const extension = join(f.root, "eof-cleanup.ts");
  await writeFile(extension, `import { writeFile } from "node:fs/promises";
    export default function (pi) {
      pi.on("session_shutdown", async (_event, ctx) => {
        ctx.ui.notify("disposal started");
        await new Promise(resolve => setTimeout(resolve, 150));
        await writeFile(${JSON.stringify(marker)}, "finished");
      });
    }`);
  const rpc = f.start(["--extension", extension]);
  await rpc.request("get_state");
  rpc.endInput();
  await rpc.wait(record => record.message === "disposal started");
  rpc.signal("SIGTERM");
  await rpc.waitForExit();
  assert.equal(await readFile(marker, "utf8"), "finished");
});

test("RPC extension shutdown lets its active prompt settle", { timeout: 30_000 }, async t => {
  const f = await fixture(t);
  const extension = join(f.root, "settled-shutdown.ts");
  await writeFile(extension, `export default function (pi) {
    pi.on("before_agent_start", (_event, ctx) => { ctx.shutdown(); });
  }`);
  const rpc = f.start(["--extension", extension]);
  rpc.send({ type: "prompt", id: "final-prompt", message: "finish before exiting" });
  await rpc.waitForExit();
  assert.deepEqual(rpc.records.find(record => record.id === "final-prompt")?.data, { disposition: "started" });
  const settled = rpc.records.find(record => record.type === "agent_settled");
  assert.equal(settled?.aborted, false);
  assert.ok(rpc.records.some(record => record.type === "message_end" && record.message?.role === "assistant"
    && record.message.content.some((block: RecordValue) => block.text === "fixture finished")));
});

test("RPC a repeated signal exits during pending disposal", { timeout: 30_000, skip: process.platform === "win32" }, async t => {
  const f = await fixture(t);
  const extension = join(f.root, "repeated-signal.ts");
  await writeFile(extension, `export default function (pi) {
    pi.on("session_shutdown", async (_event, ctx) => {
      ctx.ui.notify("pending disposal");
      await new Promise(() => {});
    });
  }`);
  const rpc = f.start(["--extension", extension]);
  await rpc.request("get_state");
  rpc.signal("SIGTERM");
  await rpc.wait(record => record.message === "pending disposal");
  rpc.signal("SIGTERM");
  await rpc.waitForExit(143);
});

test("RPC bash output only correlates supplied command IDs", { timeout: 30_000 }, async t => {
  const f = await fixture(t);
  const rpc = f.start(["--no-session"]);
  rpc.send({ type: "bash", command: "printf uncorrelated" });
  await rpc.wait(record => record.type === "response" && record.command === "bash");
  const update = await rpc.wait(record => record.type === "bash_execution_update");
  assert.equal(Object.hasOwn(update, "id"), false);
  const from = rpc.records.length;
  rpc.send({ id: "correlated-bash", type: "bash", command: "printf correlated" });
  await rpc.wait(record => record.type === "response" && record.id === "correlated-bash", from);
  assert.equal((await rpc.wait(record => record.type === "bash_execution_update", from)).id, "correlated-bash");
  await rpc.close();
});

test("RPC framing preserves Unicode separators, CRLF, large commands and a final unterminated record", { timeout: 30_000 }, async t => {
  const f = await fixture(t);
  const rpc = f.start(["--no-session"]);
  const name = "RPC\u2028line\u2029paragraph";
  rpc.sendRaw("invalid JSON\n");
  const parse = await rpc.wait(record => record.command === "parse");
  assert.equal(parse.success, false);
  assert.equal(Object.hasOwn(parse, "id"), false);
  rpc.send({ type: "unrecognized", id: "unknown-request" });
  assert.deepEqual(await rpc.wait(record => record.id === "unknown-request"), {
    type: "response", id: "unknown-request", command: "unrecognized", success: false,
    error: "Unknown command: unrecognized",
  });
  rpc.sendRaw(JSON.stringify({ id: "name", type: "set_session_name", name }) + "\r\n");
  assert.equal((await rpc.wait(record => record.id === "name")).success, true);
  assert.equal((await rpc.request("get_state")).sessionName, name);
  rpc.send({ id: "large", type: "get_state", padding: "x".repeat(17 * 1024 * 1024) });
  assert.equal((await rpc.wait(record => record.id === "large")).success, true);
  rpc.endInput(JSON.stringify({ id: "final-record", type: "get_state" }));
  await rpc.waitForExit();
  assert.equal(rpc.records.find(record => record.id === "final-record")?.success, true);
});

test("RPC rejects startup file arguments", { timeout: 30_000 }, async t => {
  const f = await fixture(t);
  const rpc = f.start(["@missing-input.txt"]);
  await rpc.waitForExit(1);
  assert.match(rpc.diagnostics(), /@file arguments are not supported in RPC mode/);
  assert.equal(f.requests.length, 0);
});

test("RPC JS stream hooks seed once and replay text and tool deltas exactly once", { timeout: 40_000 }, async t => {
  const f = await fixture(t, true, (response, tool) => {
    if (!tool) { stream(response, false); return; }
    response.writeHead(200, { "content-type": "text/event-stream" });
    const deltas = [
      { content: "first " }, { content: "second" },
      { tool_calls: [{ index: 0, id: "write-fixture", type: "function",
        function: { name: "write", arguments: '{"path":"approved.txt",' } }] },
      { tool_calls: [{ index: 0, function: { arguments: '"content":"allowed"}' } }] },
    ];
    const chunks = deltas.map(delta => ({ choices: [{ index: 0, delta, finish_reason: null }] }));
    response.end([...chunks, { choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }],
      usage: { prompt_tokens: 8, completion_tokens: 4, total_tokens: 12 } }]
      .map(chunk => `data: ${JSON.stringify(chunk)}\n\n`).join("") + "data: [DONE]\n\n");
  });
  const report = join(f.root, "stream-hooks.json");
  const extension = join(f.root, "stream-hooks.ts");
  await writeFile(extension, `import { writeFileSync } from "node:fs";
    export default function (pi) {
      const events = [];
      pi.on("message_update", event => { events.push(JSON.parse(JSON.stringify(event))); });
      pi.on("agent_end", () => { writeFileSync(${JSON.stringify(report)}, JSON.stringify(events)); });
    }`);
  const rpc = f.start(["--no-session", "--extension", extension]);
  await rpc.request("prompt", { message: "exercise incremental stream hooks" });
  await rpc.wait(record => record.type === "agent_settled");
  const events: RecordValue[] = JSON.parse(await readFile(report, "utf8"));
  const first = events.find(event => event.assistantMessageEvent?.type === "text_start");
  assert.ok(first, "real pi-js-plugin bridge delivered the initial seed");
  assert.equal(first.message.content[0].text, "");
  assert.deepEqual(events.filter(event => event.assistantMessageEvent?.type === "text_delta")
    .map(event => event.message.content[0].text), ["first ", "first second", "fixture finished"]);
  const call = events.find(event => event.assistantMessageEvent?.type === "toolcall_end");
  assert.ok(call, "real pi-js-plugin bridge delivered the assembled tool call");
  assert.deepEqual(call.assistantMessageEvent.toolCall.arguments, { path: "approved.txt", content: "allowed" });
  assert.deepEqual(call.message.content, [
    { type: "text", text: "first second" },
    { type: "toolCall", id: "write-fixture", name: "write", arguments: { path: "approved.txt", content: "allowed" } },
  ]);
  assert.equal(await readFile(join(f.root, "approved.txt"), "utf8"), "allowed");
  assert.equal(f.requests.length, 2);
  await rpc.close();
});

for (const decision of ["allow", "deny", "abort", "disconnect"] as const) {
  test(`RPC tool confirmation ${decision} crosses the JS/NAPI bridge`, { timeout: 40_000 }, async t => {
    const f = await fixture(t, true);
    if (decision === "abort") {
      // The model must be registered to test current catalog metadata, rather
      // than the intentionally unknown metadata of an unregistered fallback.
      await writeFile(join(f.agent, "models.json"), JSON.stringify({ providers: {
        "openai-compatible": {
          api: "openai-completions", baseUrl: f.baseUrl, apiKey: "fixture-only",
          models: [{ id: "gpt-4o-mini", name: "Fixture", reasoning: false, input: ["text"],
            contextWindow: 32000, maxTokens: 1024,
            cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }],
        },
      } }));
    }
    const extension = join(f.root, "permission.ts");
    await writeFile(extension, `export default function (pi) {
      pi.on("tool_call", async (event, ctx) => {
        if (!ctx.hasUI) throw new Error("RPC confirm must advertise UI availability");
        const allowed = await ctx.ui.confirm("sikemux-tool-permission", JSON.stringify({
          toolCallId: event.toolCallId, toolName: event.toolName, input: event.input,
        }), { signal: ctx.signal });
        if (!allowed) return { block: true, reason: "declined by fixture" };
      });
    }`);
    const upstreamExtension = process.env.PI_RS_SIKEMUX_EXTENSION_PATH;
    const rpc = f.start(["--extension", upstreamExtension ? resolve(upstreamExtension) : extension], {
      SIKEMUX_PI_PERMISSIONS: "normal",
    });
    await rpc.request("prompt", { message: "write the fixture" });
    const question = await rpc.wait(record => record.type === "extension_ui_request" && record.method === "confirm");
    assert.equal(question.title, "sikemux-tool-permission");
    assert.equal(JSON.parse(question.message).toolName, "write");
    assert.equal(await exists(join(f.root, "approved.txt")), false);
    rpc.send({ type: "extension_ui_response", id: "unrelated", confirmed: true });
    const activeState = await rpc.request("get_state");
    assert.equal(activeState.isStreaming, true, "RPC must remain responsive during confirmation");
    assert.equal(await exists(join(f.root, "approved.txt")), false, "unknown IDs cannot approve a tool");
    if (decision === "disconnect") {
      await rpc.close();
    } else {
      if (decision === "abort") {
        await rpc.request("abort");
        rpc.send({ type: "extension_ui_response", id: question.id, confirmed: true });
      } else {
        rpc.send({ type: "extension_ui_response", id: question.id, confirmed: decision === "allow" });
      }
      await rpc.wait(record => record.type === "agent_settled");
      if (decision === "abort") {
        const toolcallEnd = await rpc.wait(record => record.type === "message_update"
          && record.assistantMessageEvent?.type === "toolcall_end");
        const toolEnd = await rpc.wait(record => record.type === "tool_execution_end");
        const ended = await rpc.wait(record => record.type === "agent_end");
        const history = await rpc.request("get_messages");
        const journal = await rpc.request("get_entries");
        assert.deepEqual(toolcallEnd.assistantMessageEvent.toolCall, {
          type: "toolCall", id: "write-fixture", name: "write",
          arguments: { path: "approved.txt", content: "allowed" },
        }, "the real host serializes the tagged tool-call block");
        assert.equal(toolEnd.isError, true);
        assert.deepEqual(toolEnd.result.details, {}, "scheduler cancellation has explicit error details");
        assert.equal(activeState.model.api, "openai-completions");
        assert.equal(activeState.thinkingLevel, "off");
        const surfaces: Array<[string, RecordValue[]]> = [
          ["message_end", rpc.records.filter(record => record.type === "message_end").map(record => record.message)],
          ["agent_end", ended.messages],
          ["get_messages", history.messages],
          ["get_entries", journal.entries.filter((entry: RecordValue) => entry.type === "message")
            .map((entry: RecordValue) => entry.message)],
        ];
        for (const [surface, messages] of surfaces) {
          const results = messages.filter(message => message.role === "toolResult" && message.isError);
          assert.equal(results.length, 1, `${surface}: one cancelled tool result`);
          const result = results[0];
          assert.ok(result, `${surface}: cancelled tool result is present`);
          assert.deepEqual(result.details, {}, `${surface}: explicit scheduler error details`);
          assert.equal("durationMs" in result, false, `${surface}: unexecuted tools have no duration`);
          const aborted = messages.filter(message => message.role === "assistant" && message.stopReason === "aborted");
          assert.equal(aborted.length, 1, `${surface}: one terminal aborted assistant`);
          const message = aborted[0];
          assert.ok(message, `${surface}: terminal aborted assistant is present`);
          assert.deepEqual({
            api: message.api, provider: message.provider, model: message.model,
            thinkingLevel: message.thinkingLevel, errorMessage: message.errorMessage, content: message.content,
          }, {
            api: "openai-completions", provider: activeState.model.provider, model: activeState.model.id,
            thinkingLevel: "off", errorMessage: "This operation was aborted", content: [],
          }, `${surface}: current pre-request cancellation metadata`);
          assert.deepEqual(message.usage, {
            input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
            cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
          }, `${surface}: pre-request cancellation has no provider usage`);
          assert.equal(typeof message.durationMs, "number", `${surface}: response duration is present`);
          assert.ok(message.durationMs >= 0, `${surface}: response duration is nonnegative`);
        }
        assert.equal(f.requests.length, 1, "pre-request cancellation does not start another provider request");
      }
      await rpc.close();
    }
    assert.equal(await exists(join(f.root, "approved.txt")), decision === "allow");
    if (decision === "allow") assert.equal(await readFile(join(f.root, "approved.txt"), "utf8"), "allowed");
  });
}
