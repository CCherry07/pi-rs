import assert from 'node:assert/strict'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test, { type TestContext } from 'node:test'

import { ExtensionHost } from '../src/extension-host.js'
import { parseGenerationManifest } from '../src/extension-protocol.js'
import type { NativeExtensionContext } from '../src/native-binding.js'

async function fixture(t: TestContext, source: string) {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-context-batch-'))
  const extension = join(root, 'extension.ts')
  await writeFile(extension, source)
  const host = new ExtensionHost()
  const manifest = parseGenerationManifest(await host.dispatch(JSON.stringify({
    type: 'prepareGeneration',
    request: { projectTrusted: true, agentDir: join(root, 'agent'), extensionPaths: [extension], mode: 'print', cwd: root },
  })))
  t.after(async () => {
    await host.dispatch(JSON.stringify({ type: 'retireGeneration', generationId: manifest.generationId }))
    await rm(root, { recursive: true, force: true })
  })
  const plugin = manifest.agentPlugins[0]
  assert.ok(plugin)
  const callbacks = plugin.hooks.map(hook => ({ callbackId: hook.callbackId, context: { cwd: root } }))
  const read = async (nativeContext?: NativeExtensionContext) => {
    const command = plugin.commands[0]
    assert.ok(command)
    const result = JSON.parse(await host.dispatch(JSON.stringify({
      type: 'invoke', invocation: {
        invocationId: 'read', generationId: manifest.generationId, callbackId: command.callbackId,
        kind: 'command', payload: { arguments: '', context: { cwd: root } },
      },
    }), nativeContext)) as { text: string }
    return JSON.parse(result.text)
  }
  return { host, generationId: manifest.generationId, callbacks, read }
}

test('context batches isolate inputs and yield replacements for Rust validation before continuing', async (t) => {
  const { host, generationId, callbacks, read } = await fixture(t, `
    export default (pi: any) => {
      const seen: string[] = [];
      let retained: any;
      let signal: AbortSignal;
      pi.on("context", (event: any, ctx: any) => {
        seen.push("first:" + event.messages[0].content);
        retained = event;
        signal = ctx.signal;
        event.messages[0].content = "unreturned mutation";
      });
      pi.on("context", (event: any) => {
        if (!signal.aborted) throw new Error("previous callback signal is still live");
        seen.push("second:" + event.messages[0].content);
        throw new Error("hook failed");
      });
      pi.on("context", () => ({ messages: 42 }));
      pi.on("context", (event: any) => {
        seen.push("replace:" + event.messages[0].content);
        return { messages: [{ role: "user", content: "replacement", timestamp: 2 }] };
      });
      pi.on("context", (event: any) => {
        seen.push("after:" + event.messages[0].content);
        const cycle: any = {}; cycle.self = cycle; return cycle;
      });
      pi.on("context", (event: any) => { seen.push("last:" + event.messages[0].content); });
      pi.registerCommand("read", { handler: () => ({ action: "transform", text: JSON.stringify({ seen, retained: retained.messages[0].content }) }) });
    };
  `)
  const original = [{ role: 'user', content: 'original', timestamp: 1 }]
  const batch = async (start: number, messages: object[]) => JSON.parse(await host.dispatch(JSON.stringify({
    type: 'invokeContextHookBatch',
    invocation: { invocationId: `batch-${start}`, generationId, callbacks: callbacks.slice(start), messages },
  }))) as { processed: number; errors: { callbackId: string; message: string }[]; replacement?: { messages: object[] } }

  const first = await batch(0, original)
  assert.equal(first.processed, 3)
  assert.deepEqual(first.replacement, { messages: 42 })
  assert.deepEqual(first.errors, [{ callbackId: callbacks[1]?.callbackId, message: 'hook failed' }])
  // Rust rejects the invalid result, then resumes from the unchanged input.
  const second = await batch(3, original)
  assert.equal(second.processed, 1)
  assert.deepEqual(second.errors, [])
  assert.ok(second.replacement)
  const last = await batch(4, second.replacement.messages)
  assert.equal(last.processed, 2)
  assert.equal(last.replacement, undefined)
  assert.equal(last.errors.length, 1)
  assert.equal(last.errors[0]?.callbackId, callbacks[4]?.callbackId)
  assert.match(last.errors[0]?.message ?? '', /circular/i)
  assert.deepEqual(await read(), {
    seen: ['first:original', 'second:original', 'replace:original', 'after:replacement', 'last:replacement'],
    retained: 'unreturned mutation',
  })
  assert.deepEqual(original, [{ role: 'user', content: 'original', timestamp: 1 }])
})

