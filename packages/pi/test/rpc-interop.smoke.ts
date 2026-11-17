import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { access, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { createServer, type ServerResponse } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";
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

async function fixture(t: TestContext, tool: FixtureTool = false) {
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
      stream(response, requests.length === 1 ? tool : false);
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
  const env: NodeJS.ProcessEnv = {
    HOME: root, USERPROFILE: root, XDG_CONFIG_HOME: join(root, "config"), PI_OFFLINE: "1",
    OPENAI_API_KEY: "fixture-only", OPENAI_BASE_URL: `http://127.0.0.1:${address.port}/v1`,
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
    const lines = createInterface({ input: child.stdout });
    lines.on("line", line => {
      try { records.push(JSON.parse(line)); } catch { parseError = new Error(`Non-JSON RPC output: ${line}`); }
      wake();
    });
    child.on("error", error => { parseError = error; wake(); });
    const closed = new Promise<number | null>(done => child.on("close", code => {
      exited = true; wake(); done(code);
    }));
    children.push(async () => {
      if (!exited) child.kill("SIGKILL");
      await closed;
      lines.close();
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
    async function waitForExit() {
      assert.equal(await closed, 0, errorOutput);
    }
    return { records, wait, request, send, waitForExit, async close(batch: RecordValue[] = []) {
      child.stdin.end(batch.map(value => JSON.stringify(value) + "\n").join(""));
      await waitForExit();
    } };
  }
  return { root, agent, requests, start };
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
    pi.registerCommand("rpc-shutdown", { handler: (_args, ctx) => {
      ctx.ui.notify("shutdown requested");
      ctx.shutdown();
    } });
  }`);
  const rpc = f.start(["--extension", extension]);
  await rpc.request("get_state");
  rpc.send({ type: "prompt", id: "shutdown-command", message: "/rpc-shutdown" });
  await rpc.waitForExit();
  const notices = rpc.records.filter(record => record.method === "notify"
    && ["shutdown requested", "last shutdown notice: quit"].includes(record.message));
  assert.deepEqual(notices.map(record => record.message), ["shutdown requested", "last shutdown notice: quit"]);
  assert.equal(notices.at(-1)?.notifyType, "warning");
});

for (const decision of ["allow", "deny", "abort", "disconnect"] as const) {
  test(`RPC tool confirmation ${decision} crosses the JS/NAPI bridge`, { timeout: 40_000 }, async t => {
    const f = await fixture(t, true);
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
    assert.equal((await rpc.request("get_state")).isStreaming, true, "RPC must remain responsive during confirmation");
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
      await rpc.close();
    }
    assert.equal(await exists(join(f.root, "approved.txt")), decision === "allow");
    if (decision === "allow") assert.equal(await readFile(join(f.root, "approved.txt"), "utf8"), "allowed");
  });
}
