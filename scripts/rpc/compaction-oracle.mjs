#!/usr/bin/env node
// Executes the pinned legacy SessionManager/projector/estimator without provider calls.
import assert from 'node:assert/strict';
import { spawnSync, execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { closeSync, mkdirSync, mkdtempSync, openSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = fileURLToPath(new URL('../../', import.meta.url));
const args = process.argv.slice(2);
const value = flag => { const i = args.indexOf(flag); assert(i >= 0 && args[i + 1], `Required: ${flag}`); return resolve(args[i + 1]); };
const legacy = value('--legacy');
const commit = '05fd3fbda0bbb9a1c7a76e4bf92ab58ed77c8d94';
assert.equal(execFileSync('git', ['rev-parse', 'HEAD'], { cwd: legacy, encoding: 'utf8' }).trim(), commit);
execFileSync('git', ['diff', '--quiet', 'HEAD', '--'], { cwd: legacy });

if (!args.includes('--worker')) {
  const evidenceRoot = value('--evidence-dir');
  mkdirSync(evidenceRoot, { recursive: true, mode: 0o700 });
  const evidence = mkdtempSync(resolve(evidenceRoot, 'compaction-oracle-'));
  const home = resolve(evidence, 'home');
  const agentDir = resolve(home, '.pi/agent');
  const cwd = resolve(evidence, 'workspace');
  mkdirSync(agentDir, { recursive: true }); mkdirSync(cwd);
  const workerArgs = args.slice();
  workerArgs[workerArgs.indexOf('--legacy') + 1] = legacy;
  const command = ['--disable-warning=ExperimentalWarning', '--experimental-transform-types',
    '--import', resolve(legacy, 'packages/coding-agent/src/experimental/source-resolver.ts'),
    fileURLToPath(import.meta.url), ...workerArgs, '--worker'];
  writeFileSync(resolve(evidence, 'result.json'), `${JSON.stringify({ status: 'running', commit, command }, null, 2)}\n`);
  const stdout = openSync(resolve(evidence, 'stdout.log'), 'wx', 0o600);
  const stderr = openSync(resolve(evidence, 'stderr.log'), 'wx', 0o600);
  let result;
  try {
    result = spawnSync(process.execPath, command, {
      cwd, timeout: 30000, stdio: ['ignore', stdout, stderr],
      env: { PATH: process.env.PATH ?? '', HOME: home, USERPROFILE: home, PI_AGENT_DIR: agentDir, PI_OFFLINE: '1', CI: '1', TERM: 'dumb', NO_COLOR: '1' },
    });
  } finally {
    closeSync(stdout); closeSync(stderr);
  }
  writeFileSync(resolve(evidence, 'result.json'), `${JSON.stringify({ commit, command, status: result.status, signal: result.signal, error: result.error?.message }, null, 2)}\n`);
  console.log(`Compaction oracle evidence: ${evidence}`);
  assert.equal(result.status, 0, `Oracle failed; inspect ${evidence}/stderr.log`);
  process.stdout.write(readFileSync(resolve(evidence, 'stdout.log'), 'utf8'));
} else {
  const module = path => import(pathToFileURL(resolve(legacy, path)).href);
  const { SessionManager, sessionEntryToContextMessages } = await module('packages/coding-agent/src/core/session-manager.ts');
  const { estimateTokens } = await module('packages/coding-agent/src/core/compaction/compaction.ts');
  const { getCurrentSystemMessage } = await module('packages/ai/src/utils/transcript.ts');
  const timestamp = '2025-02-01T00:00:00.123Z';
  const millis = Date.parse(timestamp);
  const system = [
    { role: 'system', content: 'base 😀', sections: { old: 'remove', keep: '段落' }, toolsAdded: [{ name: 'before', description: 'old', parameters: { type: 'object' } }], timestamp: 1 },
    { role: 'system', content: '', sections: { old: null, empty: '' }, toolsRemoved: [{ name: 'before' }], toolsAdded: [{ name: 'after', description: '工具', parameters: { type: 'object', properties: { x: { type: 'string' } } } }], timestamp: 2 },
  ];
  const usage = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } };
  const user = { role: 'user', content: '你好😀', timestamp: 3 };
  const assistant = { role: 'assistant', content: [{ type: 'text', text: 'answer' }], api: 'openai-completions', provider: 'fixture', model: 'model', usage, stopReason: 'stop', timestamp: 4 };
  const messages = [...system, user, assistant];
  const ids = ['system-a', 'system-b', 'user', 'assistant'];
  const entries = messages.map((message, i) => ({ type: 'message', id: ids[i], parentId: i ? ids[i - 1] : null, timestamp, message }));
  const header = { type: 'session', version: 3, id: 'fixture-session', timestamp, cwd: '/fixture' };
  const cases = [];
  for (const spec of [
    { name: 'default-keep-tail', summary: 'summary', firstKeptEntryId: 'user', fromHook: false, details: {} },
    { name: 'hook-retain-none-empty-summary', summary: '', firstKeptEntryId: null, fromHook: true, details: { custom: false } },
    { name: 'missing-hook-flag', summary: 'summary', firstKeptEntryId: 'user' },
  ]) {
    const manager = SessionManager.inMemory('/fixture', undefined, structuredClone([header, ...entries]));
    const id = manager.appendCompaction(spec.summary, spec.firstKeptEntryId, 100, spec.details, spec.fromHook);
    const record = manager.getEntry(id);
    const context = manager.buildSessionProjection().messages;
    const rawTimestamp = record.timestamp;
    const rawMillis = Date.parse(rawTimestamp);
    // Only this operation's generated ID and timestamps are normalized. Keep every field.
    const normalize = object => JSON.parse(JSON.stringify(object, (key, item) => {
      if (['id', 'parentId', 'firstKeptEntryId'].includes(key) && item === id) return 'compaction';
      if (key === 'timestamp' && item === rawTimestamp) return timestamp;
      if (key === 'timestamp' && item === rawMillis) return millis;
      return item;
    }));
    cases.push({ ...spec, entries, record: normalize(record), context: normalize(context), estimatedTokensAfter: context.reduce((total, message) => total + estimateTokens(message), 0) });
  }
  const importedSystem = { role: 'system', content: '', sections: { empty: '', keep: 'instruction' }, toolsAdded: [{ name: 'kept', description: 'kept', parameters: { type: 'object' }, futureTool: { keep: true } }], toolsRemoved: [{ name: 'removed' }], timestamp: millis, futureMessage: { keep: false } };
  const imported = { type: 'compaction', id: 'imported', parentId: null, timestamp, summary: '', firstKeptEntryId: 'imported', tokensBefore: 1, fromHook: false, systemMessage: importedSystem };
  const branchCases = [];
  for (const summary of ['', 'branch 段落😀']) {
    for (const fromHook of [undefined, false, true]) {
      const entry = { type: 'branch_summary', id: 'branch', parentId: null, timestamp, fromId: 'from', summary, ...(fromHook === undefined ? {} : { fromHook }) };
      branchCases.push({ entry, context: sessionEntryToContextMessages(entry) });
    }
  }
  const tokenMessages = [
    importedSystem, user,
    { ...assistant, content: [{ type: 'text', text: 'abc😀' }, { type: 'thinking', thinking: '思考' }, { type: 'toolCall', id: 'call', name: '工具', arguments: { n: 1, flag: false, text: '🧪' } }] },
    { role: 'toolResult', toolCallId: 'call', toolName: 'fixture', content: [{ type: 'text', text: '你好' }, { type: 'image', data: 'AA==', mimeType: 'image/png' }], isError: false, timestamp: 1 },
    { role: 'custom', customType: 'fixture', content: 'custom 😀', display: false, timestamp: 1 },
    { role: 'bashExecution', command: 'echo 你好', output: '😀', exitCode: 0, cancelled: false, truncated: false, timestamp: 1 },
    { role: 'compactionSummary', summary: '', tokensBefore: 100, timestamp: 1 },
    { role: 'branchSummary', summary: '段落😀', fromId: 'from', timestamp: 1 },
    { role: 'future', content: 'ignored', timestamp: 1 },
  ];
  const fixture = {
    commit, evidenceLevel: 'Executed legacy in-memory SessionManager and pure projection/token functions; no provider request',
    sources: ['packages/coding-agent/src/core/session-manager.ts', 'packages/coding-agent/src/core/compaction/compaction.ts', 'packages/ai/src/utils/transcript.ts'],
    systemFold: { messages: system, expected: getCurrentSystemMessage(system) },
    cases, imported: { entry: imported, context: sessionEntryToContextMessages(imported) }, branchCases,
    tokens: tokenMessages.map(message => ({ message, expected: estimateTokens(message) })),
  };
  const path = resolve(root, 'crates/pi-session/tests/fixtures/rpc-compaction-oracle.json');
  const contents = `${JSON.stringify(fixture, null, 2)}\n`;
  if (args.includes('--check')) assert.equal(readFileSync(path, 'utf8'), contents, 'Compaction oracle changed');
  else { mkdirSync(dirname(path), { recursive: true }); writeFileSync(path, contents); }
  console.log(`${args.includes('--check') ? 'Checked' : 'Generated'} ${path}`);
  console.log(`Fixture SHA256: ${createHash('sha256').update(contents).digest('hex')}`);
}
