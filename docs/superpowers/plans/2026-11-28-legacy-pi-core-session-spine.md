# Legacy Pi Core Session Spine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add C02–C04 and C11 acceptance, including coding tools and a transparent non-destructive v1/v2/v3-to-v4 resume path, so later product scenarios can rely on deterministic persistence, replacement, cancellation, and cleanup.

**Architecture:** Extend the Foundation scenario/fixture library with scripted provider states, filesystem/process probes, and deterministic lifecycle barriers. Move the existing Legacy session import codec into the session-storage owner, expose one staged RAII import transaction to explicit import and transparent resume, and publish the compatible copy only after generation preparation succeeds. Vertical failures trigger focused regressions in `pi-agent`, tool plugins, `pi-session`, or `MultiSessionManager`; scenario-specific workarounds are forbidden.

**Tech Stack:** Rust 2024, Tokio, `pi-agent`, `pi-session`, coding tool plugins, Node conformance drivers, local scripted providers and child processes.

**Spec:** `docs/superpowers/specs/2026-11-28-legacy-pi-core-scenario-conformance-design.md`

## Global Constraints

- Inherit the master plan and Foundation interfaces unchanged.
- Preserve Pi v4 JSONL, mutation sequence, lanes, provider projection, lazy materialization, and unknown wire extensions.
- Never replay an external side effect merely to recover context.
- Legacy source files are opened read-only and remain byte-identical; a staged destination is removed on validation, preparation, cancellation, or activation failure.
- Current-format v4 resume keeps the selected path and bypasses migration.
- Every race uses fixture barriers or observable states, never sleep-based ordering.
- C11 Rust-only isolated-tree behavior is recorded as `P` plus a real-product regression, not Legacy `D`.

## Review Focus

- Cancellation between staged import and publication must delete only the destination and leave both source and current session untouched; Task 5 pins all three hashes/paths.
- Tool call/result persistence must never expose a dangling assistant call to the next provider request; Tasks 1 and 3 inspect provider requests and JSONL.
- Shell cancellation must remove descendants while retaining bounded and full-output evidence; Task 2 records PIDs and output hashes.
- Queue/abort/retry recovery must preserve accepted queued work exactly once; Task 6 asserts IDs and ordering after reopen.
- A candidate that activates but cannot publish must not leak generation resources or replace the current handle; Tasks 7–8 use publication barriers and leak inventories.

---

## Task 1: Implement C02 provider streaming, tool continuation, failure, and cancellation variants

**Files:**
- Create: `conformance/legacy-pi-core/fixtures/provider-script.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C02-02.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C02-03.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C02-04.mjs`
- Create: `conformance/legacy-pi-core/test/C02-agent-turn.test.mjs`
- Modify only on observed product failure: owning tests under `crates/pi-agent/src/` or `crates/pi-session/tests/`

**Interfaces:**
- Consumes Foundation product/sandbox/receipt APIs.
- Produces `ScriptedTurnServer` with `awaitRequest`, `release`, `failBeforeHeaders`, `failMidStream`, and request-history inspection.

- [ ] **Step 1: Write failing scenario tests**

Add assertions for C02-02 serial versus parallel-safe tool ordering and result-before-next-request; C02-03 transient versus permanent error classification and bounded retry count; C02-04 pre-header cancellation, mid-stream cancellation, and context-overflow terminal state. Assert one terminal `agent_end`/settled state and no provider request after cancellation unless the contract explicitly retries.

- [ ] **Step 2: Run desired-red**

Run: `node --test conformance/legacy-pi-core/test/C02-agent-turn.test.mjs`

Expected: FAIL because scenarios and fixture controls are missing.

- [ ] **Step 3: Implement the fixture and semantic projectors**

Keep provider request bodies and event streams as raw evidence. Project tool IDs/timestamps only through declared normalizers; retain event order, retry count, error class, terminal metadata, and usage.

- [ ] **Step 4: Run baseline then formal C02**

Run focused Node tests, then:

