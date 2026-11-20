import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { Fixture } from '../lib/fixture.mjs';

test('loopback provider holds before headers and releases only the exact controller frames', { timeout: 4000 }, async t => {
  const directory = await mkdtemp(join(tmpdir(), 'rpc-acceptance-fixture-'));
  const fixture = new Fixture();
  await fixture.listen();
  fixture.reset(directory);
  t.after(async () => { await fixture.close(); await rm(directory, { recursive: true, force: true }); });
  const response = fetch(fixture.url + '/chat/completions', {
    method: 'POST', headers: { 'content-type': 'application/json', authorization: 'test header must not be recorded' },
    body: JSON.stringify({ model: 'fixture', messages: [{ role: 'user', content: 'barrier' }] }),
  });
  const request = await fixture.request(0);
  assert.equal(request.opened, false);
  assert.equal(request.done, false);
  fixture.text(0, 'exact delta', { role: true });
  fixture.finish(0);
  const body = await (await response).text();
  assert.match(body, /exact delta/);
  assert.ok(body.endsWith('data: [DONE]\n\n'));
  await fixture.disconnected();
  const evidence = await readFile(join(directory, 'fixture.jsonl'), 'utf8');
  assert.ok(!evidence.includes('authorization'));
  assert.ok(!evidence.includes('test header must not be recorded'));
  assert.deepEqual(request.body.messages, [{ role: 'user', content: 'barrier' }]);
});

test('shutdown gate is an explicit request/release handshake, independent from RPC stdout', { timeout: 4000 }, async t => {
  const fixture = new Fixture();
  await fixture.listen();
  t.after(() => fixture.close());
  const response = fetch(fixture.url + '/control/gate', { method: 'POST', body: JSON.stringify({ name: 'shutdown' }) });
  const gate = await fixture.gate(0);
  assert.equal(gate.released, false);
  fixture.releaseGate(0, 'probe');
  assert.deepEqual(await (await response).json(), { action: 'probe' });
});
