import assert from 'node:assert/strict';
import test from 'node:test';
import { normalizeRpcRecords } from './wire-compare.mjs';

const normalized = records => normalizeRpcRecords(records).records;
const event = (message) => [{ type: 'message_end', message }];

test('only clock values change; absent/null/false/empty/content shape and roles do not', () => {
  const left = { role: 'assistant', timestamp: 1, durationMs: 2, content: [] };
  assert.deepEqual(normalized(event(left)), normalized(event({ ...left, timestamp: 7, durationMs: 9 })));
  for (const message of [
    { role: 'assistant', timestamp: 1, content: [] }, { ...left, content: '' }, { ...left, role: 'toolResult' },
    { ...left, flag: null }, { ...left, flag: false }, { ...left, flag: [] },
  ]) assert.notDeepEqual(normalized(event(left)), normalized(event(message)));
  assert.throws(() => normalized(event({ ...left, durationMs: null })));
});

test('generated entry identities keep parent/leaf aliasing bijectively', () => {
  const rows = (a, b) => [{ type: 'response', command: 'get_entries', data: { entries: [
    { id: a, parentId: null, type: 'message', timestamp: '2026-01-01T00:00:00Z' },
    { id: b, parentId: a, type: 'message', timestamp: '2026-01-01T00:00:01Z' },
  ], leafId: b } }];
  assert.deepEqual(normalized(rows('a', 'b')), normalized(rows('x', 'y')));
  assert.notDeepEqual(normalized(rows('a', 'b')), normalized(rows('x', 'x')));
});

test('opaque extension data and ordered deltas are never normalized away', () => {
  for (const key of ['structuredContent', 'details', 'arguments', 'args']) {
    const left = [{ type: 'tool_execution_end', [key]: { id: 'one', timestamp: 1, durationMs: 2, parentId: 'parent' } }];
    const right = [{ type: 'tool_execution_end', [key]: { id: 'two', timestamp: 3, durationMs: 4, parentId: 'other' } }];
    assert.notDeepEqual(normalized(left), normalized(right));
  }
  const first = { type: 'message_update', assistantMessageEvent: { type: 'text_delta', delta: 'a' } };
  const second = { type: 'message_update', assistantMessageEvent: { type: 'text_delta', delta: 'b' } };
  assert.notDeepEqual(normalized([first, second]), normalized([second, first]));
  assert.notDeepEqual(normalized([first, second]), normalized([first]));
});
