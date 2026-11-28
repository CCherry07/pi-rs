import assert from 'node:assert/strict'
import { mkdtemp, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'

import { ExtensionHost } from '../src/extension-host.js'
import { parseGenerationManifest } from '../src/extension-protocol.js'

async function prepare(paths: string[]) {
  return parseGenerationManifest(await new ExtensionHost().dispatch(JSON.stringify({
    type: 'prepareGeneration',
    request: { projectTrusted: true, extensionPaths: paths, mode: 'print' },
  })))
}

test('captures validated load-time MCP registrations with extension ownership', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-mcp-registration-'))
  const extension = join(root, 'extension.ts')
  await writeFile(extension, `
    export default function (pi: any) {
      pi.registerMcpServer('docs', { command: 'first', exposure: 'codemode-deferred' });
      const copies = pi.getMcpServers();
      copies[0].config.command = 'mutated-copy';
      pi.registerMcpServer('docs', { command: 'second', args: ['--stdio'], exposure: 'direct' });
      pi.registerMcpServer('temporary', { url: 'https://example.com/mcp' });
      pi.unregisterMcpServer('temporary');
    }
  `)

  const manifest = await prepare([extension])
  assert.deepEqual(manifest.mcpServerRegistrations, [{
    pluginId: manifest.agentPlugins[0]?.id,
    path: extension,
    name: 'docs',
    config: { command: 'second', args: ['--stdio'], exposure: 'direct' },
  }])
})

test('rejects MCP ownership and normalized namespace collisions across extensions', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-mcp-collision-'))
  const first = join(root, 'first.ts')
  const sameName = join(root, 'same-name.ts')
  const sameNamespace = join(root, 'same-namespace.ts')
  await writeFile(first, `export default (pi: any) => pi.registerMcpServer('dev-radius', { command: 'one' })`)
  await writeFile(sameName, `export default (pi: any) => pi.registerMcpServer('dev-radius', { command: 'two' })`)
  await writeFile(sameNamespace, `export default (pi: any) => pi.registerMcpServer('dev_radius', { command: 'two' })`)

  await assert.rejects(prepare([first, sameName]), /already registered by/)
  await assert.rejects(prepare([first, sameNamespace]), /namespace "dev_radius".*conflicts/)
})

test('ignores unregister attempts from an extension that does not own the MCP server', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-mcp-owner-'))
  const owner = join(root, 'owner.ts')
  const other = join(root, 'other.ts')
  await writeFile(owner, `export default (pi: any) => pi.registerMcpServer('docs', { command: 'owner' })`)
  await writeFile(other, `export default (pi: any) => pi.unregisterMcpServer('docs')`)

  const manifest = await prepare([owner, other])
  assert.equal(manifest.mcpServerRegistrations.length, 1)
  assert.equal(manifest.mcpServerRegistrations[0]?.path, owner)
})

test('rejects invalid extension MCP registrations before emitting a manifest', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-mcp-invalid-'))
  const fixtures: Array<[string, string, RegExp]> = [
    ['bad name', `{ command: 'server' }`, /invalid MCP server name/],
    ['empty', `{ command: '  ' }`, /command must be a non-empty string/],
    ['legacy', `{ type: 'sse', url: 'https:\/\/example.com\/sse' }`, /legacy SSE transport is not supported/],
    ['headers', `{ url: 'https:\/\/example.com\/mcp', headers: { Authorization: false } }`, /headers must map names to strings/],
    ['timeout', `{ command: 'server', timeout: Infinity }`, /timeout must be a positive number/],
    ['credentials', `{ url: 'https:\/\/user:secret-marker@example.com\/mcp' }`, /without credentials or a fragment/],
    ['fragment', `{ url: 'https:\/\/example.com\/mcp#secret-marker' }`, /without credentials or a fragment/],
    ['callback-credentials', `{ url: 'https:\/\/example.com\/mcp', oauth: { callbackUrl: 'http:\/\/user:secret-marker@localhost\/callback' } }`, /oauth.callbackUrl must be an http URI on localhost/],
    ['metadata-fragment', `{ url: 'https:\/\/example.com\/mcp', oauth: { authServerMetadataUrl: 'https:\/\/auth.example.com\/metadata#secret-marker' } }`, /oauth.authServerMetadataUrl must be an https URL/],
    ['auth-space', `{ url: 'https:\/\/example.com\/mcp', auth: { provider: '  ' } }`, /auth.provider must be a provider name/],
    ['auth', `{ url: 'http:\/\/example.com\/mcp', auth: { provider: 'example' } }`, /auth requires an https URL/],
  ]
  for (const [name, config, error] of fixtures) {
    const extension = join(root, `${name.replaceAll(' ', '-')}.ts`)
    await writeFile(extension, `export default (pi: any) => pi.registerMcpServer(${JSON.stringify(name)}, ${config})`)
    await assert.rejects(prepare([extension]), caught => {
      assert.ok(caught instanceof Error)
      assert.match(caught.message, error)
      if (config.includes('secret-marker')) assert.doesNotMatch(caught.message, /secret-marker/)
      return true
    })
  }
})

