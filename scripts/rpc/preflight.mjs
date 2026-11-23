#!/usr/bin/env node
// Real legacy CLI launch preflight. No prompt/provider call; full stdout/stderr retained.
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { appendFileSync, mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
const args = process.argv.slice(2);
const index = args.indexOf('--legacy');
assert(index >= 0 && args[index + 1], 'Usage: preflight.mjs --legacy <isolated-checkout> --evidence-dir <persistent-directory>');
const legacy = resolve(args[index + 1]);
const evidenceIndex = args.indexOf('--evidence-dir');
assert(evidenceIndex >= 0 && args[evidenceIndex + 1], 'Required: --evidence-dir <persistent-directory>');
const commit = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: legacy, encoding: 'utf8' }).trim();
assert.equal(commit, '05fd3fbda0bbb9a1c7a76e4bf92ab58ed77c8d94');
execFileSync('git', ['diff', '--quiet', 'HEAD', '--'], { cwd: legacy });
const evidenceRoot = resolve(args[evidenceIndex + 1]);
mkdirSync(evidenceRoot, { recursive: true, mode: 0o700 });
const evidence = mkdtempSync(resolve(evidenceRoot, 'preflight-'));
const stdoutPath = resolve(evidence, 'stdout.jsonl');
const stderrPath = resolve(evidence, 'stderr.log');
writeFileSync(stdoutPath, '', { mode: 0o600 });
writeFileSync(stderrPath, '', { mode: 0o600 });
const home = resolve(evidence, 'home');
const agentDir = resolve(home, '.pi/agent');
const cwd = resolve(evidence, 'workspace');
mkdirSync(agentDir, { recursive: true });
mkdirSync(cwd);
writeFileSync(resolve(agentDir, 'settings.json'), JSON.stringify({ defaultProvider: 'rpc-fixture', defaultModel: 'fixture', autoCompaction: false }));
writeFileSync(resolve(agentDir, 'models.json'), JSON.stringify({ providers: { 'rpc-fixture': {
  api: 'openai-completions', baseUrl: 'http://127.0.0.1:9', apiKey: 'fixture-not-a-secret',
  models: [{ id: 'fixture', name: 'Fixture', reasoning: false, input: ['text'], contextWindow: 32000, maxTokens: 1024, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }],
} } }));
const command = [
  '--disable-warning=ExperimentalWarning', '--experimental-transform-types',
  '--import', resolve(legacy, 'packages/coding-agent/src/experimental/source-resolver.ts'),
  resolve(legacy, 'packages/coding-agent/src/cli.ts'), '--mode', 'rpc', '--no-session',
  '--no-extensions', '--no-skills', '--no-prompt-templates', '--no-approve',
  '--provider', 'rpc-fixture', '--model', 'fixture',
];
// A whitelist prevents accidental model credentials, user extensions or proxy settings from leaking in.
const env = { PATH: process.env.PATH ?? '', HOME: home, USERPROFILE: home, PI_AGENT_DIR: agentDir, PI_OFFLINE: '1', CI: '1', TERM: 'dumb', NO_COLOR: '1' };
writeFileSync(resolve(evidence, 'result.json'), `${JSON.stringify({ status: 'running', commit, command: [process.execPath, ...command], cwd, evidence }, null, 2)}\n`);
const child = spawn(process.execPath, command, { cwd, env, stdio: ['pipe', 'pipe', 'pipe'] });
let buffer = '', state, timedOut = false;
const parseErrors = [];
child.stdout.setEncoding('utf8');
child.stderr.setEncoding('utf8');
child.stdout.on('data', text => {
  appendFileSync(stdoutPath, text);
  buffer += text;
  for (;;) {
    const end = buffer.indexOf('\n'); if (end < 0) break;
    const line = buffer.slice(0, end); buffer = buffer.slice(end + 1);
    try {
      const value = JSON.parse(line);
      if (value.type === 'response' && value.id === 'preflight') { state = value; child.stdin.end(); }
    } catch (error) { parseErrors.push(String(error)); }
  }
});
child.stderr.on('data', text => { appendFileSync(stderrPath, text); });
child.stdin.on('error', error => { appendFileSync(stderrPath, `\nstdin: ${error.message}\n`); });
child.stdin.write('{"id":"preflight","type":"get_state"}\n');
const timeout = setTimeout(() => { timedOut = true; child.kill('SIGKILL'); }, 20000);
const result = await new Promise((resolveExit, reject) => {
  child.once('error', reject);
  child.once('close', (code, signal) => resolveExit({ code, signal }));
});
clearTimeout(timeout);
writeFileSync(resolve(evidence, 'result.json'), `${JSON.stringify({ status: result.code === 0 && !timedOut && parseErrors.length === 0 && state?.success === true && state?.data?.model?.provider === 'rpc-fixture' ? 'passed' : 'failed', commit, command: [process.execPath, ...command], cwd, evidence, ...result, timedOut, state, parseErrors }, null, 2)}\n`);
console.log(`Preflight evidence: ${evidence}`);
assert.equal(timedOut, false, 'RPC preflight timed out');
assert.equal(result.code, 0, `Legacy CLI failed; inspect ${evidence}/stderr.log`);
assert.deepEqual(parseErrors, [], 'Non-JSON stdout');
assert.equal(state?.success, true, 'No successful get_state response');
assert.equal(state.data.model.provider, 'rpc-fixture');
console.log('Real legacy RPC get_state + EOF passed (no provider request).');