test('cancelling a context batch aborts the active callback and skips the rest', { timeout: 10_000 }, async (t) => {
  const { host, generationId, callbacks, read } = await fixture(t, `
    export default (pi: any) => {
      let calls = 0;
      pi.on("context", async (_event: any, ctx: any) => {
        ctx.sessionManager.appendSessionInfo("context-started");
        await new Promise<void>(resolve => {
          if (ctx.signal.aborted) resolve();
          else ctx.signal.addEventListener("abort", () => resolve(), { once: true });
        });
      });
      pi.on("context", () => { calls++; });
      pi.registerCommand("read", { handler: () => ({ action: "transform", text: JSON.stringify(calls) }) });
    };
  `)
  let markStarted: () => void = () => {}
  const started = new Promise<void>(resolve => { markStarted = resolve })
  const pending = host.dispatch(JSON.stringify({
    type: 'invokeContextHookBatch',
    invocation: { invocationId: 'cancel-context', generationId, callbacks, messages: [] },
  }), {
    query: () => 'null',
    notify: raw => {
      const operation = JSON.parse(raw) as { type: string; name: string }
      if (operation.type === 'setSessionName' && operation.name === 'context-started') markStarted()
    },
    request: async () => 'null',
  })
  await started
  await host.dispatch(JSON.stringify({ type: 'cancel', invocationId: 'cancel-context' }))
  assert.deepEqual(JSON.parse(await pending), { processed: 1, errors: [] })
  assert.equal(await read(), 0)
})

test('each context callback rebinds retained Pi APIs after an interleaved invocation', { timeout: 10_000 }, async (t) => {
  const { host, generationId, callbacks, read } = await fixture(t, `
    export default (pi: any) => {
      const seen: string[][] = [];
      let resume: (() => void) | undefined;
      pi.on("context", async () => {
        await new Promise<void>(resolve => {
          resume = resolve;
          pi.setSessionName("context-started");
        });
      });
      pi.on("context", () => { seen.push(pi.getActiveTools()); });
      pi.registerCommand("read", { handler: () => {
        resume?.();
        return { action: "transform", text: JSON.stringify(seen) };
      } });
    };
  `)
  let markStarted: () => void = () => {}
  const started = new Promise<void>(resolve => { markStarted = resolve })
  const pending = host.dispatch(JSON.stringify({
    type: 'invokeContextHookBatch',
    invocation: { invocationId: 'rebind-context', generationId, callbacks, messages: [] },
  }), {
    query: () => JSON.stringify(['original-session']),
    notify: () => { markStarted() },
    request: async () => 'null',
  })
  await started
  await read({
    query: () => JSON.stringify(['interleaved-session']),
    notify: () => {},
    request: async () => 'null',
  })
  assert.deepEqual(JSON.parse(await pending), { processed: 2, errors: [] })
  assert.deepEqual(await read(), [['original-session']])
})

test('context batches end the callback signal before serializing its result', async (t) => {
  const { host, generationId, callbacks } = await fixture(t, `
    export default (pi: any) => {
      pi.on("context", (_event: any, ctx: any) => ({
        toJSON: () => ({ messages: [{ role: "user", content: ctx.signal.aborted ? "finished" : "running", timestamp: 1 }] }),
      }));
    };
  `)
  const entry = callbacks[0]
  assert.ok(entry)
  const individual = JSON.parse(await host.dispatch(JSON.stringify({
    type: 'invoke', invocation: {
      invocationId: 'individual-result', generationId, callbackId: entry.callbackId, kind: 'agentHook',
      payload: { hook: 'context', context: entry.context, event: { type: 'context', messages: [] } },
    },
  })))
  assert.equal(individual.messages[0].content, 'finished')
  const batch = JSON.parse(await host.dispatch(JSON.stringify({
    type: 'invokeContextHookBatch',
    invocation: { invocationId: 'batch-result', generationId, callbacks, messages: [] },
  })))
  assert.deepEqual(batch, { processed: 1, errors: [], replacement: individual })
})