```bash
node scripts/check-legacy-pi-core.mjs baseline --manifest "$CORE_MANIFEST" --group C02 --rust-cli "$RUST_PI" --node-cli "$NODE_PI" --receipt-dir "$CORE_RECEIPTS/session-spine-c02-baseline"
node scripts/check-legacy-pi-core.mjs formal --manifest "$CORE_MANIFEST" --group C02 --legacy-dir "$LEGACY_PI_DIR" --rust-cli "$RUST_PI" --node-cli "$NODE_PI" --native-binding "$PI_RS_NATIVE_BINDING_UNDER_TEST" --receipt-dir "$CORE_RECEIPTS/session-spine-c02-formal"
```

Expected: checked-in comparison emits `R`; same-run matching cells emit `D`; any first unexplained difference remains Failed and receives an owning-module red test before a fix.

- [ ] **Step 5: Commit**

```bash
git add conformance/legacy-pi-core/fixtures/provider-script.mjs conformance/legacy-pi-core/scenarios/C02-0{2,3,4}.mjs conformance/legacy-pi-core/test/C02-agent-turn.test.mjs conformance/legacy-pi-core/baselines/C02
git commit -m "test(conformance): cover core agent turn variants"
```

## Task 2: Implement C03 filesystem, search, shell, and image journeys

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C03-01.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C03-02.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C03-03.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C03-04.mjs`
- Create: `conformance/legacy-pi-core/fixtures/shell-tree.mjs`
- Create: `conformance/legacy-pi-core/fixtures/images/` deterministic PNG fixture
- Create: `conformance/legacy-pi-core/test/C03-tools.test.mjs`
- Modify only on observed failure: owning plugin tests under `plugins/tools/`, `crates/pi-shell/`, or `crates/pi-media/`

**Interfaces:**
- Produces side-effect records `{relativePath, sha256, mode, exists}` and process records `{pid, parentPid, exit, signal, streamedSha256, fullOutputSha256}`.

- [ ] **Step 1: Write failing C03 tests**

Assert C03-01 exact bytes and mutation ordering for read/write/edit plus relative, parent-relative, `~`, and absolute paths; C03-02 deterministic grep/find/ls ordering, context, and truncation marker; C03-03 stream/exit/timeout/cancel and descendant cleanup with retained full output; C03-04 image input and tool-result image policy normalization while raw image hashes remain available.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C03-tools.test.mjs`

Expected: FAIL because C03 scenarios are absent.

- [ ] **Step 3: Implement scenarios using real registered tools**

Invoke each product entry point, not plugin functions directly. Adapter code may reference historical tool fixtures but must record their original oracle revision and must not relabel that history as the new live oracle.

- [ ] **Step 4: Run platform-focused checks**

```bash
node --test conformance/legacy-pi-core/test/C03-tools.test.mjs
cargo test --locked -p pi-plugin-read -p pi-plugin-write -p pi-plugin-edit -p pi-plugin-bash -p pi-plugin-powershell -p pi-plugin-grep -p pi-plugin-find -p pi-plugin-ls -p pi-shell -p pi-media
```

Expected: PASS on the current platform; Windows receipts exercise PowerShell/path variants, Unix receipts exercise process-group cleanup.

- [ ] **Step 5: Capture/review baselines and commit**

Capture the pinned Legacy C03 group, review raw side effects, add semantic captures, then commit scenario/fixture/baseline files with message `test(conformance): cover coding tool journeys`.

