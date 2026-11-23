#!/usr/bin/env node
// Full, ordered stdout comparison between actual products. No response-only sorting.
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { appendFileSync, createReadStream, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createScriptedProvider } from './scripted-provider.mjs';
import { normalizeRpcRecords } from './wire-compare.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const args = process.argv.slice(2);
const option = name => { const i = args.indexOf(name); if (i < 0) return undefined; assert(args[i + 1], `${name} needs a value`); return args[i + 1]; };
const requiredPath = name => { const value = option(name); assert(value, `Required: ${name}`); return resolve(value); };
const legacy = requiredPath('--legacy');
const evidenceRoot = requiredPath('--evidence-dir');
const preparationPath = requiredPath('--preparation');
const rust = option('--rust') && resolve(option('--rust'));
const nodeLauncher = option('--node-launcher') && resolve(option('--node-launcher'));
assert(Boolean(rust) !== Boolean(nodeLauncher), 'Supply exactly one of --rust or --node-launcher');
const binding = nodeLauncher ? requiredPath('--binding') : undefined;
const scenario = option('--scenario') ?? 'idle';
assert(['idle', 'clear-queue', 'models', 'text', 'text-controlled', 'queues', 'abort', 'eof-active', 'hup', 'term', 'repeated-term', 'ui', 'hooks', 'tools'].includes(scenario), 'Unknown scenario');
const needsExtension = ['ui', 'hooks', 'tools', 'repeated-term'].includes(scenario);
assert(!needsExtension || nodeLauncher, 'This extension scenario requires the actual Node/NAPI candidate');
const preparation = JSON.parse(readFileSync(preparationPath, 'utf8'));
assert.equal(preparation.status, 'ready');
const git = (cwd, ...parameters) => execFileSync('git', parameters, { cwd, encoding: 'utf8' }).trim();
const commit = git(legacy, 'rev-parse', 'HEAD');
assert.equal(commit, '05fd3fbda0bbb9a1c7a76e4bf92ab58ed77c8d94');
assert.equal(preparation.commit, commit);
assert.equal(preparation.destination, legacy);
assert.equal(git(legacy, 'rev-parse', 'HEAD^{tree}'), preparation.tree);
assert.equal(git(legacy, 'status', '--porcelain', '--untracked-files=no'), '', 'Oracle source is not clean');
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
assert.equal(sha256(readFileSync(resolve(legacy, 'package-lock.json'))), preparation.lockSha256);
const digestFile = async path => { const hash = createHash('sha256'); for await (const chunk of createReadStream(path)) hash.update(chunk); return hash.digest('hex'); };
mkdirSync(evidenceRoot, { recursive: true, mode: 0o700 });
const evidence = mkdtempSync(resolve(evidenceRoot, `rpc-${scenario}-differential-`));
const provenance = {
  head: git(root, 'rev-parse', 'HEAD'), baseTree: git(root, 'rev-parse', 'HEAD^{tree}'),
  trackedDiffSha256: sha256(git(root, 'diff', '--no-ext-diff', '--binary', 'HEAD')),
  scripts: Object.fromEntries(['differential.mjs', 'scripted-provider.mjs', 'wire-compare.mjs'].map(file => [file, sha256(readFileSync(new URL(file, import.meta.url)))])),
  artifacts: Object.fromEntries(await Promise.all([rust, nodeLauncher, binding].filter(Boolean).map(async path => [path, await digestFile(path)]))),
};
writeFileSync(resolve(evidence, 'provenance.json'), JSON.stringify({ status: 'running', scenario, preparation, provenance }, null, 2));
console.log(`Differential evidence: ${evidence}`);
const fixture = await createScriptedProvider();
// Run sequentially with exactly the same sandbox paths and freshly reset inputs.
// Do not normalize different cwd/home/prompt strings to conceal differing inputs.
const sandbox = resolve(evidence, 'sandbox');
const cwd = resolve(sandbox, 'workspace'), home = resolve(sandbox, 'home'), agentDir = resolve(home, '.pi/agent');
const extension = resolve(evidence, 'fixture-extension.mjs');
const shutdownMarker = resolve(evidence, 'shutdown-started');
const common = ['--mode', 'rpc', '--no-session', '--no-approve', '--provider', 'rpc-fixture', '--model', 'fixture'];
if (needsExtension) common.push('--extension', extension); else common.push('--no-extensions');
if (needsExtension) {
  const source = scenario === 'repeated-term' ? `
    import { writeFileSync } from 'node:fs';
    export default function(pi) {
      pi.on('session_start', (_event, ctx) => { void ctx.ui.confirm('pending', 'retire me').catch(() => {}); });
      pi.on('session_shutdown', async () => { writeFileSync(${JSON.stringify(shutdownMarker)}, 'entered'); await new Promise(() => {}); });
    }` : scenario === 'ui' ? `
    export default function(pi) {
      pi.on('input', async (_event, ctx) => {
        const answer = await ctx.ui.confirm('Confirm', 'Continue?');
        return answer ? undefined : { action: 'handled' };
      });
    }` : scenario === 'hooks' ? `
    export default function(pi) {
      pi.on('session_start', () => { throw new Error('startup boom'); });
      pi.on('session_start', (_event, ctx) => { ctx.ui.notify('startup continued', 'info'); });
      pi.on('agent_start', () => { throw new Error('agent boom'); });
      pi.on('message_end', (event) => { if(event.message.role === 'assistant') return { message: { role: 'user', content: 'invalid', timestamp: 1 } }; });
      pi.on('user_bash', () => { throw new Error('bash boom'); });
    }` : `
    import { Type } from '@earendil-works/pi-ai';
    export default function(pi) {
      pi.registerTool({ name: 'rpc_echo', label: 'Echo', description: 'Fixture echo', parameters: Type.Object({ value: Type.String() }),
        async execute(_id, parameters, _signal, onUpdate) {
          onUpdate({ content: [{ type: 'text', text: 'working' }], structuredContent: null, details: { stage: 1 } });
          return { content: [{ type: 'text', text: parameters.value }], structuredContent: { value: parameters.value }, details: { stage: 2 } };
        }
      });
    }`;
  writeFileSync(extension, source);
}

