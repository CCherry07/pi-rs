// Scenario-controller self-tests only. These doubles are never oracle/product targets.
import assert from 'node:assert/strict';
import test from 'node:test';
import { SCENARIOS, executeScenario } from '../scenarios.mjs';

const scenario = id => SCENARIOS.find(item => item.id === id);

test('backpressure input survives legitimate name trimming without removing internal Unicode separators', async () => {
  let name;
  const stop = new Error('input captured');
  const rpc = {
    async request(type, fields) {
      if (type === 'get_state') return { success: true, data: { isStreaming: false } };
      assert.equal(type, 'set_session_name');
      name = fields.name;
      throw stop;
    },
  };
  await assert.rejects(executeScenario(scenario('backpressure-eof'), { rpc, fixture: {}, observations: {} }), error => error === stop);
  assert.ok(name === name.trim(), 'Do not mistake legal session-name trim for stdout loss');
  assert.ok(name.includes('\u2028\u2029'), 'LF-only framing still needs internal Unicode separators');
  assert.ok(Buffer.byteLength(name) > 1024 * 1024, 'Retain the large-output backpressure witness');
});

test('abort queue scenario observes idle acknowledgement before explicitly starting and settling the next run', async () => {
  const actions = [];
  const records = [];
  const messages = [];
  let pending = 0;
  let busy = false;
  let sequence = 0;
  const addRequest = () => {
    fixture.requests.push({ body: { messages: structuredClone(messages) }, opened: false });
  };
  const rpc = {
    records,
    async request(type, fields = {}) {
      actions.push(type === 'prompt' ? `prompt:${fields.message}` : type);
      let data;
      switch (type) {
        case 'get_state': data = { isStreaming: busy, pendingMessageCount: pending }; break;
        case 'prompt':
          assert.equal(busy, false);
          messages.push({ role: 'user', content: fields.message });
          busy = true;
          addRequest();
          data = { disposition: 'started' };
          break;
        case 'follow_up': pending++; data = { disposition: 'queued' }; break;
        case 'get_messages': data = { messages: structuredClone(messages) }; break;
        case 'get_entries': data = { entries: [] }; break;
        default: throw new Error(`Unexpected scenario command ${type}`);
      }
      const response = { type: 'response', id: `self-${++sequence}`, command: type, success: true, data };
      records.push(response);
      return response;
    },
    async command(type) {
      assert.equal(type, 'abort');
      actions.push('abort');
      busy = false;
      records.push({ type: 'agent_settled', aborted: true });
      const id = `self-${++sequence}`;
      records.push({ type: 'response', id, command: type, success: true });
      return id;
    },
    async response(id) { return records.find(record => record.type === 'response' && record.id === id); },
    async waitFor(predicate, { after = 0 } = {}) {
      const event = records.slice(after).find(predicate);
      assert.ok(event, 'Scenario awaited an event before driving its prerequisite');
      return event;
    },
    async endInput() { actions.push('EOF'); },
  };
  const fixture = {
    requests: [],
    async request(index) {
      actions.push(`provider:${index}`);
      assert.ok(this.requests[index], 'Abort must not be expected to implicitly resume queued input');
      return this.requests[index];
    },
    text(index, delta) {
      this.requests[index].opened = true;
      records.push({ type: 'message_update', assistantMessageEvent: { type: 'text_delta', delta } });
    },
    finish(index) {
      this.requests[index].done = true;
      if (index === 1) {
        messages.push({ role: 'user', content: 'surviving follow-up' });
        pending = 0;
        addRequest();
      } else {
        assert.equal(index, 2);
        busy = false;
        records.push({ type: 'agent_settled', aborted: false });
      }
    },
    async disconnected() { actions.push('provider-disconnected'); },
  };
  const result = await executeScenario(scenario('abort-continues-queue'), { rpc, fixture, observations: {} });
  assert.equal(result, 0);
  assert.ok(actions.indexOf('abort') < actions.indexOf('prompt:resume after abort'));
  assert.ok(actions.indexOf('prompt:resume after abort') < actions.indexOf('provider:1'));
  assert.equal(fixture.requests.length, 3);
  assert.equal(pending, 0);
  assert.equal(actions.at(-1), 'EOF');
});