## Task 3: Add C04 native-v4 persistence and interrupted-pair evidence

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C04-01.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C04-02.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C04-03.mjs`
- Create: `conformance/legacy-pi-core/test/C04-persistence.test.mjs`
- Modify only on observed failure: `crates/pi-session/src/history/journal/`, `crates/pi-session/src/runtime/recovery.rs`, and focused tests under `crates/pi-session/tests/`

**Interfaces:**
- Produces a history projector that preserves unknown entry fields and reports materialization time, call/result pairing, lanes, sequence, and reopened context.

- [ ] **Step 1: Write failing persistence tests**

Assert no JSONL before first assistant `message_end`; shell-only and quit-before-response remain unsaved; assistant tool call and matching result persist before the next provider request; interruption recovery never projects a dangling call; direct v4 reopen retains path, entries, model/configuration, unknown extension data, and next sequence.

- [ ] **Step 2: Run desired-red**

Run: `node --test conformance/legacy-pi-core/test/C04-persistence.test.mjs`

Expected: FAIL because C04 scenarios/projector are missing.

- [ ] **Step 3: Implement and compare**

Read session bytes only after the product exits or reaches an explicit fixture barrier. Keep full raw JSONL; use a compatibility projector only for common Legacy fields and separately assert Rust-native fields survive reopen.

- [ ] **Step 4: Run focused Rust and product checks**

```bash
node --test conformance/legacy-pi-core/test/C04-persistence.test.mjs
cargo test --locked -p pi-session --lib
cargo test --locked -p pi-session --test boundaries --test system_history
```

Expected: PASS before formal C04-01…03 comparison.

- [ ] **Step 5: Commit**

Commit scenario/tests/reviewed captures as `test(conformance): cover native session persistence`.

## Task 4: Move the Legacy import codec into the session-storage owner

**Files:**
- Create: `crates/pi-session/src/history/import.rs`
- Create: `crates/pi-session/tests/legacy_import.rs`
- Modify: `crates/pi-session/src/history/mod.rs`
- Modify: `crates/pi-session/src/lib.rs`
- Modify: `crates/pi-session/Cargo.toml`
- Modify: `plugins/features/pi-plugin-session-transfer/src/import.rs`
- Modify: `plugins/features/pi-plugin-session-transfer/src/lib.rs`
- Modify: `plugins/features/pi-plugin-session-transfer/Cargo.toml`
- Modify: `docs/architecture.md`

**Interfaces:**
- Produces in `pi_session::history::import`:
  - `SessionFileFormat::{V4, Legacy { version: u32 }}`
  - `inspect_session_file(path: &Path) -> Result<SessionFileFormat, SessionError>`
  - `stage_session_import(source: &Path, destination: &Path) -> Result<StagedSessionImport, SessionError>`
  - `StagedSessionImport::store(&self) -> &SessionStore`
  - `StagedSessionImport::report(&self) -> &LegacySessionImportReport`
  - `StagedSessionImport::commit(self) -> LegacySessionImportReport`
- The existing `import_session_file` remains as a compatibility wrapper that stages then commits.

- [ ] **Step 1: Run GitNexus impact before moving symbols**

Run upstream impact for `import_session_file`, `inspect_session_file`, and `SessionStore::open`; record callers/processes/risk. If UNKNOWN, confirm callers with text search before editing.

- [ ] **Step 2: Write failing `pi-session` codec/RAII tests**

Move/copy reviewed v1/v2/v3/v4 cases into `legacy_import.rs`. Assert successful conversion/provenance/unknown data; source hash/mode unchanged; destination validation; existing destination refusal; malformed/unsupported inputs; destination deletion when a staged guard drops; and persistence only after `commit`.

- [ ] **Step 3: Verify desired-red**

Run: `cargo test --locked -p pi-session --test legacy_import`

Expected: compile failure because `pi_session::history::import` and `StagedSessionImport` do not exist.

- [ ] **Step 4: Move the codec without changing conversion semantics**

Move the conversion implementation from the feature plugin into `pi-session`; add the existing `time` parsing dependency at the owning crate; make the plugin module a thin re-export/wrapper. `Drop` removes an uncommitted destination. Preserve `IMPORT_METADATA_KEY = "piCodingAgentImport"` and source version/header provenance.

- [ ] **Step 5: Verify both owners**

```bash
cargo test --locked -p pi-session --test legacy_import
cargo test --locked -p pi-plugin-session-transfer
cargo clippy --locked -p pi-session -p pi-plugin-session-transfer --all-targets -- -D warnings
```

Expected: all prior explicit `/import` behavior passes plus RAII rollback tests.

- [ ] **Step 6: Commit**

After full-workspace format/diff checks and complete GitNexus change analysis, commit as `refactor(session): centralize legacy import codec`.

## Task 5: Implement transparent non-destructive Legacy resume

**Files:**
- Modify: `crates/pi-session/src/management/replacement.rs`
- Modify: `crates/pi-session/src/runtime/lifecycle.rs`
- Modify: `crates/pi-session/src/management/replacement/tests.rs`
- Create: `crates/pi-session/tests/legacy_resume.rs`
- Create: `conformance/legacy-pi-core/scenarios/C04-04.mjs`
- Create: `conformance/legacy-pi-core/test/C04-legacy-resume.test.mjs`
- Modify: `crates/pi-rpc/tests/commands.rs`

**Interfaces:**
- `SessionTarget::Open` carries both selected source and a generated destination in the managed session directory.
- `PreparedAgentSession::with_activation_notice(level, message)` publishes a transient frontend/RPC notice only after activation.
- Existing public `PiSession::resume_session(&Path)` signature remains unchanged.

- [ ] **Step 1: Run GitNexus impact**

Analyze upstream impact for `SessionTarget`, `ResolvedSessionTarget::resolve`, `PreparedAgentSession::activate`, and `PiSession::resume_session`; warn before any HIGH/CRITICAL edit.

- [ ] **Step 2: Write failing replacement tests**

For each v1/v2/v3 fixture assert: a unique staged v4 path under the current managed session directory; source byte hash unchanged; generation request points at destination; activation commits destination and replaces current; notice states source version/source unchanged/destination; preparation failure, before-switch cancellation, dropped future, and manager shutdown remove destination and preserve current session/path; v4 resume uses source directly and emits no migration notice.

- [ ] **Step 3: Verify desired-red**

Run: `cargo test --locked -p pi-session legacy_resume -- --nocapture`

Expected: existing resume rejects Legacy schema or lacks staging/notice semantics.

- [ ] **Step 4: Implement staged resume**

Inspect only after `session_before_switch` permits the transition. For Legacy input, create a UUID destination in the current store's parent, call `stage_session_import`, prepare generation from its store, attach commit and activation notice, and publish only after activation succeeds. Direct startup open uses the selected source directory as managed destination root. Do not add a second converter.

- [ ] **Step 5: Add product/RPC evidence**

C04-04 drives real `/resume` or corresponding RPC command for v1/v2/v3. Assert transcript/RPC notice, new v4 path/provenance, byte-identical source, and no partial file after injected failure/cancel.

- [ ] **Step 6: Verify green and commit**

```bash
cargo test --locked -p pi-session legacy_resume -- --nocapture
cargo test --locked -p pi-rpc --test commands legacy_resume -- --nocapture
node --test conformance/legacy-pi-core/test/C04-legacy-resume.test.mjs
```

Run the full workspace gate because session storage/replacement contracts changed, then commit as `feat(session): resume legacy history through staged copy`.

## Task 6: Implement C04 navigation, compaction, queue, abort, retry, and recovery

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C04-05.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C04-06.mjs`
- Create: `conformance/legacy-pi-core/test/C04-navigation-recovery.test.mjs`
- Modify only on observed failure: focused tests/code under `crates/pi-session/src/runtime/`, `crates/pi-session/src/management/`, or frontend command owners

