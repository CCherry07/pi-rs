# Legacy Pi Core JavaScript Extension Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add C07 acceptance for JavaScript/TypeScript extension loading, callbacks, UI, generation replacement, retirement, and process cleanup through the Node/NAPI Rust product, plus standalone Rust rejection/native boundaries.

**Architecture:** Reviewed extension fixtures run unchanged where APIs overlap, with thin endpoint adapters only for documented representation differences. The Node host owns JavaScript object lifetimes and transports typed manifests/callbacks across NAPI; Rust publishes immutable whole generations. Reload and cleanup scenarios use explicit callback/generation barriers and verify stale contexts fail after retirement.

**Tech Stack:** Node.js/TypeScript, `tsx`, Node/NAPI bridge, `pi-js-plugin`, coding generation, local scripted provider, existing extension UI transcript and PTY support.

**Spec:** `docs/superpowers/specs/2026-11-28-legacy-pi-core-scenario-conformance-design.md`

## Global Constraints

- Inherit the master and completed earlier tranche interfaces.
- General JavaScript extension acceptance uses Node/NAPI Rust. Standalone Rust must return an actionable rejection and may separately exercise native plugins.
- Extension errors and manifests must not leak config values, tokens, environment secrets, or callback payload secrets.
- Runtime callback/mutation publication occurs through complete replacement generations; no live registry mutation is presented as Legacy-equivalent object identity.
- Retired callbacks and contexts remain invalid even if a later generation reuses a registration name.
- Process exit and RPC EOF cancel active callbacks and let both Node and Rust terminate without an orphan runtime.

## Review Focus

- A callback response arriving after retirement must not mutate the replacement generation; Tasks 5–6 assert generation ID and stale rejection.
- Failed Node notification/reload must restore the prior registry and keep its callbacks usable; Task 5 injects failures before and after manifest preparation.
- Extension config/input containing secrets must be redacted in validation and transport errors; Task 6 scans raw stderr, RPC errors, and receipts.
- Explicit extension paths must remain independent from discovery-disable flags; Tasks 1–2 cover explicit/discovered/disabled combinations.
- UI cancellation and process exit must settle outstanding Promises rather than pinning Node/Rust; Tasks 4 and 6 hold dialogs/callbacks at barriers.

---

## Task 1: Create the cross-product extension fixture pack

**Files:**
- Create: `conformance/legacy-pi-core/fixtures/extensions/basic.ts`
- Create: `conformance/legacy-pi-core/fixtures/extensions/patches.ts`
- Create: `conformance/legacy-pi-core/fixtures/extensions/ui.ts`
- Create: `conformance/legacy-pi-core/fixtures/extensions/lifecycle.ts`
- Create: `conformance/legacy-pi-core/fixtures/extensions/provider.ts`
- Create: `conformance/legacy-pi-core/fixtures/extensions/control.mjs`
- Create: `conformance/legacy-pi-core/test/extension-fixtures.test.mjs`

**Interfaces:**
- Fixtures emit control records `{generation, callback, phase, payloadHash}` to a dedicated control channel and write no evidence directly.
- Produces deterministic registrations for one tool, command, provider, agent hooks, session hooks, patches/custom entries, dialogs/widgets, and a blocked callback.

- [ ] **Step 1: Write failing fixture-contract tests**

Load each fixture through the Legacy extension loader and the Node `ExtensionHost` test adapter. Assert the declared registration inventory, no undeclared filesystem/network access, deterministic callback markers, and no product-specific branching except the provided adapter module.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/extension-fixtures.test.mjs`

Expected: FAIL because fixtures/control channel do not exist.

- [ ] **Step 3: Implement focused fixtures**

Keep one responsibility per extension file. Use the inherited control descriptor/socket rather than stdout. Embed `secret-marker` only in a validation-failure input whose expected output must be redacted.

- [ ] **Step 4: Verify and commit**

Run focused test, then commit as `test(conformance): add extension fixture pack`.

## Task 2: Implement C07-01 extension load, discovery, tool/command, and lifecycle hooks

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C07-01.mjs`
- Create: `conformance/legacy-pi-core/test/C07-load-hooks.test.mjs`
- Modify only on observed failure: `packages/pi/src/extension-host.ts`, `packages/pi/src/extension-runtime.ts`, `crates/pi-js-plugin/src/lib.rs`, `domains/coding/src/generation.rs`, and focused tests

