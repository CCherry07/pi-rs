import assert from 'node:assert/strict'
import { mkdtemp, mkdir, realpath, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'

import { ExtensionHost } from '../src/extension-host.js'
import { isRecord, parseGenerationManifest, parseJson } from '../src/extension-protocol.js'
import { execCommand } from '../src/extension-runtime.js'
import type { NativeExtensionContext } from '../src/native-binding.js'

test("rejects malformed host operations before dispatch", async () => {
  const host = new ExtensionHost();
  await assert.rejects(
    host.dispatch(JSON.stringify({ type: "retireGeneration", generation_id: "legacy-shape" })),
    /at generationId/,
  );
  await assert.rejects(
    host.dispatch(JSON.stringify({ type: "invoke", invocation: { kind: "unknown" } })),
    /at invocation\.kind/,
  );
  await assert.rejects(
    host.dispatch(JSON.stringify({
      type: "prepareGeneration",
      request: {
        cwd: "/workspace",
        projectTrusted: true,
        extensionPaths: [],
        mode: "print",
        agentDir: "/legacy-node-discovery",
      },
    })),
    /Unrecognized key: "agentDir"/,
  );
});

test('pi.exec reports timeout and pre-aborted process termination', async () => {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'pi-rs-js-exec-')))
  const timedOut = await execCommand(
    process.execPath,
    ['-e', 'setInterval(() => {}, 1000)'],
    root,
    { timeout: 20 },
  )
  assert.equal(timedOut.killed, true)
  assert.equal(timedOut.stdout, '')

  const controller = new AbortController()
  controller.abort()
  const aborted = await execCommand(
    process.execPath,
    ['-e', 'setInterval(() => {}, 1000)'],
    root,
    { signal: controller.signal },
  )
  assert.equal(aborted.killed, true)
  assert.equal(aborted.stdout, '')
})

test('extension flags accept Pi value forms, keep the first registration, and reject unknown names', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-flags-'))
  const first = join(root, 'first.ts')
  const second = join(root, 'second.ts')
  await writeFile(first, `
    export default function (pi: any) {
      pi.registerFlag("shared", { type: "string", default: "first" });
      pi.registerCommand("first-flag", {
        handler: () => ({ action: "transform", text: String(pi.getFlag("shared")) })
      });
    }
  `)
  await writeFile(second, `
    export default function (pi: any) {
      pi.registerFlag("shared", { type: "boolean", default: false });
      pi.registerCommand("second-flag", {
        handler: () => ({ action: "transform", text: String(pi.getFlag("shared")) })
      });
    }
  `)

  const host = new ExtensionHost()
  const manifest = parseGenerationManifest(await host.dispatch(JSON.stringify({
    type: 'prepareGeneration',
    request: {
      projectTrusted: true,
      extensionPaths: [first, second],
      mode: 'print',
      cwd: root,
      flagValues: { shared: 'command-line' },
    },
  })))
  assert.deepEqual(manifest.diagnostics, [])
  for (const plugin of manifest.agentPlugins) {
    const command = plugin.commands[0]
    assert.ok(command)
    const result = parseJson(await host.dispatch(JSON.stringify({
      type: 'invoke',
      invocation: {
        invocationId: command.name,
        generationId: manifest.generationId,
        callbackId: command.callbackId,
        kind: 'command',
        payload: { context: { cwd: root }, arguments: '' },
      },
    }))) as { text: string }
    assert.equal(result.text, 'command-line')
  }

  await assert.rejects(
    host.dispatch(JSON.stringify({
      type: 'prepareGeneration',
      request: {
        projectTrusted: true,
        extensionPaths: [first],
        mode: 'print',
        cwd: root,
        flagValues: { missing: true },
      },
    })),
    /Unknown JavaScript extension flag: --missing/,
  )
})