**Interfaces:**
- Produces semantic records for transition reason/path/parent, lane/tree, label/name, compaction/branch summary, queue operation ID, abort/retry, and recovered context.

- [ ] **Step 1: Write failing C04-05/06 tests**

Exercise new, resume, fork-before, fork-at, tree navigation, labels, naming; manual/automatic compaction, branch summaries, context edits/usage; queue/steer/follow-up ordering; abort retaining queued work; retry; interrupted recovery reduction; unknown extension wire survival.

- [ ] **Step 2: Run desired-red**

Run: `node --test conformance/legacy-pi-core/test/C04-navigation-recovery.test.mjs`

Expected: FAIL because scenarios are missing.

- [ ] **Step 3: Implement real-product drivers and projectors**

Use command/RPC surfaces each product actually exposes. Where one frontend lacks a command, mark that endpoint non-applicable only if the manifest already declares it; do not substitute direct `AgentSession` calls for acceptance.

- [ ] **Step 4: Verify module and vertical evidence**

```bash
cargo test --locked -p pi-session
node --test conformance/legacy-pi-core/test/C04-navigation-recovery.test.mjs
node scripts/check-legacy-pi-core.mjs formal --manifest "$CORE_MANIFEST" --group C04 --legacy-dir "$LEGACY_PI_DIR" --rust-cli "$RUST_PI" --node-cli "$NODE_PI" --native-binding "$PI_RS_NATIVE_BINDING_UNDER_TEST" --receipt-dir "$CORE_RECEIPTS/session-spine-c04-formal"
```

Expected: all mandatory variants have individual terminal receipts.

- [ ] **Step 5: Commit**

Commit as `test(conformance): cover session navigation and recovery`.

