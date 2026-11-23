import assert from 'node:assert/strict';
import test from 'node:test';
import { compareStreams, normalize } from '../lib/compare.mjs';

test('comparison retains opaque usage, IDs, timestamps and legitimate checkpoint records', () => {
  const legacy = [{ type: 'response', id: 'query', command: 'get_entries', success: true, data: { entries: [], leafId: null } }];
  const candidate = structuredClone(legacy);
  candidate[0].data.entries.push({ type: 'custom', id: 'random', parentId: null, timestamp: '2026-01-01T00:00:00Z', customType: 'active_tools_change', data: { timestamp: 42, id: 'payload-id', usage: { output: 7 } } });
  const result = compareStreams(legacy, candidate);
  assert.equal(result.equal, false);
  const normalized = normalize(candidate);
  assert.equal(normalized.records[0].data.entries.length, 1);
  assert.deepEqual(normalized.records[0].data.entries[0].data, candidate[0].data.entries[0].data);
});

test('only documented generated identities and time values are mapped, without changing type or omission', () => {
  const records = [{ type: 'message_end', message: { role: 'assistant', timestamp: 100, durationMs: 25, content: [], usage: { input: 1, output: 2 } } }];
  const later = structuredClone(records);
  later[0].message.timestamp = 200;
  later[0].message.durationMs = 90;
  assert.equal(compareStreams(records, later).equal, true);
  delete later[0].message.durationMs;
  assert.equal(compareStreams(records, later).equal, false);
  later[0].message.durationMs = '25';
  assert.equal(compareStreams(records, later).equal, false);
  later[0].message.durationMs = 25;
  later[0].message.usage.output = 3;
  assert.equal(compareStreams(records, later).equal, false);
});

test('mixed event/response order is never sorted away', () => {
  const records = [{ type: 'agent_settled', aborted: true }, { type: 'response', id: 'abort', command: 'abort', success: true }];
  assert.equal(compareStreams(records, [...records].reverse()).equal, false);
});
