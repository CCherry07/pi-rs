# MCP post-auth parity ledger (#132)

## Frozen comparison

- Rust baseline: `4d17e8f0a222cc1a61ab1acfbbf82af47979e312` (local `main`).
- Behavior oracle: `legacy/pi@0ab23c37b074252d04e31f5cfb99491f0613fb80`, verified clean.
- Owner: @Jinah, continuing #128/#131; no second implementation or worker.
- This is an **open acceptance ledger**, not a claim of full MCP parity. Baseline findings below
  stay fixed; implementation/evidence updates belong in the delivery column or tranche notes.
- `L/` below means `legacy/pi/packages/coding-agent/`; Rust paths are repository-relative.
- Existing evidence is reusable while its inputs remain unchanged: #131 workspace tests, workspace
  Clippy `-D warnings`, fmt and diff checks passed (`caf04fc5`); local OAuth/provider and four native
  PTY cases passed (`706ab552`). These runs do **not** establish official conformance or the open rows.

## Corrections to the previous handoff

The oracle's `src/extensions/mcp/index.ts:876–916` offers a **read-only Tools view** displaying names,
first-line descriptions and effective exposure. It edits server exposure, not individual tool
exposure. A per-tool exposure editor is shared non-support, not a missing legacy feature.

A follow-up direct read of `config.rs:expand_with` corrected the initial C1 wording: `!command`
is passed through literally, not rejected. Both behaviors differ from the oracle's value resolver.

The oracle logs server `notifications/message` to a rotated file (`log.ts`); it does not expose a
live log viewer in the MCP manager. File logging and connection/stderr status are the parity work.
An interactive live log viewer would be an additional product feature.

## Confirmed open observable contracts

