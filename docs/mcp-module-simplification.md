# MCP module simplification plan

Status: Slice 1 implemented; Slices 2 and 3 remain pending. This document does not change executable behavior.

## Decision

Keep the existing MCP architecture: rmcp provides the protocol service, request correlation,
stdio and OAuth foundations; `http_transport` owns HTTP session and stream lifetimes;
`http_client` owns bounded HTTP I/O; the supervisor owns reusable connections; generation-local
views own executable policy. Improve Depth and Locality within those Modules rather than
introducing another client runtime, another crate or a generic middleware framework.

The implementation scope is three sequential slices:

1. **Implemented (2026-11-28):** Consolidate HTTP Adapter forwarding around canonical request paths.
2. **Pending:** Extract Agent tool adaptation from the client Module.
3. **Pending:** Separate library preparation inputs from prepared generation state.

Slice 1 was implemented from baseline HEAD `98d5411`. Its affected source tree is:

- `plugins/features/pi-plugin-mcp/src/http_client/{post,stream}.rs`
- `plugins/features/pi-plugin-mcp/src/http_client.rs`
- `plugins/features/pi-plugin-mcp/src/http_client/tests.rs`
- `plugins/features/pi-plugin-mcp/src/client/operation/http.rs`
- `plugins/features/pi-plugin-mcp/src/auth.rs`
- `plugins/features/pi-plugin-mcp/src/auth/refresh.rs`

Focused evidence on the final affected tree passed: SDK request constructors 2/2, HTTP backend
13/13, operation controls 3/3, private HTTP transport 25/25, provider auth 3/3 plus cached cleanup
1/1, OAuth refresh 43/43, HTTP auth handoff 2/2, and lifecycle retry 7/7. The complete
`pi-plugin-mcp` package passed 395 tests with 2 opt-in live tests ignored. Targeted all-target
Clippy with warnings denied, workspace formatting, and diff checks also passed. Slices 2 and 3 were
not changed, and this evidence does not close parity rows or establish official MCP conformance.

Each slice preserves the public Interface and observable behavior. Only the first slice is
expected to directly remove a meaningful amount of forwarding code. The other slices reduce
state-management obligations and the number of concepts a maintainer must understand together.

## Baseline and execution prerequisite

The source review was performed on 2026-11-27 with HEAD `202eb9a` and existing uncommitted
MCP work, including the private HTTP transport replacement. HEAD alone does not identify the
reviewed source. This plan does not authorize modifying or reverting another task's changes.

The earlier size sample covered only Rust files in `plugins/features/pi-plugin-mcp`:
approximately 12,700 production-source lines and 17,200 test/fixture lines, including comments
and blank lines. The classification is approximate, not an AST-based executable-line count.
The private HTTP transport was about 750 lines; its HTTP backend was about 800 production lines.
These numbers describe a working-tree sample, not a size target or a compatibility verdict.

Before implementation, finish the overlapping transport work or obtain an explicit source
handoff, record the resulting inputs and establish the affected regression baseline. Do not
interleave structural changes with unfinished behavior fixes in the same files. Existing test
results remain reusable only while their inputs are unchanged. A reproduced existing failure
must remain visible with its disposition; an unexplained failure stops the relevant slice.

Primary references:

