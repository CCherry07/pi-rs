import assert from 'node:assert/strict';

const source = 'legacy/pi@05fd3fbda0bbb9a1c7a76e4bf92ab58ed77c8d94';
export const SCENARIOS = [
  { id: 'eof-probes', family: 'eof' },
  { id: 'eof-active', family: 'active-stop', stop: 'EOF' },
  { id: 'hup-idle', family: 'idle-stop', stop: 'SIGHUP' },
  { id: 'term-idle', family: 'idle-stop', stop: 'SIGTERM' },
  { id: 'hup-active', family: 'active-stop', stop: 'SIGHUP' },
  { id: 'term-active', family: 'active-stop', stop: 'SIGTERM' },
  { id: 'queue-idle-clear', family: 'queue-idle' },
  { id: 'queue-order', family: 'queue-order' },
  { id: 'clear-then-abort', family: 'clear-abort' },
  { id: 'abort-before-headers', family: 'abort-before' },
  { id: 'abort-continues-queue', family: 'abort-queue' },
  { id: 'queue-lifecycle-only', family: 'abort-queue', historyQueries: false },
  { id: 'stream-batched', family: 'batched' },
  { id: 'backpressure-eof', family: 'backpressure', stop: 'EOF' },
  { id: 'backpressure-hup', family: 'backpressure', stop: 'SIGHUP' },
  { id: 'backpressure-term', family: 'backpressure', stop: 'SIGTERM', allowIncompleteOutput: true },
  { id: 'hup-dispose', family: 'dispose', stop: 'SIGHUP', fixture: 'shutdown', holdShutdown: true },
  { id: 'term-dispose', family: 'dispose', stop: 'SIGTERM', fixture: 'shutdown', holdShutdown: true },
  { id: 'eof-then-term', family: 'dispose', stop: 'EOF', following: 'SIGTERM', fixture: 'shutdown', holdShutdown: true },
  { id: 'repeated-hup', family: 'repeat', stop: 'SIGHUP', following: 'SIGHUP', fixture: 'shutdown', holdShutdown: true },
  { id: 'repeated-term', family: 'repeat', stop: 'SIGTERM', following: 'SIGTERM', fixture: 'shutdown', holdShutdown: true },
  { id: 'term-then-hup', family: 'repeat', stop: 'SIGTERM', following: 'SIGHUP', fixture: 'shutdown', holdShutdown: true },
  { id: 'eof-then-repeated-term', family: 'eof-repeat', stop: 'EOF', fixture: 'shutdown', holdShutdown: true },
  { id: 'ui-client-cancel', family: 'ui-cancel', fixture: 'input-ui' },
  { id: 'ui-abort', family: 'ui-abort', fixture: 'tool-ui', tools: ['write'] },
  { id: 'ui-eof', family: 'ui-stop', stop: 'EOF', fixture: 'background-ui', holdShutdown: true },
  { id: 'ui-hup', family: 'ui-stop', stop: 'SIGHUP', fixture: 'background-ui', holdShutdown: true },
  { id: 'ui-term', family: 'ui-stop', stop: 'SIGTERM', fixture: 'background-ui', holdShutdown: true },
].map(scenario => ({
  ...scenario,
  oracleSource: `${source}/packages/coding-agent/src/modes/rpc/rpc-mode.ts:377-403,440-457,735-834`,
  queueSource: `${source}/packages/coding-agent/src/core/agent-session.ts:2378-2420,2584-2627`,
  scope: scenario.historyQueries === false ? 'Queue lifecycle only: no history-query inputs; compare every emitted stdout record in order.'
    : scenario.family === 'batched' ? 'Unpaced SSE; preserves live snapshot/usage differences.'
    : 'Only this explicit RPC/provider/control-barrier schedule; not arbitrary scheduling equivalence.',
}));

const text = message => typeof message.content === 'string' ? message.content
  : (message.content ?? []).filter(block => block.type === 'text').map(block => block.text).join('');
const receiptIndex = (rpc, id) => rpc.records.findIndex(record => record.type === 'response' && record.id === id);
const settledIndex = rpc => rpc.records.findLastIndex(record => record.type === 'agent_settled');

