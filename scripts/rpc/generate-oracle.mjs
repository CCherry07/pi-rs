#!/usr/bin/env node
// Executes pinned legacy functions, not a reimplementation of their expected results.
// This is a function-level oracle. It does NOT replace the real two-CLI differential.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { stripTypeScriptTypes } from 'node:module';
import { resolve, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const commit = '05fd3fbda0bbb9a1c7a76e4bf92ab58ed77c8d94';
const root = fileURLToPath(new URL('../../', import.meta.url));
const args = process.argv.slice(2);
const legacyIndex = args.indexOf('--legacy');
assert(legacyIndex >= 0 && args[legacyIndex + 1], 'Usage: generate-oracle.mjs --legacy <checkout> [--check]');
const legacy = resolve(args[legacyIndex + 1]);
assert.equal(execFileSync('git', ['rev-parse', 'HEAD'], { cwd: legacy, encoding: 'utf8' }).trim(), commit, 'Unexpected legacy commit');
const paths = [
  'packages/coding-agent/src/modes/rpc/rpc-mode.ts',
  'packages/coding-agent/src/modes/rpc/jsonl.ts',
  'packages/coding-agent/src/modes/json-event.ts',
  'packages/coding-agent/src/core/agent-session.ts',
];
for (const path of paths) {
  assert.equal(await readFile(resolve(legacy, path), 'utf8'), execFileSync('git', ['show', `HEAD:${path}`], { cwd: legacy, encoding: 'utf8' }), `Modified oracle source: ${path}`);
}
const { serializeJsonLine } = await import(pathToFileURL(resolve(legacy, paths[1])).href);
const { toJsonEvent } = await import(pathToFileURL(resolve(legacy, paths[2])).href);
const json = value => JSON.parse(serializeJsonLine(value));

const rpcSource = await readFile(resolve(legacy, paths[0]), 'utf8');
const start = rpcSource.indexOf('\tconst createExtensionUIContext = ');
const end = rpcSource.indexOf('\n\truntimeHost.setRebindSession', start);
assert(start >= 0 && end > start, 'RPC UI factory moved; review the oracle extraction');
const factory = stripTypeScriptTypes(rpcSource.slice(start, end));
const notifications = [];
const ui = new Function('theme', 'output', `${factory}; return createExtensionUIContext();`)({}, record => notifications.push(record));
const uiCalls = [
  ['getAllThemes', []], ['getTheme', ['unknown']],
  ['setTheme', ['dark']], ['setTheme', ['unknown']], ['setTheme', [{ name: 'custom' }]],
  ['onTerminalInput', []], ['getEditorText', []], ['getToolsExpanded', []],
  ['setToolsExpanded', [true]], ['setWorkingMessage', ['busy']], ['setWorkingVisible', [true]],
  ['setWorkingIndicator', [{}]], ['setHiddenThinkingLabel', ['thinking']],
  ['setHeader', []], ['setFooter', []], ['addAutocompleteProvider', []],
  ['setEditorComponent', []], ['getEditorComponent', []], ['custom', []],
];
const uiCases = [];
for (const [method, parameters] of uiCalls) {
  assert.equal(typeof ui[method], 'function', `Missing oracle method ${method}`);
  const result = await ui[method](...parameters);
  if (typeof result === 'function') {
    // onTerminalInput's unsubscribe must be callable repeatedly and have no effects.
    uiCases.push(json({ method, parameters, resultType: 'function', unsubscribeResult: result(), repeatedResult: result() }));
  } else {
    uiCases.push(json({ method, parameters, resultType: typeof result, result }));
  }
}
assert.deepEqual(notifications, [], 'Inactive RPC methods unexpectedly emitted output');

const toolCases = [];
for (const structured of [undefined, null, false, '', [], { answer: 42, nested: { empty: '' } }]) {
  const result = { content: [{ type: 'text', text: 'visible' }], details: { fixture: true } };
  if (structured !== undefined) result.structuredContent = structured;
  for (const type of ['tool_execution_update', 'tool_execution_end']) {
    const input = { type, toolCallId: 'call-1', toolName: 'fixture', ...(type === 'tool_execution_update' ? { args: { value: 1 }, partialResult: result } : { result, isError: false }) };
    toolCases.push({ input, expected: json(toJsonEvent(input)) });
  }
}

for (const durationMs of [0, 42, 1000]) {
  const input = { ...toolCases[1].input, durationMs };
  toolCases.push({ input, expected: json(toJsonEvent(input)) });
}

const sessionSource = await readFile(resolve(legacy, paths[3]), 'utf8');
const methodStart = sessionSource.indexOf('\tgetLastAssistantText(): string | undefined {');
assert(methodStart >= 0, 'Last assistant text method moved');
const method = sessionSource.slice(methodStart, sessionSource.indexOf('\n\t}', methodStart) + 3);
const Oracle = new Function(`${stripTypeScriptTypes(`class Oracle { messages = []; ${method} }`)}; return Oracle;`)();
const session = new Oracle();
const assistant = (stopReason, text) => ({ role: 'assistant', stopReason, content: text === undefined ? [] : [{ type: 'text', text }] });
const earlier = assistant('stop', ' earlier ');
const lastTextCases = [];
for (const [name, messages] of [
  ['empty', []], ['whitespace', [assistant('stop', '  ')]], ['text', [assistant('stop', ' answer ')]],
  ['aborted-text', [earlier, assistant('aborted', ' partial ')]],
  ['error-text', [earlier, assistant('error', ' failure partial ')]],
  ['stop-no-text', [earlier, assistant('stop')]], ['stop-whitespace', [earlier, assistant('stop', '  ')]],
  ['aborted-no-text', [earlier, assistant('aborted')]], ['error-whitespace', [earlier, assistant('error', '  ')]],
]) {
  session.messages = messages;
  lastTextCases.push({ name, messages, expected: json({ text: session.getLastAssistantText() }) });
}

const files = [
  ['packages/pi/test/fixtures/rpc-ui-oracle.json', { commit, source: paths[0], cases: uiCases }],
  ['crates/pi-rpc/tests/fixtures/rpc-tool-events.json', { commit, source: paths[2], cases: toolCases }],
  ['crates/pi-rpc/tests/fixtures/rpc-last-text.json', { commit, source: paths[3], cases: lastTextCases }],
];
for (const [path, value] of files) {
  const contents = `${JSON.stringify(value, null, 2)}\n`;
  const destination = resolve(root, path);
  if (args.includes('--check')) assert.equal(await readFile(destination, 'utf8'), contents, `Stale fixture ${path}`);
  else { await mkdir(dirname(destination), { recursive: true }); await writeFile(destination, contents); }
  console.log(`${args.includes('--check') ? 'Checked' : 'Generated'} ${path}: ${value.cases.length} cases`);
}
