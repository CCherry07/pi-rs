# Legacy Pi Core Frontend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add C09 and C10 acceptance for print, NDJSON, RPC, ACP, and interactive TUI/PTY behavior through real product executables.

**Architecture:** Frontend drivers share the same scripted provider/session fixtures but capture distinct presentation/protocol surfaces. Print/NDJSON/RPC use descriptor-separated process drivers; Unix interactive scenarios use a bounded Python PTY driver with semantic screen snapshots, while Windows receives declared non-PTY input/exit/restore smoke until a reviewed ConPTY driver exists. ACP remains a Rust-only additive `P` gate.

**Tech Stack:** Rust CLI/RPC/ACP, Node/NAPI host, Node conformance harness, Python 3 `pty`/`termios`, pinned `pyte`, local provider/extensions/native-plugin fixtures.

**Spec:** `docs/superpowers/specs/2026-11-28-legacy-pi-core-scenario-conformance-design.md`

## Global Constraints

- Inherit the master and all completed prior tranche constraints.
- A real executable/adapter is mandatory; direct `AgentSession` calls are supporting regressions, not C09/C10 acceptance.
- Presentation and protocol descriptors are captured separately; no JSON/RPC protocol bytes may appear on the TUI presentation descriptor.
- TUI comparison is semantic screen/layout/state comparison, not byte-for-byte ANSI or pixel equality.
- Node-hosted Rust owns JavaScript extension UI; standalone Rust owns native behavior and actionable JS rejection.
- Windows required cells are only the manifest-declared non-PTY input/exit/restore smoke until ConPTY is separately designed.
- ACP cells record `P` and must not contribute false Legacy `D`.

## Review Focus

- Backpressure must bound memory and preserve correlation/order rather than dropping terminal events; Task 3 stalls the reader and measures queue limits.
- EOF/HUP/TERM during an active callback/tool/provider stream must restore the terminal and stop descendants; Tasks 3 and 7 inventory modes/PIDs.
- Unicode/IME input must not duplicate or split grapheme content when resize/completion occurs; Task 6 compares submitted Unicode scalar/grapheme sequence.
- Copy/selection/scroll tests must distinguish transcript state from the visible viewport; Task 6 records both semantic transcript and screen snapshots.
- Protocol bytes must never leak to the presentation descriptor, even on error/shutdown; Tasks 5–7 scan raw PTY bytes and separated RPC output.

---

## Task 1: Define shared frontend semantic records and descriptor separation

**Files:**
- Create: `conformance/legacy-pi-core/drivers/frontend.mjs`
- Create: `conformance/legacy-pi-core/normalize/frontend.mjs`
- Create: `conformance/legacy-pi-core/test/frontend-driver.test.mjs`

**Interfaces:**
- Produces `runPrint`, `runNdjson`, `runRpc`, and semantic record types `FrontendMessage`, `FrontendError`, `FrontendEvent`, `RpcExchange`, `PresentationFrame`, `TerminalState`.
- Every raw result labels `stdout`, `stderr`, `protocol`, and `presentation` descriptors explicitly.

- [ ] **Step 1: Write failing driver tests**

Use fake executables to assert descriptor routing, bounded capture, UTF-8 chunk repair, exit/signal reporting, protocol/presentation leak detection, and preservation of unknown terminal/event fields.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/frontend-driver.test.mjs`

Expected: FAIL because frontend driver/projector are absent.

- [ ] **Step 3: Implement shared driver/projector**

Keep endpoint-specific raw envelopes. Common projection includes semantic role/type/content/error/terminal state/correlation/order only when both endpoints expose it; unknown Rust fields remain in raw/native assertions.

- [ ] **Step 4: Verify and commit**

Run focused test and commit as `test(conformance): add frontend drivers`.

## Task 2: Implement C09-01 print and C09-02 NDJSON

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C09-01.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C09-02.mjs`
- Create: `conformance/legacy-pi-core/test/C09-print-ndjson.test.mjs`
- Modify only on observed failure: `apps/pi-cli/src/output.rs`, `apps/pi-cli/src/lib.rs`, and focused tests

