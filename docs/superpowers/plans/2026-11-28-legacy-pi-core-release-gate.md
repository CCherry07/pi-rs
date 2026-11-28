# Legacy Pi Core Packaging and Release Gate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add C12, cross-platform formal execution, approved divergence accounting, baseline-review tooling, and the clean-commit release gate that turns the preceding scenario evidence into a defensible core-conformance result.

**Architecture:** C12 verifies host/artifact selection and release metadata independently from scenario behavior. A manual/formal GitHub workflow builds pinned Legacy and Rust products, executes manifest-declared platform shards, uploads immutable receipts, and aggregates only shards with identical source/manifest/fixture hashes. A divergence registry is schema-validated and cannot mask unexplained failures; version alignment requires explicit product-owner authority before any version file is changed.

**Tech Stack:** Node conformance runner, Rust/Node release scripts, GitHub Actions matrix jobs, SHA-256 artifact manifests, JSON divergence schema, existing Release Please configuration.

**Spec:** `docs/superpowers/specs/2026-11-28-legacy-pi-core-scenario-conformance-design.md`

## Global Constraints

- Inherit all master and preceding tranche constraints.
- Current dirty-snapshot versions (`Cargo`/Release Please `0.8.6`, npm manifest `2.0.0`, npm lock root `1.0.0`) are evidence of a mismatch, not authority to choose a version.
- Before changing any version file, the product/release owner must explicitly confirm the canonical version. Record that reviewed value in the Task 2 receipt; otherwise C12-02 remains `Blocked` and no version is rewritten.
- Formal workflows may fetch/install exact pinned sources/dependencies during setup, but product scenario execution itself uses only loopback/local fixtures and fake credentials.
- Routine PR CI uses reviewed captures and cannot issue fresh `D`; the formal workflow executes pinned Legacy live.
- Baseline-update tooling creates a review diff/artifact only; it never commits or overwrites captures automatically.
- Every supported release target gets a package-selection cell; native execution occurs on available macOS/Linux/Windows runners and absent target hardware is reported by the declared matrix, not inferred.
- Final reports show evidence-state counts/groups and exact missing cells, never one percentage.

## Review Focus

- A stale `PI_RS_NATIVE_BINDING` must not override the explicitly built local artifact during validation; Task 1 tests stale, absent, wrong-target, and explicit-binding cases.
- Matrix aggregation must reject one shard from another commit/manifest/fixture set even when every cell says pass; Task 4 mutates each identity independently.
- A divergence entry must cover exact scenario variants/cells and have approval/owner/docs/regression; Task 3 rejects broad group-level waivers.
- Baseline promotion must expose old/new raw and semantic diffs and require the expected old hash; Task 4 tests concurrent/stale promotion attempts.
- A clean formal result must be reproducible without installed global `pi`, user HOME, real credentials, or stale build outputs; Tasks 1, 4, and 6 run from a fresh checkout and explicit artifacts.

---

## Task 1: Implement C12-01 standalone/Node host selection and installation independence

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C12-01.mjs`
- Create: `conformance/legacy-pi-core/test/C12-host-selection.test.mjs`
- Modify only on observed failure: `packages/pi/src/native-binding.ts`, `packages/pi/src/index.ts`, Node bin entry points, standalone CLI extension diagnostics, and focused tests

**Interfaces:**
- Produces host-selection records containing requested entry point, resolved CLI/binding canonical path/hash/target, source root, environment override, extension capability, and exit.

- [ ] **Step 1: Write failing C12-01 tests**

Assert standalone selects native product and rejects general JS with actionable Node-host guidance; Node CLI selects the explicit local NAPI binding; stale packaged/external `PI_RS_NATIVE_BINDING` is rejected or ignored according to explicit validation mode; wrong target/architecture fails before launch; explicit extension works in Node; fixture paths do not depend on npm-global/PATH/current user HOME; offline host environment restores exact prior variables after success and failure; offline/other overlap rejects while online/online remains allowed.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C12-host-selection.test.mjs`

Expected: FAIL because C12-01 scenario is absent.

- [ ] **Step 3: Implement through explicit built artifacts**

Build local Node dist/native binding first; run the scenario with a deliberately stale fake binding path and a scrubbed PATH/HOME. Record selected hash from the running host handshake, not only pre-launch resolution.

- [ ] **Step 4: Verify package/bridge/formal evidence**

