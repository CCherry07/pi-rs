import assert from 'node:assert/strict'
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'

import { ExtensionHost } from '../src/extension-host.js'
import { isRecord, parseGenerationManifest, parseJson } from '../src/extension-protocol.js'

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

      export default function (pi: { registerTool(definition: unknown): void }) {
        pi.registerTool({
          name: "peer_modules",
          description: "Verify host-owned peer modules",
          parameters: Type.Object({ mode: StringEnum(["fast", "thorough"] as const) }),
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
    required: ['mode'],
    properties: {
      mode: { type: 'string', enum: ['fast', 'thorough'] },
    },
  })
})
