import assert from 'node:assert/strict'
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'

import { ExtensionHost } from '../src/extension-host.js'
import { isRecord, parseGenerationManifest, parseJson } from '../src/extension-protocol.js'
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
        cwd: "/legacy-node-discovery",
        projectTrusted: true,
        extensionPaths: [],
        mode: "print",
      },
    })),
    /Unrecognized key: "cwd"/,
  );
});

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

test('skips extensions whose unsupported current or legacy Pi TUI peer is unavailable', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-host-missing-tui-peer-'))
  const earendilUiExtension = join(root, 'earendil-ui-extension.ts')
  const marioUiExtension = join(root, 'mario-ui-extension.ts')
  const activeExtension = join(root, 'active-extension.ts')
  await writeFile(
    earendilUiExtension,
    `
      import { SelectList } from "@earendil-works/pi-tui";
      void SelectList;
      export default function (pi: any) {
        pi.registerShortcut("ctrl+t", () => undefined);
      }
    `,
  )
  await writeFile(
    marioUiExtension,
    `
      import { SelectList } from "@mariozechner/pi-tui";
      void SelectList;
      export default function (pi: any) {
        pi.registerShortcut("ctrl+m", () => undefined);
      }
    `,
  )
  await writeFile(
    activeExtension,
    `
      export default function (pi: any) {
        pi.registerCommand("still-active", {
          handler: async () => ({ action: "handled" })
        });
      }
    `,
  )

  const host = new ExtensionHost()
  const manifest = parseGenerationManifest(await host.dispatch(JSON.stringify({
    type: 'prepareGeneration',
    request: {
      projectTrusted: true,
      extensionPaths: [earendilUiExtension, marioUiExtension, activeExtension],
      mode: 'tui',
    },
  })))

  assert.equal(manifest.agentPlugins.length, 1)
  assert.equal(manifest.agentPlugins[0]?.commands[0]?.name, 'still-active')
  assert.deepEqual(
    manifest.diagnostics.map(diagnostic => ({
      path: diagnostic.path,
      feature: diagnostic.feature,
    })),
    [
      { path: earendilUiExtension, feature: '@earendil-works/pi-tui' },
      { path: marioUiExtension, feature: '@mariozechner/pi-tui' },
    ],
  )
  assert.ok(manifest.diagnostics.every(diagnostic => /extension was skipped/.test(diagnostic.message)))
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
            ctx.abort();
            ctx.compact({ customInstructions: "shorten" });
            await ctx.waitForIdle();
            let replacementSessionId;
            const replacement = await ctx.newSession({
              withSession: async (next: any) => {
                replacementSessionId = next.sessionManager.getSessionId();
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
    { type: 'abort' },
    { type: 'compact', customInstructions: 'shorten' },
  ])
  assert.deepEqual(requests.map(request => request.type), [
    'waitForIdle',
    'newSession',
    'reload',
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