```bash
(cd packages/pi && npm run check && npm test && npm run build:native && env -u PI_RS_NATIVE_BINDING npm run test:bridge)
node --test conformance/legacy-pi-core/test/C12-host-selection.test.mjs
node scripts/check-legacy-pi-core.mjs formal --manifest "$CORE_MANIFEST" --scenario C12-01 --legacy-dir "$LEGACY_PI_DIR" --rust-cli "$RUST_PI" --node-cli "$NODE_PI" --native-binding "$PI_RS_NATIVE_BINDING_UNDER_TEST" --receipt-dir "$CORE_RECEIPTS/release-c12-host-formal"
```

- [ ] **Step 5: Commit**

Commit as `test(conformance): cover product host selection`.

## Task 2: Resolve version authority and implement C12-02 release consistency

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C12-02.mjs`
- Create: `conformance/legacy-pi-core/test/C12-release.test.mjs`
- Modify after explicit version approval only: `.release-please-manifest.json`
- Modify after explicit version approval only: `Cargo.toml`
- Modify after explicit version approval only: `Cargo.lock`
- Modify after explicit version approval only: `packages/pi/package.json`
- Modify after explicit version approval only: `packages/pi/package-lock.json`
- Modify only if the contract itself is wrong: `packages/pi/scripts/release.ts`, `release-please-config.json`, `.github/workflows/release.yml`, `.github/workflows/release-please.yml`

**Interfaces:**
- Consumes an explicitly approved canonical semantic version; after approval, `.release-please-manifest.json["."]` is the machine-readable authority and every product file must equal it.
- Produces release matrix/artifact/libc/manifest semantic records.

- [ ] **Step 1: Write the failing C12-02 test before changing versions**

Assert Cargo workspace, Release Please manifest, npm package, both npm lock root fields, runtime `VERSION`, and requested tag are identical; each supported native target has one unique runner/NAPI suffix; Linux artifacts use the reviewed glibc 2.36 Bookworm container; Release Please owns version PRs and dispatches the product release workflow without a long-lived npm token.

- [ ] **Step 2: Run and preserve current desired-red**

```bash
node --test conformance/legacy-pi-core/test/C12-release.test.mjs
(cd packages/pi && npm test)
```

Expected on the summarized snapshot: four release tests fail fast at `validateReleaseConfiguration` because `0.8.6`, `2.0.0`, and `1.0.0` differ. Record this as one version-authority blocker; do not claim matrix/glibc/workflow assertions failed because they were not reached.

- [ ] **Step 3: Obtain and record explicit product-owner version approval**

The approval must state the exact canonical version. If it is absent, write a `Blocked` C12-02 receipt with unblock action and stop this task—do not edit version files. If the intended version differs from `.release-please-manifest.json["."]`, update that manifest in the reviewed version-alignment change first.

- [ ] **Step 4: Synchronize all version owners to the approved manifest value**

Use the existing release sync tooling for Cargo.lock and npm lock rather than hand-editing generated lock entries. Do not change target matrix/container/workflows merely to bypass the fail-fast version check.

- [ ] **Step 5: Run the now-reachable release assertions**

```bash
(cd packages/pi && npm run release:sync-lock && npm test && npm run release:check && npm run release:matrix)
node --test conformance/legacy-pi-core/test/C12-release.test.mjs
```

Expected: version check passes, then matrix/glibc/Release Please assertions execute and pass or expose independent focused failures.

- [ ] **Step 6: Repair only independently proven release defects**

For each post-version failure, add/retain a focused `packages/pi/test/release.test.ts` assertion and fix its owning release/workflow file. Do not bundle unrelated release behavior into version synchronization.

- [ ] **Step 7: Commit**

Commit approved version alignment separately as `chore: align product version metadata`; commit any independent release fix separately. Include approval reference in the receipt/commit body, not a secret or chat transcript.

## Task 3: Add a schema-validated divergence register

**Files:**
- Create: `conformance/legacy-pi-core/schemas/divergence.schema.json`
- Create: `conformance/legacy-pi-core/divergences.json`
- Create: `conformance/legacy-pi-core/drivers/divergences.mjs`
- Create: `conformance/legacy-pi-core/test/divergences.test.mjs`
- Create: `docs/legacy-pi-core-divergences.md`

**Interfaces:**
- Produces `loadDivergences(path, manifest)`, `validateDivergenceCoverage(run, register)`, and stable IDs `LPCD-####`.
- Required fields: exact scenario/variant/cell keys, Legacy behavior, Rust behavior, user consequence, rationale, migration guidance, owner, approval date/reference, docs paths, regression paths, and reopen conditions.