export async function executeScenario(scenario, { rpc, fixture, observations }) {
  const ok = async (type, fields) => {
    const response = await rpc.request(type, fields);
    assert.equal(response.success, true, `${type}: ${JSON.stringify(response)}`);
    return response.data;
  };
  const waitEvent = (type, after = 0) => rpc.waitFor(record => record.type === type, { after, label: type });
  const settled = async (after = 0) => {
    const event = await waitEvent('agent_settled', after);
    assert.equal((await ok('get_state')).isStreaming, false);
    return event;
  };
  const history = async () => {
    await ok('get_messages');
    // Legitimate native v4 checkpoints are intentionally kept in the compared stream.
    await ok('get_entries');
  };
  const stop = async method => {
    if (method === 'EOF') await rpc.endInput(); else rpc.signal(method);
  };
  const close = async () => { await rpc.endInput(); return 0; };
  const expectedCode = method => method === 'SIGHUP' ? 129 : method === 'SIGTERM' ? 143 : 0;
  const delta = async (index, value) => {
    await fixture.request(index);
    const after = rpc.records.length;
    fixture.text(index, value, { role: true });
    await rpc.waitFor(record => record.type === 'message_update'
      && record.assistantMessageEvent?.type === 'text_delta'
      && record.assistantMessageEvent.delta === value, { after, label: `observed delta ${value}` });
  };
  const reply = async (index, value = `answer-${index}`) => {
    await delta(index, value);
    fixture.finish(index);
  };
  const start = async () => {
    assert.deepEqual(await ok('prompt', { message: 'acceptance root' }), { disposition: 'started' });
    await fixture.request(0);
  };
  const queue = async () => {
    assert.deepEqual(await ok('steer', { message: 'steer-one' }), { disposition: 'queued' });
    assert.deepEqual(await ok('follow_up', { message: 'follow-one' }), { disposition: 'queued' });
    assert.equal((await ok('get_state')).pendingMessageCount, 2);
  };
  const assertAbort = async id => {
    const response = await rpc.response(id);
    assert.equal(response.success, true, JSON.stringify(response));
    assert.ok(settledIndex(rpc) >= 0 && settledIndex(rpc) < receiptIndex(rpc, id), 'abort response must follow final settlement');
    assert.equal((await settled()).aborted, true);
  };
  const pulseShutdown = async index => {
    // An actual HTTP round trip while the shutdown hook remains held, not a sleep.
    const after = fixture.events.length;
    fixture.releaseGate(index, 'probe');
    await fixture.event('shutdown-pulse', after);
    await fixture.gate(index + 1);
    assert.equal(rpc.exited, false, 'one signal must not bypass the held cleanup');
  };

  if (scenario.fixture) await fixture.event('fixture-ready');
  if (scenario.fixture === 'background-ui') {
    await rpc.waitFor(record => record.type === 'extension_ui_request' && record.method === 'confirm', { label: 'startup background UI' });
  }
  const ready = await ok('get_state');
  assert.equal(ready.isStreaming, false);
  assert.equal(ready.sessionFile, undefined, '--no-session must not expose a persistent session file');

  switch (scenario.family) {
    case 'eof': {
      const commands = ['get_state', 'get_available_models', 'get_last_assistant_text'].map(type => ({ type, id: rpc.nextId }));
      await rpc.endInput(commands);
      await rpc.waitForClose();
      for (const command of commands) assert.equal((await rpc.response(command.id)).success, true);
      assert.equal(fixture.requests.length, 0);
      return 0;
    }
    case 'idle-stop':
      await stop(scenario.stop);
      return expectedCode(scenario.stop);
    case 'active-stop':
      await start();
      await delta(0, 'held partial');
      await stop(scenario.stop);
      await fixture.disconnected();
      observations.providerCancelledBeforeDone = !fixture.requests[0].done;
      return expectedCode(scenario.stop);
    case 'queue-idle': {
      for (const [type, message] of [['steer', 'same'], ['follow_up', 'follow-one'], ['steer', 'same'], ['follow_up', 'follow-two']]) {
        assert.deepEqual(await ok(type, { message }), { disposition: 'queued' });
      }
      assert.equal((await ok('get_state')).pendingMessageCount, 4);
      const before = rpc.records.length;
      assert.deepEqual(await ok('clear_queue'), { steering: ['same', 'same'], followUp: ['follow-one', 'follow-two'] });
      const updates = rpc.records.slice(before).filter(record => record.type === 'queue_update');
      assert.deepEqual(updates, [{ type: 'queue_update', steering: [], followUp: [] }]);
      assert.equal((await ok('get_state')).pendingMessageCount, 0);
      assert.deepEqual(await ok('clear_queue'), { steering: [], followUp: [] });
      assert.equal(fixture.requests.length, 0);
      return close();
    }
    case 'queue-order': {
      await ok('set_steering_mode', { mode: 'one-at-a-time' });
      await ok('set_follow_up_mode', { mode: 'one-at-a-time' });
      await start();
      await queue();
      assert.deepEqual(await ok('prompt', { message: 'steer-two', streamingBehavior: 'steer' }), { disposition: 'queued' });
      assert.deepEqual(await ok('prompt', { message: 'follow-two', streamingBehavior: 'followUp' }), { disposition: 'queued' });
      const rejected = await rpc.request('prompt', { message: 'must reject without streamingBehavior' });
      assert.equal(rejected.success, false);
      assert.equal((await ok('get_state')).pendingMessageCount, 4);
      const expected = ['acceptance root', 'steer-one', 'steer-two', 'follow-one', 'follow-two'];
      for (let index = 0; index < expected.length; index++) {
        const request = await fixture.request(index);
        assert.deepEqual(request.body.messages.filter(message => message.role === 'user').map(text), expected.slice(0, index + 1));
        await reply(index);
      }
      assert.equal((await settled()).aborted, false);
      const messages = (await ok('get_messages')).messages;
      assert.deepEqual(messages.filter(message => message.role === 'user').map(text), expected);
      await ok('get_entries');
      assert.equal(fixture.requests.length, 5);
      return close();
    }
    case 'clear-abort': {
      await start();
      await delta(0, 'abort partial');
      await queue();
      assert.deepEqual(await ok('clear_queue'), { steering: ['steer-one'], followUp: ['follow-one'] });
      await assertAbort(await rpc.command('abort'));
      await fixture.disconnected();
      assert.equal((await ok('get_state')).pendingMessageCount, 0);
      assert.equal(fixture.requests.length, 1, 'cleared input must not make a subsequent provider call');
      await history();
      return close();
    }
    case 'abort-before': {
      await start();
      assert.equal(fixture.requests[0].opened, false);
      await assertAbort(await rpc.command('abort'));
      await fixture.disconnected();
      await history();
      return close();
    }
    case 'abort-queue': {
      await start();
      await delta(0, 'abort then continue');
      assert.deepEqual(await ok('follow_up', { message: 'surviving follow-up' }), { disposition: 'queued' });
      await assertAbort(await rpc.command('abort'));
      await fixture.disconnected();
      const idle = await ok('get_state');
      assert.equal(idle.isStreaming, false);
      assert.equal(idle.pendingMessageCount, 1, 'abort is not clear_queue');
      assert.equal(fixture.requests.length, 1, 'abort must not implicitly start a queued continuation');
      if (scenario.historyQueries !== false) {
        assert.deepEqual((await ok('get_messages')).messages.filter(message => message.role === 'user').map(text), ['acceptance root']);
      }
      observations.queueAfterAbort = { pendingMessageCount: idle.pendingMessageCount, providerRequests: fixture.requests.length };

      // Pinned agent-session.ts:1985 skips continue after abort; a new prompt resets the run.
      // agent-loop.ts:254 returns on aborted, before draining follow-ups at :311.
      const resumedAfter = rpc.records.length;
      assert.deepEqual(await ok('prompt', { message: 'resume after abort' }), { disposition: 'started' });
      const expected = ['acceptance root', 'resume after abort', 'surviving follow-up'];
      for (let index = 1; index <= 2; index++) {
        const request = await fixture.request(index);
        assert.deepEqual(request.body.messages.filter(message => message.role === 'user').map(text), expected.slice(0, index + 1));
        await reply(index);
      }
      assert.equal((await settled(resumedAfter)).aborted, false, 'observe the new run, not the old aborted settlement');
      assert.equal((await ok('get_state')).pendingMessageCount, 0);
      assert.equal(fixture.requests.length, 3);
      if (scenario.historyQueries !== false) {
        assert.deepEqual((await ok('get_messages')).messages.filter(message => message.role === 'user').map(text), expected);
        await history();
      }
      return close();
    }
    case 'batched':
      await start();
      fixture.text(0, 'fast one ', { role: true });
      fixture.text(0, 'fast two');
      fixture.finish(0);
      await settled();
      await history();
      return close();
    case 'backpressure': {
      // rpc-mode.ts:672 legally trims names. Keep separators inside, not at the end.
      const name = '背压🧪\u2028\u2029'.repeat(96 * 1024) + 'end';
      await ok('set_session_name', { name });
      rpc.pauseAfterBytes();
      const id = await rpc.command('get_state');
      await rpc.waitForPause();
      assert.equal(receiptIndex(rpc, id), -1, 'large JSON record must still be incomplete when reading is paused');
      observations.backpressure = {
        expectedNameBytes: Buffer.byteLength(name), bytesReadAtPause: rpc.bytes,
        witness: 'Reader paused within a >1 MiB JSON record, not after a fully consumed catalog.',
      };
      await stop(scenario.stop);
      rpc.resumeOutput();
      await rpc.waitForClose();
      if (scenario.stop !== 'SIGTERM') assert.equal((await rpc.response(id)).data.sessionName, name);
      observations.flush = scenario.stop === 'SIGTERM'
        ? 'Oracle rpc-mode.ts:743 deliberately skips flush for TERM; partial output is recorded, not repaired.'
        : 'Complete large response required after output resumes.';
      return expectedCode(scenario.stop);
    }
    case 'dispose':
    case 'repeat':
    case 'eof-repeat': {
      await stop(scenario.stop);
      await fixture.event('shutdown-enter');
      await fixture.gate();
      assert.equal(fixture.events.filter(event => event.name === 'shutdown-enter').length, 1);
      if (scenario.family === 'repeat') {
        rpc.signal(scenario.following);
        await rpc.waitForClose();
        assert.ok(!fixture.events.some(event => event.name === 'shutdown-complete'), 'second signal must exit before releasing the hook');
        return expectedCode(scenario.following);
      }
      let gateIndex = 0;
      if (scenario.following || scenario.family === 'eof-repeat') {
        rpc.signal('SIGTERM');
        await pulseShutdown(0);
        gateIndex = 1;
      }
      if (scenario.family === 'eof-repeat') {
        rpc.signal('SIGTERM');
        await rpc.waitForClose();
        assert.ok(!fixture.events.some(event => event.name === 'shutdown-complete'));
        return 143;
      }
      fixture.releaseGate(gateIndex);
      await fixture.event('shutdown-complete');
      return expectedCode(scenario.stop);
    }
    case 'ui-cancel': {
      let retired;
      for (let index = 0; index < 2; index++) {
        const after = rpc.records.length;
        const id = await rpc.command('prompt', { message: `UI input ${index}` });
        const dialog = await rpc.waitFor(record => record.type === 'extension_ui_request' && record.method === 'confirm', { after, label: 'input UI dialog' });
        if (retired) {
          assert.notEqual(dialog.id, retired, 'a new UI request must have a new identity');
          await rpc.send({ type: 'extension_ui_response', id: retired, confirmed: true });
          await ok('get_state'); // The stale response precedes this command on the same stdin stream.
          assert.equal(receiptIndex(rpc, id), -1, 'a stale reply must not release a new UI waiter');
        }
        const eventStart = fixture.events.length;
        await rpc.send({ type: 'extension_ui_response', id: dialog.id, cancelled: true });
        assert.equal((await fixture.event('ui-released', eventStart)).data.answer, false);
        assert.deepEqual((await rpc.response(id)).data, { disposition: 'handled' });
        retired = dialog.id;
      }
      assert.equal(fixture.requests.length, 0);
      return close();
    }
    case 'ui-abort': {
      await start();
      fixture.tool();
      fixture.finish(0, 'tool_calls');
      await rpc.waitFor(record => record.type === 'extension_ui_request' && record.method === 'confirm', { label: 'tool UI waiter' });
      await assertAbort(await rpc.command('abort'));
      observations.uiRelease = fixture.events.find(event => event.name === 'ui-released') ?? null;
      observations.uiReleaseCaveat = 'A resolved UI marker proves resolution; its absence may mean the callback future was dropped, not a leaked waiter.';
      await history();
      return close();
    }
    case 'ui-stop': {
      await rpc.waitFor(record => record.type === 'extension_ui_request' && record.method === 'confirm', { label: 'background UI waiter' });
      await stop(scenario.stop);
      await fixture.event('shutdown-enter');
      await fixture.gate();
      await pulseShutdown(0);
      observations.uiReleaseAtHeldShutdown = fixture.events.find(event => event.name === 'ui-released') ?? null;
      observations.uiReleaseCaveat = 'Exit and output closure do not prove UI-promise resolution. Missing marker remains unobserved; no fixture cancellation is supplied.';
      fixture.releaseGate(1);
      await fixture.event('shutdown-complete');
      return expectedCode(scenario.stop);
    }
    default: throw new Error(`Unknown scenario ${scenario.id}`);
  }
}
