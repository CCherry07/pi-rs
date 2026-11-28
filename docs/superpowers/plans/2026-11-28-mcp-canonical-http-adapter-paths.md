# MCP Canonical HTTP Adapter Paths Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every MCP HTTP adapter route SDK POST/GET convenience methods through the existing private canonical `HttpClient::{post, open_stream}` paths without changing public interfaces or observable behavior.

**Architecture:** `PostRequest` and `StreamRequest` gain private SDK-input constructors. `PiHttpClient`, `DispatchHttpClient`, `ProviderAuthClient`, and `LocalOAuthClient` use those request envelopes as the single POST/GET implementation path; required SDK trait methods become minimal delegation. DELETE remains a separate cleanup path and the three authentication adapters remain distinct.

**Tech Stack:** Rust, Tokio, rmcp 3.3.0, reqwest, deterministic loopback HTTP/SSE fixtures.

**Spec:** `docs/mcp-module-simplification.md` — Slice 1, “canonical HTTP Adapter paths”.

## Global Constraints

- Preserve all crate-root public re-exports, public method behavior, wire formats, configuration files, credential files, and diagnostics.
- Keep `http_transport` as the sole product HTTP worker and `http_client` as its private bounded I/O backend.
- Do not introduce a second client runtime, generic middleware framework, forwarding macro, mode flags, dependency, or public seam.
- Keep static-header, provider-token, and local-OAuth adapters separate; share only the request-envelope path.
- Preserve default and caller-supplied SSE event limits exactly.
- Preserve dispatch and completion fences across every asynchronous credential/refresh wait.
- Keep setup retry, application/resource recovery, OAuth replay, and SSE recovery as distinct decisions with unchanged budgets.
- DELETE stays bounded and best-effort, using only the cached acquired token: no credential lookup, refresh, replay, credential write, body interpretation, or diagnostic-sensitive error projection.
- Explicit/ACP clients must not gain local configuration discovery, command execution, or `/mcp` registration.
- Use deterministic fixtures only; never use real providers, credentials, or browser sign-in.
- Preserve unrelated work already present in `.gitignore` and `docs/superpowers/specs/`; stage only files owned by this slice.

## Risk Warning

GitNexus reports **HIGH** upstream risk for `PiHttpClient::post_message` with a lower-bound result: three receiver-typed call sites were dropped, so actual trait-dispatch impact may be larger. It reports `UNKNOWN` for `DispatchHttpClient::post_message` and `LocalOAuthClient::post_message`; text search confirms they are reached through rmcp trait dispatch and deterministic MCP tests. This is not a low-risk refactor. If HEAD or any affected source changes before execution, rerun impact analysis before editing.

## Review Focus

- Default POST/GET must apply the rmcp default event limit exactly once; explicit limits must survive unchanged.
- Provider and OAuth convenience calls must acquire/refresh credentials exactly once, never through nested adapter calls.
- Cancellation, supersession, retirement, and completion fences must still run after credential waits and before/after wire dispatch at their current points.
- Local OAuth GET must retain its GET-local authentication reaction and must not consume or recreate the originating application recovery budget.
- DELETE after success, auth rejection, cancellation, or poisoned token state must remain lookup-free, refresh-free, bounded, and best-effort.

---

### Task 1: Canonicalize SDK POST/GET entry paths

**Files:**
- Modify: `plugins/features/pi-plugin-mcp/src/http_client/post.rs`
- Modify: `plugins/features/pi-plugin-mcp/src/http_client/stream.rs`
- Modify: `plugins/features/pi-plugin-mcp/src/http_client/tests.rs`
- Modify: `plugins/features/pi-plugin-mcp/src/http_client.rs`
- Modify: `plugins/features/pi-plugin-mcp/src/client/operation/http.rs`
- Modify: `plugins/features/pi-plugin-mcp/src/auth.rs`
- Modify: `plugins/features/pi-plugin-mcp/src/auth/refresh.rs`
- Verify: `plugins/features/pi-plugin-mcp/src/client/operation/tests.rs`
- Verify: `plugins/features/pi-plugin-mcp/src/auth/refresh/supervised_tests.rs`
- Verify: `plugins/features/pi-plugin-mcp/src/auth/refresh/stream_control_tests.rs`
- Verify: `plugins/features/pi-plugin-mcp/tests/http_auth_handoff.rs`
- Verify: `plugins/features/pi-plugin-mcp/tests/lifecycle_retry.rs`
- Update after green: `docs/mcp-module-simplification.md`

**Interfaces:**
- Consumes: existing private `HttpClient::{post, open_stream}`, `PostRequest::execute`, `StreamRequest::execute`, and rmcp `StreamableHttpClient`.
- Produces: private `PostRequest::from_sdk(...) -> PostRequest` and `StreamRequest::from_sdk(...) -> StreamRequest`; no public interface changes.

- [ ] **Step 1: Reconfirm blast radius before editing**

Run GitNexus upstream impact for these exact current symbols:

