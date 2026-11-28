# Legacy Pi Core Conformance Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Establish the self-contained manifest, artifact, sandbox, process, normalization, comparison, and receipt foundation, then prove it with the C02-01 text-stream golden path.

**Architecture:** Pure Node 22 ESM modules under `conformance/legacy-pi-core/` orchestrate explicitly built product artifacts and local fixtures without adding a production dependency. Raw evidence is immutable, semantic evidence is derived through named normalizers, routine baseline comparison and formal same-run dual execution are separate command modes, and every expected scenario/variant/endpoint/platform cell is enumerated before execution.

**Tech Stack:** Node.js 22 ESM, `node:test`, JSON/JSON Schema, SHA-256, child processes, loopback HTTP/SSE, existing Rust CLI and Node/NAPI binaries.

**Spec:** `docs/superpowers/specs/2026-11-28-legacy-pi-core-scenario-conformance-design.md`

## Global Constraints

- Inherit every constraint from `2026-11-28-legacy-pi-core-conformance-master.md`.
- Pin `legacy/pi@2126569de40d5595dd3f41f7af126001f04c1de0` in one exported constant and in `manifest.json`.
- The committed manifest contains exactly 48 definitions and all variants named in the design; adding/removing a definition requires a design amendment.
- Harness code may use only Node built-ins in this tranche.
- No command updates a checked-in baseline implicitly.
- C02-01 uses a scripted loopback provider and explicit product paths; it does not call a live model.

## Review Focus

- Missing variants/cells must fail validation before any process starts; Task 1 tests omissions at definition, variant, endpoint, and platform levels.
- A stale/global artifact must be rejected even if executable; Task 2 tests source-root containment and exact Legacy commit verification.
- A timeout/cancellation must kill descendants and close loopback listeners; Task 3 uses a child/grandchild fixture and registry leak assertion.
- Normalization must not hide order/status changes; Task 5 tests forbidden deletion, reordering, and success conversion.
- Baseline mode must never emit `D`, and formal mode must never use captures from another run; Tasks 6–8 pin both rules.

---

## File Structure

Create these focused files:

```text
conformance/legacy-pi-core/
  README.md                         # operator contract and evidence semantics
  manifest.json                     # all 48 definitions and required cells
  schemas/manifest.schema.json      # committed interchange contract
  schemas/receipt.schema.json       # cell/run receipt contract
  drivers/manifest.mjs              # validation and required-cell expansion
  drivers/artifacts.mjs             # explicit artifact/commit/hash verification
  drivers/sandbox.mjs               # isolated HOME/cwd/agent/session directories
  drivers/process-registry.mjs      # bounded process/listener cleanup
  drivers/product.mjs               # common product-driver interface
  drivers/legacy.mjs                # pinned Legacy command adapter
  drivers/rust-standalone.mjs       # standalone Rust command adapter
  drivers/rust-node.mjs             # Node/NAPI Rust command adapter
  drivers/receipt.mjs               # immutable cell receipts and run index
  drivers/run.mjs                   # validate/baseline/capture/formal orchestration
  normalize/index.mjs               # named normalizer registry and invariants
  normalize/common.mjs              # tmp path, UUID, timestamp, port replacements
  scenarios/C02-01.mjs              # text-stream golden contract
  fixtures/scripted-provider.mjs    # loopback deterministic provider
  fixtures/process-tree-child.mjs   # descendant-cleanup fixture
  baselines/C02-01/linux/legacy.json
  test/*.test.mjs                   # harness unit/integration tests
scripts/check-legacy-pi-core.mjs     # stable repository entry point
```

## Task 1: Freeze and validate the 48-definition manifest

**Files:**
- Create: `conformance/legacy-pi-core/manifest.json`
- Create: `conformance/legacy-pi-core/schemas/manifest.schema.json`
- Create: `conformance/legacy-pi-core/drivers/manifest.mjs`
- Create: `conformance/legacy-pi-core/test/manifest.test.mjs`

**Interfaces:**
- Produces: `LEGACY_ORACLE_COMMIT`, `loadManifest(path)`, `validateManifest(value)`, and `expandRequiredCells(manifest)`.
- `RequiredCell` fields: `scenarioId`, `variantId`, `endpoint`, `platform`, `acceptanceKind` (`dual` or `product-divergence`).

- [ ] **Step 1: Write the failing manifest tests**

Add tests named:

