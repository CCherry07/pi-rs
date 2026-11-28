# Legacy Pi Core MCP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add C08 acceptance for configured and extension MCP servers, HTTP/session/recovery authority, OAuth flow and disk interoperability, dynamic publication, and complete cleanup.

**Architecture:** Local stdio, Streamable HTTP, SSE, and authorization fixtures expose deterministic barriers and protocol transcripts. Existing `pi-plugin-mcp` transport/recovery tests remain the exhaustive owning seam; C08 drives the real standalone and Node/NAPI products. OAuth interoperability first records a bidirectional corpus against the pinned Legacy store, then gives Rust a shared Legacy-compatible disk codec and lock protocol while retaining stronger atomic writes and explicit recovery evidence.

**Tech Stack:** Rust `pi-plugin-mcp`, Node conformance fixtures, JSON-RPC/MCP, stdio, HTTP/SSE, local OAuth 2.0/PKCE fixture, file locks and atomic replacement.

**Spec:** `docs/superpowers/specs/2026-11-28-legacy-pi-core-scenario-conformance-design.md`

## Global Constraints

- Inherit all master and prior tranche constraints.
- Configured global/project MCP runs in standalone Rust and Node/NAPI Rust; extension MCP additionally runs through Node/NAPI.
- HTTP authority is dispatch-affine: credential wait/retry re-reads active session before dispatch, while admitted streams retain their opening protocol/session/cursor context.
- Safe resource reads may be retried according to the reviewed policy; ambiguous `tools/call` must never be replayed.
- Response-tail cancellation stops only that tail and does not poison/reinitialize the service.
- Expired transports drain admitted work before deleting only their captured session identity; repeated expiry remains lazy after replacement allowance is exhausted.
- Extension registrations remain generation/session runtime-local and never write `mcp.json`, settings, or session history.
- Disk configured entries override normalized extension namespaces and produce visible diagnostics.
- No real OAuth credential/browser/network is used. Every token and secret is fake and marker-scanned.
- Do not claim Legacy/Rust can safely share `mcp-auth.json` until Task 5's bidirectional, lock, and crash suite passes or remaining differences have approved `P` records.

## Review Focus

- A delayed old HTTP response must not overwrite a newer active session ID; Task 3 asserts response-opening authority and final store identity.
- Recovery must never replay a tool side effect even when the first response is ambiguous; Task 3 counts fixture calls and durable results.
- Sequential and concurrent Legacy/Rust auth-store access must use the same canonical key and mutually visible lock, or sharing remains Blocked; Tasks 4–5 inspect exact files/locks.
- Killing a store writer must leave either the old or complete new document, never a silently accepted partial credential; Task 5 injects crashes at truncate/write/fsync/rename boundaries where the product controls them.
- Removing the final extension MCP registration must retire old connections/observers without exposing an empty `/mcp`; Task 7 inventories commands, transports, callbacks, and scopes.

---

## Task 1: Build deterministic MCP stdio/HTTP/SSE/OAuth fixtures

**Files:**
- Create: `conformance/legacy-pi-core/fixtures/mcp/stdio-server.mjs`
- Create: `conformance/legacy-pi-core/fixtures/mcp/http-server.mjs`
- Create: `conformance/legacy-pi-core/fixtures/mcp/oauth-server.mjs`
- Create: `conformance/legacy-pi-core/fixtures/mcp/control.mjs`
- Create: `conformance/legacy-pi-core/test/mcp-fixtures.test.mjs`

**Interfaces:**
- Produces `McpFixture.start(options)` with endpoints/control barriers and transcript methods `requests()`, `sessions()`, `toolCalls()`, `resourceReads()`, `openStreams()`, `release(name)`, and `close()`.
- Supports JSON response, SSE response, GET resume stream, session rotation/404/expiry, delayed tail, malformed events, auth challenge/token/refresh/logout, and child-process lifecycle.

- [ ] **Step 1: Write failing fixture self-tests**