test('runs non-UI extension actions and streams prepared tool updates through the native seam', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-host-core-actions-'))
  const extension = join(root, 'core-actions.ts')
  await writeFile(extension, `
    export default function (pi: any) {
      pi.registerFlag("fixture-mode", { type: "string", default: "safe" });
      pi.registerTool({
        name: "prepared-tool",
        description: "Exercises preparation and updates",
        parameters: { type: "object", properties: { value: { type: "string" } } },
        prepareArguments(input: any) {
          return { ...input, value: String(input.value).toUpperCase() };
        },
        async execute(_id: string, input: any, _signal: AbortSignal, update: any) {
          update({
            content: [{ type: "text", text: "working:" + input.value }],
            details: { phase: 1 }
          });
          return { content: [{ type: "text", text: "done:" + input.value }] };
        }
      });
      pi.registerCommand("core-actions", {
        async handler() {
          pi.sendMessage({ customType: "fixture", content: "context" }, { triggerTurn: false });
          pi.sendUserMessage("continue", { deliverAs: "followUp" });
          pi.appendEntry("fixture-state", { count: 1 });
          pi.setSessionName("Core actions");
          pi.setLabel("entry-1", "checkpoint");
          pi.setActiveTools(["prepared-tool"]);
          pi.setThinkingLevel("high");
          const selected = await pi.setModel({ provider: "scripted", id: "model-1" });
          const executed = await pi.exec(process.execPath, ["-e", "process.stdout.write(process.cwd())"]);
          return {
            action: "transform",
            text: JSON.stringify({
              flag: pi.getFlag("fixture-mode"),
              sessionName: pi.getSessionName(),
              activeTools: pi.getActiveTools(),
              tools: pi.getAllTools(),
              commands: pi.getCommands(),
              thinking: pi.getThinkingLevel(),
              selected,
              executed
            })
          };
        }
      });
    }
  `)

  const host = new ExtensionHost()
  const manifest = parseGenerationManifest(await host.dispatch(JSON.stringify({
    type: 'prepareGeneration',
    request: {
      projectTrusted: true,
      extensionPaths: [extension],
      mode: 'print',
      cwd: root,
      flagValues: { 'fixture-mode': 'fast' },
    },
  })))
  assert.deepEqual(manifest.diagnostics, [])

  const notifications: Record<string, unknown>[] = []
  const requests: Record<string, unknown>[] = []
  const updates: Record<string, unknown>[] = []
  const nativeContext: NativeExtensionContext = {
    query(operation) {
      const { type } = parseJson(operation) as { type: string }
      const values: Record<string, unknown> = {
        sessionName: 'Native session',
        activeTools: ['read'],
        allTools: [{ name: 'read' }],
        commands: [{ name: 'help' }],
        thinkingLevel: 'medium',
      }
      return JSON.stringify(values[type] ?? null)
    },
    notify(operation) { notifications.push(parseJson(operation) as Record<string, unknown>) },
    async request(operation) {
      requests.push(parseJson(operation) as Record<string, unknown>)
      return 'true'
    },
    update(result) { updates.push(parseJson(result) as Record<string, unknown>) },
  }

  const tool = manifest.agentPlugins[0]?.tools[0]
  assert.ok(tool)
  assert.ok(tool.prepareCallbackId)
  const preparedInput = parseJson(await host.dispatch(JSON.stringify({
    type: 'invoke',
    invocation: {
      invocationId: 'prepare-tool', generationId: manifest.generationId,
      callbackId: tool.prepareCallbackId, kind: 'toolPrepareArguments',
      payload: { input: { value: 'hello' } },
    },
  }), nativeContext))
  assert.deepEqual(preparedInput, { value: 'HELLO' })
  const toolResult = parseJson(await host.dispatch(JSON.stringify({
    type: 'invoke',
    invocation: {
      invocationId: 'prepared-tool', generationId: manifest.generationId,
      callbackId: tool.callbackId, kind: 'tool',
      payload: { context: { cwd: root, toolCallId: 'call-1' }, input: preparedInput },
    },
  }), nativeContext))
  assert.deepEqual(toolResult, {
    content: [{ type: 'text', text: 'done:HELLO' }],
    isError: false,
    terminate: false,
  })
  assert.deepEqual(updates, [{
    content: [{ type: 'text', text: 'working:HELLO' }],
    details: { phase: 1 },
  }])

  const command = manifest.agentPlugins[0]?.commands[0]
  assert.ok(command)
  const commandResult = parseJson(await host.dispatch(JSON.stringify({
    type: 'invoke',
    invocation: {
      invocationId: 'core-actions', generationId: manifest.generationId,
      callbackId: command.callbackId, kind: 'command',
      payload: { context: { cwd: root }, arguments: '' },
    },
  }), nativeContext)) as { action: string; text: string }
  assert.equal(commandResult.action, 'transform')
  assert.deepEqual(JSON.parse(commandResult.text), {
    flag: 'fast',
    sessionName: 'Native session',
    activeTools: ['read'],
    tools: [{ name: 'read' }],
    commands: [{ name: 'help' }],
    thinking: 'medium',
    selected: true,
    executed: { stdout: await realpath(root), stderr: '', code: 0, killed: false },
  })
  assert.deepEqual(notifications.map(operation => operation.type), [
    'sendMessage', 'sendUserMessage', 'appendEntry', 'setSessionName',
    'setLabel', 'setActiveTools', 'setThinkingLevel',
  ])
  assert.deepEqual(requests, [{ type: 'setModel', provider: 'scripted', modelId: 'model-1' }])
})