```js
test("v1 manifest pins the oracle and exactly 48 definitions", async () => {
  const manifest = await loadManifest(manifestPath);
  assert.equal(manifest.legacy.commit, "2126569de40d5595dd3f41f7af126001f04c1de0");
  assert.equal(manifest.scenarios.length, 48);
  assert.deepEqual(manifest.scenarios.map(({ id }) => id), expectedIdsC01ThroughC12);
});

test("every named variant expands to all declared endpoint and platform cells", async () => {
  const cells = expandRequiredCells(await loadManifest(manifestPath));
  assert.equal(new Set(cells.map(cellKey)).size, cells.length);
  assert.ok(cells.every((cell) => cell.variantId && cell.endpoint && cell.platform));
});
```

Also clone the manifest in memory and assert `validateManifest` rejects: one missing definition, one empty variant list, one undeclared skip, ACP marked `dual`, duplicate cell keys, an unknown evidence state, and a normalizer name absent from the registry declaration.

- [ ] **Step 2: Run the test and verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/manifest.test.mjs`

Expected: FAIL because the manifest module/files do not exist.

- [ ] **Step 3: Implement schema and validation**

Use this top-level manifest shape:

```js
/** @typedef {{schemaVersion: 1, legacy: {repository: string, commit: string}, normalizers: string[], scenarios: ScenarioDefinition[]}} CoreManifest */
/** @typedef {{id: string, group: string, contract: string, variants: VariantDefinition[], cells: CellDeclaration[], fixtures: string[], driver: string, normalizers: string[], owningTests: string[], divergence?: string}} ScenarioDefinition */
```

Transcribe C01-01…C12-02 and every semicolon-separated mandatory variant from the design. Encode ACP cells as `product-divergence`; refuse optional/skipped required cells.

- [ ] **Step 4: Run the focused test**

Run: `node --test conformance/legacy-pi-core/test/manifest.test.mjs`

Expected: PASS with exactly 48 definitions and no duplicate/missing required cells.

- [ ] **Step 5: Commit**

```bash
git add conformance/legacy-pi-core/manifest.json conformance/legacy-pi-core/schemas/manifest.schema.json conformance/legacy-pi-core/drivers/manifest.mjs conformance/legacy-pi-core/test/manifest.test.mjs
git commit -m "test(conformance): freeze legacy core manifest"
```

## Task 2: Verify explicit product artifacts and source identity

**Files:**
- Create: `conformance/legacy-pi-core/drivers/artifacts.mjs`
- Create: `conformance/legacy-pi-core/test/artifacts.test.mjs`

**Interfaces:**
- Consumes: `LEGACY_ORACLE_COMMIT` from Task 1.
- Produces: `resolveArtifacts({repositoryRoot, legacyDir, rustCli, nodeCli, nativeBinding, formal}) -> Promise<ArtifactSet>` and `sha256File(path)`.
- `ArtifactSet` records canonical paths, SHA-256, Rust git commit/dirty flag, Legacy commit/dirty flag, and Node/native binding hashes.

- [ ] **Step 1: Write failing artifact-selection tests**

Assert exact-path success for temporary executables and rejection for: Legacy commit mismatch; dirty Legacy worktree in formal mode; dirty Rust worktree in formal mode; missing/non-executable artifacts; a relative path escaping the repository; a `pi` resolved only from `PATH`; Node CLI under another installation; native binding outside the requested build root; and stale hash metadata.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/artifacts.test.mjs`

Expected: FAIL because `resolveArtifacts` is missing.

- [ ] **Step 3: Implement artifact verification**

Use `git -C <root> rev-parse HEAD`, `git status --porcelain`, canonical `realpath`, executable checks, and streaming SHA-256. Artifact resolution must never search `PATH`. Formal mode requires clean source roots and exact Legacy SHA; routine mode records but does not conceal dirty Rust state.

- [ ] **Step 4: Verify green**

Run: `node --test conformance/legacy-pi-core/test/artifacts.test.mjs`

Expected: PASS; the mismatch tests fail before a scenario child is spawned.

- [ ] **Step 5: Commit**

```bash
git add conformance/legacy-pi-core/drivers/artifacts.mjs conformance/legacy-pi-core/test/artifacts.test.mjs
git commit -m "test(conformance): verify product artifacts"
```

## Task 3: Build isolated sandboxes and deterministic process cleanup