## Task 7: Add deterministic lifecycle barriers to the harness

**Files:**
- Create: `conformance/legacy-pi-core/fixtures/lifecycle-control.mjs`
- Create: `conformance/legacy-pi-core/fixtures/lifecycle-extension.ts`
- Create: `conformance/legacy-pi-core/test/lifecycle-control.test.mjs`

**Interfaces:**
- Produces named barriers: `candidatePrepared`, `candidateActivated`, `beforePublish`, `descendantAcquiring`, `mutationSubmitted`, `release(name)`, and resource inventory snapshots.

- [ ] **Step 1: Write failing barrier tests**

Assert barriers wait for explicit protocol messages, release once, reject unknown/duplicate transitions, cancel waiters on process exit, and inventory child PIDs, listeners, lock files, session registrations, callback IDs, and partial files before/after cleanup.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/lifecycle-control.test.mjs`

Expected: FAIL because lifecycle control fixture is absent.

- [ ] **Step 3: Implement control protocol**

Use a dedicated inherited pipe or loopback control socket, never stdout timing text. Keep presentation/protocol output separate from control messages.

- [ ] **Step 4: Verify and commit**

Run the focused test and commit as `test(conformance): add lifecycle barriers`.

## Task 8: Implement C11 failed preparation, transition ordering, close trees, and mutation interleaving

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C11-01.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C11-02.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C11-03.mjs`
- Create: `conformance/legacy-pi-core/test/C11-lifecycle.test.mjs`
- Modify only on observed failure: `crates/pi-session/src/management/mod.rs`, `crates/pi-session/src/management/replacement.rs`, `domains/coding/src/dynamic_mcp.rs`, `domains/coding/src/dynamic_providers.rs`, and their focused tests

**Interfaces:**
- Consumes Task 7 barriers.
- Produces cleanup inventories and transition-order records.

- [ ] **Step 1: Write failing C11 tests**

C11-01: invalid candidate leaves old generation usable; concurrent reload/restart/new/resume/fork has one declared order and no partial publication. C11-02: manager/slot/owner-tree close covers duplicate close, in-flight acquisition, and activated-unpublished candidate. C11-03: provider/MCP mutation during preparation is applied transactionally or rejected, with no orphan process/callback/transport/lock/session/file.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C11-lifecycle.test.mjs`

Expected: FAIL because C11 scenarios are missing; preserve any product mismatch receipt before fixing.

- [ ] **Step 3: Add the smallest owning regression for each actual mismatch**

Name the regression after the observable boundary, reproduce with existing Rust pause/barrier helpers, and run it red. Do not edit generic runtime based on a scenario ID.

- [ ] **Step 4: Implement minimal owning fixes and rerun unchanged scenarios**

Run `cargo test --locked -p pi-session --lib`, targeted `pi-coding` lifecycle tests, Node host tests if mutation wire is involved, then C11 formal scenarios. Rust-only isolated-tree cells receive approved `P` records plus the real-product regression.

- [ ] **Step 5: Commit**

Commit scenario evidence and any independently reviewed owning fixes; use separate commits for harness and product changes.

## Task 9: Complete the session-spine tranche gate

**Files:**
- Modify: `conformance/legacy-pi-core/README.md`
- Modify: `.github/workflows/ci.yml`
- Modify: `docs/architecture.md` if transparent migration/replacement invariants changed

- [ ] **Step 1: Add routine CI shards**

Shard C02/C03 and C04/C11 baseline comparisons after required artifacts build. Platform jobs run Windows C03 path/PowerShell and macOS process cleanup subsets; undeclared skips fail.

- [ ] **Step 2: Run tranche verification**

```bash
node --test conformance/legacy-pi-core/test/{C02-agent-turn,C03-tools,C04-persistence,C04-legacy-resume,C04-navigation-recovery,C11-lifecycle}.test.mjs
cargo test --locked -p pi-agent -p pi-session -p pi-plugin-session-transfer
cargo clippy --locked -p pi-agent -p pi-session -p pi-plugin-session-transfer --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Then run the full workspace test/Clippy gate because storage and replacement are foundational.

- [ ] **Step 3: Run complete GitNexus change detection and commit**

Expected: no partial/truncated result; review HIGH/CRITICAL risks, then commit CI/docs as `ci: gate legacy session spine`.