test('loads a TypeScript Pi tool and retires its callback generation', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-host-'))
  const extensionDirectory = join(root, '.pi/extensions')
  await mkdir(extensionDirectory, { recursive: true })
  await writeFile(
    join(extensionDirectory, 'hello.ts'),
    `
      import { CONFIG_DIR_NAME, defineTool } from "@earendil-works/pi-coding-agent";

      interface TestExtensionApi {
        registerTool(definition: unknown): void;
        registerCommand(name: string, options: unknown): void;
        on(event: string, handler: unknown): void;
      }
      interface TestExtensionContext {
        mode: string;
        signal: AbortSignal;
        isProjectTrusted(): boolean;
      }

      export default function (pi: TestExtensionApi) {
        pi.registerTool(defineTool({
          name: "hello",
          label: "Hello",
          description: "Say hello from " + CONFIG_DIR_NAME,
          parameters: {
            type: "object",
            properties: { name: { type: "string" } },
            required: ["name"]
          },
          promptSnippet: "Say hello",
          promptGuidelines: ["Use hello for greetings."],
          executionMode: "sequential",
          async execute(_id: string, input: { name: string }, _signal: AbortSignal, _update: unknown, ctx: TestExtensionContext) {
            return {
              content: [{ type: "text", text: "Hello " + input.name }],
              details: {
                mode: ctx.mode,
                projectTrusted: ctx.isProjectTrusted(),
                hasSignal: ctx.signal instanceof AbortSignal
              }
            };
          }
        }));
        pi.registerCommand("echo", {
          description: "Echo into the model",
          handler: async (args: string) => ({ action: "transform", text: args })
        });
        pi.on("input", (event: { text: string }) => ({
          action: "transform",
          text: event.text + "!"
        }));
        pi.on("before_provider_request", (event: { payload: Record<string, unknown> }) => ({
          ...event.payload,
          fromJavaScript: true
        }));
        pi.on("session_start", () => undefined);
      }
    `,
  )

  const host = new ExtensionHost()
  const manifest = parseGenerationManifest(
    await host.dispatch(
      JSON.stringify({
        type: 'prepareGeneration',
        request: {
          projectTrusted: true,
          extensionPaths: [join(extensionDirectory, 'hello.ts')],
          mode: 'tui',
        },
      }),
    ),
  )
  const agentPlugin = manifest.agentPlugins[0]
  const providerPlugin = manifest.providerPlugins[0]
  const sessionPlugin = manifest.sessionPlugins[0]
  assert.ok(agentPlugin)
  assert.ok(providerPlugin)
  assert.ok(sessionPlugin)
  const tool = agentPlugin.tools[0]
  const inputHook = agentPlugin.hooks[0]
  assert.ok(tool)
  assert.ok(inputHook)
  assert.equal(agentPlugin.commands[0]?.name, 'echo')
  assert.equal(inputHook.name, 'input')
  assert.equal(providerPlugin.hooks[0]?.name, 'before_provider_request')
  assert.equal(sessionPlugin.hooks[0]?.name, 'session_start')
  assert.equal(tool.executionMode, 'sequential')
  assert.equal(tool.promptSnippet, 'Say hello')
  const invocation = {
    invocationId: 'invocation-1',
    generationId: manifest.generationId,
    callbackId: tool.callbackId,
    kind: 'tool',
    payload: {
      context: { cwd: root, toolCallId: 'call-1' },
      input: { name: 'Cherry' },
    },
  }

  const result = parseJson(await host.dispatch(JSON.stringify({ type: 'invoke', invocation })))
  assert.ok(isRecord(result))
  const content = result.content
  const details = result.details
  assert.ok(Array.isArray(content))
  assert.ok(isRecord(content[0]))
  assert.ok(isRecord(details))
  assert.equal(content[0].text, 'Hello Cherry')
  assert.equal(details.mode, 'tui')
  assert.equal(details.projectTrusted, true)
  assert.equal(details.hasSignal, true)

  const inputResult = parseJson(
    await host.dispatch(
      JSON.stringify({
        type: 'invoke',
        invocation: {
          ...invocation,
          invocationId: 'invocation-2',
          callbackId: inputHook.callbackId,
          kind: 'agentHook',
          payload: {
            hook: 'input',
            context: { cwd: root },
            event: { type: 'input', text: 'hello' },
          },
        },
      }),
    ),
  )
  assert.deepEqual(inputResult, { action: 'transform', text: 'hello!' })

  await host.dispatch(JSON.stringify({ type: 'retireGeneration', generationId: manifest.generationId }))
  await assert.rejects(host.dispatch(JSON.stringify({ type: 'invoke', invocation })), /generation is retired/)
})

test('provides host-owned pi-ai and typebox peer modules to managed extensions', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-host-peers-'))
  const extension = join(root, 'peer-extension.ts')
  await writeFile(
    extension,
    `
      import { StringEnum } from "@earendil-works/pi-ai";
      import { Type } from "typebox";
      import { Compile } from "typebox/compile";
      import { Value } from "typebox/value";
      import { Type as LegacyType } from "@sinclair/typebox";
      import { Compile as LegacyCompile } from "@sinclair/typebox/compile";
      import { Value as LegacyValue } from "@sinclair/typebox/value";

      export default function (pi: { registerTool(definition: unknown): void }) {
        pi.registerTool({
          name: "peer_modules",
          description:
            typeof Compile === "function" && typeof Value.Check === "function" &&
            typeof LegacyCompile === "function" && typeof LegacyValue.Check === "function"
              ? "Verify host-owned peer modules"
              : "Missing TypeBox subpath exports",
          parameters: Type.Object({
            mode: StringEnum(["fast", "thorough"] as const),
            legacy: LegacyType.Boolean()
          }),
          async execute() {
            return { content: [{ type: "text", text: "ok" }] };
          }
        });
      }
    `,
  )

  const host = new ExtensionHost()
  const manifest = parseGenerationManifest(
    await host.dispatch(
      JSON.stringify({
        type: 'prepareGeneration',
        request: {
          projectTrusted: true,
          extensionPaths: [extension],
          mode: 'print',
        },
      }),
    ),
  )

  assert.deepEqual(manifest.agentPlugins[0]?.tools[0]?.parameters, {
    type: 'object',
    required: ['mode', 'legacy'],
    properties: {
      mode: { type: 'string', enum: ['fast', 'thorough'] },
      legacy: { type: 'boolean' },
    },
  })
  assert.equal(manifest.agentPlugins[0]?.tools[0]?.description, 'Verify host-owned peer modules')
})

