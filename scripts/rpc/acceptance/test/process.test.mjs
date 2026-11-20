import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { OwnedRpc } from '../lib/process.mjs';

const child = new URL('./probe.mjs', import.meta.url);

test('owned child capture preserves bytes, mixed order, Unicode separators, and real HUP exit', { timeout: 5000 }, async t => {
  const directory = await mkdtemp(join(tmpdir(), 'rpc-acceptance-selftest-'));
  const rpc = new OwnedRpc({ executable: process.execPath, args: [child.pathname], cwd: directory, env: {}, directory, timeoutMs: 2000 });
  t.after(async () => { await rpc.cleanup(); await rm(directory, { recursive: true, force: true }); });
  await rpc.waitFor(record => record.type === 'ready', { label: 'probe ready' });
  await rpc.request('echo', { value: '字🧪\u2028line\u2029' });
  rpc.signal('SIGHUP');
  const result = await rpc.waitForClose();
  assert.equal(result.code, 129);
  assert.equal(result.signal, null);
  assert.deepEqual(rpc.records.map(record => record.type), ['ready', 'notice', 'response']);
  assert.equal(rpc.records[2].data.value, '字🧪\u2028line\u2029');
  assert.deepEqual(rpc.parseErrors, []);
  assert.equal(rpc.trailingBytes, 0);
  assert.match(await readFile(join(directory, 'stderr.raw'), 'utf8'), /observed SIGHUP/);
  assert.match(await readFile(join(directory, 'controller.jsonl'), 'utf8'), /"signal":"SIGHUP"/);
  assert.throws(() => rpc.signal('SIGTERM'), /no longer owned/);
});

test('invalid UTF-8 remains raw evidence and cannot silently normalize into a valid string', { timeout: 5000 }, async t => {
  const directory = await mkdtemp(join(tmpdir(), 'rpc-acceptance-utf8-'));
  const rpc = new OwnedRpc({ executable: process.execPath, args: [child.pathname], cwd: directory, env: {}, directory, timeoutMs: 2000 });
  t.after(async () => { await rpc.cleanup(); await rm(directory, { recursive: true, force: true }); });
  await rpc.waitFor(record => record.type === 'ready');
  await rpc.request('invalid-utf8');
  assert.ok(rpc.parseErrors.some(error => error.error.includes('UTF-8')));
  assert.ok((await readFile(join(directory, 'stdout.raw'))).includes(255));
});

test('a hard deadline records and kills only the owned blocked child', { timeout: 5000 }, async t => {
  const directory = await mkdtemp(join(tmpdir(), 'rpc-acceptance-deadline-'));
  const rpc = new OwnedRpc({ executable: process.execPath, args: [child.pathname], cwd: directory, env: {}, directory, timeoutMs: 200 });
  t.after(async () => { await rpc.cleanup(); await rm(directory, { recursive: true, force: true }); });
  await rpc.waitFor(record => record.type === 'ready');
  const result = await rpc.waitForClose();
  assert.equal(result.timedOut, true);
  assert.equal(result.signal, 'SIGKILL');
  assert.equal(result.pgid, result.pid);
  const actions = (await readFile(join(directory, 'controller.jsonl'), 'utf8')).trim().split('\n').map(line => JSON.parse(line));
  const signal = actions.find(action => action.action === 'cleanup-signal');
  assert.equal(signal.pid, result.pid);
  assert.equal(signal.reason, 'hard-deadline');
  assert.ok(actions.findIndex(action => action.action === 'hard-deadline') < actions.findIndex(action => action.action === 'process-close'));
});