**Interfaces:**
- Consumes Task 1 drivers and provider/session fixtures.
- Produces print final/error records and ordered NDJSON event/terminal records.

- [ ] **Step 1: Write failing C09-01/02 tests**

Assert print writes only final user-facing text to the declared stream, returns documented exit for provider/tool/config errors, and leaves no session on quit-before-response. Assert NDJSON preserves event order, tool updates/results, errors, usage/response/terminal fields, one terminal state, and valid one-object-per-line framing under split UTF-8 writes.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C09-print-ndjson.test.mjs`

Expected: FAIL because scenarios are missing.

- [ ] **Step 3: Implement real executable scenarios**

Run both standalone and Node/NAPI products wherever the manifest declares shared support; assert their Rust semantic outputs agree before comparing the common contract with Legacy.

- [ ] **Step 4: Verify and commit**

Run focused CLI/Node tests and formal C09-01/02; commit as `test(conformance): cover print and NDJSON frontends`.

## Task 3: Implement C09-03 RPC correlation, dialogs, cancellation, EOF, signals, and backpressure

**Files:**
- Create: `conformance/legacy-pi-core/drivers/rpc.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C09-03.mjs`
- Create: `conformance/legacy-pi-core/test/C09-rpc.test.mjs`
- Modify only on observed failure: `crates/pi-rpc/src/`, `apps/pi-cli/src/rpc_signals.rs`, Node RPC host/bridge, and focused tests

**Interfaces:**
- Produces `RpcDriver.request(method, params)`, `notify`, `awaitEvent`, `pauseReads`, `resumeReads`, `closeInput`, and process/resource inventory.

- [ ] **Step 1: Write failing RPC tests**

Assert command request/response correlation with interleaved events; queue/steer ordering; dialog request/response/cancel; unknown command error; active turn cancellation; EOF and TERM/HUP handling; malformed input containment; bounded backpressure while reads pause; terminal events delivered after resume; no callback/session/process leak.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C09-rpc.test.mjs`

Expected: FAIL because driver/scenario are absent.

- [ ] **Step 3: Implement from existing RPC contract**

Reference `docs/rpc-conformance.md` and existing `crates/pi-rpc/tests`; do not change historical RPC fixtures. Use explicit high/low watermarks and fixture barriers rather than timing assumptions.

- [ ] **Step 4: Verify owner and formal evidence**

```bash
cargo test --locked -p pi-rpc
node --test conformance/legacy-pi-core/test/C09-rpc.test.mjs
node scripts/check-legacy-pi-core.mjs formal --manifest "$CORE_MANIFEST" --scenario C09-03 --legacy-dir "$LEGACY_PI_DIR" --rust-cli "$RUST_PI" --node-cli "$NODE_PI" --native-binding "$PI_RS_NATIVE_BINDING_UNDER_TEST" --receipt-dir "$CORE_RECEIPTS/frontends-c09-rpc-formal"
```

- [ ] **Step 5: Commit**

Commit as `test(conformance): cover RPC frontend lifecycle`.

## Task 4: Add the Rust-only ACP additive gate

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C09-03-acp.mjs`
- Create: `conformance/legacy-pi-core/test/C09-acp.test.mjs`
- Modify only on observed failure: `crates/pi-acp/src/`, CLI ACP wiring, and focused tests

**Interfaces:**
- Produces only `P` receipts for manifest-declared ACP variants: stable-v1 setup, transient MCP servers, permission prompts, cancellation, and cleanup.

- [ ] **Step 1: Write failing ACP tests**

Drive the real ACP adapter and assert setup/capabilities, transient MCP scope/no disk persistence, permission prompt response, cancellation, session cleanup, and EOF. Assert receipt issuance rejects `D` for every ACP cell.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C09-acp.test.mjs`

Expected: FAIL because ACP scenario is absent.

- [ ] **Step 3: Implement ACP driver variant and run owner tests**