test('resolves every non-UI module exposed by the current Pi extension loader', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-host-all-pi-peers-'))
  const extension = join(root, 'all-pi-peers.ts')
  await writeFile(
    extension,
    `
      import * as EarendilCodingAgent from "@earendil-works/pi-coding-agent";
      import * as EarendilAgentCore from "@earendil-works/pi-agent-core";
      import * as EarendilAi from "@earendil-works/pi-ai";
      import * as EarendilAiCompat from "@earendil-works/pi-ai/compat";
      import * as EarendilAiOauth from "@earendil-works/pi-ai/oauth";
      import * as EarendilAiProviders from "@earendil-works/pi-ai/providers/all";
      import * as MarioCodingAgent from "@mariozechner/pi-coding-agent";
      import * as MarioAgentCore from "@mariozechner/pi-agent-core";
      import * as MarioAi from "@mariozechner/pi-ai";
      import * as MarioAiCompat from "@mariozechner/pi-ai/compat";
      import * as MarioAiOauth from "@mariozechner/pi-ai/oauth";
      import * as MarioAiProviders from "@mariozechner/pi-ai/providers/all";

      const hostModules = [
        EarendilCodingAgent,
        EarendilAgentCore,
        EarendilAi,
        EarendilAiCompat,
        EarendilAiOauth,
        EarendilAiProviders,
        MarioCodingAgent,
        MarioAgentCore,
        MarioAi,
        MarioAiCompat,
        MarioAiOauth,
        MarioAiProviders,
      ];

      export default function (pi: any) {
        pi.registerCommand("all-host-modules", {
          handler: async () => ({
            action: hostModules.every(module => typeof module === "object")
              ? "handled"
              : "failed"
          })
        });
      }
    `,
  )

  const host = new ExtensionHost()
  const manifest = parseGenerationManifest(await host.dispatch(JSON.stringify({
    type: 'prepareGeneration',
    request: {
      projectTrusted: true,
      extensionPaths: [extension],
      mode: 'print',
    },
  })))

  const command = manifest.agentPlugins[0]?.commands[0]
  assert.equal(command?.name, 'all-host-modules')
  assert.deepEqual(
    parseJson(await host.dispatch(JSON.stringify({
      type: 'invoke',
      invocation: {
        generationId: manifest.generationId,
        invocationId: 'all-host-modules-command',
        callbackId: command?.callbackId,
        kind: 'command',
        payload: {
          arguments: '',
          context: { cwd: root },
        },
      },
    }))),
    { action: 'handled' },
  )
})

test('activates provider header mutation and response observation hooks', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-provider-wire-hooks-'))
  const extension = join(root, 'provider-wire-hooks.ts')
  await writeFile(extension, `
    export default function (pi: any) {
      pi.on("before_provider_headers", (event: any) => {
        event.headers["X-Trace"] = "trace-1";
        event.headers["X-Remove"] = null;
        return { ignored: true };
      });
      pi.on("after_provider_response", (event: any) => {
        if (event.status !== 429 || event.headers["retry-after"] !== "2") {
          throw new Error("unexpected provider response event");
        }
        return { ignored: true };
      });
    }
  `)

  const host = new ExtensionHost()
  const manifest = parseGenerationManifest(await host.dispatch(JSON.stringify({
    type: 'prepareGeneration',
    request: {
      projectTrusted: true,
      extensionPaths: [extension],
      mode: 'print',
      cwd: root,
    },
  })))

  assert.deepEqual(manifest.diagnostics, [])
  assert.deepEqual(
    manifest.providerPlugins[0]?.hooks.map(hook => hook.name),
    ['before_provider_headers', 'after_provider_response'],
  )

  const headerHook = manifest.providerPlugins[0]?.hooks[0]
  assert.ok(headerHook)
  const headers = parseJson(await host.dispatch(JSON.stringify({
    type: 'invoke',
    invocation: {
      invocationId: 'provider-headers',
      generationId: manifest.generationId,
      callbackId: headerHook.callbackId,
      kind: 'providerHook',
      payload: {
        hook: 'before_provider_headers',
        context: { cwd: root },
        event: {
          type: 'before_provider_headers',
          headers: { Existing: 'yes', 'X-Remove': 'remove-me' },
        },
      },
    },
  })))
  assert.deepEqual(headers, {
    Existing: 'yes',
    'X-Remove': null,
    'X-Trace': 'trace-1',
  })

  const responseHook = manifest.providerPlugins[0]?.hooks[1]
  assert.ok(responseHook)
  const observed = parseJson(await host.dispatch(JSON.stringify({
    type: 'invoke',
    invocation: {
      invocationId: 'provider-response',
      generationId: manifest.generationId,
      callbackId: responseHook.callbackId,
      kind: 'providerHook',
      payload: {
        hook: 'after_provider_response',
        context: { cwd: root },
        event: {
          type: 'after_provider_response',
          status: 429,
          headers: { 'retry-after': '2' },
        },
      },
    },
  })))
  assert.equal(observed, null)
})