**Files:**
- Create: `conformance/legacy-pi-core/drivers/sandbox.mjs`
- Create: `conformance/legacy-pi-core/drivers/process-registry.mjs`
- Create: `conformance/legacy-pi-core/fixtures/process-tree-child.mjs`
- Create: `conformance/legacy-pi-core/test/sandbox-process.test.mjs`

**Interfaces:**
- Produces: `createScenarioSandbox({root, cellKey}) -> ScenarioSandbox`, `ProcessRegistry.spawn(spec)`, `ProcessRegistry.registerCloseable(value)`, `ProcessRegistry.closeAll(reason)`, and `runBounded(spec)`.
- `ScenarioSandbox` exposes isolated `home`, `cwd`, `agentDir`, `sessionDir`, `tempDir`, `env`, and `dispose()`.

- [ ] **Step 1: Write failing sandbox/process tests**

Assert permissions and separation for HOME/cwd/agent/session directories; scrub credential/proxy/provider variables; preserve only an allowlisted environment; reject fixture paths outside the sandbox; kill a child and grandchild on timeout/cancellation; close a registered loopback server; and report zero live children/listeners after `closeAll` runs twice.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/sandbox-process.test.mjs`

Expected: FAIL because the sandbox and registry do not exist.

- [ ] **Step 3: Implement sandbox and registry**

Use process groups on Unix and `taskkill /T` on Windows. `runBounded` returns exit/signal/stdout/stderr/timing markers and throws a typed timeout only after cleanup completes. Do not use sleeps as assertions; the process-tree fixture writes ready/child-PID markers through pipes.

- [ ] **Step 4: Verify green on the current platform**

Run: `node --test conformance/legacy-pi-core/test/sandbox-process.test.mjs`

Expected: PASS and no process from `process-tree-child.mjs` remains.

- [ ] **Step 5: Commit**

```bash
git add conformance/legacy-pi-core/drivers/sandbox.mjs conformance/legacy-pi-core/drivers/process-registry.mjs conformance/legacy-pi-core/fixtures/process-tree-child.mjs conformance/legacy-pi-core/test/sandbox-process.test.mjs
git commit -m "test(conformance): isolate fixture processes"
```

## Task 4: Define immutable raw evidence and receipt state transitions

**Files:**
- Create: `conformance/legacy-pi-core/schemas/receipt.schema.json`
- Create: `conformance/legacy-pi-core/drivers/receipt.mjs`
- Create: `conformance/legacy-pi-core/test/receipt.test.mjs`

**Interfaces:**
- Consumes: `RequiredCell[]` and `ArtifactSet`.
- Produces: `writeRawArtifact`, `writeCellReceipt`, `readCellReceipt`, `finalizeRunIndex`, and `scanReceiptSecrets`.

- [ ] **Step 1: Write failing receipt tests**

Assert that writes are create-only and atomic; each receipt binds manifest/fixture/driver/artifact hashes; state is one of the seven design states; baseline mode cannot write `D`; formal `D` requires same-run Legacy plus applicable Rust raw artifacts; mixed commits/manifests/platforms are rejected; missing required cells fail finalization; and a `secret-marker` anywhere in raw or semantic output fails the scan without echoing the secret.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/receipt.test.mjs`

Expected: FAIL because receipt functions are missing.

- [ ] **Step 3: Implement receipt persistence**

Use sibling temporary files plus `rename`, `O_EXCL`/`wx` semantics for final files, relative artifact references, and a run index with counts by state/group rather than one completion percentage. Existing receipts are never overwritten.

- [ ] **Step 4: Verify green**

Run: `node --test conformance/legacy-pi-core/test/receipt.test.mjs`

Expected: PASS including mixed-run and secret rejection.

- [ ] **Step 5: Commit**

```bash
git add conformance/legacy-pi-core/schemas/receipt.schema.json conformance/legacy-pi-core/drivers/receipt.mjs conformance/legacy-pi-core/test/receipt.test.mjs
git commit -m "test(conformance): add immutable run receipts"
```

## Task 5: Implement reviewed normalization and semantic comparison

**Files:**
- Create: `conformance/legacy-pi-core/normalize/index.mjs`
- Create: `conformance/legacy-pi-core/normalize/common.mjs`
- Create: `conformance/legacy-pi-core/test/normalize.test.mjs`

**Interfaces:**
- Produces: `NORMALIZERS`, `applyDeclaredNormalizers(records, names, context)`, and `compareScenarioCell(oracle, candidate, contract)`.