test('retiring a replacement generation aborts its active MCP observer and invalidates old callbacks', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-mcp-retirement-'))
  const extension = join(root, 'extension.ts')
  await writeFile(extension, `
    export default function (pi: any) {
      pi.on('mcp_servers_change', async (_event: any, ctx: any) => {
        ctx.ui.notify('observer-started');
        await new Promise<void>(resolve => {
          if (ctx.signal.aborted) resolve();
          else ctx.signal.addEventListener('abort', () => resolve(), { once: true });
        });
        ctx.ui.notify('observer-after-abort');
      });
    }
  `)

  const host = new ExtensionHost()
  const replacement = parseGenerationManifest(await host.dispatch(JSON.stringify({
    type: 'prepareGeneration',
    request: {
      projectTrusted: true,
      extensionPaths: [extension],
      mode: 'print',
      mcpServerMutations: [{
        type: 'register',
        extensionPath: extension,
        name: 'late',
        config: { command: 'server', enabled: false },
      }],
      mcpServersChanged: true,
    },
  })))
  const changeHook = replacement.sessionPlugins[0]?.hooks[0]
  assert.equal(changeHook?.name, 'session_start')
  assert.ok(changeHook)

  let startedResolve: (() => void) | undefined
  const started = new Promise<void>(resolve => { startedResolve = resolve })
  const notifications: Record<string, unknown>[] = []
  const nativeContext = {
    query: () => 'false',
    notify: (operation: string) => {
      const notification = JSON.parse(operation) as Record<string, unknown>
      notifications.push(notification)
      if (notification.type === 'uiNotify' && notification.message === 'observer-started') startedResolve?.()
    },
    request: async () => 'null',
  }
  const invocation = {
    type: 'invoke',
    invocation: {
      invocationId: 'active-observer',
      generationId: replacement.generationId,
      callbackId: changeHook.callbackId,
      kind: 'sessionHook',
      payload: { context: {}, event: { type: 'session_start', reason: 'reload' } },
    },
  }
  const running = host.dispatch(JSON.stringify(invocation), nativeContext)
  await started
  assert.equal(await host.dispatch(JSON.stringify({
    type: 'retireGeneration',
    generationId: replacement.generationId,
  })), 'null')
  await assert.rejects(running, /JavaScript generation is retired/)
  assert.equal(notifications.some(notification => notification.message === 'observer-after-abort'), false)
  await assert.rejects(host.dispatch(JSON.stringify(invocation), nativeContext), /JavaScript generation is retired/)
})

test('forwards runtime MCP mutations and replays them into the next generation', async () => {
  const root = await mkdtemp(join(tmpdir(), 'pi-rs-js-mcp-runtime-'))
  const extension = join(root, 'extension.ts')
  await writeFile(extension, `
    export default function (pi: any) {
      pi.registerMcpServer('base', { command: 'server', enabled: false });
      let observed = -1;
      pi.on('mcp_servers_change', (event: any) => { observed = event.servers.length });
      pi.registerCommand('late-mcp', {
        handler: () => pi.registerMcpServer('late', { command: 'server', enabled: false })
      });
      pi.registerCommand('remove-mcp', {
        handler: () => pi.unregisterMcpServer('late')
      });
      pi.registerCommand('observed-mcp', { handler: () => String(observed) });
    }
  `)

  const host = new ExtensionHost()
  const manifest = parseGenerationManifest(await host.dispatch(JSON.stringify({
    type: 'prepareGeneration',
    request: { projectTrusted: true, extensionPaths: [extension], mode: 'print' },
  })))
  assert.deepEqual(manifest.mcpServerRegistrations.map(server => server.name), ['base'])
  assert.equal(manifest.sessionPlugins.length, 0)
  const notifications: Record<string, unknown>[] = []
  const command = manifest.agentPlugins[0]?.commands.find(candidate => candidate.name === 'late-mcp')
  assert.ok(command)
  await host.dispatch(JSON.stringify({
    type: 'invoke',
    invocation: {
      invocationId: 'late-mcp',
      generationId: manifest.generationId,
      callbackId: command.callbackId,
      kind: 'command',
      payload: { arguments: '', context: {} },
    },
  }), {
    query: () => 'null',
    notify: operation => notifications.push(JSON.parse(operation) as Record<string, unknown>),
    request: async () => 'null',
  })
  assert.deepEqual(notifications, [{
    type: 'registerMcpServer',
    extensionPath: extension,
    name: 'late',
    config: { command: 'server', enabled: false },
  }])

  const replacement = parseGenerationManifest(await host.dispatch(JSON.stringify({
    type: 'prepareGeneration',
    request: {
      projectTrusted: true,
      extensionPaths: [extension],
      mode: 'print',
      mcpServerMutations: [{
        type: 'register',
        extensionPath: extension,
        name: 'late',
        config: { command: 'server', enabled: false },
      }],
      mcpServersChanged: true,
    },
  })))
  assert.deepEqual(replacement.mcpServerRegistrations.map(server => server.name), ['base', 'late'])
  const changeHook = replacement.sessionPlugins[0]?.hooks[0]
  assert.equal(changeHook?.name, 'session_start')
  assert.ok(changeHook)
  await host.dispatch(JSON.stringify({
    type: 'invoke',
    invocation: {
      invocationId: 'mcp-change',
      generationId: replacement.generationId,
      callbackId: changeHook.callbackId,
      kind: 'sessionHook',
      payload: { context: {}, event: { type: 'session_start', reason: 'reload' } },
    },
  }), {
    query: () => 'null',
    notify: () => undefined,
    request: async () => 'null',
  })
  const observed = replacement.agentPlugins[0]?.commands.find(candidate => candidate.name === 'observed-mcp')
  assert.ok(observed)
  const result = JSON.parse(await host.dispatch(JSON.stringify({
    type: 'invoke',
    invocation: {
      invocationId: 'observed-mcp',
      generationId: replacement.generationId,
      callbackId: observed.callbackId,
      kind: 'command',
      payload: { arguments: '', context: {} },
    },
  })))
  assert.deepEqual(result, { action: 'transform', text: '2' })
})
