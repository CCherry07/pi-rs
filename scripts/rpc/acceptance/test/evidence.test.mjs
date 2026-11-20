import assert from 'node:assert/strict';
import { mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { fingerprint, isolatedEnvironment } from '../lib/evidence.mjs';

test('runtime inventory detects changed bytes and newly added dependency files', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'rpc-inventory-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  await writeFile(join(directory, 'runtime.js'), 'original');
  const before = await fingerprint(directory);
  await writeFile(join(directory, 'runtime.js'), 'changed');
  assert.notEqual(await fingerprint(directory), before);
  await writeFile(join(directory, 'runtime.js'), 'original');
  assert.equal(await fingerprint(directory), before);
  await writeFile(join(directory, 'dependency.js'), 'additional');
  assert.notEqual(await fingerprint(directory), before);
});

test('runtime inventory refuses unpinned external symlinks', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'rpc-external-link-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  await symlink(process.execPath, join(directory, 'outside'));
  await assert.rejects(fingerprint(directory), /Unpinned external symlink/);
});

test('product environment never copies ambient provider credentials or proxies', () => {
  const previous = process.env.OPENAI_API_KEY;
  process.env.OPENAI_API_KEY = 'self-test-sentinel';
  try {
    const env = isolatedEnvironment('/isolated');
    assert.equal(env.OPENAI_API_KEY, undefined);
    assert.equal(env.HOME, '/isolated/home');
    assert.equal(env.PI_AGENT_DIR, '/isolated/home/.pi/agent');
    assert.equal(env.HTTPS_PROXY, undefined);
  } finally {
    if (previous === undefined) delete process.env.OPENAI_API_KEY; else process.env.OPENAI_API_KEY = previous;
  }
});