- [ ] **Step 1: Write failing normalizer tests**

Test replacements for declared temporary roots, generated UUIDs, timestamps, and assigned loopback ports. Add mutation tests proving a normalizer is rejected if it changes record count/order/type, changes `ok:false` to success, removes unknown history fields, inserts future data into an earlier record, or touches a field outside its declared JSON paths.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/normalize.test.mjs`

Expected: FAIL because the normalizer registry is missing.

- [ ] **Step 3: Implement normalizer registry and invariants**

Every normalizer exports `{name, paths, normalize(value, context)}`. Apply it only to copied parsed records, preserve raw bytes separately, and validate structural invariants before and after each rule. `compareScenarioCell` returns ordered field-level differences; it never updates expected output.

- [ ] **Step 4: Verify green**

Run: `node --test conformance/legacy-pi-core/test/normalize.test.mjs`

Expected: PASS including all forbidden transformations.

- [ ] **Step 5: Commit**

```bash
git add conformance/legacy-pi-core/normalize conformance/legacy-pi-core/test/normalize.test.mjs
git commit -m "test(conformance): constrain semantic normalization"
```

## Task 6: Add real-product drivers and keep baseline/formal modes distinct

**Files:**
- Create: `conformance/legacy-pi-core/drivers/product.mjs`
- Create: `conformance/legacy-pi-core/drivers/legacy.mjs`
- Create: `conformance/legacy-pi-core/drivers/rust-standalone.mjs`
- Create: `conformance/legacy-pi-core/drivers/rust-node.mjs`
- Create: `conformance/legacy-pi-core/drivers/run.mjs`
- Create: `conformance/legacy-pi-core/test/product-driver.test.mjs`

**Interfaces:**
- Consumes: artifact, sandbox, process, receipt, and normalizer APIs from Tasks 2–5.
- Produces: `runProductScenario(driver, scenario, sandbox, registry)`, `runBaseline(options)`, `captureOracle(options)`, and `runFormal(options)`.

- [ ] **Step 1: Write failing mode/driver tests**

Use tiny fake executables to assert argv/env/cwd/stdin capture and endpoint identity. Verify `runBaseline` reads a committed oracle capture and can emit only `R`/Failed/Blocked; `captureOracle` writes only run-local `O`; `runFormal` launches Legacy and Rust in one run and issues `D` only after both semantic results match. Assert a formal run rejects a capture path from an older run.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/product-driver.test.mjs`

Expected: FAIL because product drivers do not exist.

- [ ] **Step 3: Implement drivers**

Legacy command defaults to `<legacyDir>/packages/coding-agent/dist/bundle/cli.js`; Node/Rust command defaults to the explicit `--node-cli`; standalone uses explicit `--rust-cli`. Drivers must identify the mode they actually invoke (print/JSON/RPC/TUI), capture protocol and presentation descriptors separately, and reject an unsupported scenario cell rather than silently reroute it.

- [ ] **Step 4: Verify green**

Run: `node --test conformance/legacy-pi-core/test/product-driver.test.mjs`

Expected: PASS; fake process invocation proves no `PATH` lookup and no cross-run `D`.

- [ ] **Step 5: Commit**

```bash
git add conformance/legacy-pi-core/drivers/product.mjs conformance/legacy-pi-core/drivers/legacy.mjs conformance/legacy-pi-core/drivers/rust-standalone.mjs conformance/legacy-pi-core/drivers/rust-node.mjs conformance/legacy-pi-core/drivers/run.mjs conformance/legacy-pi-core/test/product-driver.test.mjs
git commit -m "test(conformance): add product run modes"
```

## Task 7: Prove the foundation with C02-01

**Files:**
- Create: `conformance/legacy-pi-core/fixtures/scripted-provider.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C02-01.mjs`
- Create: `conformance/legacy-pi-core/test/C02-01.test.mjs`
- Create after reviewed capture: `conformance/legacy-pi-core/baselines/C02-01/linux/legacy.json`

**Interfaces:**
- Produces: `C02_01` scenario with variants `text-stream` and `terminal-metadata`; provider fixture methods `start()`, `waitForRequest(index)`, `releaseChunk(index)`, and `close()`.

- [ ] **Step 1: Write the failing scenario test**