Use a small reference JSON-RPC client to assert initialize/list/call/read/prompt, ordered SSE metadata/data, session capture/rotation, resume cursor, 401 challenge, refresh, explicit DELETE, stdio shutdown, barriers, and zero open streams/children/listeners after repeated close.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/mcp-fixtures.test.mjs`

Expected: FAIL because fixtures are missing.

- [ ] **Step 3: Implement fixtures**

All state transitions occur after explicit control commands; no timing sleep determines correctness. Log hashes/redacted headers rather than token bodies, while preserving raw protocol bodies in protected run-local evidence for the marker scanner.

- [ ] **Step 4: Verify and commit**

Run focused test and commit as `test(conformance): add deterministic MCP fixtures`.

## Task 2: Implement C08-01/02 configured transports, exposure, schema, and partial startup

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C08-01.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C08-02.mjs`
- Create: `conformance/legacy-pi-core/test/C08-configured.test.mjs`
- Modify only on observed failure: `plugins/features/pi-plugin-mcp/src/config.rs`, `library.rs`, `library/supervised.rs`, `plugin/manager.rs`, `apps/pi-cli/src/mcp_commands.rs`, and focused tests

**Interfaces:**
- Consumes Task 1 fixtures and C01 project tree.
- Produces semantic inventory records for server scope/source/status, qualified tool/resource/prompt names, exposure decision, and output schema.

- [ ] **Step 1: Write failing configured-MCP tests**

C08-01 asserts global/project stdio and HTTP JSON/SSE connection, trust/precedence, ordered folded caller headers, tool/resource/prompt discovery, and real invocation. C08-02 asserts one healthy plus one failing server still exposes healthy inventory, exposure policy/qualified naming/output-schema projection, visible failure diagnostics, and cleanup.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C08-configured.test.mjs`

Expected: FAIL because scenarios are absent.

- [ ] **Step 3: Implement real-product scenarios**

Configure products through isolated `mcp.json`; never call `McpLibrary` directly for acceptance. Capture `/mcp`/inventory presentation and fixture protocol transcripts.

- [ ] **Step 4: Verify owner and formal evidence**

```bash
cargo test --locked -p pi-plugin-mcp
cargo test --locked -p pi-cli --test mcp_management
node --test conformance/legacy-pi-core/test/C08-configured.test.mjs
node scripts/check-legacy-pi-core.mjs formal --manifest "$CORE_MANIFEST" --scenario C08-01 --scenario C08-02 --legacy-dir "$LEGACY_PI_DIR" --rust-cli "$RUST_PI" --node-cli "$NODE_PI" --native-binding "$PI_RS_NATIVE_BINDING_UNDER_TEST" --receipt-dir "$CORE_RECEIPTS/mcp-c08-configured-formal"
```

- [ ] **Step 5: Commit**

Commit as `test(conformance): cover configured MCP servers`.

## Task 3: Implement C08-03/04/05 HTTP authority, safe recovery, expiry, reconnect, and shutdown

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C08-03.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C08-04.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C08-05.mjs`
- Create: `conformance/legacy-pi-core/test/C08-http-recovery.test.mjs`
- Modify only on observed failure: `plugins/features/pi-plugin-mcp/src/http_client/`, `http_transport/`, `client/request.rs`, `client/retry.rs`, `client/lifetime.rs`, `client/recovery.rs`, and focused tests

**Interfaces:**
- Produces request/stream records that bind each dispatch to protocol version, session ID, cursor, operation kind, retry reason, cleanup identity, and call count.

- [ ] **Step 1: Write failing C08-03 tests**

Assert HTTP JSON/SSE framing, CR/LF/CRLF and chunk boundaries, bootstrap protocol negotiation, successful early session capture, rejected response non-authority, credential-wait late authority, GET resume with current session and originating protocol/cursor, tail affinity, and cancellation after primary reply.

- [ ] **Step 2: Write failing C08-04/05 tests**

Assert safe resource retry after repair, exactly one `tools/call` side effect under ambiguous response, lazy expired-session replacement, repeated expiry without eager third setup, explicit reconnect, session 404 replacement, admitted-work drain before DELETE, captured-identity cleanup, and shutdown with zero streams/tasks/listeners.