| ID | Oracle contract and source | Rust baseline / owning seam | Required evidence and delivery |
| --- | --- | --- | --- |
| M1 | `L/src/extensions/mcp/cli.ts:add`: `--oauth-client-id`, `--oauth-client-secret`, `--oauth-callback-port`, `--oauth-client-name`, `--oauth-client-registration`; HTTP-only, validated before save | `apps/pi-cli/src/mcp_commands.rs` omits these flags; `McpServerDefinition` cannot author OAuth even though config supports it | **DELIVERED `edddce1e8`**. Typed library authoring + CLI parser tests, round-trip unresolved secrets, invalid/misplaced flags leave disk unchanged |
| M2 | `index.ts:showTools`: full offered-tool inventory, description and differing effective exposure, including tools hidden by policy | `plugin.rs:manage` shows only aggregate counts; inspection can already inventory hidden tools | **DELIVERED `edddce1e8`**. Read-only semantic view; cancellation and no disk/generation mutation; preserve remote names separately from normalized tool names |
| M3 | `index.ts:serverMenu/saveConfig`: Enable/Disable in this project for a global server, with explicit trusted-project override ownership | Existing overrides merge correctly; manager cannot create this project-only override | **DELIVERED `edddce1e8`**. Trusted project-only write, global bytes unchanged, inherited credentials untouched; reload failure retains old generation |
| M4 | `index.ts:pickServer` and command handler: omitted login/logout/reconnect name auto-selects the only eligible/preferred server, otherwise selector; contextual actions, completion, returning/back/cancel | Rust commands require names and offer a fixed action list, including ineligible sign-in actions | **DELIVERED `edddce1e8`**. Eligibility/state-dependent choices, non-UI behavior, no duplicate side effects on cancellation, retired-context handoff |
| M5 | `cli.ts:list`: optional `authProvider`, always-present `errors` array, raw remote tool names with differing `toolExposure`, actionable names/provider guidance | `McpServerReport` has no `authProvider`; CLI already emits `errors: []` and differing tool exposures, which must be preserved | **DELIVERED `edddce1e8`**. Exact stable JSON-shape and needs-auth output regressions; preserve useful Rust-only inspection APIs |
| M6 | `cli.ts:runMcpCommand/editDistance`: unknown login/logout name suggests a nearby configured name, lists configured names and ignored-project guidance | Rust error lacks the same discovery assistance | **DELIVERED `edddce1e8`**. Deterministic suggestion/unknown/untrusted-scope cases; never echo credentials |
| D1 | `L/src/extensions/mcp/log.ts`: timestamp/server/level/logger, multiline indentation, append per message, rotate above 5 MiB to `mcp.log.1`, best-effort failures | `client_handler.rs` handles roots/progress only; no server log sink | **OPEN**. Notification-to-private-file fixture, rotation and unavailable sink, concurrent writers; logging cannot break requests or write outside renderer to terminal |
| D2 | `runtime.ts:connectOnce/handleClientClose`: drain stdio stderr; last 2,000 characters explain initialization failure or disconnect | `client.rs` sends stderr to null | **OPEN**. Bounded drain that cannot deadlock the child, redacted diagnostic tail, failure/exit cleanup and no terminal pollution |
| C1 | `runtime.ts:createDefaultTransport/oauthSettings` + `L/src/core/resolve-config-value.ts`: uncached `!command` for env/header/client-secret values, 10-second bound, trim/nonempty success; parse/save do not execute | `config.rs:expand_with` treats command-backed strings as literal values instead of executing them; transport-only APIs are explicit input and must not gain implicit shell execution | **OPEN**. Explicit local-config resolution capability after trust; no execution for untrusted/disabled entries, static parse/authoring/inspection metadata; cancellation, output limits, redacted failure, refresh/reconnect rereads |
| C2 | Same resolver: embedded `$VAR`/`${VAR}`, `$$` and `$!` escaping, missing/empty env rejection; command/args only home expansion in legacy | Rust supports `${env:VAR}` and whole `$VAR`, expands additional transport fields; exact template behavior differs | **OPEN**. Table-driven shared syntax tests; preserve documented Rust extensions separately, never execute command/args/URL as value commands |
| L1 | `index.ts:session_start/startConnection`: config and independent connection failures retain healthy servers plus one startup diagnostic | Product prepare tolerates typed needs-auth only; unrelated config/connection errors reject the whole generation | **OPEN**. Partial-fault candidate containing healthy tools and management state; still reject generation-wide invariant/collision failures and preserve transactional publication |
| L2 | `index.ts:before_agent_start/tool_call`, `agent-session-mcp.test.ts:768–922`: background connection; first prompt waits only for direct servers, bounded at 10 seconds; scripts/search/resources wait only for relevant servers; pending directory summaries | Rust waits for all connections before publication. Registry immutability is intentional, but the resulting observable latency is not thereby approved as equivalent | **APPROVED TIMING DIVERGENCE; implementation/evidence OPEN**. Bounded startup and next-idle whole-generation publication; typed pending/retry diagnostics during active runs instead of same-turn selective waits. See decision below |
| L3 | `runtime.ts:open`: HTTP transient initialization retries after 250/1000 ms (408/429/5xx except 501/network), abort stops backoff; no stdio blanket retry | Rust has protocol-handshake fallback, not this retry policy | **OPEN**. Typed failure classification, exact attempts/backoff, close/cancel stops work; no auth/permanent error retries |
| L4 | `runtime.ts:request`: one transient HTTP retry for resource reads/listings, never tool calls; expired-session 404 allows one request retry on a new session without cancelling other in-flight requests | No product-owned retry/session-expiry contract over rmcp has been established | **OPEN**. Local POST/GET counts; mutating tool not repeated on ambiguous failure; expired-session concurrent calls share reconnect safely |
| L5 | `runtime.ts:getClient/handleClientClose/reconnect`: disconnected state, lazy reconnect on next call, coalesced connect, explicit reconnect and shutdown lifetime | Published `McpToolSet` holds fixed services; manual reconnect currently probes then reloads | **OPEN**. Transport replacement behind frozen descriptors; schema changes need a new generation, no silent live registry mutation; cleanup/parallel callers/failed retry |
| L6 | `runtime.ts:refreshTools` + `index.ts:registerTools`: tools/list_changed adds/updates/retracts tools, retaining old inventory on failed refresh | Client handler does not observe list-changed notifications | **OPEN**. Coalesced notification revision; whole-generation schema publication, removed names become unreachable, active-run safety, failed candidate rollback |
| E1 | `L/src/core/extensions/{types,loader}.ts`, `core/mcp-servers.ts`, `index.ts:registeredServers`: register/get/unregister, owner-only removal, same-owner replacement, namespace collision rejection, config precedence (including normalized names) | No corresponding JS registration API/registry integration found; explicit `McpToolSet` is not an equivalent registration API | **OPEN**. Domain-owned configuration snapshots projected through generation factories; no MCP-specific executable policy in generic `pi-plugin`; registration order/ownership/collision tests |
| E2 | `index.ts:mcp_servers_change`: late extension registration/replacement/removal, old work invalidation/cleanup, session-only manager edits, report registrations without a connector | No registration-change adapter or transient extension management scope | **OPEN**. Generation-safe adapter, stale-attempt rejection, restart/close ordering, no mcp.json or Pi v4 config persistence |
| E3 | `L/src/cli/args.ts` and built-in extension loading: `--no-mcp`; `--no-extensions` disables built-ins as well as discovered extensions while explicit extension paths remain valid | No `--no-mcp`; Rust `--no-extensions` currently only gates extension discovery, not MCP | **OPEN**. CLI/product option, no accidental connect/tool/schema/prompt entries; no impact on explicit ACP client server isolation; Node argument-splitting coverage if touched |
| Q1 | `L/test/mcp-conformance/{README.md,run.ts,client.ts,baseline.json}`: official pinned suite `0.2.0-alpha.11`, modes 2025-03-26/06-18/11-25; checks and client completion baseline; missing scenarios fail | Local rmcp fixtures exist, no equivalent official runner/baseline coverage is established | **OPEN**. Product-path client adapter, pinned reproducible runner, local fixture servers only, per-check reviewed baseline; never regenerate to hide failures |
| Q2 | Published SDK/harness docs describe shipped behavior | `packages/pi/docs/mcp.md` still says OAuth/provider auth unsupported and documents pre-management APIs; release copies it as shipped documentation | **DELIVERED `edddce1e8`**. Sync authored docs deliberately, verify links and release input; docs alone do not require rerunning unchanged Rust gates |