- [ ] **Step 1: Write failing divergence tests**

Reject missing approval/owner/regression/docs, wildcard-only scenario groups, cells not in manifest, overlapping active entries, expired/reopened entries, a `P` receipt without exact coverage, and a divergence that tries to waive Failed/Blocked/missing evidence. Accept ACP additive entries and reviewed Rust architecture/timing differences only for exact cells.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/divergences.test.mjs`

Expected: FAIL because registry/validator are absent.

- [ ] **Step 3: Implement registry and migrate existing approved prose**

Add entries only for differences already approved in design/docs (for example ACP additive mode and reviewed MCP publication/connection-reuse timing). Proposed but unapproved differences remain Failed/Blocked and are not inserted as active entries.

- [ ] **Step 4: Verify and commit**

Run focused test plus manifest validation; commit as `docs(conformance): register approved product divergences`.

## Task 4: Add formal shard aggregation and review-only baseline promotion

**Files:**
- Create: `conformance/legacy-pi-core/drivers/shards.mjs`
- Create: `conformance/legacy-pi-core/drivers/baseline-review.mjs`
- Create: `conformance/legacy-pi-core/test/shards-baselines.test.mjs`
- Modify: `scripts/check-legacy-pi-core.mjs`

**Interfaces:**
- Adds commands:
  - `finalize-shards --input "$CORE_RECEIPTS/shards" --output "$CORE_RECEIPTS/formal-aggregate"`
  - `baseline-diff --oracle-receipt "$CORE_RECEIPTS/oracle-capture" --expected-old-hash "$EXPECTED_BASELINE_SHA256" --output "$CORE_RECEIPTS/baseline-review"`
  - `promote-baseline --review "$CORE_RECEIPTS/baseline-review" --approval "$BASELINE_APPROVAL_FILE"` (local explicit operation only)

- [ ] **Step 1: Write failing shard tests**

Assert complete required-cell coverage and identical Rust commit, Legacy commit, manifest hash, fixture/driver hashes, and run nonce. Reject duplicate/missing cells, mixed platform declaration, stale captures, and a passing shard with secret-scan findings.

- [ ] **Step 2: Write failing baseline-review tests**

Assert old/new raw and semantic artifacts plus diff are present; expected old hash must match; concurrent/stale promotion fails; no change is a no-op; promotion cannot run from `capture`/`formal` automatically; approval names exact scenario/variant cells.

- [ ] **Step 3: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/shards-baselines.test.mjs`

Expected: FAIL because shard/baseline modules are absent.

- [ ] **Step 4: Implement aggregation and promotion safeguards**

Finalization writes a new create-only run index. Baseline review writes outside `baselines/`; promotion copies reviewed semantic files only after hash/approval validation and never deletes historical raw receipts.

- [ ] **Step 5: Verify and commit**

Run all harness tests and commit as `test(conformance): aggregate formal evidence safely`.

## Task 5: Add the pinned formal cross-platform workflow

**Files:**
- Create: `.github/workflows/legacy-core-conformance.yml`
- Create: `conformance/legacy-pi-core/drivers/network-sandbox.mjs`
- Create: `conformance/legacy-pi-core/test/network-sandbox.test.mjs`
- Modify: `.github/workflows/ci.yml` for routine validation only

**Interfaces:**
- Formal workflow has `workflow_dispatch`; matrix shards declare platform/group cells and upload receipt artifacts; aggregate job invokes Task 4 finalization.
- Network helper returns an execution wrapper that permits loopback/local child processes and refuses formal execution when the required platform isolation policy cannot be established.

- [ ] **Step 1: Write failing network-policy tests**