- [ ] **Step 3: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C08-http-recovery.test.mjs`

Expected: FAIL because vertical scenarios are absent; if current product diverges, preserve the first failure receipt.

- [ ] **Step 4: Add owning red regressions for actual differences**

Before editing, run GitNexus impact for the precise transport/recovery symbol. Reproduce the divergence in `http_transport/tests`, `http_client/tests`, or `client/recovery_tests`; prove old behavior fails; implement the smallest fix; retain red/green command output.

- [ ] **Step 5: Verify owner and formal evidence**

```bash
cargo test --locked -p pi-plugin-mcp http_transport -- --nocapture
cargo test --locked -p pi-plugin-mcp recovery -- --nocapture
node --test conformance/legacy-pi-core/test/C08-http-recovery.test.mjs
node scripts/check-legacy-pi-core.mjs formal --manifest "$CORE_MANIFEST" --scenario C08-03 --scenario C08-04 --scenario C08-05 --legacy-dir "$LEGACY_PI_DIR" --rust-cli "$RUST_PI" --node-cli "$NODE_PI" --native-binding "$PI_RS_NATIVE_BINDING_UNDER_TEST" --receipt-dir "$CORE_RECEIPTS/mcp-c08-http-formal"
```

Expected: no replayed tool call and no leaked tail/session cleanup.

- [ ] **Step 6: Commit**

Commit harness as `test(conformance): cover MCP HTTP recovery`; commit each owning fix separately.

## Task 4: Capture a bidirectional Legacy OAuth-store compatibility corpus

**Files:**
- Create: `conformance/legacy-pi-core/fixtures/mcp/auth-store-legacy.mjs`
- Create: `conformance/legacy-pi-core/fixtures/mcp/auth-store-corpus.json`
- Create: `conformance/legacy-pi-core/test/mcp-auth-store-corpus.test.mjs`
- Create: `plugins/features/pi-plugin-mcp/tests/fixtures/auth-store/` reviewed JSON fixtures

**Interfaces:**
- Produces corpus operations: empty load, namespaced save/load/remove, same-URL/different-name isolation, URL-only takeover, client registration, tokens/expiry, pending PKCE state, refresh replacement, unrelated/unknown field preservation, malformed/oversized input, concurrent writer, and killed writer.

- [ ] **Step 1: Write failing corpus-generation tests**

Invoke the pinned Legacy `McpOAuthCredentialStore` through its source resolver/test runtime and assert exact before/after JSON, normalized key, visible state, lock path, exit, and source hash. The test must fail when Legacy SHA differs.

- [ ] **Step 2: Verify desired-red**

Run with explicit pinned checkout: `node --test conformance/legacy-pi-core/test/mcp-auth-store-corpus.test.mjs`

Expected: FAIL because corpus runner/data are missing.

- [ ] **Step 3: Implement read-only oracle runner and capture corpus**

Do not modify Legacy source. Record that Legacy's canonical top-level document is a map keyed by `<mcpNamespace(name)>|<canonical URL>`, with the older URL-only key eligible for one atomic takeover under its lock. Preserve raw JSON and lock observations.

- [ ] **Step 4: Review and copy deterministic fixtures**

Redact only token values with stable fixture markers that remain valid fake tokens. Commit corpus with oracle SHA and generator hash; no Rust compatibility claim is made yet.

- [ ] **Step 5: Commit**

Commit as `test(mcp): capture legacy OAuth store corpus`.

## Task 5: Make the Rust OAuth store sequentially and concurrently interoperable

**Files:**
- Create: `plugins/features/pi-plugin-mcp/src/auth/store.rs`
- Create: `plugins/features/pi-plugin-mcp/src/auth/store/codec.rs`
- Create: `plugins/features/pi-plugin-mcp/src/auth/store/refresh_lock.rs`
- Modify: `plugins/features/pi-plugin-mcp/src/auth.rs`
- Modify: `plugins/features/pi-plugin-mcp/src/auth/refresh.rs`
- Modify: `plugins/features/pi-plugin-mcp/Cargo.toml`
- Create: `plugins/features/pi-plugin-mcp/tests/auth_store_interop.rs`
- Modify: `docs/architecture.md`

**Interfaces:**
- Public `McpOAuthCredentialStore` constructors/methods remain source-compatible.
- Private codec produces/consumes canonical Legacy state fields `serverUrl`, `clientInformation`, `tokens`, `tokensExpireAt`, `codeVerifier`, `oauthState`, and `discovery`, while preserving unrelated top-level/state fields.
- Produces `ServerStore::with_refresh_lock(signal, operation)` with constants `20_000 ms` stale, `25_000 ms` wait, and `100 ms` retry to match the pinned oracle contract.

- [ ] **Step 1: Run GitNexus impact**

Analyze `McpOAuthCredentialStore`, `ServerStore`, `with_file_document`, `CredentialStore::save`, and refresh callers. Treat UNKNOWN as unresolved and text-search cross-language/NAPI call sites.

- [ ] **Step 2: Write failing Rust interop tests**

For every Task 4 fixture assert Rust read/write is Legacy-readable, Legacy output is Rust-readable, URL-only takeover occurs once, same URL/different names stay isolated, logout targets one identity, unknown unrelated state survives, symlink aliases share a lock, file mode stays protected, malformed/oversized input is explicit and redacted, and current Rust files using the `servers` envelope migrate once without credential loss.

- [ ] **Step 3: Write failing lock/crash tests**

Spawn Rust/Rust and Legacy/Rust writers against one file. Assert mutual exclusion uses Legacy-compatible `<auth-path>.lock`; refresh uses the same hashed lock identity; waiting is cancellable/bounded; killed holder becomes recoverable; Rust-controlled writes leave old-or-new complete JSON after injected write/fsync/rename failure. If pinned Legacy killed-write behavior is weaker, record common compatibility `D` plus a proposed Rust-strength `P`; do not normalize corruption into success.

- [ ] **Step 4: Verify desired-red**

Run: `cargo test --locked -p pi-plugin-mcp --test auth_store_interop -- --nocapture`

Expected: FAIL because the current Rust `servers` envelope and `.mcp-auth.json.lock` file are not Legacy-compatible.

- [ ] **Step 5: Implement codec and lock protocol**

Split storage from OAuth flow. Read both the old Rust envelope and canonical Legacy top-level map; write canonical Legacy shape only. Use canonical URL plus `name.replace('-', '_')` key semantics. Translate rmcp credentials/tokens and the single active PKCE state; reject a second simultaneous pending flow for one identity rather than silently overwriting it. Preserve unknown JSON. Use the same proper-lockfile-compatible lock-directory names; implement heartbeat/stale takeover safely and atomic temp-file `fsync`/rename with parent synchronization where supported.

- [ ] **Step 6: Verify interoperability and broader MCP tests**

```bash
cargo test --locked -p pi-plugin-mcp --test auth_store_interop -- --nocapture
cargo test --locked -p pi-plugin-mcp
cargo clippy --locked -p pi-plugin-mcp --all-targets -- -D warnings
```

Expected: sequential/bidirectional and declared concurrent cases pass; any unavoidable oracle crash weakness is explicit evidence, not a false green.

- [ ] **Step 7: Commit**

After full workspace gate and GitNexus change detection, commit as `fix(mcp): share legacy OAuth credential format`.

## Task 6: Implement C08-06 OAuth login/logout/refresh/crash evidence

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C08-06.mjs`
- Create: `conformance/legacy-pi-core/test/C08-oauth.test.mjs`
- Modify only on observed failure: `apps/pi-cli/src/mcp_oauth.rs`, `apps/pi-cli/src/mcp_oauth/`, `plugins/features/pi-plugin-mcp/src/auth.rs`, `auth/refresh.rs`