test('captures configured providers at load time and forwards runtime mutations', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-provider-registration-'))
  const extension = join(root, 'provider-registration.ts')
  await writeFile(extension, `
    export default function (pi: any) {
      pi.registerProvider("temporary", {
        baseUrl: "https://temporary.example/v1",
        api: "openai-responses",
        models: [{ id: "temporary-model" }]
      });
      pi.unregisterProvider("temporary");
      pi.registerProvider("proxy", {
        name: "Proxy",
        baseUrl: "https://proxy.example/v1",
        apiKey: "$PROXY_API_KEY",
        api: "openai-responses",
        authHeader: true,
        models: [{
          id: "model-a",
          name: "Model A",
          reasoning: true,
          input: ["text", "image"],
          cost: { input: 1, output: 2, cacheRead: 0.1, cacheWrite: 0.2 },
          contextWindow: 200000,
          maxTokens: 8192,
          headers: { "X-Model": "a" },
          compat: { supportsDeveloperRole: true }
        }]
      });
      pi.registerProvider("future", {
        streamSimple() { throw new Error("inactive callback must not run"); }
      });
      pi.registerCommand("mutate-provider", {
        handler() {
          pi.registerProvider("proxy", { baseUrl: "https://runtime.example/v1" });
          pi.unregisterProvider("proxy");
        }
      });
    }
  `)

  const host = new ExtensionHost()
  const manifest = parseGenerationManifest(await host.dispatch(JSON.stringify({
    type: 'prepareGeneration',
    request: {
      projectTrusted: true,
      extensionPaths: [extension],
      mode: 'print',
      cwd: root,
    },
  })))

  assert.deepEqual(manifest.providerRegistrations, [{
    pluginId: 'js:0:provider-registration.ts',
    path: extension,
    name: 'proxy',
    config: {
      name: 'Proxy',
      baseUrl: 'https://proxy.example/v1',
      apiKey: '$PROXY_API_KEY',
      api: 'openai-responses',
      authHeader: true,
      models: [{
        id: 'model-a',
        name: 'Model A',
        reasoning: true,
        input: ['text', 'image'],
        cost: { input: 1, output: 2, cacheRead: 0.1, cacheWrite: 0.2 },
        contextWindow: 200000,
        maxTokens: 8192,
        headers: { 'X-Model': 'a' },
        compat: { supportsDeveloperRole: true },
      }],
    },
  }])
  assert.deepEqual(
    manifest.diagnostics.map(diagnostic => diagnostic.feature),
    ['pi.registerProvider.streamSimple'],
  )

  const notifications: Record<string, unknown>[] = []
  const command = manifest.agentPlugins[0]?.commands[0]
  assert.ok(command)
  await host.dispatch(JSON.stringify({
    type: 'invoke',
    invocation: {
      invocationId: 'mutate-provider',
      generationId: manifest.generationId,
      callbackId: command.callbackId,
      kind: 'command',
      payload: { context: { cwd: root }, arguments: '' },
    },
  }), {
    query: () => 'null',
    notify: operation => notifications.push(parseJson(operation) as Record<string, unknown>),
    request: async () => 'null',
  })
  assert.deepEqual(notifications, [
    {
      type: 'registerProvider',
      name: 'proxy',
      config: { baseUrl: 'https://runtime.example/v1' },
    },
    { type: 'unregisterProvider', name: 'proxy' },
  ])
})