Do not launch Legacy or use RPC as an ACP proxy. Preserve raw ACP wire.

- [ ] **Step 4: Verify and commit**

Run `cargo test --locked -p pi-acp`, focused Node test, then commit as `test(conformance): add ACP product gate`.

## Task 5: Extract a reusable, pinned Unix PTY acceptance driver

**Files:**
- Create: `conformance/legacy-pi-core/fixtures/pty/requirements.in`
- Create: `conformance/legacy-pi-core/fixtures/pty/requirements.txt`
- Create: `conformance/legacy-pi-core/drivers/pty.py`
- Create: `conformance/legacy-pi-core/test/pty_driver_test.py`
- Modify: `packages/pi/test/ui-runtime.pty.py` to consume the shared driver or a compatibility wrapper
- Modify: `packages/pi/test/requirements-pty.txt` to point at/use the same lock

**Interfaces:**
- Produces Python `TerminalHarness` with `wait_for`, `send`, `resize`, `screen_snapshot`, `transcript`, `terminal_modes`, `signal`, `close`, and leak inventory.
- Node launches it through a JSON-lines control protocol; PTY output never shares that control pipe.

- [ ] **Step 1: Read CLI/TUI docs and run GitNexus impact**

Read `apps/pi-cli/README.md` and focused `apps/pi-cli/src/tui.rs` tests. Analyze impact for any extracted test helper references; no production terminal ownership moves out of `apps/pi-cli`.

- [ ] **Step 2: Write failing PTY driver tests**

Use a tiny ANSI/composer fixture to assert screen parsing, split UTF-8, resize, cursor/color/scheme/cell query replies, alternate/main screen modes, signal/EOF, descriptor separation, timeout diagnostics, and terminal restoration. Assert dependency lock pins `pyte==0.8.2` plus all transitives with hashes.

- [ ] **Step 3: Verify desired-red**

Run: `python3 -m unittest conformance/legacy-pi-core/test/pty_driver_test.py`

Expected: FAIL because shared driver/lock are absent.

- [ ] **Step 4: Extract without weakening existing Node/NAPI PTY coverage**

Move reusable mechanics from `packages/pi/test/ui-runtime.pty.py`; leave scenario-specific assertions there. Generate a hash-locked requirements file and document the exact Python version range.

- [ ] **Step 5: Verify existing and new PTY tests**

```bash
python3 -m pip install --require-hashes -r conformance/legacy-pi-core/fixtures/pty/requirements.txt
python3 -m unittest conformance/legacy-pi-core/test/pty_driver_test.py
python3 packages/pi/test/ui-runtime.pty.py
```

Expected: both pass with no protocol bytes on PTY output.

- [ ] **Step 6: Commit**

Commit as `test(tui): share semantic PTY driver`.

## Task 6: Implement C10-01/02 screen semantics and composer interaction

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C10-01.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C10-02.mjs`
- Create: `conformance/legacy-pi-core/test/C10_tui_test.py`
- Modify only on observed failure: `apps/pi-cli/src/tui.rs`, `apps/pi-cli/src/tui/`, `apps/pi-cli/src/input.rs`, `text_selection.rs`, `clipboard.rs`, and focused tests

**Interfaces:**
- Consumes Task 5 PTY driver.
- Produces semantic screen frames with roles/regions/relative spacing and composer/transcript state probes.

- [ ] **Step 1: Write failing C10-01 tests**

Assert fullscreen default/main-screen opt-out; streamed Markdown; distinct user/assistant/tool/error/working semantics; stable spacing/alignment relationships; no protocol bytes; resize preserves semantic entries.

- [ ] **Step 2: Write failing C10-02 tests**

Assert Unicode and IME composition, grapheme-safe editing, history, completion, queued-input preview, `/model`, `/resume`, `/mcp`, `/skill:` selectors, scroll, selection, copy, and transcript state independent of viewport.

- [ ] **Step 3: Verify desired-red**

Run: `python3 -m unittest conformance/legacy-pi-core/test/C10_tui_test.py`

Expected: FAIL because C10 scenarios are absent.

- [ ] **Step 4: Implement semantic drivers and add owning regressions only for mismatches**

Compare named regions/roles/order/spacing inequalities, not exact ANSI bytes. Any production fix stays in app layer and gets a focused Ratatui/input test first.

- [ ] **Step 5: Verify Unix formal evidence and Windows smoke applicability**

Run CLI tests, Python C10 tests, and formal PTY cells on Linux/macOS. Windows executes only manifest-declared non-PTY input/exit/restore smoke and must not claim PTY `D`.

- [ ] **Step 6: Commit**

Commit as `test(conformance): cover interactive TUI semantics`.

## Task 7: Implement C10-03 extension/native UI, suspend/resume, signals, and restoration

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C10-03.mjs`
- Extend: `conformance/legacy-pi-core/test/C10_tui_test.py`
- Create: `apps/pi-cli/tests/fixtures/native-ui-plugin/` manifest, crate, and source
- Modify only on observed failure: Node UI runtime, NAPI bridge, or `apps/pi-cli` terminal lifecycle