**Interfaces:**
- Consumes OAuth server fixture and compatible store from Tasks 1/5.
- Produces login challenge/callback/refresh/logout/store/crash records with token values redacted.

- [ ] **Step 1: Write failing C08-06 tests**

Assert local browser URL/callback and pasted redirect paths, cancellation, client metadata/DCR variant, token save/use, concurrent refresh coalescing, step-up challenge where supported, logout, URL-only migration, Legacy-to-Rust and Rust-to-Legacy reuse, killed refresh holder recovery, and no token marker in any output.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C08-oauth.test.mjs`

Expected: FAIL because C08-06 scenario is absent.

- [ ] **Step 3: Implement through real `/mcp` and CLI login/logout surfaces**

Inject a fake browser opener; never launch the user's browser. Bind callbacks only to loopback. Record store bytes under run-local restricted evidence and semantic redactions separately.

- [ ] **Step 4: Verify formal and commit**

Run MCP auth owner tests, C08-06 Node test, same-run formal comparison, and secret scan. Commit as `test(conformance): cover MCP OAuth interoperability`.

## Task 7: Implement C08-07 dynamic registration, precedence, scope, and observer retirement

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C08-07.mjs`
- Create: `conformance/legacy-pi-core/test/C08-dynamic.test.mjs`
- Modify only on observed failure: `packages/pi/src/mcp-servers.ts`, extension host/protocol, `domains/coding/src/dynamic_mcp.rs`, `domains/coding/src/generation.rs`, MCP manager/lifecycle tests