## Newly identified auth-boundary carryovers (not silently waived)

These are observable deltas found by the post-auth comparison, not a withdrawal of the already-green
#131 implementation evidence. They must be fixed in an appropriate tranche or individually accepted
before claiming full parity.

| ID | Oracle | Rust baseline | Required evidence / disposition |
| --- | --- | --- | --- |
| A1 | `cli.ts:login/waitForRedirectUrl` allows non-TTY browser callback login; only terminal paste is disabled; already-authorized probe also works without TTY | CLI rejects nonterminal input before probing | **DELIVERED `edddce1e8`**. Piped-stdin local fake-browser callback and already-authorized no-browser tests, bounded cleanup |
| A2 | `cli.ts` accepts positive finite fractional `--timeout` seconds | Rust restricts integers to 1–86400 | **DELIVERED `edddce1e8`**. Fractional timer/invalid numeric parser tests or approved documented limit |
| A3 | Official auth cases include step-up, retry-limit and CIMD behavior under several negotiated protocols | Existing deterministic tests are narrower than the official baseline | **OPEN via Q1**, not presumed equivalent or failing until measured |

## Management tranche delivery and evidence

M1–M6 and A1–A2 are delivered by `edddce1e8082b961ade289e3189d4883d94fc6d6`, verified on
local `main` with a clean checkout; nothing was pushed. Q2 deliberately synchronizes only the
MCP introduction and local-library/management/auth sections of `packages/pi/docs/mcp.md`; unrelated
resource, configuration and Cua sections remain authored as before. No full parity claim follows.

- M1: typed OAuth authoring in `library.rs` and CLI parser; library round-trip and native
  `apps/pi-cli/tests/mcp_management.rs` cases preserve unresolved values and reject invalid writes.
- M2: offered inventory in `client.rs`; `plugin/tests/inventory.rs` drives nonempty raw-name/hidden
  Tools menus with local HTTP and proves no execution, write or reload.