function launch(name, executable, parameters, extraEnv = {}) {
  rmSync(sandbox, { recursive: true, force: true });
  rmSync(shutdownMarker, { force: true });
  mkdirSync(agentDir, { recursive: true }); mkdirSync(cwd, { recursive: true });
  const run = resolve(evidence, name); mkdirSync(run);
  const stdoutPath = resolve(run, 'stdout.jsonl'), stderrPath = resolve(run, 'stderr.log');
  writeFileSync(stdoutPath, '', { mode: 0o600 }); writeFileSync(stderrPath, '', { mode: 0o600 });
  const metadata = scenario === 'models' ? { promptCache: { short: 30, long: 300 }, samplingParamsByThinkingLevel: { off: { temperature: 0, custom: null }, high: {} } } : {};
  // Both source implementations support global SYSTEM.md and defaultTools;
  // the standalone Rust CLI does not expose Pi's --no-tools/--system-prompt flags.
  writeFileSync(resolve(agentDir, 'SYSTEM.md'), 'RPC fixture');
  writeFileSync(resolve(agentDir, 'settings.json'), JSON.stringify({ defaultProvider: 'rpc-fixture', defaultModel: 'fixture', defaultTools: scenario === 'tools' ? ['rpc_echo'] : [], compaction: { enabled: false }, retry: { enabled: false } }));
  writeFileSync(resolve(agentDir, 'models.json'), JSON.stringify({ providers: { 'rpc-fixture': {
    api: 'openai-completions', baseUrl: fixture.url, apiKey: 'fixture-not-a-secret',
    models: [{ id: 'fixture', name: 'Fixture', reasoning: false, input: ['text'], contextWindow: 32000, maxTokens: 1024, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, ...metadata }],
  } } }));
  const env = { PATH: process.env.PATH ?? '', HOME: home, USERPROFILE: home, PI_AGENT_DIR: agentDir, PI_OFFLINE: '1', CI: '1', TERM: 'dumb', NO_COLOR: '1', ...extraEnv };
  const command = [executable, ...parameters];
  writeFileSync(resolve(run, 'result.json'), JSON.stringify({ status: 'running', command, cwd, env }, null, 2));
  const child = spawn(executable, parameters, { cwd, env, stdio: ['pipe', 'pipe', 'pipe'] });
  const records = [], waits = new Set(), errors = [], sent = [];
  let buffer = '', exited = false, exitResult;
  const completeWaits = () => { for (const waiter of [...waits]) { const value = records.slice(waiter.after).find(waiter.predicate); if (value !== undefined) { waits.delete(waiter); waiter.resolve(value); } } };
  child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8');
  child.stdout.on('data', text => {
    appendFileSync(stdoutPath, text); buffer += text;
    for (;;) {
      const end = buffer.indexOf('\n'); if (end < 0) break;
      const line = buffer.slice(0, end); buffer = buffer.slice(end + 1);
      try { records.push(JSON.parse(line)); } catch (error) { errors.push(String(error)); }
    }
    completeWaits();
  });
  child.stderr.on('data', text => appendFileSync(stderrPath, text));
  child.stdin.on('error', error => errors.push(`stdin: ${error.message}`));
  const completed = new Promise(resolveExit => {
    child.on('error', error => errors.push(error.message));
    child.on('close', (code, signal) => {
      exited = true;
      for (const waiter of [...waits]) waiter.reject(new Error(`${name} exited before its barrier: ${code}/${signal}`));
      waits.clear();
      exitResult = { status: 'exited', command, cwd, code, signal, errors, trailingBytes: buffer.length, sent };
      writeFileSync(resolve(run, 'result.json'), JSON.stringify(exitResult, null, 2)); resolveExit(exitResult);
    });
  });
  const wait = (predicate, after = 0) => new Promise((resolveWait, reject) => {
    const existing = records.slice(after).find(predicate); if (existing !== undefined) { resolveWait(existing); return; }
    if (exited) { reject(new Error(`${name} already exited`)); return; }
    const timer = setTimeout(() => { waits.delete(waiter); reject(new Error(`${name} record barrier timeout`)); }, 15000);
    const waiter = { predicate, after, resolve: value => { clearTimeout(timer); resolveWait(value); }, reject: error => { clearTimeout(timer); reject(error); } };
    waits.add(waiter);
  });
  const send = command => { sent.push(command); child.stdin.write(`${JSON.stringify(command)}\n`); };
  return { records, completed, wait, send, close: () => child.stdin.end(), signal: signal => child.kill(signal), stop: () => { if (!exited) child.kill('SIGKILL'); },
    request: command => { const response = wait(value => value.type === 'response' && value.id === command.id, records.length); send(command); return response; },
  };
}

