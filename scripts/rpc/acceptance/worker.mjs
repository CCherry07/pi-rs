// Private per-scenario worker. run.mjs owns an independent outer hard deadline.
import assert from 'node:assert/strict';
import { mkdirSync, readFileSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { SCENARIOS, executeScenario } from './scenarios.mjs';
import { Fixture } from './lib/fixture.mjs';
import { OwnedRpc } from './lib/process.mjs';
import { compareStreams, normalize } from './lib/compare.mjs';
import { errorRecord, hashFile, isolatedEnvironment, jsonFile, readJson, under } from './lib/evidence.mjs';

const config = readJson(process.argv[2]);
const inputs = readJson(config.inputsPath);
const scenario = SCENARIOS.find(item => item.id === config.scenario);
assert(scenario, 'Unknown scenario');
const fixture = new Fixture();
await fixture.listen();
const root = config.workDirectory;
const sandbox = join(root, 'sandbox');
const sourceDirectory = fileURLToPath(new URL('.', import.meta.url));
assert(under(root, resolve(sourceDirectory, '../../../.build/rpc-validation')), 'Worker scratch must be in this checkout\'s owned build area');
const results = {};
const captures = {};

function prepareSandbox() {
  // Only the worker-created sandbox below a fresh run directory is ever removed.
  assert(resolve(sandbox) === resolve(root, 'sandbox'));
  rmSync(sandbox, { recursive: true, force: true });
  const env = isolatedEnvironment(sandbox);
  for (const path of [env.HOME, env.PI_AGENT_DIR, env.XDG_CONFIG_HOME, env.XDG_DATA_HOME, env.XDG_CACHE_HOME, env.TMPDIR, join(sandbox, 'workspace')]) {
    mkdirSync(path, { recursive: true, mode: 0o700 });
  }
  const settings = {
    defaultProvider: 'rpc-fixture', defaultModel: 'fixture', defaultTools: scenario.tools ?? [],
    compaction: { enabled: false }, retry: { enabled: false },
  };
  const models = { providers: { 'rpc-fixture': {
    api: 'openai-completions', baseUrl: fixture.url, apiKey: 'fixture-not-a-secret',
    models: [{ id: 'fixture', name: 'Fixture', reasoning: false, input: ['text'], contextWindow: 32000, maxTokens: 1024,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }],
  } } };
  jsonFile(join(env.PI_AGENT_DIR, 'settings.json'), settings);
  jsonFile(join(env.PI_AGENT_DIR, 'models.json'), models);
  writeFileSync(join(env.PI_AGENT_DIR, 'SYSTEM.md'), 'RPC acceptance controlled system prompt.');
  return { env, settings, models };
}

async function launchPins(target) {
  return Object.fromEntries(await Promise.all([...new Set([
    target.executable, ...target.args.filter(arg => arg.startsWith('/')),
    target.env?.PI_RS_NATIVE_BINDING, scenario.fixture && target.nativePlugin,
  ].filter(Boolean))].map(async path => [path, await hashFile(path)])));
}

try {
  for (const name of config.targets) {
    const target = inputs.targets[name];
    const directory = join(config.evidenceDirectory, name);
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    if (!target || (scenario.fixture && name === 'rust' && !target.nativePlugin)) {
      results[name] = { status: 'blocked', reason: !target ? 'No pinned target artifact' : 'No exact-build native acceptance fixture supplied; JS tests cannot stand in for standalone Rust.' };
      jsonFile(join(directory, 'result.json'), results[name]);
      continue;
    }
    if (process.platform === 'win32' && (scenario.stop || scenario.following)) {
      results[name] = { status: 'blocked', reason: 'These are real POSIX process-signal scenarios, not Windows emulation.' };
      jsonFile(join(directory, 'result.json'), results[name]);
      continue;
    }
    const { env, settings, models } = prepareSandbox();
    fixture.reset(directory);
    const flags = ['--mode', 'rpc', '--no-session', '--no-approve', '--provider', 'rpc-fixture', '--model', 'fixture'];
    if (scenario.fixture) {
      if (name === 'rust') flags.push('--no-extensions', '--plugin', target.nativePlugin);
      else flags.push('--extension', join(sourceDirectory, 'fixtures/extension.mjs'));
      Object.assign(env, {
        RPC_ACCEPTANCE_CONTROL_PORT: String(fixture.port), RPC_ACCEPTANCE_FIXTURE: scenario.fixture,
        RPC_ACCEPTANCE_HOLD_SHUTDOWN: scenario.holdShutdown ? '1' : '0',
      });
    } else flags.push('--no-extensions');
    Object.assign(env, target.env);
    const before = await launchPins(target);
    jsonFile(join(directory, 'input.json'), { scenario, settings, models, systemPrompt: 'RPC acceptance controlled system prompt.', launchPins: before });
    let rpc;
    const observations = {};
    let status = 'passed';
    let error;
    let exit;
    try {
      rpc = new OwnedRpc({
        executable: target.executable, args: [...target.args, ...flags], cwd: join(sandbox, 'workspace'), env,
        directory, timeoutMs: config.timeoutMs,
        onSpawn: identity => process.send?.({ type: 'owned', ...identity }),
      });
      const expectedCode = await executeScenario(scenario, { rpc, fixture, observations });
      exit = await rpc.waitForClose();
      assert.equal(exit.timedOut, false, 'Product hit hard deadline');
      assert.equal(exit.code, expectedCode, 'Wrong numeric exit status');
      assert.equal(exit.signal, null, 'Product exited via unhandled OS signal rather than owned shutdown path');
      assert.deepEqual(exit.transportErrors, []);
      if (!scenario.allowIncompleteOutput) {
        assert.deepEqual(exit.parseErrors, [], 'Non-JSON protocol output');
        assert.equal(exit.trailingBytes, 0, 'Truncated final protocol record');
      }
      assert.equal(existsSync(join(sandbox, 'workspace', 'must-not-exist.txt')), false, 'UI fixture must never execute the write');
    } catch (failure) {
      status = 'failed';
      error = errorRecord(failure);
    } finally {
      if (rpc) {
        exit = await rpc.cleanup();
        process.send?.({ type: 'retired', pid: rpc.pid, pgid: rpc.pgid });
      }
    }
    const after = await launchPins(target);
    if (JSON.stringify(before) !== JSON.stringify(after)) {
      status = 'blocked';
      error = { message: 'Launch artifact drift invalidates this observation.' };
    }
    const raw = rpc ? readFileSync(join(directory, 'stdout.raw')) : Buffer.alloc(0);
    const lastLF = raw.lastIndexOf(10);
    const tail = raw.subarray(lastLF + 1);
    const normalized = normalize(rpc?.records ?? []);
    writeFileSync(join(directory, 'normalized.jsonl'), normalized.records.map(record => JSON.stringify(record) + '\n').join(''), { mode: 0o600 });
    jsonFile(join(directory, 'normalization.json'), { rules: normalized.rules, mappings: normalized.mappings });
    captures[name] = { records: rpc?.records ?? [], tail, parseErrors: rpc?.parseErrors ?? [] };
    observations.uiReleases = fixture.events.filter(event => event.name === 'ui-released');
    observations.shutdownEvents = fixture.events.filter(event => event.name.startsWith('shutdown-'));
    observations.providerRequests = fixture.requests.length;
    observations.providerCancelled = fixture.requests.filter(request => request.closed && !request.done).map(request => request.index);
    results[name] = { status, error, observations, exit, pins: { before, after }, evidenceLevel: 'actual-owned-child-process' };
    jsonFile(join(directory, 'result.json'), results[name]);
    jsonFile(join(directory, 'provider-requests.json'), fixture.requests.map(({ method, url, body }) => ({ method, url, body })));
    await fixture.waitFor(() => fixture.requests.every(request => request.closed) && fixture.gates.every(gate => gate.closed), 'previous process connections closed');
  }
} finally {
  await fixture.close();
}

const comparisons = {};
for (const name of config.targets.filter(name => name !== 'legacy')) {
  if (!captures.legacy || !captures[name]) {
    comparisons[name] = { status: 'blocked', reason: 'Both real process captures are required.' };
    continue;
  }
  const comparison = compareStreams(captures.legacy.records, captures[name].records);
  const tailsEqual = captures.legacy.tail.equals(captures[name].tail);
  const malformed = captures.legacy.parseErrors.length > 0 || captures[name].parseErrors.length > 0;
  let uiWaiter;
  if (scenario.fixture?.includes('ui')) {
    const legacy = results.legacy.observations.uiReleases.map(event => event.data);
    const candidate = results[name].observations.uiReleases.map(event => event.data);
    uiWaiter = { legacy, candidate, status: !legacy.length || !candidate.length ? 'unobserved'
      : JSON.stringify(legacy) === JSON.stringify(candidate) ? 'passed' : 'failed',
    caveat: 'An absent resume marker is inconclusive, not a proof of a leaked UI waiter. Process exit is a separate observation.' };
  }
  comparisons[name] = {
    status: comparison.equal && tailsEqual && !malformed ? 'passed' : 'failed',
    firstDifference: comparison.firstDifference, recordCounts: comparison.recordCounts,
    tailsEqual, tailBytes: { legacy: captures.legacy.tail.length, candidate: captures[name].tail.length }, malformed, uiWaiter,
    rule: 'Complete ordered mixed stdout records, with disclosed identity/time mappings only; unterminated bytes also compare exactly.',
  };
}
const candidateResults = config.targets.filter(name => name !== 'legacy').map(name => results[name]);
const status = results.legacy?.status !== 'passed' || candidateResults.some(result => result.status === 'blocked')
  ? 'blocked' : candidateResults.some(result => result.status !== 'passed')
    || Object.values(comparisons).some(result => result.status !== 'passed' || result.uiWaiter?.status === 'failed')
    ? 'failed' : Object.values(comparisons).some(result => result.uiWaiter?.status === 'unobserved') ? 'blocked' : 'passed';
jsonFile(join(config.evidenceDirectory, 'result.json'), { scenario, status, results, comparisons });
console.log(`${scenario.id}: ${status}`);
if (process.connected) process.disconnect();
