import assert from 'node:assert/strict'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test, { type TestContext } from 'node:test'

import { ExtensionHost } from '../src/extension-host.js'
import { parseGenerationManifest, parseJson, type InvocationKind } from '../src/extension-protocol.js'
import type { NativeExtensionContext } from '../src/native-binding.js'

async function fixture(t: TestContext, body: string) {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-product-hooks-'))
  t.after(() => rm(root, { recursive: true, force: true }))
  const path = join(root, 'extension.ts')
  await writeFile(path, `export default function (pi: any) { ${body} }`)
  const host = new ExtensionHost()
  const manifest = parseGenerationManifest(await host.dispatch(JSON.stringify({
    type: 'prepareGeneration', request: { projectTrusted: true, extensionPaths: [path], mode: 'rpc' },
  })))
  t.after(() => host.dispatch(JSON.stringify({ type: 'retireGeneration', generationId: manifest.generationId })))
  let sequence = 0
  const invoke = (callbackId: string, kind: InvocationKind, payload: unknown, native?: NativeExtensionContext, invocationId = String(sequence++)) =>
    host.dispatch(JSON.stringify({ type: 'invoke', invocation: {
      invocationId, generationId: manifest.generationId, callbackId, kind, payload,
    } }), native).then(parseJson)
  const hook = async (name: string, event: unknown) => {
    const registration = manifest.sessionPlugins[0]?.hooks.find(hook => hook.name === name)
    assert.ok(registration, `missing session hook ${name}`)
    return invoke(registration.callbackId, 'sessionHook', { event, context: {} })
  }
  return { host, manifest, hook, invoke }
}

test('model_select and user_bash are active session hooks with Pi payloads', async t => {
  const { hook, manifest } = await fixture(t, `
    pi.on('model_select', (event: unknown) => event);
    pi.on('thinking_level_select', (event: unknown) => event);
    pi.on('user_bash', (event: { command: string; cwd: string; excludeFromContext: boolean }) => ({
      result: { output: JSON.stringify(event), exitCode: undefined, cancelled: true, truncated: false }
    }));
  `)
  assert.deepEqual(manifest.sessionPlugins[0]?.hooks.map(hook => hook.name), ['model_select', 'thinking_level_select', 'user_bash'])
  const selection = { type: 'model_select', model: { provider: 'test', id: 'new' }, previousModel: { provider: 'test', id: 'old' }, source: 'cycle' }
  assert.deepEqual(await hook('model_select', selection), selection)
  const thinking = { type: 'thinking_level_select', level: 'high', previousLevel: 'low' }
  assert.deepEqual(await hook('thinking_level_select', thinking), thinking)
  const bash = { type: 'user_bash', command: 'remote command', excludeFromContext: true, cwd: '/remote' }
  assert.deepEqual(await hook('user_bash', bash), {
    result: { output: JSON.stringify(bash), cancelled: true, truncated: false },
  })
})

test('user_bash allows undefined fallback and validates replacement values', async t => {
  const { hook } = await fixture(t, `
    pi.on('user_bash', (event: { command: string }) => {
      switch (event.command) {
        case 'fallback': return undefined;
        case 'null': return null;
        case 'both': return { operations: { exec() {} }, result: { output: '', exitCode: 0, cancelled: false, truncated: false } };
        case 'no exitCode': return { result: { output: '', cancelled: false, truncated: false } };
        case 'invalid exitCode': return { result: { output: '', exitCode: null, cancelled: false, truncated: false } };
        case 'invalid operations': return { operations: { exec: 'not callable' } };
      }
    });
  `)
  const dispatch = (command: string) => hook('user_bash', { type: 'user_bash', command, cwd: '/remote', excludeFromContext: false })
  assert.equal(await dispatch('fallback'), null)
  for (const invalid of ['null', 'both', 'no exitCode', 'invalid exitCode', 'invalid operations']) {
    await assert.rejects(dispatch(invalid), /Invalid user_bash handler result/)
  }
})

test('custom bash operations retain their receiver, stream bytes, and retire after execution', async t => {
  const { hook, invoke } = await fixture(t, `
    const operations = {
      prefix: 'remote',
      async exec(command: string, cwd: string, options: { onData(data: Buffer): void; signal: AbortSignal }) {
        if (this !== operations) throw new Error('receiver changed');
        if (options.signal.aborted) throw new Error('already aborted');
        options.onData(Buffer.from(this.prefix + ':' + command + ':' + cwd + ':中文'));
        return { exitCode: 19 };
      }
    };
    pi.on('user_bash', () => ({ operations }));
  `)
  const prepared = await hook('user_bash', { type: 'user_bash', command: 'command', cwd: '/remote', excludeFromContext: false }) as { operationsCallbackId: string }
  const chunks: Uint8Array[] = []
  const native: NativeExtensionContext = {
    query: () => 'null', notify: () => {}, request: async () => 'null',
    bashData: data => chunks.push(data),
  }
  assert.deepEqual(await invoke(prepared.operationsCallbackId, 'userBash', { command: 'command', cwd: '/remote' }, native), { exitCode: 19 })
  assert.equal(Buffer.concat(chunks).toString('utf8'), 'remote:command:/remote:中文')
  await assert.rejects(invoke(prepared.operationsCallbackId, 'userBash', { command: 'command', cwd: '/remote' }, native), /Unknown JavaScript callback/)
})

test('custom bash operations receive cancellation through their invocation signal', async t => {
  const { host, hook, invoke } = await fixture(t, `
    pi.on('user_bash', () => ({ operations: {
      async exec(_command: string, _cwd: string, options: { onData(data: Buffer): void; signal: AbortSignal }) {
        options.onData(Buffer.from('started'));
        await new Promise<void>(resolve => options.signal.addEventListener('abort', () => resolve(), { once: true }));
        options.onData(Buffer.from('cancelled'));
        return { exitCode: null };
      }
    }}));
  `)
  const prepared = await hook('user_bash', { type: 'user_bash', command: 'wait', cwd: '/remote', excludeFromContext: false }) as { operationsCallbackId: string }
  const chunks: Uint8Array[] = []
  let onStarted = () => {}
  const started = new Promise<void>(resolve => { onStarted = resolve })
  const native: NativeExtensionContext = {
    query: () => 'null', notify: () => {}, request: async () => 'null',
    bashData: data => { chunks.push(data); onStarted() },
  }
  const running = invoke(prepared.operationsCallbackId, 'userBash', { command: 'wait', cwd: '/remote' }, native, 'bash-running')
  await started
  await host.dispatch(JSON.stringify({ type: 'cancel', invocationId: 'bash-running' }))
  assert.deepEqual(await running, { exitCode: null })
  assert.equal(Buffer.concat(chunks).toString('utf8'), 'startedcancelled')
})