**Interfaces:**
- Consumes Task 1 basic fixture and C01 project-tree fixture.
- Produces ordered registration/invocation/session lifecycle records.

- [ ] **Step 1: Write failing C07-01 tests**

Assert explicit and discovered TS/JS load through Node/NAPI; tool/command execute; agent/session hooks run in registration order with correct context; trusted project discovery and disable flags match C01; standalone Rust explicitly rejects general JS with Node-host guidance while native-only operation remains available.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C07-load-hooks.test.mjs`

Expected: FAIL because scenario is missing.

- [ ] **Step 3: Implement real-product scenario and projection**

Record Node manifest generation ID, Rust generation identity, callback invocation, transcript, and session hook order. Do not compare JavaScript object identity.

- [ ] **Step 4: Verify Node/Rust bridge and formal evidence**

```bash
(cd packages/pi && npm run check && npm test && npm run build:native && env -u PI_RS_NATIVE_BINDING npm run test:bridge)
node --test conformance/legacy-pi-core/test/C07-load-hooks.test.mjs
node scripts/check-legacy-pi-core.mjs formal --manifest "$CORE_MANIFEST" --scenario C07-01 --legacy-dir "$LEGACY_PI_DIR" --rust-cli "$RUST_PI" --node-cli "$NODE_PI" --native-binding "$PI_RS_NATIVE_BINDING_UNDER_TEST" --receipt-dir "$CORE_RECEIPTS/extensions-c07-01-formal"
```

- [ ] **Step 5: Commit**

Commit as `test(conformance): cover extension loading and hooks`.

## Task 3: Implement C07-02 patches, custom entries/messages, and provider registration

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C07-02.mjs`
- Create: `conformance/legacy-pi-core/test/C07-patches-provider.test.mjs`
- Modify only on observed failure: `packages/pi/src/extension-protocol.ts`, `packages/pi/src/extension-api.ts`, `crates/pi-js-plugin/src/`, `domains/coding/src/dynamic_providers.rs`

**Interfaces:**
- Produces semantic records for input patch, tool-result patch, before-provider request, registered provider request, custom message, and custom history entry.

- [ ] **Step 1: Write failing C07-02 tests**

Assert input patches feed the provider exactly once; tool-result patches persist before continuation; before-provider request sees the final projected request; registered provider executes through product routing; custom messages/entries survive v4 reopen; malformed patches fail with source/callback context but redact marker values.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C07-patches-provider.test.mjs`

Expected: FAIL because scenario is absent.

- [ ] **Step 3: Implement and compare**

Keep raw Legacy and Rust representations; use a scenario-specific common projector only where both expose the same user-visible content with different envelope fields.

- [ ] **Step 4: Run focused package/formal checks and commit**

Run `pi-js-plugin`, `pi-coding`, Node host/bridge, then C07-02 formal. Commit as `test(conformance): cover extension patches and providers`.

## Task 4: Implement C07-03 Node-hosted dialogs/widgets

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C07-03.mjs`
- Create: `conformance/legacy-pi-core/test/C07-extension-ui.test.mjs`
- Modify only on observed failure: `packages/pi/src/extension-context.ts`, Node UI runtime files, NAPI UI bridge, and focused UI transcript tests

**Interfaces:**
- Consumes Task 1 UI fixture.
- Produces presentation-neutral dialog/widget request/result/cancel records; PTY rendering itself remains C10-03.

- [ ] **Step 1: Write failing UI contract tests**

Assert select/confirm/input/custom widget request and result, timeout/cancel, session replacement cancellation, and no UI request after context retirement. Verify standalone rejection does not pretend to host the JS UI.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C07-extension-ui.test.mjs`

Expected: FAIL because C07-03 scenario is absent.

- [ ] **Step 3: Implement using Node-hosted Rust UI adapters**

Capture semantic UI notifications independently of terminal bytes. Reuse existing `custom-ui`, `extension-ui`, and `extension-ui-transcript` fixtures where possible without changing their historical purpose.

- [ ] **Step 4: Verify and commit**

Run package UI tests, bridge tests, and C07-03 formal; commit as `test(conformance): cover extension UI contract`.

## Task 5: Implement C07-04 successful reload, rollback, and stale-context retirement

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C07-04.mjs`
- Create: `conformance/legacy-pi-core/test/C07-reload-retirement.test.mjs`
- Modify only on observed failure: `packages/pi/src/extension-host.ts`, `packages/pi/src/extension-runtime.ts`, `domains/coding/src/generation.rs`, `crates/pi-session/src/management/replacement.rs`