- [Architecture and generation ownership](architecture.md#mcp-configuration-and-generation-ownership)
- [MCP behavior and validation ledger](mcp-post-auth-gap-matrix.md)
- [MCP public entry points](../plugins/features/pi-plugin-mcp/src/lib.rs)
- [MCP feature documentation](../plugins/features/pi-plugin-mcp/README.md)

## Interfaces and invariants to preserve

| Module | Interface responsibility that remains here |
| --- | --- |
| rmcp | Typed MCP messages, service/peer, request correlation, setup lifecycle, stdio and OAuth foundations |
| `http_transport` | The sole product HTTP worker; session state, POST admission, control dispatch, GET recovery and response-tail lifetime |
| `http_client` | HTTP headers, status classification, bounded response decoding and I/O; no autonomous retry or credential refresh |
| Authentication Adapters | Static-header, provider-token and OAuth behavior, including the existing refresh coordinator |
| Supervisor | Configuration/attempt identity, shared connection acquisition, lazy reconnect, inventory refresh and connection retirement |
| Generation view | Frozen tools, exposure, selection intent, diagnostics and next-idle publication |
| `McpLibrary` | Trust-filtered configuration, management operations and preparation through the existing public Interface |

In particular:

- Preserve `McpToolSet`, `McpLibrary`, `PreparedMcpGeneration`, configuration types, public
  re-exports and method behavior. Public JSON reports, configuration and credential files, Pi v4
  records and plugin contracts retain their existing shapes.
- Explicit/ACP clients remain independent of local configuration discovery and `/mcp` registration.
- Keep next-idle whole-generation publication, active-view immutability, failed-candidate rollback,
  selection intent and unchanged-input Reload reuse.
- An executable view may retain a connection, but must not acquire the supervisor's strong
  readiness lease. That lease remains in the host's generation-freshness source.
- Preserve distinct dispatch and completion checks: ordinary supersession can allow an admitted
  response to finish while prohibiting another dispatch. Cancellation and retirement remain terminal.
- Keep setup retries, application/resource recovery, OAuth replay and SSE stream recovery as
  distinct decisions. Preserve their existing budgets and Explicit/Supervised ownership.
- A response-stream GET must not reexecute the original tool RPC. Final response receipt must not
  reopen that response stream; existing-tail handling remains unchanged.
- Token acquisition and reactive refresh must preserve checks after asynchronous waits. GET openings
  retain their own bounded authentication reaction instead of consuming an originating POST's budget.
- DELETE remains bounded, best-effort cleanup using the transport's cached acquired token: no token
  lookup, refresh, replay, credential writes or diagnostic-sensitive logging.
- Preserve private diagnostics, late local-value resolution and current credential write conditions.
  This refactor does not establish new cross-process or mixed-runtime credential guarantees.

These are behavior-preserving changes, not new Pi conformance work. Existing deliberate Rust
choices and open protocol gaps retain their disposition.

## Slice 1: canonical HTTP Adapter paths

### Problem and evidence

`DispatchHttpClient`, `ProviderAuthClient` and `LocalOAuthClient` each implement ordinary and
size-configured GET/POST methods. Much of the argument forwarding and authentication entry logic
is repeated. The private `HttpClient::open_stream(StreamRequest)` additionally carries the GET
control information that the SDK Interface does not express.

The existing sibling pattern is already appropriate: `PiHttpClient::post_message` delegates to
`post_message_with_max_sse_event_size` with the SDK's default limit. Follow that pattern outward.

Sources:

- [HTTP backend](../plugins/features/pi-plugin-mcp/src/http_client.rs)
- [GET request Interface](../plugins/features/pi-plugin-mcp/src/http_client/stream.rs)
- [Static-header Adapter](../plugins/features/pi-plugin-mcp/src/client/operation/http.rs)
- [Provider Adapter](../plugins/features/pi-plugin-mcp/src/auth.rs)
- [OAuth Adapter](../plugins/features/pi-plugin-mcp/src/auth/refresh.rs)

### Change

- Give each Adapter one canonical POST implementation: the size-configured method. The ordinary
  POST method supplies the unchanged default and delegates before any token acquisition or refresh.
- Give each Adapter one canonical GET implementation accepting explicit stream inputs and optional
  request control. The production `open_stream` path supplies that control; SDK convenience methods
  enter the same Implementation without inventing request control or an application recovery budget.
- Keep DELETE separate. It must not pass through the ordinary authorization request helper.
- Keep the three Adapters: they implement genuinely different authentication behavior. Share the
  forwarding shape, not a new universal retry loop or a collection of mode flags.
- Keep the SDK trait compatibility surface needed by existing consumers and stock-characterization
  tests. Do not remove it merely because the product now uses a private HTTP worker.
- Keep `HttpClient` private. Do not publish the extra GET seam or add forwarding macros/frameworks.

### Deletion and acceptance

Delete the replaced duplicate method bodies in the same slice; do not leave two implementations
behind a new wrapper. Keep required trait methods as minimal delegation where needed.

Acceptance requires equivalent status/error projection, default and explicit event limits, token
lookup counts, refresh counts, replay counts, cancellation checks and wire attempts across static,
provider and OAuth paths. In particular, delegation must not acquire a token twice or nest two
refresh loops. Cover POST, initial common GET, resumed GET and DELETE independently.

Primary regressions: `http_client::tests`, `auth::refresh`,
[HTTP auth handoff](../plugins/features/pi-plugin-mcp/tests/http_auth_handoff.rs),
[lifecycle retry](../plugins/features/pi-plugin-mcp/tests/lifecycle_retry.rs), and
[request recovery](../plugins/features/pi-plugin-mcp/src/client/recovery_tests.rs).

## Slice 2: localize Agent tool adaptation

### Problem and evidence

`client.rs` holds server declarations, connection setup, the tool pool and Agent tool/Plugin
adaptation. In particular, `McpTool`, `McpToolPlugin`, tool description/schema projection and
registration are mixed with protocol connection setup. The result is a broad Module to navigate,
not evidence that the underlying behaviors should disappear.

### Change

- Add a private `client/tools.rs` Module for `McpTool`, `McpToolPlugin` and their cohesive
  construction, registration and execution adaptation.
- Keep `McpToolSet` as the existing facade. Its callers continue to connect, inspect descriptors,
  obtain a plugin and shut down through the same Interface.
- Keep connection setup, discovery, request execution and lifetime management in the existing
  client Modules. Keep management commands in `plugin.rs` / `plugin/manager.rs`.
- Continue using `naming`, `output_schema`, `result` and `resources`; do not duplicate their
  projection helpers in the extracted Module.
- Keep frozen-descriptor checks and collision-before-filtering behavior unchanged. Catalog
  construction remains a single shared path for explicit and supervised clients.
- Export only what the parent Module needs, preferably with `pub(super)`. Avoid compensating for
  the move with broadly public fields or a new pass-through interface per helper.
- Leave public configuration/error definitions in place in this slice. A smaller `client.rs` is
  useful, but creating a separate file for every type is not a goal.

### Deletion and acceptance

Move the Implementation and its directly owned tests; remove the old definitions rather than
retaining forwarding duplicates. Keep crate-root public re-exports unchanged.

Acceptance requires identical normalized names, collision handling, descriptions, input/output
schemas, annotations, execution ordering, resource reachability, full structured results,
model-facing output limits and prompt instructions. The explicit client still registers tools
without local management; the local library still adds `/mcp`.

Primary regressions: `client::contract_tests`, `client::snapshot_tests`, naming/output/result
Module tests, [entry points](../plugins/features/pi-plugin-mcp/tests/entry_points.rs),
[resources](../plugins/features/pi-plugin-mcp/tests/resources.rs), and Coding MCP
reload/deferred/resource tests. Inspect [plugin registration](../plugins/features/pi-plugin-mcp/src/plugin.rs)
to preserve its existing delegation to the tool-only Plugin rather than duplicating registration.

This slice improves Locality. It is not expected to materially reduce total line count.

## Slice 3: separate preparation inputs and prepared library state

### Problem and evidence

`McpLibrary` currently contains both stable management context and generation-derived fields.
Strict preparation clones it and manually resets generation, readiness, directory, pending and
selection fields; supervised preparation clones it and fills those fields individually.
Selection preparation also repurposes input fields as output state.

There is an important effect to preserve: successful bound-library operations such as remove,
disable and logout invalidate the active server. Moving state only into command handlers would
lose that behavior for callers using the library directly.

Sources:

- [Library fields and strict preparation](../plugins/features/pi-plugin-mcp/src/library.rs)
- [Supervised preparation and lifecycle methods](../plugins/features/pi-plugin-mcp/src/library/supervised.rs)
- [Selection input and activation](../plugins/features/pi-plugin-mcp/src/library/selection.rs)
- [Mutation invalidation regression](../plugins/features/pi-plugin-mcp/src/library/supervised/tests/auth.rs)

### Internal model

Retain the public `McpLibrary` facade. Separate its private data into three concepts:

| Private concept | Contents and lifetime |
| --- | --- |
| Management context | Paths, trust, authentication dependencies, host adapter and feature options; reusable across preparations |
| `SelectionInputs` | Preserved/current candidates, historical candidates and the weak live desired-selection reader supplied for preparation |
| Prepared view | Frozen readiness/directory/pending diagnostics, the derived selection activation data, and the optional weak readiness handle |

Use a private tagged binding to distinguish an unbound library from a prepared view. A proposed
`library/view.rs` Module owns prepared-view construction and lifecycle access. Immutable view data
may be shared between the product Plugin and its command, but contains no strong supervisor or
readiness lease. `PreparedMcpGeneration.readiness_watch` retains its existing host handoff.

The facade continues to mediate management effects. This is internal state separation, not a
requirement for CLI/ACP/Coding callers to learn a second public library Interface.

### Change

- Make selection preparation produce activation data rather than clearing and repurposing fields
  in the caller's library. Keep the three public `with_*tool_selection*` builders and their behavior.
- Construct strict and supervised prepared views explicitly. Replace the long reset/assignment
  sequences with those constructors; an original library remains reusable after either preparation.
- Route publication, shutdown, reconnect eligibility, status/directory reads and selection activation
  through the prepared view. Preserve publication-before-fallible-UI ordering.
- Keep public management methods responsible for successful mutation invalidation via their optional
  prepared binding. Do not move this effect exclusively into `McpCommand`.
- Preserve operation-specific behavior: absent removal does not invalidate; disabling invalidates
  only after a successful write; enabling is not silently given the disabling behavior; successful
  logout retains its current invalidation behavior even when no stored credential was removed.
- Preserve retired-view behavior, reason-aware shutdown, next-idle handoff and failed-candidate
  cleanup. Do not change supervisor epochs, leases or connection admission algorithms in this slice.
- Keep public `PreparedMcpGeneration` fields and serialized reports unchanged, even if that requires
  an explicit immutable data copy. Avoid a public contract change merely to save a clone.

### Deletion and acceptance

Remove the replaced individual generation-derived fields and manual reset sequence from the facade.
Do not retain a second writable mirror of the same state. Distinguish intentional public result
snapshots from mutable internal duplication.

Acceptance requires:

- Strict preparation ignores selection inputs as before and does not contaminate later supervised
  preparation using the same original library.
- A denied/failed write cannot invalidate a healthy transport; successful bound remove/disable/logout
  still invalidates through the library Interface itself.
- Ordinary Reload reuses eligible connections; New/Resume/Fork retain their distinct ownership rules.
- Dropping the readiness owner stops work even if commands, plugins or old executable views survive.
- Current selection, historical candidates and explicit deselection retain their distinct meanings.
- Late registration failure preserves the published connection; unpublished candidate cleanup remains
  confined to the candidate.
- Manager statuses, prompt directory text and command navigation remain observably unchanged.

Primary regressions: `library::selection`, `library::supervised::tests`, supervisor pinning/transaction
and refresh tests, plugin management tests, and Coding `mcp_lifecycle`, `mcp_notifications`,
`mcp_recovery` and `mcp_reload` tests.

This slice has the highest ownership coupling and runs last. Its purpose is fewer required state
resets and fewer invalid state combinations, not fewer safety checks.

## Test and fixture policy

Test at the same Interface that production callers use: real `McpToolSet`/`McpLibrary` entry
points, the product transport and deterministic loopback servers. In-process projection and
selection logic can be tested directly. HTTP/OAuth fixtures substitute for external servers;
no real providers, credentials or browser sign-ins are needed.

Preserve observations that distinguish the behavior: request counts, no-extra-GET checks,
shutdown/cancellation, one tool execution, retained sibling requests, emitted diagnostics,
selection and session persistence. Updating imports or internal fixture construction is allowed;
weakening these assertions to accommodate a refactor is not.

Reuse a fixture only when its protocol and observation model actually match. Keep tests at
multiple layers when they prove different handoffs. Stock SDK characterization and product
conformance have different purposes; clearly identify them and do not treat one as a substitute
for the other.

This plan does not preselect tests for deletion. A proposed deletion requires a named old-to-new
coverage mapping, proof that no unique behavior is lost, and review before removal. Do not build
a general test harness solely to lower fixture line count.

## Sequence and verification

Run the slices sequentially after the overlapping transport baseline is ready. They share client,
auth, library and test files; concurrent writers would make behavior attribution harder. Each
slice is a separately reviewable change with its own before/after evidence. Do not bundle behavior
fixes, credential migrations, dependency updates or broad formatting changes into these slices.

During implementation:

1. Run the smallest owning regressions before and after each change. Add a targeted check when
   compilation coverage is useful. Preserve unchanged evidence rather than rerunning it mechanically.
2. After each completed slice, run affected-package tests and targeted all-target Clippy, plus
   workspace formatting and diff checks.
3. The prepared-view slice additionally exercises Coding integration, ACP isolation and CLI
   management. Since this work restructures generation ownership representation, run full workspace
   gates on final inputs before declaring the integrated refactor complete or preparing a requested
   commit/PR. A known failure must be reported, not silently excluded.

Representative affected-package commands, for implementation time only:

```bash
cargo test -p pi-plugin-mcp
```

```bash
cargo test -p pi-coding --test integration mcp_
```

```bash
cargo test -p pi-acp mcp_auth_tests
```

```bash
cargo test -p pi-cli --test mcp_management
```

```bash
cargo clippy -p pi-plugin-mcp --all-targets -- -D warnings
```

Full gates remain those in the repository guide: workspace tests, workspace/all-target Clippy
with warnings denied, `cargo fmt --all -- --check`, and `git diff --check`. Expand targeted Clippy
to other changed Rust packages. Run Node/native/PTY checks only where their inputs or Interfaces
are affected; unchanged Node/NAPI behavior is not a reason to introduce another validation project.
CI remains authoritative. No compilation or runtime tests were run to author this document.

## Completion criteria

- Public callers require no new setup sequence, dependency or lifecycle responsibility.
- Each Adapter has one canonical implementation per ordinary request kind; compatibility methods
  only delegate, and cleanup retains its separate semantics.
- Agent tool adaptation is local to one private Module and reused by both entry points.
- Prepared-view construction replaces field-by-field resetting and selection-input repurposing.
- Readiness strong ownership and observable mutation invalidation are unchanged.
- Production source, forwarding bodies, state mirrors and fixture duplication are measured
  separately. File moves are reported as Locality improvements, not claimed as deleted complexity.
- No new framework, public seam or crate is introduced merely to redistribute lines.
- The architecture documentation explains the adopted internal ownership model after implementation;
  the parity ledger records real evidence without closing unrelated HTTP/OAuth gaps.

The expected outcome is a smaller Interface burden and a more local Implementation, not an
arbitrary percentage reduction in source size.
