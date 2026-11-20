#!/usr/bin/env node
// Explicit, repeatable isolated checkout/dependency preparation; never writes the source checkout.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const commit = '05fd3fbda0bbb9a1c7a76e4bf92ab58ed77c8d94';
const root = fileURLToPath(new URL('../../', import.meta.url));
const args = process.argv.slice(2);
const sourceIndex = args.indexOf('--source');
assert(sourceIndex >= 0 && args[sourceIndex + 1], 'Usage: prepare-oracle.mjs --source <local-git-checkout> --evidence-dir <persistent-directory> [--oracle-root <disposable-directory>] [--skip-install]');
const evidenceIndex = args.indexOf('--evidence-dir');
assert(evidenceIndex >= 0 && args[evidenceIndex + 1], 'Required: --evidence-dir <persistent-directory>');
const evidenceRoot = resolve(args[evidenceIndex + 1]);
mkdirSync(evidenceRoot, { recursive: true, mode: 0o700 });
const evidence = mkdtempSync(resolve(evidenceRoot, 'oracle-preparation-'));
const metadataPath = resolve(evidence, 'preparation.json');
const source = resolve(args[sourceIndex + 1]);
const oracleIndex = args.indexOf('--oracle-root');
assert(oracleIndex < 0 || args[oracleIndex + 1], '--oracle-root requires a directory');
const oracleRoot = oracleIndex < 0 ? resolve(root, 'target/rpc-oracle') : resolve(args[oracleIndex + 1]);
const destination = resolve(oracleRoot, 'legacy');
assert(source !== destination, 'Source must be separate from the owned destination');
const git = (cwd, ...parameters) => execFileSync('git', parameters, { cwd, encoding: 'utf8' }).trim();
assert.equal(git(source, 'rev-parse', `${commit}^{commit}`), commit);
mkdirSync(oracleRoot, { recursive: true });
if (!existsSync(destination)) {
  // --no-local avoids shared object hardlinks/alternates. No branch in the source is modified.
  execFileSync('git', ['clone', '--no-local', '--no-checkout', '--', source, destination], { stdio: 'inherit' });
  execFileSync('git', ['checkout', '--detach', commit], { cwd: destination, stdio: 'inherit' });
}
assert.equal(git(destination, 'rev-parse', 'HEAD'), commit, 'Existing oracle checkout has a different commit');
assert.equal(git(destination, 'status', '--porcelain', '--untracked-files=no'), '', 'Refusing modified oracle sources');
const lockSha256 = createHash('sha256').update(readFileSync(resolve(destination, 'package-lock.json'))).digest('hex');
writeFileSync(metadataPath, `${JSON.stringify({
  status: 'preparing', source, oracleRoot, destination, commit, tree: git(destination, 'rev-parse', 'HEAD^{tree}'), lockSha256,
  node: process.version, dependencyCommand: 'npm ci --ignore-scripts --no-audit --no-fund',
}, null, 2)}\n`);
if (!args.includes('--skip-install')) {
  execFileSync(process.platform === 'win32' ? 'npm.cmd' : 'npm', ['ci', '--ignore-scripts', '--no-audit', '--no-fund'], { cwd: destination, stdio: 'inherit' });
  assert.equal(git(destination, 'status', '--porcelain', '--untracked-files=no'), '', 'Dependency installation modified oracle sources');
}
// Follow the oracle's nix/package.nix: hydrate ignored provider data from the
// content-addressed catalog pinned by that exact commit, not today's live catalog.
const catalogPin = JSON.parse(readFileSync(resolve(destination, 'nix/model-catalog.json'), 'utf8')).revision;
assert.match(catalogPin, /^sha256-[a-f0-9]{64}$/);
const catalogUrl = `https://pi.dev/api/models/revisions/${catalogPin}?types=chat,image,classifier`;
const catalogPath = resolve(oracleRoot, 'models.all.json');
const expectedHash = catalogPin.slice('sha256-'.length);
const validCatalog = existsSync(catalogPath)
  && createHash('sha256').update(readFileSync(catalogPath)).digest('hex') === expectedHash;
if (!validCatalog) {
  const response = await fetch(catalogUrl, { signal: AbortSignal.timeout(30000) });
  assert(response.ok, `Catalog download failed: ${response.status}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  assert.equal(createHash('sha256').update(bytes).digest('hex'), expectedHash, 'Pinned catalog digest mismatch');
  writeFileSync(catalogPath, bytes);
}
execFileSync(process.execPath, ['packages/ai/scripts/hydrate-model-catalog.ts', catalogPath], { cwd: destination, stdio: 'inherit' });
assert.equal(git(destination, 'status', '--porcelain', '--untracked-files=no'), '', 'Hydration modified oracle source');
const metadata = JSON.parse(readFileSync(metadataPath, 'utf8'));
writeFileSync(metadataPath, `${JSON.stringify({ ...metadata, status: 'ready', installRan: !args.includes('--skip-install'), catalogUrl, catalogSha256: expectedHash }, null, 2)}\n`);
console.log(`Prepared pinned oracle at ${destination}`);
console.log(`Preparation manifest: ${metadataPath}`);