- M3/M4: `plugin/manager.rs`, `plugin/tests/{management,auth}.rs` cover project ownership, contextual
  navigation, omitted-name selection, cancellation and replacement success/failure. M4 additionally
  adds frozen owner-neutral registry completion metadata in `pi-plugin::command::completion`;
  `apps/pi-cli/src/tui/tests/command_completions.rs` exercises generic selector behavior and actual
  successful/failed replacement, old snapshot stability, no provider request and lazy persistence.
- M5/M6: optional provider metadata, stable JSON, native needs-auth/redacted guidance and
  deterministic unknown-name/untrusted-project diagnostics in library and native management tests.
- A1/A2: native parser tests and six local OAuth fixture modes (callback/paste/cancel/timeout and
  pipe-callback/pipe-timeout); pipe callback also proves already-authorized login opens no browser.
- Focused management/native evidence `05877924`: MCP, CLI MCP/native authoring tests, build, all
  six OAuth fixture modes, fmt and diff green. Completion-contract run `96e54156` passed plugin/MCP
  tests, then found a missing id in the CLI fixture. Repaired run `ced373fb` passed both CLI completion
  regressions and all six native management tests, plus fmt/diff. Shipped-doc run `b849d0e3` passed
  package typecheck, all 14 release unit tests, synchronized-section/local-link/release-input checks.