test('keeps inactive JavaScript UI registrations non-fatal while agent_settled stays active', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-host-inactive-'))
  const extension = join(root, 'inactive-extension.ts')
  await writeFile(
    extension,
    `
      export default function (pi: any) {
        let eventValue;
        const unsubscribe = pi.events.on("fixture:event", (value: unknown) => { eventValue = value; });
        pi.events.emit("fixture:event", 42);
        unsubscribe();
        if (eventValue !== 42) throw new Error("generation event bus did not dispatch");
        pi.registerShortcut("ctrl+x", () => undefined);
        pi.registerMessageRenderer("custom", () => undefined);
        pi.on("agent_settled", (event: any, ctx: any) => ({
          type: event.type,
          idle: ctx.isIdle()
        }));
        pi.registerCommand("ui-defaults", {
          async handler(_args: string, ctx: any) {
            ctx.ui.notify("ignored");
            ctx.ui.setWidget("example", () => { throw new Error("factory must not run") });
            const selected = await ctx.ui.select("Choose", ["a"]);
            const confirmed = await ctx.ui.confirm("Confirm", "Continue?");
            const edited = await ctx.ui.editor("Edit", "value");
            if (ctx.hasUI || selected !== undefined || confirmed !== false || edited !== undefined) {
              throw new Error("unexpected UI fallback");
            }
          }
        });
      }
    `,
  )

  const host = new ExtensionHost()
  const manifest = parseGenerationManifest(
    await host.dispatch(JSON.stringify({
      type: 'prepareGeneration',
      request: {
        projectTrusted: true,
        extensionPaths: [extension],
        mode: 'tui',
      },
    })),
  )

  const settledHook = manifest.agentPlugins[0]?.hooks[0]
  assert.equal(settledHook?.name, 'agent_settled')
  assert.deepEqual(
    manifest.diagnostics.map(diagnostic => diagnostic.feature),
    ['pi.registerShortcut', 'pi.registerMessageRenderer'],
  )

  assert.deepEqual(parseJson(await host.dispatch(JSON.stringify({
    type: 'invoke',
    invocation: {
      invocationId: 'active-agent-settled',
      generationId: manifest.generationId,
      callbackId: settledHook?.callbackId,
      kind: 'agentHook',
      payload: {
        hook: 'agent_settled',
        context: { cwd: root },
        event: { type: 'agent_settled' },
      },
    },
  }))), { type: 'agent_settled', idle: true })

  const command = manifest.agentPlugins[0]?.commands[0]
  assert.ok(command)
  const result = parseJson(await host.dispatch(JSON.stringify({
    type: 'invoke',
    invocation: {
      invocationId: 'inactive-ui-command',
      generationId: manifest.generationId,
      callbackId: command.callbackId,
      kind: 'command',
      payload: { context: { cwd: root }, arguments: '' },
    },
  })))
  assert.deepEqual(result, { action: 'handled' })
})

test('keeps non-UI contributions from extensions that import current or legacy Pi TUI', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-host-inert-tui-'))
  const cases = [
    { scope: 'earendil', module: '@earendil-works/pi-tui' },
    { scope: 'mario', module: '@mariozechner/pi-tui' },
  ] as const
  const extensionPaths: string[] = []

  for (const fixture of cases) {
    const extension = join(root, `${fixture.scope}-extension.ts`)
    extensionPaths.push(extension)
    await writeFile(
      extension,
      `
        import { Key, Text, truncateToWidth } from "${fixture.module}";
        const preview = new Text(truncateToWidth("todo preview", 7, "…"), 0, 0);
        const shortcut = Key.ctrlAlt("f");

        export default function (pi: any) {
          pi.registerTool({
            name: "${fixture.scope}-todo",
            description: "Mixed extension tool",
            parameters: { type: "object", properties: {} },
            renderCall: () => preview,
            async execute() {
              return { content: [{ type: "text", text: "ok" }] };
            }
          });
          pi.registerCommand("${fixture.scope}-todos", {
            handler: async () => ({ action: "handled" })
          });
          pi.registerShortcut(shortcut, () => undefined);
        }
      `,
    )
  }

  const host = new ExtensionHost()
  const manifest = parseGenerationManifest(await host.dispatch(JSON.stringify({
    type: 'prepareGeneration',
    request: {
      projectTrusted: true,
      extensionPaths,
      mode: 'tui',
    },
  })))

  assert.deepEqual(
    manifest.agentPlugins.map(plugin => ({
      tools: plugin.tools.map(tool => tool.name),
      commands: plugin.commands.map(command => command.name),
    })),
    [
      { tools: ['earendil-todo'], commands: ['earendil-todos'] },
      { tools: ['mario-todo'], commands: ['mario-todos'] },
    ],
  )
  assert.deepEqual(
    manifest.diagnostics.map(diagnostic => diagnostic.feature),
    ['pi.registerShortcut', 'pi.registerShortcut'],
  )
})

test('keeps ordinary missing extension dependencies fatal', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-host-missing-peer-'))
  const extension = join(root, 'broken-extension.ts')
  await writeFile(
    extension,
    `
      import missing from "a-package-that-definitely-does-not-exist";
      export default function () { return missing; }
    `,
  )

  const host = new ExtensionHost()
  await assert.rejects(
    host.dispatch(JSON.stringify({
      type: 'prepareGeneration',
      request: {
        projectTrusted: true,
        extensionPaths: [extension],
        mode: 'tui',
      },
    })),
    /Cannot find module 'a-package-that-definitely-does-not-exist'/,
  )
})