```bash
node .gitnexus/run.cjs impact 'Function:plugins/features/pi-plugin-mcp/src/http_client.rs:PiHttpClient.post_message#5' --direction upstream --repo .
node .gitnexus/run.cjs impact 'Function:plugins/features/pi-plugin-mcp/src/client/operation/http.rs:DispatchHttpClient.post_message#5' --direction upstream --repo .
node .gitnexus/run.cjs impact 'Function:plugins/features/pi-plugin-mcp/src/auth.rs:ProviderAuthClient.post_message#5' --direction upstream --repo .
node .gitnexus/run.cjs impact 'Function:plugins/features/pi-plugin-mcp/src/auth/refresh.rs:LocalOAuthClient.post_message#5' --direction upstream --repo .
```

Expected: `PiHttpClient::post_message` remains HIGH/lower-bound; UNKNOWN results are followed by `rg` confirmation of trait implementations/callers. Stop if a new HIGH/CRITICAL path is outside the MCP/auth/transport modules described by the spec.

- [ ] **Step 2: Write failing request-constructor tests**

Add two tests in `http_client/tests.rs`:

```rust
#[test]
fn sdk_post_request_preserves_inputs_without_private_dispatch_state() {
    // Build distinct URI, request message, session, token, headers, and max bytes.
    // Call PostRequest::from_sdk.
    // Assert every supplied value is retained.
    // Assert dispatch and session_observer are None.
}

#[test]
fn sdk_stream_request_preserves_inputs_without_private_authority_state() {
    // Build distinct URI, session, cursor, token, headers, and max bytes.
    // Call StreamRequest::from_sdk.
    // Assert every supplied value is retained.
    // Assert control, dispatch, and session_observer are None.
}
```

The tests must compare actual fields, including the exact `max_event_bytes`; they must not inspect source text or use mocks.

- [ ] **Step 3: Run the new tests and verify RED**

Run:

```bash
cargo test -p pi-plugin-mcp http_client::tests::sdk_ --no-fail-fast
```

Expected: compilation fails only because `PostRequest::from_sdk` and `StreamRequest::from_sdk` do not exist. Any unrelated failure stops the task.

- [ ] **Step 4: Add the minimal private constructors**

In `http_client/post.rs`, add:

```rust
pub(crate) fn from_sdk(
    uri: Arc<str>,
    message: ClientJsonRpcMessage,
    session: Option<Arc<str>>,
    token: Option<String>,
    headers: HashMap<HeaderName, HeaderValue>,
    max_event_bytes: usize,
) -> Self
```

It sets only the supplied fields and initializes `dispatch` and `session_observer` to `None`.

In `http_client/stream.rs`, add:

```rust
pub(crate) fn from_sdk(
    uri: Arc<str>,
    session: Option<Arc<str>>,
    cursor: Option<String>,
    token: Option<String>,
    headers: HashMap<HeaderName, HeaderValue>,
    max_event_bytes: usize,
) -> Self
```

It sets only the supplied fields and initializes `control`, `dispatch`, and `session_observer` to `None`.

- [ ] **Step 5: Run the new tests and verify GREEN**

Run:

```bash
cargo test -p pi-plugin-mcp http_client::tests::sdk_ --no-fail-fast
```

Expected: both tests pass with no warnings.

- [ ] **Step 6: Refactor `PiHttpClient` without changing its trait behavior**

In `http_client.rs`:

- Keep ordinary `post_message` delegating to `post_message_with_max_sse_event_size` with `StreamableHttpClientTransportConfig::default().max_sse_event_size`.
- Replace the manual `PostRequest { ... }` in the size-configured method with `PostRequest::from_sdk(...)`, then call `execute(...).await?.into_sdk()` exactly as before.
- Keep ordinary `get_stream` delegating to `get_stream_with_max_sse_event_size` with the same default.
- Replace the manual `StreamRequest { ... }` in the size-configured method with `StreamRequest::from_sdk(...)`, then return only `response.body` exactly as before.
- Do not edit `delete_session`.

Run:

```bash
cargo test -p pi-plugin-mcp http_client::tests --no-fail-fast
```

Expected: all backend tests pass.

- [ ] **Step 7: Refactor the static-header adapter through the private canonical path**

In `client/operation/http.rs`:

- Import `StreamableHttpClientTransportConfig` and use `StreamableHttpClientTransportConfig::default().max_sse_event_size` for both ordinary-method delegations.
- Make ordinary POST delegate to the size-configured POST using that unchanged rmcp default.
- Make size-configured POST construct `PostRequest::from_sdk(...)` with the caller's `auth` token and call `<Self as HttpClient>::post`; convert with `PostResponse::into_sdk`.
- Make ordinary GET delegate to the size-configured GET using the unchanged rmcp default.
- Make size-configured GET construct `StreamRequest::from_sdk(...)` with the caller's `auth` token and call `<Self as HttpClient>::open_stream`; return only the body.
- Retain `RequestControl` dispatch checks inside the private `HttpClient::post` implementation.
- Do not edit DELETE.

Run:

```bash
cargo test -p pi-plugin-mcp client::operation::tests --no-fail-fast
cargo test -p pi-plugin-mcp http_transport::tests --no-fail-fast
```

Expected: private controls remain off-wire; transport/session/stream tests remain green.