**Interfaces:**
- Consumes C07 lifecycle fixture and MCP fixtures.
- Produces extension registration snapshots, generation/scope revisions, visible override diagnostics, `/mcp` inventory, observer callback records, and resource cleanup inventory.

- [ ] **Step 1: Write failing C08-07 tests**

Assert register/replace/unregister ownership and namespace normalization; configured global/trusted-project precedence; relative cwd/session path semantics; whole-generation publication at idle boundary; ordinary reload overlay replay without false change event; full snapshot after actual mutation; new/resume/fork fresh scope isolation; failed notification rollback; final removal publishes lifecycle-only candidate, removes `/mcp` if otherwise empty, retires observer/connection, and leaves no disk/history mutation.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C08-dynamic.test.mjs`

Expected: FAIL because C08-07 vertical scenario is absent.

- [ ] **Step 3: Add owning regressions for actual mismatches, then fix minimally**

Use existing DynamicMcpRegistry revision transactions and replacement barriers. Do not add scenario-name branches or mutate live registries.

- [ ] **Step 4: Verify Node/bridge/Rust/formal evidence**

```bash
(cd packages/pi && npm test && npm run build:native && env -u PI_RS_NATIVE_BINDING npm run test:bridge)
cargo test --locked -p pi-coding mcp
cargo test --locked -p pi-plugin-mcp
node --test conformance/legacy-pi-core/test/C08-dynamic.test.mjs
node scripts/check-legacy-pi-core.mjs formal --manifest "$CORE_MANIFEST" --scenario C08-07 --legacy-dir "$LEGACY_PI_DIR" --rust-cli "$RUST_PI" --node-cli "$NODE_PI" --native-binding "$PI_RS_NATIVE_BINDING_UNDER_TEST" --receipt-dir "$CORE_RECEIPTS/mcp-c08-dynamic-formal"
```

- [ ] **Step 5: Commit**

Commit as `test(conformance): cover dynamic MCP lifecycle`; keep fixes separate.

## Task 8: Run official fixture conformance, update ledger, and gate C08 in CI

**Files:**
- Modify: `docs/mcp-post-auth-gap-matrix.md`
- Modify: `conformance/legacy-pi-core/README.md`
- Modify: `.github/workflows/ci.yml`
- Modify: `docs/architecture.md` for any accepted auth/storage/lifecycle invariant

- [ ] **Step 1: Add fixture-only official conformance command**

Expose it through `scripts/check-legacy-pi-core.mjs` as supporting evidence with its own oracle/tool version and receipt; it cannot substitute for C08 product cells.

- [ ] **Step 2: Add routine C08 CI shards**

Linux runs full baseline C08; macOS runs stdio/process cleanup; Windows runs stdio/path/header subset. Node dynamic cells build the local native binding and unset stale external binding.

- [ ] **Step 3: Run tranche gate**

```bash
node --test conformance/legacy-pi-core/test/{mcp-fixtures,mcp-auth-store-corpus,C08-configured,C08-http-recovery,C08-oauth,C08-dynamic}.test.mjs
cargo test --locked -p pi-plugin-mcp -p pi-coding -p pi-cli
cargo clippy --locked -p pi-plugin-mcp -p pi-coding -p pi-cli --all-targets -- -D warnings
(cd packages/pi && npm run check && npm test && npm run build:native && env -u PI_RS_NATIVE_BINDING npm run test:bridge)
cargo fmt --all -- --check
git diff --check
```

Run full workspace tests/Clippy because auth storage and generation lifecycle are foundational.

- [ ] **Step 4: Update evidence without rewriting history**

Append current exact counts/receipts and remaining Failed/Blocked/P entries. Preserve older oracle revisions and earlier failure receipts verbatim.

- [ ] **Step 5: Run complete GitNexus change detection and commit**

Commit CI/docs as `ci: gate legacy MCP scenarios` after non-partial/non-truncated analysis.
