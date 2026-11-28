# Legacy Pi Core-Scenario Conformance Master Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build and execute a release-grade, 48-definition conformance program that compares pinned Legacy Pi with the applicable standalone Rust, Node/NAPI Rust, ACP, RPC, and PTY products without replacing intentional Rust architecture.

**Architecture:** A Node orchestrator owns the manifest, artifact verification, isolated fixture processes, semantic normalization, comparison, and receipts. Seven independently reviewable tranches establish the harness, then add the session spine, composition, extension, MCP, frontend, and release gates; product fixes occur at the deepest owning seam and retain focused red/green evidence. Checked-in Legacy captures support routine Rust CI, while only a same-run pinned Legacy plus Rust execution can issue fresh `D` evidence.

**Tech Stack:** Rust 2024 workspace, Tokio, Node.js 22 ESM and `node:test`, TypeScript/tsx for the Node host, Python 3 plus pinned `pyte` for Unix PTY semantics, JSON/JSON Schema, local HTTP/SSE/stdio fixtures, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-11-28-legacy-pi-core-scenario-conformance-design.md`

## Global Constraints

- New vertical scenarios use exactly `legacy/pi@2126569de40d5595dd3f41f7af126001f04c1de0`; existing Prompt, Tool, RPC, and MCP captures retain their historical oracle revisions.
- The v1 manifest contains exactly 48 required definitions, C01-01 through C12-02; every named variant needs its own cell receipt.
- Formal `D` evidence requires pinned Legacy and every applicable Rust product to execute under one manifest hash, fixture hash set, platform cell, and run index.
- A Rust match against a checked-in capture is regression evidence only; it must not be relabeled as fresh `D`.
- `ACP` has no Legacy counterpart and therefore records a Rust-only `P` gate, never false dual-product evidence.
- JavaScript extension UI scenarios run through the Node/NAPI Rust interactive product; standalone Rust covers native-plugin UI and actionable general-JavaScript rejection only.
- Legacy v1/v2/v3 resume creates a staged v4 copy, leaves the source byte-identical, publishes only after full preparation, and shares the explicit import codec and rollback path.
- Fixture execution uses fake credentials and only loopback servers/local subprocesses; it must not depend on live providers, paid requests, user home data, or installed global packages.
- Generated receipts live under ignored `.eval/project-checks/legacy-pi-core/<run-id>/`; only reviewed deterministic baselines are committed.
- Normalizers may replace declared volatile values but may not drop/reorder records, convert failures to success, or erase Rust-native wire extensions.
- Required evidence states are `S`, `O`, `R`, `D`, `P`, `Blocked`, and `Failed`; skips, timeouts, `S`, `O`, and `R` do not constitute acceptance.
- Concurrency fixtures use barriers and observable state, never sleeps as correctness assertions.
- Implementation begins in a clean isolated worktree from an explicitly agreed baseline. Do not execute these plans in the current shared dirty checkout.
- Before editing a named symbol, run GitNexus upstream impact analysis and warn on HIGH/CRITICAL risk; treat UNKNOWN as unresolved and confirm it with text search. Before each commit/PR, run complete, non-partial GitNexus change detection.
- Use repository commit conventions. Each task's focused check is followed by affected-package tests, targeted Clippy, `cargo fmt --all -- --check`, and `git diff --check`; public/foundational contract changes additionally run the full workspace gate.
- Formal acceptance is rebuilt from one clean commit and records source and artifact SHA-256 hashes. Historical receipts are immutable.
- Commands in every tranche assume these explicit inputs are exported: `CORE_MANIFEST=conformance/legacy-pi-core/manifest.json`, `CORE_RECEIPTS=.eval/project-checks/legacy-pi-core`, `LEGACY_PI_DIR` is the pinned clean checkout, `RUST_PI` is the just-built standalone executable, `NODE_PI=packages/pi/dist/bin/pi.js`, and `PI_RS_NATIVE_BINDING_UNDER_TEST` is the just-built local NAPI library. A missing input is an error, never a `PATH` fallback.

## Review Focus

- **Manifest completeness versus apparent green output:** validation must fail if one required definition, named variant, endpoint, or platform cell is absent even when every emitted receipt passed. Pinned in Foundation Tasks 1 and 6.
- **Cancellation after side effects begin:** cancellation must retain admitted durable work, remove only staged/unpublished artifacts, and leave no descendant processes or transports. Pinned in Session Spine Tasks 2, 6, and 8 and MCP Tasks 3 and 7.
- **Cross-process file races and crashes:** session migration and OAuth storage must preserve source files, use atomic replacement, recover stale work, and avoid lost updates. Pinned in Session Spine Task 5 and MCP Tasks 4–5.
- **Stale or wrong product artifact:** formal runs must reject global binaries, stale NAPI bindings, dirty commits, mixed shard commits, and an unpinned Legacy checkout before launching a scenario. Pinned in Foundation Tasks 2 and 6 and Release Task 1.
- **Over-normalization and secret leakage:** semantic comparison must retain record order and error class, while raw/semantic receipts and diagnostics must contain no credential markers. Pinned in Foundation Tasks 4–5, MCP Task 6, and Release Task 5.

---

## Plan Suite and Dependency Order

Execute the plans in this exact order. A tranche may be reviewed and merged independently, but the next tranche consumes the committed interfaces of all earlier tranches.

| Tranche | Plan | Required deliverable | Acceptance boundary |
| --- | --- | --- | --- |
| 1 | `2026-11-28-legacy-pi-core-foundation.md` | Manifest, schemas, artifacts, sandbox/process control, receipt/normalizer engine, C02-01 smoke | 48 definitions validate; routine baseline and formal same-run modes cannot be confused |
| 2 | `2026-11-28-legacy-pi-core-session-spine.md` | C02–C04, C03 tools, C11 lifecycle, transparent Legacy session resume | Session persistence/replacement and process cleanup are reliable enough for later groups |
| 3 | `2026-11-28-legacy-pi-core-composition.md` | C01, C05, C06 | Configuration/trust, provider/auth protocol families, prompts/context/skills are evidenced |
| 4 | `2026-11-28-legacy-pi-core-extensions.md` | C07 | Node/NAPI extension lifecycle and standalone rejection are evidenced |
| 5 | `2026-11-28-legacy-pi-core-mcp.md` | C08 and OAuth-store interoperability | Static/dynamic MCP, HTTP recovery, auth, and cleanup are evidenced |
| 6 | `2026-11-28-legacy-pi-core-frontends.md` | C09, C10, ACP additive gate | Real print/NDJSON/RPC/PTY products and terminal restoration are evidenced |
| 7 | `2026-11-28-legacy-pi-core-release-gate.md` | C12, divergence register, CI matrix, formal clean-commit report | All required cells are `D` or common-contract `D` plus approved `P`; no Blocked/Failed |

## Shared Interfaces Frozen by Tranche 1

Later plans must consume these names instead of inventing parallel harness APIs:

```js
loadManifest(path) -> Promise<CoreManifest>
expandRequiredCells(manifest) -> RequiredCell[]
resolveArtifacts(options) -> Promise<ArtifactSet>
createScenarioSandbox(options) -> Promise<ScenarioSandbox>
runProductScenario(driver, scenario, sandbox, registry) -> Promise<RawScenarioResult>
applyDeclaredNormalizers(raw, declarations) -> SemanticScenarioResult
compareScenarioCell(oracle, candidate, contract) -> ComparisonResult
writeCellReceipt(runDirectory, receipt) -> Promise<void>
finalizeRunIndex(runDirectory, expectedCells) -> Promise<RunIndex>
```

The exact JSDoc records and JSON schemas are defined in the Foundation plan. Any signature change after Tranche 1 requires updating every consuming plan and rerunning manifest/receipt compatibility tests.

## Evidence and Stop Rules

1. Scenario execution first preserves raw output and identifies the first semantic divergence.
2. If the divergence is an obvious harness defect, fix the harness with a failing harness test and rerun the unchanged scenario.
3. If it is a product defect, stop that scenario, add the smallest failing regression in the owning Rust/Node module, run GitNexus impact, fix that seam, and retain both red and green receipts.
4. If behavior is intentionally different, do not normalize it away. Add a proposed divergence record and wait for review before assigning `P`.
5. If observation is impossible, issue `Blocked` with a concrete unblock action. Do not skip or infer another platform's result.
6. Three unsuccessful fixes to one symptom stop the tranche for architectural review.

## Per-Tranche Entry Gate

- [ ] Previous tranche commit is clean and its required focused checks pass from that commit.
- [ ] `node .gitnexus/run.cjs analyze --index-only --repo .` has produced a current index; `meta.json` is newer than the base commit or the executor records why it cannot be refreshed.
- [ ] The implementation worktree contains no unrelated staged/unstaged files.
- [ ] The executor has read this master plan, the tranche plan, the design spec, `AGENTS.md`, and any owning architecture/CLI/native-plugin docs named by the tranche.
- [ ] Product artifact paths are explicit; `PI_RS_NATIVE_BINDING` is unset unless the scenario deliberately tests binding selection.

## Per-Tranche Exit Gate

- [ ] Every task-specific desired-red was observed or the plan explicitly says the test protects already-correct behavior; no test was weakened to obtain green.
- [ ] All new scenario cells have raw evidence, semantic evidence, cleanup evidence, and one terminal state.
- [ ] No receipt contains `secret-marker`, fixture passwords/tokens, real home paths, or unexpected external hosts.
- [ ] `node scripts/check-legacy-pi-core.mjs validate` passes and rejects a deliberately incomplete manifest fixture.
- [ ] Affected package tests and targeted Clippy pass.
- [ ] `cargo fmt --all -- --check` and `git diff --check` pass.
- [ ] `node .gitnexus/run.cjs detect-changes --scope all --repo .` completes without `partial`/`truncated`; HIGH/CRITICAL risks are reviewed.
- [ ] The tranche has a reviewer-readable receipt summary that distinguishes baseline regression, live oracle observation, dual-product evidence, divergence, Blocked, and Failed.

## Final Full Gate

Run only after all seven tranche plans are complete and version authority for C12 is resolved:

```bash
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
(
  cd packages/pi
  npm ci --ignore-scripts --no-audit --no-fund
  npm run check
  npm test
  npm run build:native
  env -u PI_RS_NATIVE_BINDING npm run test:bridge
  npm run release:check
)
python3 -m pip install --require-hashes -r conformance/legacy-pi-core/fixtures/pty/requirements.txt
node scripts/check-legacy-pi-core.mjs validate
node scripts/check-legacy-pi-core.mjs formal \
  --legacy-dir "$LEGACY_PI_DIR" \
  --rust-cli target/release/pi \
  --node-cli packages/pi/dist/bin/pi.js \
  --native-binding "$PI_RS_NATIVE_BINDING_UNDER_TEST" \
  --all-required-cells
git diff --check
node .gitnexus/run.cjs detect-changes --scope all --repo .
```

Expected: all commands exit 0; `formal` reports no Failed, Blocked, missing, skipped, mixed-commit, mixed-manifest, dirty-source, unapproved-difference, or secret-scan findings. This is the first point at which the project may claim formal core-scenario acceptance, and only for the exact commits/platform cells in the run index.