**Interfaces:**
- Produces Node-hosted JS dialog/widget PTY records and standalone native-plugin UI PTY records, plus terminal mode before/after and resource inventory.

- [ ] **Step 1: Read native-plugin ABI docs before fixture implementation**

Read `crates/pi-plugin/docs/native.md` in full and use existing export/manifest macros. The fixture exports exactly one unified Plugin and uses UI context through supported callbacks.

- [ ] **Step 2: Write failing C10-03 tests**

Assert JS extension dialog/widget through Node-hosted Rust; native plugin dialog/notification through standalone Rust; standalone general-JS rejection; resize during dialog; suspend/resume; Ctrl-C; EOF/HUP/TERM; active callback cancellation; alternate-screen/mouse/paste/cursor restoration; zero child/callback/process leak.

- [ ] **Step 3: Verify desired-red**

Run the focused Python test selecting C10-03.

Expected: FAIL because scenario/native fixture are absent.

- [ ] **Step 4: Implement fixtures and real-product PTY actions**

Build the native fixture through repository-supported test packaging; do not introduce a second dynamic-library loader. Reuse C07 UI fixture for Node-hosted behavior.

- [ ] **Step 5: Verify and commit**

Run native-plugin manager tests, Node PTY test, C10 formal on Linux/macOS, and Windows exit/restore smoke. Commit as `test(conformance): cover TUI lifecycle and extension UI`.

## Task 8: Complete frontend CI and documentation

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `conformance/legacy-pi-core/README.md`
- Modify: `docs/rpc-conformance.md` only by appending new current receipts
- Modify: `apps/pi-cli/README.md` only if product behavior/guidance changes

- [ ] **Step 1: Add routine frontend jobs**

Linux runs C09 plus C10 PTY; macOS runs PTY/signal subset; Windows runs C09 and declared non-PTY C10 smoke. Install PTY dependencies from the hash lock.

- [ ] **Step 2: Run tranche gate**

```bash
node --test conformance/legacy-pi-core/test/{frontend-driver,C09-print-ndjson,C09-rpc,C09-acp}.test.mjs
python3 -m unittest conformance/legacy-pi-core/test/pty_driver_test.py conformance/legacy-pi-core/test/C10_tui_test.py
cargo test --locked -p pi-cli -p pi-rpc -p pi-acp -p pi-plugin-manager
cargo clippy --locked -p pi-cli -p pi-rpc -p pi-acp -p pi-plugin-manager --all-targets -- -D warnings
(cd packages/pi && npm run check && npm test && npm run build:native && env -u PI_RS_NATIVE_BINDING npm run test:bridge)
cargo fmt --all -- --check
git diff --check
```

- [ ] **Step 3: Run GitNexus change detection and commit**

Commit CI/docs as `ci: gate legacy frontend scenarios`; report platform cells explicitly rather than inferring Windows PTY behavior.
