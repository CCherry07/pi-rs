#!/usr/bin/env node
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { appendFileSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { SCENARIOS } from './scenarios.mjs';
import { ORACLE_COMMIT, digest, errorRecord, fingerprint, hashFile, jsonFile, loadInputs, readJson, under, verifyInputs } from './lib/evidence.mjs';

const directory = fileURLToPath(new URL('.', import.meta.url));
const repository = fileURLToPath(new URL('../../../', import.meta.url));
const args = process.argv.slice(2);
const values = new Map();
for (let index = 0; index < args.length; index++) {
  const flag = args[index];
  if (['--list', '--help'].includes(flag)) { values.set(flag, true); continue; }
  assert(['--inputs', '--evidence-dir', '--work-dir', '--scenarios', '--targets', '--timeout-ms', '--fingerprint'].includes(flag), `Unknown option ${flag}`);
  assert(args[index + 1] && !args[index + 1].startsWith('--'), `Value required for ${flag}`);
  values.set(flag, args[++index]);
}
if (values.has('--help')) {
  console.log('Usage: node scripts/rpc/acceptance/run.mjs --inputs <pinned-inputs.json> --evidence-dir <private-persistent-root> [--scenarios id,id] [--targets rust,node] [--timeout-ms 25000]\n       --list | --fingerprint <file-or-runtime-directory>\nNo builds, installation, product patching, or credential discovery are performed.');
  process.exit(0);
}
if (values.has('--list')) { console.log(JSON.stringify(SCENARIOS, null, 2)); process.exit(0); }
if (values.has('--fingerprint')) {
  console.log(await fingerprint(resolve(values.get('--fingerprint'))));
  process.exit(0);
}
assert(values.has('--inputs') && values.has('--evidence-dir'), '--inputs and --evidence-dir are required');
const inputsPath = resolve(values.get('--inputs'));
const evidenceRoot = resolve(values.get('--evidence-dir'));
assert(!under(evidenceRoot, repository) && evidenceRoot !== resolve(repository), 'Evidence must live outside the worktree/build/cache so cleanup cannot erase the only record');
const workRoot = resolve(values.get('--work-dir') ?? join(repository, '.build/rpc-validation'));
const permittedWorkRoot = resolve(repository, '.build/rpc-validation');
assert(workRoot === permittedWorkRoot || under(workRoot, permittedWorkRoot), 'Use this checkout\'s explicit .build/rpc-validation area');
const selection = values.get('--scenarios')?.split(',') ?? SCENARIOS.map(scenario => scenario.id);
assert(selection.length > 0 && selection.every(id => SCENARIOS.some(scenario => scenario.id === id)), 'Unknown scenario selection');
const targets = ['legacy', ...new Set((values.get('--targets') ?? 'rust,node').split(',').filter(name => name !== 'legacy'))];
assert(targets.slice(1).every(name => ['rust', 'node'].includes(name)), 'Targets must be rust,node');
const timeoutMs = Number(values.get('--timeout-ms') ?? 25_000);
assert(Number.isInteger(timeoutMs) && timeoutMs >= 1000 && timeoutMs <= 120_000, 'Invalid target timeout');
mkdirSync(evidenceRoot, { recursive: true, mode: 0o700 });
mkdirSync(workRoot, { recursive: true });
const evidence = mkdtempSync(join(evidenceRoot, 'acceptance-'));
const workDirectory = mkdtempSync(join(workRoot, 'acceptance-'));
console.log(`Acceptance evidence: ${evidence}`);
console.log(`Disposable work: ${workDirectory}`);
const codeInventory = () => Object.fromEntries(readdirSync(directory, { recursive: true })
  .filter(path => /\.(?:mjs|rs|md|json)$/.test(path)).sort()
  .map(path => [path, digest(readFileSync(join(directory, path)))]));
const code = codeInventory();
const manifest = {
  schemaVersion: 1, task: 41, status: 'running', startedAt: new Date().toISOString(),
  oracleCommit: ORACLE_COMMIT, inputsPath, inputsSha256: digest(readFileSync(inputsPath)),
  evidence, workDirectory, targets, timeoutMs, code, cases: [],
  scope: 'Acceptance runner delivery only, not a declaration of complete RPC conformance. Self-tests are not product evidence.',
};
jsonFile(join(evidence, 'manifest.json'), manifest);
let interrupted = false;
const ownedWorkers = new Map();
const kill = (pid, pgid, reason) => {
  assert(Number.isInteger(pid) && pid > 1 && pid !== process.pid);
  try { process.kill(pgid ? -pgid : pid, 'SIGKILL'); }
  catch (error) { if (error.code !== 'ESRCH') throw error; }
  appendFileSync(join(evidence, 'watchdog.jsonl'), JSON.stringify({ action: 'kill-owned', pid, pgid, reason }) + '\n', { mode: 0o600 });
};
for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.on(signal, () => {
  interrupted = true;
  for (const [pid, worker] of ownedWorkers) {
    for (const child of worker.children.values()) kill(child.pid, child.pgid, `runner ${signal}`);
    kill(pid, worker.pgid, `runner ${signal}`);
  }
});

async function runCase(config) {
  const path = join(config.evidenceDirectory, 'worker-input.json');
  jsonFile(path, config);
  const child = spawn(process.execPath, [join(directory, 'worker.mjs'), path], {
    cwd: repository, stdio: ['ignore', 'pipe', 'pipe', 'ipc'], detached: process.platform !== 'win32',
  });
  const worker = { pgid: process.platform === 'win32' ? null : child.pid, children: new Map() };
  ownedWorkers.set(child.pid, worker);
  let expired = false;
  child.on('message', message => {
    if (message.type === 'owned') {
      assert(Number.isInteger(message.pid) && message.pid > 1 && message.pid !== child.pid && message.pid !== process.pid);
      assert(message.pgid === null || message.pgid === message.pid);
      worker.children.set(message.pid, message);
    } else if (message.type === 'retired') worker.children.delete(message.pid);
  });
  child.stdout.on('data', bytes => appendFileSync(join(config.evidenceDirectory, 'worker.stdout'), bytes, { mode: 0o600 }));
  child.stderr.on('data', bytes => appendFileSync(join(config.evidenceDirectory, 'worker.stderr'), bytes, { mode: 0o600 }));
  const outerTimeout = config.timeoutMs * config.targets.length + 15_000;
  const deadline = setTimeout(() => {
    expired = true;
    for (const item of worker.children.values()) kill(item.pid, item.pgid, 'outer hard deadline');
    kill(child.pid, worker.pgid, 'outer hard deadline');
  }, outerTimeout);
  return new Promise(resolveCase => {
    child.on('error', error => {
      jsonFile(join(config.evidenceDirectory, 'worker-spawn-error.json'), errorRecord(error));
    });
    child.on('close', (code, signal) => {
      clearTimeout(deadline);
      // A crashed worker must not leave a detached product process running.
      for (const item of worker.children.values()) kill(item.pid, item.pgid, 'worker closed before retiring child');
      ownedWorkers.delete(child.pid);
      const exit = { code, signal, expired, outerTimeout };
      jsonFile(join(config.evidenceDirectory, 'worker-exit.json'), exit);
      resolveCase(exit);
    });
  });
}

try {
  const inputs = loadInputs(inputsPath);
  jsonFile(join(evidence, 'inputs.snapshot.json'), inputs);
  manifest.validationBefore = await verifyInputs(inputs);
  jsonFile(join(evidence, 'manifest.json'), manifest);
  assert(manifest.validationBefore.valid, 'Pinned input hash mismatch; no product processes started');
  for (const scenario of selection) {
    if (interrupted) break;
    const evidenceDirectory = join(evidence, scenario);
    mkdirSync(evidenceDirectory, { mode: 0o700 });
    const config = {
      inputsPath: join(evidence, 'inputs.snapshot.json'), scenario, targets, timeoutMs,
      evidenceDirectory, workDirectory: join(workDirectory, scenario),
    };
    mkdirSync(config.workDirectory);
    const exit = await runCase(config);
    let result;
    try { result = readJson(join(evidenceDirectory, 'result.json')); }
    catch (error) { result = { status: 'blocked', error: errorRecord(error), reason: 'Worker did not finish a complete result; inspect raw logs and watchdog.' }; }
    if (exit.code !== 0 || exit.expired) result.status = 'blocked';
    manifest.cases.push({ id: scenario, status: result.status, path: evidenceDirectory, exit });
    jsonFile(join(evidence, 'manifest.json'), manifest);
    console.log(`${scenario}: ${result.status}`);
  }
  manifest.validationAfter = await verifyInputs(inputs);
  manifest.codeAfter = codeInventory();
  manifest.codeStable = JSON.stringify(manifest.code) === JSON.stringify(manifest.codeAfter);
  if (!manifest.validationAfter.valid || !manifest.codeStable || interrupted) {
    manifest.status = 'blocked';
    manifest.invalidated = 'Runtime artifacts, runner source, or run lifetime changed; no passing result from this run is acceptance evidence.';
  } else if (manifest.cases.some(item => item.status === 'blocked')) manifest.status = 'blocked';
  else manifest.status = manifest.cases.some(item => item.status === 'failed') ? 'failed' : 'passed';
} catch (error) {
  manifest.status = 'blocked';
  manifest.error = errorRecord(error);
} finally {
  manifest.finishedAt = new Date().toISOString();
  manifest.evidenceFiles = {};
  for (const file of readdirSync(evidence, { recursive: true }).sort()) {
    if (file !== 'manifest.json' && lstatSync(join(evidence, file)).isFile()) {
      manifest.evidenceFiles[file] = await hashFile(join(evidence, file));
    }
  }
  jsonFile(join(evidence, 'manifest.json'), manifest);
  writeFileSync(join(evidence, 'manifest.sha256'), await hashFile(join(evidence, 'manifest.json')) + '\n', { mode: 0o600 });
}
console.log(`Run ${manifest.status}: ${join(evidence, 'manifest.json')}`);
process.exitCode = manifest.status === 'passed' ? 0 : manifest.status === 'failed' ? 1 : 2;