- [ ] **Step 8: Refactor provider authentication without duplicate token lookup**

In `auth.rs`:

- Import `StreamableHttpClientTransportConfig` and use its default `max_sse_event_size` for both ordinary-method delegations.
- Make ordinary POST delegate to size-configured POST with that rmcp default.
- Make size-configured POST call the existing private `HttpClient::post` with `PostRequest::from_sdk(...)`; preserve the existing ignored caller auth header by supplying `None`, and do not call `token_for_message` in the SDK method itself.
- Make ordinary GET delegate to size-configured GET with the rmcp default.
- Make size-configured GET call the existing private `HttpClient::open_stream` with `StreamRequest::from_sdk(...)`; preserve the existing ignored caller auth header by supplying `None`, and do not call `token` in the SDK method itself.
- Preserve `provider_response` mapping in the private path.
- Do not edit provider DELETE or `last_token` behavior.

Run:

```bash
cargo test -p pi-plugin-mcp auth::refresh::supervised_tests::provider_ --no-fail-fast
cargo test -p pi-plugin-mcp auth::refresh::supervised_tests::cleanup_reuses_last_token_without_oauth_refresh_or_provider_resolution --no-fail-fast
```

Expected: one provider resolution per admitted call, unchanged rejection projection, and lookup-free cleanup.

- [ ] **Step 9: Refactor local OAuth without nested refresh/replay loops**

In `auth/refresh.rs`:

- Import `StreamableHttpClientTransportConfig` and use its default `max_sse_event_size` for both ordinary-method delegations.
- Make ordinary POST delegate to size-configured POST with that rmcp default.
- Make size-configured POST call the existing private `HttpClient::post` with `PostRequest::from_sdk(...)`, preserving the caller's `auth_token`; the SDK method must not wrap that call in another `request(...)`.
- Make ordinary GET delegate to size-configured GET with the rmcp default.
- Make size-configured GET call the existing private `HttpClient::open_stream` with `StreamRequest::from_sdk(...)`, preserving the caller's `auth_token`; preserve GET-local authorization semantics from `authorized_request(..., false, ...)`.
- Do not edit OAuth DELETE, refresh coordination, application recovery allowance, or credential-store writes.

Run:

```bash
cargo test -p pi-plugin-mcp auth::refresh --no-fail-fast
cargo test -p pi-plugin-mcp --test http_auth_handoff --no-fail-fast
cargo test -p pi-plugin-mcp --test lifecycle_retry --no-fail-fast
```

Expected: all auth/replay/retry/cancellation cases pass, with no extra POST/GET or refresh.

- [ ] **Step 10: Run the complete affected package verification**

Run:

```bash
cargo test -p pi-plugin-mcp
cargo clippy -p pi-plugin-mcp --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Expected: package tests pass with only the two existing opt-in live tests ignored; Clippy, formatting, and diff checks exit 0.

- [ ] **Step 11: Record Slice 1 evidence without changing architecture claims**

Update `docs/mcp-module-simplification.md`:

- Mark only Slice 1 as implemented.
- Record baseline HEAD `98d5411`, the final affected tree, and exact focused/package test counts; do not invent the not-yet-created commit ID.
- Retain Slices 2 and 3 as pending.
- Do not close parity rows, change deliberate divergences, or claim official conformance.

Run:

```bash
git diff --check
git diff -- plugins/features/pi-plugin-mcp docs/mcp-module-simplification.md
```

Expected: the diff contains only canonical-path refactoring, constructor tests, and Slice 1 evidence.

- [ ] **Step 12: Run final repository gates before committing**

Because this plan commits a HIGH-risk shared HTTP seam, run the repository’s full requested-commit gates:

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

Expected: all commands exit 0. If the known depth-512 workspace test fails, report it by exact name and stop for disposition; do not skip it silently.

- [ ] **Step 13: Analyze graph changes**

Run:

```bash
node .gitnexus/run.cjs detect-changes --scope all --repo .
```

Expected: complete, non-truncated output. Any HIGH/CRITICAL newly affected path outside the reviewed MCP/auth/transport seams stops the commit. `partial: true` or `truncated: true` is unresolved and must be rerun.

- [ ] **Step 14: Commit only Slice 1 files**

Stage only the files listed under this task, excluding unrelated `.gitignore` and `docs/superpowers/specs/` work:

```bash
git add plugins/features/pi-plugin-mcp/src/http_client/post.rs \
  plugins/features/pi-plugin-mcp/src/http_client/stream.rs \
  plugins/features/pi-plugin-mcp/src/http_client/tests.rs \
  plugins/features/pi-plugin-mcp/src/http_client.rs \
  plugins/features/pi-plugin-mcp/src/client/operation/http.rs \
  plugins/features/pi-plugin-mcp/src/auth.rs \
  plugins/features/pi-plugin-mcp/src/auth/refresh.rs \
  docs/mcp-module-simplification.md
git add -f docs/superpowers/plans/2026-11-28-mcp-canonical-http-adapter-paths.md
git commit -m "refactor: canonicalize MCP HTTP adapter paths"
```

Expected: one reviewable commit; unrelated work remains unstaged and unchanged.