const idleCommands = [
  { type: 'get_last_assistant_text' }, { type: 'clear_queue' },
  { type: 'set_steering_mode', mode: 'all' }, { type: 'set_follow_up_mode', mode: 'one-at-a-time' },
  { type: 'set_auto_compaction', enabled: false }, { type: 'set_auto_retry', enabled: false },
  { type: 'abort_retry' }, { type: 'abort_bash' }, { type: 'abort' },
  { type: 'get_available_thinking_levels' }, { type: 'cycle_thinking_level' }, { type: 'cycle_model' },
  { type: 'set_model', provider: 'rpc-fixture', modelId: 'missing' },
  { type: 'set_session_name', name: ' \r\n ' }, { type: 'unknown-fixture-command' },
];
const withTimeout = async (promise, description) => {
  let timer;
  try { return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`${description} timed out`)), 20000); })]); }
  finally { clearTimeout(timer); }
};
async function runSide(name, executable, parameters, env) {
  const plans = scenario === 'queues' ? [{ hold: 'before' }, {}, {}] : ['abort', 'eof-active'].includes(scenario) ? [{ parts: ['partial'], hold: 'after' }] : scenario === 'tools' ? [{ tool: 'rpc_echo' }, {}] : [{ controlled: scenario === 'text-controlled' }];
  fixture.reset(plans);
  const child = launch(name, executable, parameters, env);
  let nextId = 0;
  const request = (type, fields = {}) => child.request({ id: `step-${nextId++}`, type, ...fields });
  try {
    if (['idle', 'clear-queue', 'models'].includes(scenario)) {
      const commands = scenario === 'models' ? [{ type: 'get_available_models' }, { type: 'set_model', provider: 'rpc-fixture', modelId: 'fixture' }, { type: 'get_state' }, { type: 'cycle_model' }] : idleCommands.filter(command => scenario === 'idle' || command.type === 'clear_queue');
      for (const { type, ...fields } of commands) await request(type, fields);
      child.close();
    } else if (['hup', 'term', 'repeated-term'].includes(scenario)) {
      await request('get_state'); // Proves input and signal handlers are installed.
      child.signal(scenario === 'hup' ? 'SIGHUP' : 'SIGTERM');
      if (scenario === 'repeated-term') {
        const deadline = Date.now() + 15000;
        while (!existsSync(shutdownMarker)) {
          assert(Date.now() < deadline, 'shutdown hook barrier timed out');
          await new Promise(resolvePoll => setTimeout(resolvePoll, 20));
        }
        child.signal('SIGTERM');
      }
    } else if (scenario === 'ui') {
      const pending = request('prompt', { message: 'handled by dialog' });
      const dialog = await child.wait(value => value.type === 'extension_ui_request' && value.method === 'confirm');
      child.send({ type: 'extension_ui_response', id: dialog.id, confirmed: false });
      assert.equal((await pending).data.disposition, 'handled');
      await request('get_last_assistant_text');
      child.close();
    } else {
      await request('set_auto_retry', { enabled: false });
      await request('set_auto_compaction', { enabled: false });
      await request('prompt', { message: 'hello' });
      if (scenario === 'text-controlled') {
        let after = 0;
        for (let part = 0; part < 2; part++) {
          const delta = await child.wait(value => value.type === 'message_update' && value.assistantMessageEvent?.type === 'text_delta', after);
          after = child.records.indexOf(delta) + 1;
          fixture.releasePart(0, part);
        }
      }
      if (scenario === 'queues') {
        await fixture.waitForRequest(1);
        await request('steer', { message: 'steered' });
        await request('follow_up', { message: 'followed' });
        await request('get_state');
        fixture.release();
      } else if (['abort', 'eof-active'].includes(scenario)) {
        await child.wait(value => value.type === 'message_update' && value.assistantMessageEvent?.type === 'text_delta');
        if (scenario === 'abort') await request('abort'); else child.close();
      }
      if (scenario !== 'eof-active') {
        await child.wait(value => value.type === 'agent_settled');
        await request('get_last_assistant_text');
        await request('get_messages');
        // Native v4 legitimately retains a tool-configuration checkpoint. History
        // graph conformance is a separate storage boundary, not hidden/filterable output.
        await request('get_session_stats');
        if (scenario === 'hooks') {
          const response = await request('bash', { command: 'fixture' });
          assert.equal(response.error, 'bash boom');
        }
        child.close();
      }
    }
    const exit = await withTimeout(child.completed, `${name} exit`);
    const expectedCode = scenario === 'hup' ? 129 : ['term', 'repeated-term'].includes(scenario) ? 143 : 0;
    assert.equal(exit.code, expectedCode); assert.equal(exit.signal, null); assert.deepEqual(exit.errors, []); assert.equal(exit.trailingBytes, 0);
    return { records: child.records, exit, requests: structuredClone(fixture.requests) };
  } finally { child.stop(); await withTimeout(child.completed, `${name} forced cleanup`); }
}