Assert the fixture records the exact provider request; releases assistant deltas only after explicit barriers; products emit ordered user acceptance, assistant deltas, terminal assistant message, usage/response metadata, and settled/agent-end records; session history agrees with the terminal message; and cleanup closes the provider and product.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C02-01.test.mjs`

Expected: FAIL because the fixture/scenario are missing.

- [ ] **Step 3: Implement fixture and scenario projection**

Use a loopback OpenAI-compatible SSE fixture with fake model/credentials. Keep endpoint-specific raw records, then project only the shared semantic contract. Do not normalize away missing usage, response metadata, terminal fields, or event-order differences.

- [ ] **Step 4: Capture and review the pinned Legacy baseline**

Run:

```bash
node conformance/legacy-pi-core/drivers/run.mjs capture \
  --manifest conformance/legacy-pi-core/manifest.json \
  --legacy-dir "$LEGACY_PI_DIR" \
  --scenario C02-01 \
  --receipt-dir .eval/project-checks/legacy-pi-core/foundation-oracle
```

Expected: `O` receipts from exact commit `2126569…`; source is clean; no baseline file changes occur. Review raw/semantic output, then copy only the reviewed semantic capture into the baseline path in a dedicated diff.

- [ ] **Step 5: Run routine Rust comparison**

Run:

```bash
node --test conformance/legacy-pi-core/test/C02-01.test.mjs
node conformance/legacy-pi-core/drivers/run.mjs baseline \
  --manifest conformance/legacy-pi-core/manifest.json \
  --scenario C02-01 \
  --rust-cli target/debug/pi \
  --node-cli packages/pi/dist/bin/pi.js
```

Expected: tests pass; baseline run emits `R`, not `D`.

- [ ] **Step 6: Run same-run formal smoke**

Run the equivalent `formal` command with `--legacy-dir`, `--rust-cli`, and `--node-cli`.

Expected: C02-01 cells emit `D` only if all applicable products match; otherwise preserve Failed evidence and stop for the owning-module workflow.

- [ ] **Step 7: Commit**

```bash
git add conformance/legacy-pi-core/fixtures/scripted-provider.mjs conformance/legacy-pi-core/scenarios/C02-01.mjs conformance/legacy-pi-core/test/C02-01.test.mjs conformance/legacy-pi-core/baselines/C02-01/linux/legacy.json
git commit -m "test(conformance): add core text-stream smoke"
```

## Task 8: Add the stable command, documentation, and routine CI gate

**Files:**
- Create: `scripts/check-legacy-pi-core.mjs`
- Create: `conformance/legacy-pi-core/README.md`
- Modify: `.github/workflows/ci.yml`
- Create: `conformance/legacy-pi-core/test/cli.test.mjs`

**Interfaces:**
- Produces stable commands `validate`, `baseline`, `capture`, and `formal`; forwards explicit artifact arguments to `drivers/run.mjs`.

- [ ] **Step 1: Write failing command tests**

Assert `validate` works without Legacy installation; baseline requires explicit Rust artifacts; capture/formal require `--legacy-dir`; unknown scenario/cell/normalizer fails; and no subcommand writes under `baselines/` except a separately documented manual promotion step that requires an expected old hash.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/cli.test.mjs`

Expected: FAIL because the script is missing.

- [ ] **Step 3: Implement CLI and README**

Document build prerequisites, explicit artifact paths, evidence states, baseline review procedure, formal same-run requirements, receipt locations, and failure-to-fix workflow. Add CI steps to run all harness unit tests, manifest validation, and the C02-01 routine baseline smoke after building the needed Rust/Node artifacts. Do not install or run Legacy in routine CI.

- [ ] **Step 4: Run the tranche gate**

```bash
node --test conformance/legacy-pi-core/test/*.test.mjs
node scripts/check-legacy-pi-core.mjs validate
cargo test --locked -p pi-cli -p pi-coding -p pi-test-support
cargo clippy --locked -p pi-cli -p pi-coding -p pi-test-support --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Expected: all pass; the run summary labels checked-in-capture comparison as regression evidence only.

- [ ] **Step 5: Run GitNexus change analysis and commit**

```bash
node .gitnexus/run.cjs detect-changes --scope all --repo .
git add scripts/check-legacy-pi-core.mjs conformance/legacy-pi-core/README.md conformance/legacy-pi-core/test/cli.test.mjs .github/workflows/ci.yml
git commit -m "ci: add legacy core conformance foundation"
```

Expected: GitNexus output is complete (not partial/truncated); commit contains no generated `.eval` receipt.