- Final public-contract gate `af36e33b-3f16-488f-98de-002f878415ee` passed
  `cargo fmt --all -- --check`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`, and
  nonpaged `git diff --check`. Real-provider opt-in tests remain ignored; no official conformance
  run is implied by these workspace results. Subsequent ledger-only edits do not invalidate them.
- CLI/MCP/SDK READMEs and architecture document the registered metadata boundary and management
  behavior. All other rows stay open; #132 must not close on this tranche. C1/C2 and D1/D2 are next,
  followed by the approved L2 boundary with L1–L6, E1–E3, and Q1/A3 official conformance. Q2 must be
  kept current as those remaining behaviors ship.

## C1/C2 validated candidate (local delivery pending)

The current uncommitted candidate adds the approved in-memory `pi-shell::capture` seam, local-library
execution opt-in, Pi value templates, and connect-time helper resolution. Run `d6ee3066` passed all
`pi-shell` / MCP tests, Coding MCP-configuration integration tests and native CLI management tests;
it then found four redundant-reference Clippy errors. After repair, `0cebbf82` passed formatting,
affected-package Clippy and diff checks. This is owning-seam evidence, not final public-contract
workspace evidence or a local landing. POSIX helper fixtures are Unix-gated; no Windows run is claimed.

The oracle (`legacy/pi/packages/mcp/src/oauth/auth-provider.ts:67`) resolves OAuth settings inside
each reactive refresh, and does not need the secret while a stored token remains usable. Approval
`question-a993de2c251d4167b3a9694b3c8a7287` selects a private MCP-owned late-settings adapter, not an
SDK fork or serialized-request rewriting. The candidate now supplies typed settings to rmcp only
for needed refresh; connection setup keeps secrets unresolved. `auth/refresh.rs` shares refresh
success/failure and owns its lifetime independently of any cancelled waiter. Conditional store
writes preserve intervening login/logout. Run `8f1339a2` passed all six adapter regressions (rotating
secret with valid/expired/rejected tokens; concurrent refresh plus cancelled waiter; shared failure
and retry without corruption; helper failure/no token request; intervening login/logout; second
401/no loop), config tests, local-library value tests, and MCP Clippy.

Final-input run `3d935b82-246a-4430-b6b7-6b128a1b83ca` passed workspace tests, workspace/all-target
Clippy with warnings denied, formatting and diff checks, a native CLI build, all six OAuth PTY/pipe
fixtures, package typecheck and all 14 release tests. This includes the library-level valid-token /
no-helper regression. Authored-doc run `07e8240c` checks synchronized local-library/value sections,
README/shipped-doc relative links, added architecture links, and release documentation inputs.
A broader scan found the pre-existing unrelated `docs/architecture.md` link to missing `plugin-ui.md`;
this tranche neither adds nor changes that link. No Windows or official conformance run is claimed.
C1 and C2 remain open until local delivery is verified; #132 remains open, with D1/D2, L1–L6, E1–E3
and Q1/A3 still outstanding. Nothing in this section waives a frozen row.

## Existing coverage to preserve

The previous tranche covers stdio/Streamable HTTP entry points; provider-safe names and output schema;
text/image/structured results and bounded model-facing output; progress/idle timeout/remote cancellation;
roots; shared resource tools and filtering; trust/merge/provenance; exact/wildcard exposure;
direct/deferred/codemode/hidden routing; automatic entry-point activation; lazy Pi v4 persistence;
real OAuth and named provider credentials; ACP isolation; and whole-generation auth freshness.
Owning tests include `plugins/features/pi-plugin-mcp/{src/*tests*,tests/{auth,entry_points,resources,management_auth}.rs}`,
`domains/coding/tests/integration/mcp*.rs`, `crates/pi-session/tests/generation_freshness*.rs`, and
`apps/pi-cli/{src/mcp_oauth/tests.rs,tests/mcp_management.rs,tests/fixtures/mcp_oauth_pty.py}`.
This summary is not a blanket equivalence claim for every legacy test: Q1 and the rows above remain open.

## Shared non-support / outside the pinned target

- Per-tool **editing UI** and an interactive live log viewer: neither is in the pinned manager.
  File `toolExposure` configuration and read-only per-tool inspection are in scope.
- Legacy standalone HTTP+SSE transport and MCP Apps rendering are not the product's supported route;
  Streamable HTTP SSE responses and resource data are in scope.
- Elicitation is explicitly unsupported in the oracle's official baseline. Rust does not advertise
  it in `McpClientHandler`; Q1 must lock in the corresponding unsupported result rather than invent support.
- The oracle excludes stateless protocol 2026-07-28. Do not conflate that with the three tested stable
  versions, or assert that rmcp's newer-version behavior has been validated merely because it compiles.
- Oracle `auth/basic-cimd` uses Pi's metadata identity rather than the suite's fixture identity;
  scope-retry-limit intentionally fails client completion. Review per-check differences rather than
  treating either scenario as an unrestricted expected-failure exemption.

## Approved timing decision (L2)

@Suzuya approved `question-6f4517b334d248c280d80e441cf473a8`: bounded startup followed by
publication of a **complete whole generation only at the next idle admission**. While an active
turn is running, use typed pending-server/retry diagnostics, never a live registry mutation or
mid-run generation handoff. This deliberately differs from legacy's same-turn selective wait.
Approval is not completion evidence. Required regressions: direct startup bound; indirect call
while pending; credential/server readiness during an active run; next-idle publication; failed
candidate preservation; resume/reload. Queue order, cancellation, tool-call/result pairing,
lazy persistence and Pi v4 journals must remain unchanged. Architecture must describe the same
limitation when the adapter lands.

## Delivery plan and decision boundaries

1. **Management/authoring:** M1–M6, A1–A2 and associated docs. Read-only tool view, not a new editor.
2. **Configuration/diagnostics:** C1–C2, D1–D2 with explicit resolver capability, trust, cancellation,
   private bounded storage and redaction. Commands only execute when an eligible local connection needs them.
3. **Connection lifecycle:** L1–L6. First settle the L2 publication/wait policy. Existing immutable
   registries, abort-safe admission, journal pairing and failed-reload preservation remain non-negotiable.
4. **Extensions/disable switches:** E1–E3, reusing factory-backed configuration projection and ownership.
5. **Official evidence:** Q1, then reconcile Q2/final ledger and full public-contract gates on final inputs.

For every row, record the implementing commit and smallest proving test, or a specific lead-approved
divergence with its rationale and regression. A prior architecture note saying "generation-based"
is not approval for all downstream timing/retry/partial-fault differences. #132 stays open while any
legacy-only row is open or while the reconciled code is not on local main. No real services,
credentials, external sign-ins, push or production actions are part of this work.
