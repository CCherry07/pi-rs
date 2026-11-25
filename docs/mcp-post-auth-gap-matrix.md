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

The oracle logs server `notifications/message` to a rotated file (`log.ts`); it does not expose a
live log viewer in the MCP manager. File logging and connection/stderr status are the parity work.
An interactive live log viewer would be an additional product feature.

## Confirmed open observable contracts

| ID | Oracle contract and source | Rust baseline / owning seam | Required evidence and delivery |
| --- | --- | --- | --- |
| M1 | `L/src/extensions/mcp/cli.ts:add`: `--oauth-client-id`, `--oauth-client-secret`, `--oauth-callback-port`, `--oauth-client-name`, `--oauth-client-registration`; HTTP-only, validated before save | `apps/pi-cli/src/mcp_commands.rs` omits these flags; `McpServerDefinition` cannot author OAuth even though config supports it | **OPEN**. Typed library authoring + CLI parser tests, round-trip unresolved secrets, invalid/misplaced flags leave disk unchanged |
| M2 | `index.ts:showTools`: full offered-tool inventory, description and differing effective exposure, including tools hidden by policy | `plugin.rs:manage` shows only aggregate counts; inspection can already inventory hidden tools | **OPEN**. Read-only semantic view; cancellation and no disk/generation mutation; preserve remote names separately from normalized tool names |
| M3 | `index.ts:serverMenu/saveConfig`: Enable/Disable in this project for a global server, with explicit trusted-project override ownership | Existing overrides merge correctly; manager cannot create this project-only override | **OPEN**. Trusted project-only write, global bytes unchanged, inherited credentials untouched; reload failure retains old generation |
| M4 | `index.ts:pickServer` and command handler: omitted login/logout/reconnect name auto-selects the only eligible/preferred server, otherwise selector; contextual actions, completion, returning/back/cancel | Rust commands require names and offer a fixed action list, including ineligible sign-in actions | **OPEN**. Eligibility/state-dependent choices, non-UI behavior, no duplicate side effects on cancellation, retired-context handoff |
| M5 | `cli.ts:list`: optional `authProvider`, always-present `errors` array, raw remote tool names with differing `toolExposure`, actionable names/provider guidance | `McpServerReport` has no `authProvider`; CLI already emits `errors: []` and differing tool exposures, which must be preserved | **OPEN**. Exact stable JSON-shape and needs-auth output regressions; preserve useful Rust-only inspection APIs |
| M6 | `cli.ts:runMcpCommand/editDistance`: unknown login/logout name suggests a nearby configured name, lists configured names and ignored-project guidance | Rust error lacks the same discovery assistance | **OPEN**. Deterministic suggestion/unknown/untrusted-scope cases; never echo credentials |
| D1 | `L/src/extensions/mcp/log.ts`: timestamp/server/level/logger, multiline indentation, append per message, rotate above 5 MiB to `mcp.log.1`, best-effort failures | `client_handler.rs` handles roots/progress only; no server log sink | **OPEN**. Notification-to-private-file fixture, rotation and unavailable sink, concurrent writers; logging cannot break requests or write outside renderer to terminal |
| D2 | `runtime.ts:connectOnce/handleClientClose`: drain stdio stderr; last 2,000 characters explain initialization failure or disconnect | `client.rs` sends stderr to null | **OPEN**. Bounded drain that cannot deadlock the child, redacted diagnostic tail, failure/exit cleanup and no terminal pollution |
| C1 | `runtime.ts:createDefaultTransport/oauthSettings` + `L/src/core/resolve-config-value.ts`: uncached `!command` for env/header/client-secret values, 10-second bound, trim/nonempty success; parse/save do not execute | `config.rs:expand_with` rejects command-backed strings; transport-only APIs are explicit input and must not gain implicit shell execution | **OPEN**. Explicit local-config resolution capability after trust; no execution for untrusted/disabled entries, static parse/authoring/inspection metadata; cancellation, output limits, redacted failure, refresh/reconnect rereads |
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
| Q2 | Published SDK/harness docs describe shipped behavior | `packages/pi/docs/mcp.md` still says OAuth/provider auth unsupported and documents pre-management APIs; release copies it as shipped documentation | **OPEN**. Sync authored docs deliberately, verify links and release input; docs alone do not require rerunning unchanged Rust gates |

## Newly identified auth-boundary carryovers (not silently waived)

These are observable deltas found by the post-auth comparison, not a withdrawal of the already-green
#131 implementation evidence. They must be fixed in an appropriate tranche or individually accepted
before claiming full parity.

| ID | Oracle | Rust baseline | Required evidence / disposition |
| --- | --- | --- | --- |
| A1 | `cli.ts:login/waitForRedirectUrl` allows non-TTY browser callback login; only terminal paste is disabled; already-authorized probe also works without TTY | CLI rejects nonterminal input before probing | **OPEN**. Piped-stdin local fake-browser callback and already-authorized no-browser tests, bounded cleanup |
| A2 | `cli.ts` accepts positive finite fractional `--timeout` seconds | Rust restricts integers to 1–86400 | **OPEN**. Fractional timer/invalid numeric parser tests or approved documented limit |
| A3 | Official auth cases include step-up, retry-limit and CIMD behavior under several negotiated protocols | Existing deterministic tests are narrower than the official baseline | **OPEN via Q1**, not presumed equivalent or failing until measured |

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