**Interfaces:**
- Consumes lifecycle barriers from Session Spine and Task 1 lifecycle fixture.
- Produces generation/callback inventory snapshots before prepare, after activation, after publication, and after retirement.

- [ ] **Step 1: Write failing reload tests**

Assert successful reload swaps the complete inventory at publication; invalid extension preparation keeps old generation/callbacks usable; failure notifying Rust restores Node registry; stale callback/context rejects after retirement; new/resume/fork creates the expected fresh context and hook ordering; callback cancellation settles before process cleanup.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C07-reload-retirement.test.mjs`

Expected: FAIL because C07-04 scenario is absent.

- [ ] **Step 3: Add owning regressions only for observed mismatches**

Run GitNexus impact for each affected generation/replacement symbol, capture desired-red in its existing package test, fix minimally, and rerun the unchanged vertical scenario.

- [ ] **Step 4: Verify and commit**

Run Node host/bridge, `pi-js-plugin`, `pi-session`, `pi-coding`, then formal C07-04. Keep harness and product fixes in separate commits.

## Task 6: Implement C07-05 RPC EOF/process-exit cleanup and secret-safe diagnostics

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C07-05.mjs`
- Create: `conformance/legacy-pi-core/test/C07-exit-cleanup.test.mjs`
- Modify only on observed failure: `packages/pi/src/index.ts`, `packages/pi/src/extension-host.ts`, `bindings/pi-napi/src/lib.rs`, `crates/pi-js-plugin/src/`, `crates/pi-rpc/src/`

**Interfaces:**
- Produces process/callback/runtime leak inventory and redacted diagnostic records.

- [ ] **Step 1: Write failing exit/diagnostic tests**

Hold a callback and UI request, then close RPC stdin, send process termination, and trigger Node host failure separately. Assert active callbacks reject/cancel, Rust session/runtime exits, Node exits, no listener/child remains, and repeated cleanup is idempotent. Feed marker-bearing invalid context/config/callback errors and assert raw stderr/RPC/receipt contain no marker.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C07-exit-cleanup.test.mjs`

Expected: FAIL because C07-05 scenario is absent.

- [ ] **Step 3: Implement and repair owning seams if needed**

Use EOF/process exit as real product signals, not direct test-only `retireGeneration` calls. Any cleanup fix gets a focused package desired-red first.

- [ ] **Step 4: Verify and commit**

Run bridge with `env -u PI_RS_NATIVE_BINDING`, RPC package tests, C07-05 formal, and secret scan. Commit as `test(conformance): cover extension exit cleanup` plus separate fixes.

## Task 7: Complete extension CI and documentation

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `conformance/legacy-pi-core/README.md`
- Modify: `packages/pi/docs/extensions.md` only if observed behavior/documented guidance changes
- Modify: `docs/architecture.md` only for lifecycle invariant changes

- [ ] **Step 1: Add C07 routine shards**

Linux/macOS run all baseline variants after Node/native build; Windows runs extension load/exit smoke. Explicitly unset any machine-installed `PI_RS_NATIVE_BINDING` except the C12 selection scenario.

- [ ] **Step 2: Run tranche gate**

```bash
node --test conformance/legacy-pi-core/test/{extension-fixtures,C07-load-hooks,C07-patches-provider,C07-extension-ui,C07-reload-retirement,C07-exit-cleanup}.test.mjs
cargo test --locked -p pi-js-plugin -p pi-session -p pi-coding -p pi-rpc
cargo clippy --locked -p pi-js-plugin -p pi-session -p pi-coding -p pi-rpc --all-targets -- -D warnings
(cd packages/pi && npm run check && npm test && npm run build:native && env -u PI_RS_NATIVE_BINDING npm run test:bridge)
cargo fmt --all -- --check
git diff --check
```

- [ ] **Step 3: Run full GitNexus change detection and commit**

Commit CI/docs as `ci: gate legacy extension scenarios`; confirm package output is fully green rather than reporting only bridge success.