Assert loopback succeeds, an external test destination is denied/not attempted, proxy/credential environment is scrubbed, unavailable required isolation yields `Blocked`, and routine mode cannot silently label its weaker environment as formal.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/network-sandbox.test.mjs`

Expected: FAIL because helper/workflow are absent.

- [ ] **Step 3: Implement formal workflow setup**

Use `actions/checkout` for Rust and a second exact-ref checkout of `earendil-works/pi` at `2126569de40d5595dd3f41f7af126001f04c1de0`; run lockfile installs/builds before the isolated scenario phase; build Rust/Node products from one commit; record all artifact hashes. Linux executes complete canonical cells, macOS/Windows execute manifest subsets, then aggregate downloaded receipts.

- [ ] **Step 4: Separate setup network from scenario execution**

Legacy uses `npm run build:offline` after dependencies are installed. During scenario execution, set product offline controls and apply the platform's declared loopback-only policy. If a runner cannot establish required policy, emit Blocked instead of continuing with uncontrolled access.

- [ ] **Step 5: Validate workflow statically and with a local shard simulation**

Run harness tests, action YAML validation available in the repository/tooling, and `finalize-shards` over synthetic Linux/macOS/Windows receipts.

Expected: aggregate rejects mixed identities and accepts complete matching cells.

- [ ] **Step 6: Commit**

Commit as `ci: add formal legacy core conformance workflow`.

## Task 6: Generate the final acceptance report from receipts

**Files:**
- Create: `conformance/legacy-pi-core/drivers/report.mjs`
- Create: `conformance/legacy-pi-core/test/report.test.mjs`
- Create: `docs/legacy-pi-core-conformance.md`
- Modify: `conformance/legacy-pi-core/README.md`
- Modify: `docs/architecture.md` only for accepted invariant/divergence changes

**Interfaces:**
- Produces `renderReport(runIndex, divergences) -> Markdown` and command `report --run <dir> --output <path>`.

- [ ] **Step 1: Write failing report tests**

Assert report lists exact commits/artifacts/platforms, counts by evidence state/group, every missing/Failed/Blocked cell, common-contract `D` plus exact `P`, historical oracle references, and gates not run. Assert it never emits a completion percentage or says complete when one required cell is absent.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/report.test.mjs`

Expected: FAIL because renderer is absent.

- [ ] **Step 3: Implement report renderer**

Committed docs describe the latest reviewed formal receipt available before the candidate is frozen and link/path to evidence without embedding secrets or ephemeral absolute paths. Dirty/routine runs may generate local reports but cannot replace the formal status section. If a newly completed formal run is written into committed docs, that documentation commit becomes a new candidate and Task 7 must run again from its clean commit.

- [ ] **Step 4: Verify and commit**

Run report/manifest/divergence tests, render from a complete synthetic and an incomplete run, then commit as `docs: report legacy core conformance evidence`.

## Task 7: Execute the clean-commit final gate

**Files:**
- No product files should change during this task.
- Generated evidence: `.eval/project-checks/legacy-pi-core/<run-id>/` (ignored)

- [ ] **Step 1: Create a fresh validation worktree/checkout from the exact candidate commit**

Use the `using-git-worktrees` skill at execution time. Confirm `git status --porcelain` is empty; build no artifact from the shared dirty checkout.

- [ ] **Step 2: Build and run the complete local gates**

Run the exact Final Full Gate from the master plan, including full workspace tests/Clippy, complete Node suite, bridge with local binding, release checks, manifest/harness tests, official fixture conformance, and formal all-required-cells execution.

- [ ] **Step 3: Verify final evidence conditions**

Assert every required cell is `D` or has common-contract `D` plus exact approved `P`; no Failed/Blocked/missing/skip; standalone and Node/NAPI agree on shared Rust contract; package versions are coherent; secret scan passes; all platform shards share identities.

- [ ] **Step 4: Run complete GitNexus change analysis**

Run `detect-changes --scope compare --base-ref main` and `--scope all`; neither may be partial/truncated. Review HIGH/CRITICAL risk and resolve before a commit/PR/merge.

- [ ] **Step 5: Preserve receipt without modifying the validated checkout**

Archive/upload the formal run directory and its run-local rendered report; retain all prior Failed/red/history receipts. `git status --porcelain` must remain empty. If the receipt reference must be added to committed documentation, make that documentation commit and restart Task 7 from Step 1 so the final receipt names the new commit.

- [ ] **Step 6: Final review gate**

Confirm `cargo fmt --all -- --check`, `git diff --check`, and clean status still hold without a post-validation commit. Request whole-branch code review before merge. Formal acceptance claims must name the exact Rust commit, Legacy SHA, manifest hash, platform cells, and receipt run ID.