const idMappings = {};
function comparable(records, side) {
  const normalized = normalizeRpcRecords(records);
  idMappings[side] = normalized.ids;
  return normalized.records;
}
let left, right, passed = false;
try {
  left = await runSide('legacy', process.execPath, ['--disable-warning=ExperimentalWarning', '--experimental-transform-types', '--import', resolve(legacy, 'packages/coding-agent/src/experimental/source-resolver.ts'), resolve(legacy, 'packages/coding-agent/src/cli.ts'), ...common]);
  right = await runSide('candidate', nodeLauncher ? process.execPath : rust, nodeLauncher ? [nodeLauncher, ...common] : common, binding ? { PI_RS_NATIVE_BINDING: binding } : {});
  assert.deepEqual(comparable(right.records, 'candidate'), comparable(left.records, 'legacy'));
  passed = true;
  console.log(`D-level ${scenario}: ${left.records.length} complete mixed records per process; candidate=${nodeLauncher ? 'Node/NAPI' : 'standalone Rust'}`);
} finally {
  await fixture.close();
  writeFileSync(resolve(evidence, 'comparison.json'), `${JSON.stringify({ passed, scenario, oracleCommit: commit, candidate: { rust, nodeLauncher, binding }, provenance, preparationPath, normalization: 'generated entry/session/UI ids bijectively; timestamp/duration values preserving presence/type; no other fields removed or records reordered', idMappings, legacy: left, candidate: right }, null, 2)}\n`);
}