test('builds command context from the native query notify and request capability', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-host-context-'))
  const extension = join(root, 'context-extension.ts')
  await writeFile(
    extension,
    `
      export default function (pi: any) {
        pi.registerCommand("inspect-context", {
          async handler(_args: string, ctx: any) {
            const before = {
              cwd: ctx.cwd,
              trusted: ctx.isProjectTrusted(),
              idle: ctx.isIdle(),
              pending: ctx.hasPendingMessages(),
              model: ctx.model,
              thinkingLevel: ctx.thinkingLevel,
              sessionId: ctx.sessionManager.getSessionId(),
              sessionName: ctx.sessionManager.getSessionName(),
              entry: ctx.sessionManager.getEntry("entry-1"),
              header: ctx.sessionManager.getHeader(),
              prompt: ctx.getSystemPrompt(),
              promptOptions: ctx.getSystemPromptOptions()
            };
            ctx.ui.notify("Extension notice", "warning");
            ctx.abort();
            ctx.compact({ customInstructions: "shorten" });
            await ctx.waitForIdle();
            await ctx.navigateTree("entry-1", {
              summarize: true,
              customInstructions: "focus",
              label: "branch"
            });
            let replacementSessionId;
            const replacement = await ctx.newSession({
              parentSession: "/sessions/parent.jsonl",
              setup: async (manager: any) => {
                manager.appendCustomEntry("initialized", { ready: true });
              },
              withSession: async (next: any) => {
                replacementSessionId = next.sessionManager.getSessionId();
                await next.sendMessage(
                  { customType: "replacement-state", content: "ready" },
                  { triggerTurn: false }
                );
                await next.sendUserMessage("continue", { deliverAs: "followUp" });
              }
            });
            await ctx.reload();
            return {
              action: "transform",
              text: JSON.stringify({ before, replacement, replacementSessionId })
            };
          }
        });
      }
    `,
  )

  const queries: string[] = []
  const notifications: Record<string, unknown>[] = []
  const requests: Record<string, unknown>[] = []
  let sessionId = 'session-before'
  const nativeContext: NativeExtensionContext = {
    query(rawOperation) {
      const operation = parseJson(rawOperation) as { type: string }
      queries.push(operation.type)
      const values: Record<string, unknown> = {
        cwd: root,
        isProjectTrusted: false,
        isIdle: true,
        hasPendingMessages: false,
        model: { provider: 'test', id: 'model-1' },
        thinkingLevel: 'high',
        sessionId,
        sessionName: 'Native session',
        sessionEntry: {
          id: 'entry-1',
          seq: 7,
          parentId: null,
          timestamp: 1_700_000_000_000,
          type: 'message',
          message: { role: 'user', content: 'hello' },
        },
        sessionHeader: {
          kind: 'header',
          version: 4,
          id: 'session-before',
          createdAt: 1_700_000_000_000,
          cwd: root,
        },
        systemPrompt: 'native prompt',
        systemPromptOptions: { cwd: root, selectedTools: ['read'] },
      }
      return JSON.stringify(values[operation.type] ?? null)
    },
    notify(rawOperation) {
      notifications.push(parseJson(rawOperation) as Record<string, unknown>)
    },
    async request(rawOperation) {
      const operation = parseJson(rawOperation) as Record<string, unknown> & { type: string }
      requests.push(operation)
      if (operation.type === 'newSession') {
        sessionId = 'session-after'
        return JSON.stringify({ cancelled: false })
      }
      if (operation.type === 'navigateTree') return JSON.stringify({ cancelled: false })
      return 'null'
    },
  }

  const host = new ExtensionHost()
  const manifest = parseGenerationManifest(await host.dispatch(JSON.stringify({
    type: 'prepareGeneration',
    request: {
      projectTrusted: true,
      extensionPaths: [extension],
      mode: 'print',
    },
  })))
  const command = manifest.agentPlugins[0]?.commands[0]
  assert.ok(command)
  const result = parseJson(await host.dispatch(JSON.stringify({
    type: 'invoke',
    invocation: {
      invocationId: 'native-context-command',
      generationId: manifest.generationId,
      callbackId: command.callbackId,
      kind: 'command',
      payload: { context: { cwd: '/stale-payload-cwd' }, arguments: '' },
    },
  }), nativeContext))
  assert.ok(isRecord(result))
  assert.equal(result.action, 'transform')
  assert.deepEqual(JSON.parse(String(result.text)), {
    before: {
      cwd: root,
      trusted: false,
      idle: true,
      pending: false,
      model: { provider: 'test', id: 'model-1' },
      thinkingLevel: 'high',
      sessionId: 'session-before',
      sessionName: 'Native session',
      entry: {
        id: 'entry-1',
        parentId: null,
        timestamp: '2023-11-14T22:13:20.000Z',
        type: 'message',
        message: { role: 'user', content: 'hello' },
      },
      header: {
        type: 'session',
        version: 3,
        id: 'session-before',
        timestamp: '2023-11-14T22:13:20.000Z',
        cwd: root,
      },
      prompt: 'native prompt',
      promptOptions: { cwd: root, selectedTools: ['read'] },
    },
    replacement: { cancelled: false },
    replacementSessionId: 'session-after',
  })
  assert.deepEqual(notifications, [
    { type: 'uiNotify', message: 'Extension notice', level: 'warning' },
    { type: 'abort' },
    { type: 'compact', customInstructions: 'shorten' },
    { type: 'appendEntry', customType: 'initialized', data: { ready: true } },
  ])
  assert.deepEqual(requests, [
    { type: 'waitForIdle' },
    {
      type: 'navigateTree',
      targetId: 'entry-1',
      summarize: true,
      customInstructions: 'focus',
      replaceInstructions: false,
      label: 'branch',
    },
    { type: 'newSession', parentSession: '/sessions/parent.jsonl' },
    {
      type: 'sendMessage',
      message: { customType: 'replacement-state', content: 'ready' },
      options: { triggerTurn: false },
    },
    {
      type: 'sendUserMessage',
      content: 'continue',
      options: { deliverAs: 'followUp' },
    },
    { type: 'reload' },
  ])
  assert.ok(queries.includes('sessionId'))
})

