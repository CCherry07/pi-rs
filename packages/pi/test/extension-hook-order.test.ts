import assert from 'node:assert/strict'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'

import { ExtensionHost } from '../src/extension-host.js'
import { parseGenerationManifest, parseJson } from '../src/extension-protocol.js'

// Pi's runner snapshots extensions in load order, then each extension's handlers.
// See legacy/pi/packages/coding-agent/src/core/extensions/runner.ts: snapshotEventHandlers.
for (const [hook, kind] of [
  ['agent_end', 'agentHook'],
  ['before_provider_request', 'providerHook'],
  ['session_start', 'sessionHook'],
] as const) {
  for (const initiallySubscribed of [true, false]) {
    test(`pi.on preserves extension load order for ${hook} with an initially ${initiallySubscribed ? 'subscribed' : 'empty'} first extension`, async t => {
      const root = await mkdtemp(join(tmpdir(), 'pi-rs-hook-order-'))
      t.after(() => rm(root, { recursive: true, force: true }))
      const paths = [join(root, 'z-first.ts'), join(root, 'a-second.ts')]
      for (const [index, path] of paths.entries()) {
        await writeFile(path, `
          export default function (pi: any) {
            const calls: string[] = [];
            const stops: (() => void)[] = [];
            pi.events.on("hook-call", (label: string) => { calls.push(label); });
            const subscribe = (label: string) => {
              stops.push(pi.on("${hook}", () => { pi.events.emit("hook-call", label); }));
            };
            pi.registerCommand("subscribe", { handler: subscribe });
            pi.registerCommand("unsubscribe", {
              handler: () => { for (const stop of stops.splice(0)) stop(); }
            });
            pi.registerCommand("read", {
              handler: () => JSON.stringify(calls.splice(0))
            });
            ${index === 1 ? 'subscribe("B1");' : initiallySubscribed ? 'subscribe("A1");' : ''}
          }
        `)
      }

      const host = new ExtensionHost()
      const manifest = parseGenerationManifest(await host.dispatch(JSON.stringify({
        type: 'prepareGeneration',
        request: { projectTrusted: true, extensionPaths: paths, mode: 'print' },
      })))
      t.after(() => host.dispatch(JSON.stringify({ type: 'retireGeneration', generationId: manifest.generationId })))
      let invocation = 0
      const command = async (extension: number, name: string, arguments_ = '') => {
        const registration = manifest.agentPlugins[extension]?.commands.find(command => command.name === name)
        assert.ok(registration)
        return parseJson(await host.dispatch(JSON.stringify({
          type: 'invoke',
          invocation: {
            invocationId: String(invocation++), generationId: manifest.generationId,
            callbackId: registration.callbackId, kind: 'command',
            payload: { arguments: arguments_, context: {} },
          },
        }))) as { text: string }
      }
      const snapshot = async () => parseJson(await host.dispatch(JSON.stringify({
        type: 'snapshotHooks', generationId: manifest.generationId, hook,
      }))) as { callbacks: { pluginId: string; callbackId: string }[] }
      const dispatch = async (handlers: Awaited<ReturnType<typeof snapshot>>) => {
        for (const callback of handlers.callbacks) {
          await host.dispatch(JSON.stringify({
            type: 'invoke',
            invocation: {
              invocationId: String(invocation++), generationId: manifest.generationId,
              callbackId: callback.callbackId, kind,
              payload: { event: { type: hook }, context: { pluginId: callback.pluginId } },
            },
          }))
        }
        return JSON.parse((await command(0, 'read')).text) as string[]
      }

      const initial = await snapshot()
      await command(0, 'subscribe', 'A2')
      assert.deepEqual(await dispatch(initial), initiallySubscribed ? ['A1', 'B1'] : ['B1'])
      const updated = await snapshot()
      const expected = initiallySubscribed ? ['A1', 'A2', 'B1'] : ['A2', 'B1']
      assert.deepEqual(await dispatch(updated), expected)

      await command(0, 'unsubscribe')
      assert.deepEqual(await dispatch(updated), expected, 'removal must preserve an in-flight snapshot')
      assert.deepEqual(await dispatch(await snapshot()), ['B1'])
      await command(0, 'subscribe', 'A3')
      await command(1, 'subscribe', 'B2')
      await command(0, 'subscribe', 'A4')
      assert.deepEqual(await dispatch(await snapshot()), ['A3', 'A4', 'B1', 'B2'])
    })
  }
}
