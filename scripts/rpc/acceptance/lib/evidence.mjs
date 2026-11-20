import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createReadStream, lstatSync, readFileSync, readdirSync, readlinkSync, realpathSync, writeFileSync } from 'node:fs';
import { isAbsolute, join, relative, resolve, sep } from 'node:path';
import { execFileSync } from 'node:child_process';

export const ORACLE_COMMIT = '05fd3fbda0bbb9a1c7a76e4bf92ab58ed77c8d94';
export const digest = bytes => createHash('sha256').update(bytes).digest('hex');
export const jsonFile = (path, value) => writeFileSync(path, JSON.stringify(value, null, 2) + '\n', { mode: 0o600 });
export const readJson = path => JSON.parse(readFileSync(path, 'utf8'));

export async function hashFile(path) {
  const hash = createHash('sha256');
  for await (const bytes of createReadStream(path)) hash.update(bytes);
  return hash.digest('hex');
}

/** Sorted filesystem inventory, NOT sorting any RPC records. Includes symlinks and their in-tree referents. */
export async function fingerprint(path) {
  const root = realpathSync(path);
  if (!lstatSync(root).isDirectory()) return hashFile(root);
  const hash = createHash('sha256');
  async function walk(directory) {
    for (const name of readdirSync(directory).sort()) {
      if (name === '.git') continue; // Git administrative cache is not runtime input.
      const absolute = join(directory, name);
      const local = relative(root, absolute).split(sep).join('/');
      const info = lstatSync(absolute);
      if (info.isSymbolicLink()) {
        const target = realpathSync(absolute);
        assert(target === root || target.startsWith(root + sep), `Unpinned external symlink: ${absolute} -> ${target}`);
        hash.update(JSON.stringify(['link', local, readlinkSync(absolute)]) + '\n');
      } else if (info.isDirectory()) {
        hash.update(JSON.stringify(['directory', local]) + '\n');
        await walk(absolute);
      } else if (info.isFile()) {
        hash.update(JSON.stringify(['file', local, info.mode & 0o777, await hashFile(absolute)]) + '\n');
      } else throw new Error(`Unsupported runtime artifact: ${absolute}`);
    }
  }
  await walk(root);
  return hash.digest('hex');
}

export function loadInputs(path) {
  const value = readJson(path);
  assert.equal(value.schemaVersion, 1, 'Expected acceptance artifact schemaVersion 1');
  assert.equal(value.oracle.commit, ORACLE_COMMIT, 'Wrong oracle pin');
  assert(isAbsolute(value.oracle.root), 'oracle.root must be absolute');
  assert(value.provenance?.ownerManifest, 'Must reference the core owner source/build manifest');
  assert(Array.isArray(value.artifacts) && value.artifacts.length > 0);
  const names = new Set();
  for (const artifact of value.artifacts) {
    assert(artifact.name && !names.has(artifact.name), 'Duplicate or missing artifact name');
    names.add(artifact.name);
    assert(isAbsolute(artifact.path), `Absolute artifact path required: ${artifact.name}`);
    assert.match(artifact.sha256, /^[a-f0-9]{64}$/, `SHA-256 required for ${artifact.name}`);
  }
  assert(names.has(value.provenance.ownerManifest), 'Owner source/build manifest must be hash-pinned');
  for (const [name, target] of Object.entries(value.targets)) {
    assert(['legacy', 'rust', 'node'].includes(name));
    assert(isAbsolute(target.executable), `${name} executable must be absolute`);
    assert(Array.isArray(target.args) && target.args.every(arg => typeof arg === 'string'));
    assert(target.artifacts?.length && target.artifacts.every(item => names.has(item)), `${name} needs a pinned inventory`);
    for (const key of Object.keys(target.env ?? {})) {
      assert.equal(key, 'PI_RS_NATIVE_BINDING', `Unexpected target environment field ${key}; no ambient credentials allowed`);
    }
    if (name === 'node') assert(isAbsolute(target.env?.PI_RS_NATIVE_BINDING), 'Node requires an explicit fresh native binding');
    if (target.nativePlugin) assert(isAbsolute(target.nativePlugin));
    const inventory = target.artifacts.map(item => value.artifacts.find(artifact => artifact.name === item));
    const covered = path => inventory.some(artifact => path === artifact.path || path.startsWith(artifact.path + sep));
    assert(covered(target.executable), `${name} executable is not in its pinned inventory`);
    for (const path of [target.env?.PI_RS_NATIVE_BINDING, target.nativePlugin, ...target.args.filter(arg => isAbsolute(arg))].filter(Boolean)) {
      assert(covered(path), `${name} launch input is not in its pinned inventory: ${path}`);
    }
  }
  assert(value.targets.legacy, 'Real legacy target is required');
  return value;
}

export async function verifyInputs(inputs) {
  const git = (...args) => execFileSync('git', args, { cwd: inputs.oracle.root, encoding: 'utf8', timeout: 15_000 }).trim();
  assert.equal(git('rev-parse', 'HEAD'), ORACLE_COMMIT);
  assert.equal(git('status', '--porcelain', '--untracked-files=no'), '', 'Oracle tracked source drift');
  const checks = [];
  for (const artifact of inputs.artifacts) {
    const actual = await fingerprint(artifact.path);
    checks.push({ ...artifact, actual, matches: actual === artifact.sha256 });
  }
  return { checkedAt: new Date().toISOString(), oracleCommit: ORACLE_COMMIT, checks, valid: checks.every(check => check.matches) };
}

export function isolatedEnvironment(sandbox, extra = {}) {
  const env = {
    HOME: join(sandbox, 'home'), USERPROFILE: join(sandbox, 'home'),
    XDG_CONFIG_HOME: join(sandbox, 'config'), XDG_DATA_HOME: join(sandbox, 'data'),
    XDG_CACHE_HOME: join(sandbox, 'cache'), TMPDIR: join(sandbox, 'tmp'),
    PI_AGENT_DIR: join(sandbox, 'home', '.pi', 'agent'),
    PI_OFFLINE: '1', CI: '1', TERM: 'dumb', NO_COLOR: '1', ...extra,
  };
  // An allowlist, never {...process.env}. Proxy variables and provider credentials are excluded.
  for (const name of ['PATH', 'SystemRoot', 'WINDIR', 'ComSpec', 'PATHEXT']) {
    if (process.env[name] !== undefined) env[name] = process.env[name];
  }
  return env;
}

export function errorRecord(error) {
  return { name: error.name, message: error.message, stack: error.stack };
}

export function under(path, root) {
  const delta = relative(resolve(root), resolve(path));
  return delta !== '' && delta !== '..' && !delta.startsWith('..' + sep) && !isAbsolute(delta);
}