test('still rejects unknown hook names as extension mistakes', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-host-unknown-hook-'))
  const extension = join(root, 'unknown-hook.ts')
  await writeFile(extension, 'export default pi => pi.on("agent_settledd", () => undefined)')
  const host = new ExtensionHost()

  await assert.rejects(
    host.dispatch(JSON.stringify({
      type: 'prepareGeneration',
      request: {
        projectTrusted: true,
        extensionPaths: [extension],
        mode: 'print',
      },
    })),
    /not a recognized Pi hook/,
  )
})

test('before_agent_start preserves message injection and exposes the chained system prompt', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-host-before-agent-start-'))
  const extension = join(root, 'before-agent-start.ts')
  await writeFile(
    extension,
    `
      export default function (pi: any) {
        pi.on("before_agent_start", (event: any, ctx: any) => ({
          message: {
            customType: "fixture-context",
            content: "injected context",
            display: false,
            details: { source: "fixture" }
          },
          systemPrompt: ctx.getSystemPrompt() + "|current"
        }));
      }
    `,
  )

  const host = new ExtensionHost()
  const manifest = parseGenerationManifest(await host.dispatch(JSON.stringify({
    type: 'prepareGeneration',
    request: {
      projectTrusted: true,
      extensionPaths: [extension],
      mode: 'print',
    },
  })))
  const hook = manifest.agentPlugins[0]?.hooks[0]
  assert.ok(hook)

  const nativeContext: NativeExtensionContext = {
    query(rawOperation) {
      const operation = parseJson(rawOperation) as { type: string }
      return JSON.stringify(operation.type === 'systemPrompt' ? 'stale native prompt' : null)
    },
    notify() {},
    async request() {
      return 'null'
    },
  }
  const result = parseJson(await host.dispatch(JSON.stringify({
    type: 'invoke',
    invocation: {
      invocationId: 'before-agent-start',
      generationId: manifest.generationId,
      callbackId: hook.callbackId,
      kind: 'agentHook',
      payload: {
        context: { cwd: root },
        event: {
          type: 'before_agent_start',
          prompt: 'hello',
          systemPrompt: 'base|previous',
          systemPromptOptions: { cwd: root },
        },
      },
    },
  }), nativeContext))

  assert.deepEqual(result, {
    message: {
      customType: 'fixture-context',
      content: 'injected context',
      display: false,
      details: { source: 'fixture' },
    },
    systemPrompt: 'base|previous|current',
  })
})

test('input images and message_end replacements cross the JavaScript host unchanged', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-host-transform-hooks-'))
  const extension = join(root, 'transform-hooks.ts')
  await writeFile(
    extension,
    `
      export default function (pi: any) {
        pi.on("input", (event: any) => ({
          action: "transform",
          text: event.text + "|rewritten",
          images: [{ type: "image", data: "aW1hZ2U=", mimeType: "image/png" }]
        }));
        pi.on("message_end", (event: any) => ({
          message: { ...event.message, content: [{ type: "text", text: "finalized" }] }
        }));
      }
    `,
  )

  const host = new ExtensionHost()
  const manifest = parseGenerationManifest(await host.dispatch(JSON.stringify({
    type: 'prepareGeneration',
    request: {
      projectTrusted: true,
      extensionPaths: [extension],
      mode: 'print',
    },
  })))
  assert.deepEqual(manifest.diagnostics, [])
  const [input, messageEnd] = manifest.agentPlugins[0]?.hooks ?? []
  assert.ok(input)
  assert.ok(messageEnd)

  const invoke = async (callbackId: string, event: Record<string, unknown>) => parseJson(
    await host.dispatch(JSON.stringify({
      type: 'invoke',
      invocation: {
        invocationId: callbackId,
        generationId: manifest.generationId,
        callbackId,
        kind: 'agentHook',
        payload: { context: { cwd: root }, event },
      },
    })),
  )
  assert.deepEqual(await invoke(input.callbackId, {
    type: 'input',
    text: 'hello',
    source: 'rpc',
    streamingBehavior: 'followUp',
  }), {
    action: 'transform',
    text: 'hello|rewritten',
    images: [{ type: 'image', data: 'aW1hZ2U=', mimeType: 'image/png' }],
  })
  assert.deepEqual(await invoke(messageEnd.callbackId, {
    type: 'message_end',
    message: { role: 'user', content: [{ type: 'text', text: 'hello' }], timestamp: 1 },
  }), {
    message: { role: 'user', content: [{ type: 'text', text: 'finalized' }], timestamp: 1 },
  })
})
