# MCP post-auth parity ledger (#132)

## Frozen comparison

- Rust baseline: `4d17e8f0a222cc1a61ab1acfbbf82af47979e312` (local `main`).
- Behavior oracle: `legacy/pi@0ab23c37b074252d04e31f5cfb99491f0613fb80`, verified clean.
- Owner: @Shino, continuing #132 from clean `f176fc33c67d2acadcd0bf6e1d4bd9848b199561` after
  @Jinah's completed local delivery; no second implementation. The frozen comparison stays unchanged.
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
| D1 | `L/src/extensions/mcp/log.ts`: timestamp/server/level/logger, multiline indentation, append per message, rotate above 5 MiB to `mcp.log.1`, best-effort failures | `client_handler.rs` handles roots/progress only; no server log sink | **IMPLEMENTED/VALIDATED `7e12ad10`; local landing held**. Private notification sink, 5 MiB rotation, exact format, redaction, unavailable/busy sink and concurrent-writer fixtures. See diagnostics tranche below |
| D2 | `runtime.ts:connectOnce/handleClientClose`: drain stdio stderr; last 2,000 characters explain initialization failure or disconnect | `client.rs` sends stderr to null | **IMPLEMENTED/VALIDATED `7e12ad10`; local landing held**. Bounded draining/redacting tail on handshake/discovery failure and disconnect, manager status, drain ownership/cancellation and no terminal or log-file stderr writes. See diagnostics tranche below |
| C1 | `runtime.ts:createDefaultTransport/oauthSettings` + `L/src/core/resolve-config-value.ts`: uncached `!command` for env/header/client-secret values, 10-second bound, trim/nonempty success; parse/save do not execute | `config.rs:expand_with` treats command-backed strings as literal values instead of executing them; transport-only APIs are explicit input and must not gain implicit shell execution | **DELIVERED `f176fc33c`**. Explicit local-config capability after trust, bounded `pi-shell` capture, redacted failures and private rmcp late-refresh coordination; valid tokens never resolve helpers. See tranche evidence below |
| C2 | Same resolver: embedded `$VAR`/`${VAR}`, `$$` and `$!` escaping, missing/empty env rejection; command/args only home expansion in legacy | Rust supports `${env:VAR}` and whole `$VAR`, expands additional transport fields; exact template behavior differs | **DELIVERED `f176fc33c`**. Table-driven Pi syntax tests preserve documented Rust extensions; command/args/URL never execute value commands |
| L1 | `index.ts:session_start/startConnection`: config and independent connection failures retain healthy servers plus one startup diagnostic | Product prepare tolerates typed needs-auth only; unrelated config/connection errors reject the whole generation | **IMPLEMENTED/VALIDATED for supervised Coding; lifecycle acceptance OPEN**. Healthy partial publication and management, with global invariant/collision rollback. See lifecycle/selection checkpoint evidence below |
| L2 | `index.ts:before_agent_start/tool_call`, `agent-session-mcp.test.ts:768–922`: background connection; first prompt waits only for direct servers, bounded at 10 seconds; scripts/search/resources wait only for relevant servers; pending directory summaries | Rust waits for all connections before publication. Registry immutability is intentional, but the resulting observable latency is not thereby approved as equivalent | **APPROVED TIMING DIVERGENCE; IMPLEMENTED/VALIDATED locally, lifecycle acceptance OPEN**. Direct-only bounded startup and next-idle whole-generation publication, not same-turn selective waits. See decision and checkpoint evidence below |
| L3 | `runtime.ts:open`: HTTP transient initialization retries after 250/1000 ms (408/429/5xx except 501/network), abort stops backoff; no stdio blanket retry | Rust has protocol-handshake fallback, not this retry policy | **IMPLEMENTED/VALIDATED locally; landing held**. Typed classification and complete-setup retries; seven product regressions prove attempts/backoff/cancellation and no auth/permanent/stdio retries. See the explicit workspace baseline exception below |
| L4 | `runtime.ts:request`: one transient HTTP retry for resource reads/listings, never tool calls; expired-session 404 allows one request retry on a new session without cancelling other in-flight requests | No product-owned retry/session-expiry contract over rmcp has been established | **IMPLEMENTED/VALIDATED locally; lifecycle acceptance OPEN**. One logical-operation recovery allowance, resource-only transient retry, session-expiry replacement, full-pagination restart, no ambiguous tool replay, and detached admitted work are covered by deterministic HTTP fixtures. See the recovery checkpoint below |
| L5 | `runtime.ts:getClient/handleClientClose/reconnect`: disconnected state, lazy reconnect on next call, coalesced connect, explicit reconnect and shutdown lifetime | Published `McpToolSet` holds fixed services; manual reconnect currently probes then reloads | **IMPLEMENTED/VALIDATED locally; lifecycle acceptance OPEN**. Exact connection slots coalesce lazy replacement behind frozen grants; descriptor changes refuse dispatch until whole-generation publication; explicit reconnect, cancellation, retirement, shutdown, cleanup, parallel waiters and failed replacement have deterministic coverage. See the recovery checkpoint below |
| L6 | `runtime.ts:refreshTools` + `index.ts:registerTools`: tools/list_changed adds/updates/retracts tools, retaining old inventory on failed refresh | Client handler does not observe list-changed notifications | **IMPLEMENTED/VALIDATED for supervised Coding; lifecycle acceptance OPEN**. Coalesced stable/modern refresh, whole-generation schema publication, frozen active views and retained inventory on error. See checkpoint evidence below |
| E1 | `L/src/core/extensions/{types,loader}.ts`, `core/mcp-servers.ts`, `index.ts:registeredServers`: register/get/unregister, owner-only removal, same-owner replacement, namespace collision rejection, config precedence (including normalized names) | Loading-time JS registration crosses the strict generation manifest into the native MCP generation; late mutations use the E2 transition | **IMPLEMENTED/VALIDATED locally; lifecycle acceptance OPEN**. The JS registry enforces ownership, same-owner replacement, normalized namespace collisions, defensive cloning and validation. Rust revalidates the manifest owner/config, preserves extension source, applies configured-file precedence with diagnostics, connects tools, and retires removed connections on reload |
| E2 | `index.ts:mcp_servers_change`: late extension registration/replacement/removal, old work invalidation/cleanup, session-only manager edits, report registrations without a connector | Late register/unregister now stage generation-external source-owned mutations and request whole-session replacement; observers receive the activated replacement snapshot. Connector-less reporting is not applicable because the native MCP component is always installed in JavaScript-hosted product generations | **PARTIALLY IMPLEMENTED/VALIDATED locally; formal lifecycle acceptance OPEN**. Ordered mutations replay after extension loading only while the source path remains resolved, pass the same Rust manifest/config validation, never persist to `mcp.json` or Pi v4 history, commit only on activation, survive ordinary reload in the same live runtime scope, stay isolated from sibling sessions, and leave mutations arriving mid-preparation for the next reload. Node notification/replay, candidate rejection/filtering/redaction, scope cleanup/isolation, failed-candidate preservation, automatic register/remove publication and complete-snapshot `mcp_servers_change` delivery have deterministic tests. Observer delivery intentionally runs after activation in the replacement generation, rather than immediately in the retiring generation. Unlike legacy's replaceable JavaScript MCP connector, an absent observer cannot strand registrations: generation assembly installs a lifecycle-only native MCP candidate whenever the JavaScript host exists, and the configured-MCP-disabled extension fixture proves that registrations connect without a handler. Formal concurrent restart/close acceptance remains open |
| E3 | `L/src/cli/args.ts` and built-in extension loading: `--no-mcp`; `--no-extensions` disables built-ins as well as discovered extensions while explicit extension paths remain valid | No `--no-mcp`; Rust `--no-extensions` currently only gates extension discovery, not MCP | **IMPLEMENTED/VALIDATED at CLI/configuration-generation seams; wider CLI acceptance OPEN**. `--no-mcp` disables configured MCP only; `--no-extensions` disables discovery and built-in MCP; explicit `-e` remains independent; ACP retains transient isolation. Parser/Node argument splitting and disabled-generation fixtures prove no configured connect/helper or `/mcp` command |
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

## C1/C2 delivery and evidence

Local commit `f176fc33c67d2acadcd0bf6e1d4bd9848b199561` delivers C1/C2; takeover verified that
commit and a clean index/worktree. No source work is recreated and nothing was pushed. The tranche adds the approved in-memory `pi-shell::capture` seam, local-library
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
C1 and C2 are delivered; the diagnostics tranche below delivers D1/D2. #132 remains open, with
L1–L6, E1–E3 and Q1/A3 still outstanding. Nothing in this section waives a frozen row.

## D1/D2 diagnostics implementation and evidence

Validated own-branch base: `7e12ad101e31e043a5531825aad58aba399eec59`. The managed commit tool
injected agent metadata despite receiving only the business title. @Suzuya has explicitly held all
further local landing/push until one metadata-only cleanup of the frozen local MCP series; retain
identical trees/author/time and old-to-new commit mappings. No individual history rewrite is allowed.
Diagnostics are implemented and validated, but local-main acceptance remains pending that cleanup.

This continuation starts from clean local `f176fc33c`, not a second implementation. The owning
modules are private `diagnostics.rs` / `diagnostics/stderr.rs`, `client_handler.rs`, and existing
client/library/manager adapters. The exact oracle is still `legacy/pi@0ab23c37`: `log.ts` owns the
message format/rotation, and `runtime.ts:connectOnce/handleClientClose` owns the stderr failure tail.

- Local-library connections (including inspection/test/login probes) supply the log path explicitly.
  Transport-only/ACP clients never discover it. Messages append privately, rotate above 5 MiB, and
  retain multiline indentation. A nonblocking cross-process lock treats contention as best-effort
  failure. Local writers serialize rotation and append; explicit unlock prevents concurrent child
  forks retaining an advisory lock after the writer closes its descriptor.
- Stderr drains continuously, retains 8 KiB of already-redacted bytes plus an incomplete known-secret
  match, and renders at most 2,000 UTF-16 units. It is never copied to `mcp.log` or printed directly.
  Configured values and inherited credential-like environment values are redacted before truncation;
  this is not an arbitrary-secret detector, so logs remain private server output. rmcp keeps child
  ownership. Drain tasks abort on owner/cancelled-wait drop, with bounded final EOF grace.
- Disconnected status and errors do not replace services, retry calls or mutate frozen tools. L3–L6
  stay open. D1/D2 are conformance work with explicit Rust privacy/bounding protections, not approval
  for a live log viewer or same-turn lifecycle publication.
- Focused run `64f06d0` passed the initial nine diagnostics unit tests plus existing config diagnostic
  coverage and five local stdio integration cases. Package run `e5641e16` exposed a concurrent-writer
  loss during child spawning; the explicit-unlock fix addresses inherited file-lock lifetime rather
  than weakening the assertion. Expanded package run `ddb176f8` passed all MCP tests before
  finding a new test-only Clippy lint; after its repair, targeted Clippy passed in `9b8d47b9`.
  Package typecheck and all 14 release tests passed in `a5e717be`. A first full gate `41253e7d`
  found the CLI's missing `Disconnected` exhaustive match; that match was added.
- Final-input workspace/all-target Clippy with warnings denied, formatting and nonpaged diff checks
  passed in `5b201c54-5fa3-4640-af24-fad5dbf290b8`. **`cargo test --workspace` was not fully green**:
  run `3e96727d` aborted in the unchanged
  `codemode::depth_limit_store_values_survive_selected_branch_reload_and_disk_resume` test.
  Focused reproduction `41ade74f` also overflowed. The historical note labels the exact focused
  run `1e2aeabd-2ad8-42de-8309-70eb04359c1b` as `main@f176fc33c`, exit 101/SIGABRT. On the
  later evidence check, its existing receipt was inaccessible from this project (`This run belongs
  to another project`), and no paired original HEAD/index/worktree-state record was available
  here to substantiate the earlier clean-tree assertion. Thus prior runs recorded this same failure,
  but whether that historical tree was clean remains **UNVERIFIED**; these records do not establish
  a baseline/toolchain root cause or an MCP causal conclusion. No new reproduction, depth-contract
  or stack change was made for this provenance check. The lead
  permits local delivery with this explicit exception if affected tests and remaining gates pass.
  Run `4e0e339f`, excluding only that exact test, subsequently found a native-fixture fingerprint
  mismatch: a copied host `pi-plugin` build output still watched the main checkout's `Cargo.lock`,
  while the fixture correctly watched this worktree. Final run
  `98f23931-8ccc-4fc9-afba-863ae6245104` removed only this checkout's `pi-plugin` package build cache
  and **passed all remaining workspace tests**, including the native fixture and affected diagnostics
  tests, without weakening native compatibility checks or adding another skip. The local diagnostics
  commit is titled `feat: add private MCP server diagnostics`; nothing is pushed.
  This is not an unrestricted full-workspace pass.
- Added fixtures cover timestamp/level/logger formatting, rotation/private modes, concurrent complete
  records, unavailable/busy sink, chunk-boundary secret redaction, Unicode/invalid bytes, pipe-pressure
  draining, EOF grace/owner drop, notification-to-file routing, protocol startup/discovery failure,
  disconnect request/manager status, unchanged tool catalog, and disabled/untrusted/explicit-client
  no-log boundaries. Stdio fixtures are Unix-gated; no Windows, real service or official-suite claim.

## Lifecycle retry checkpoint evidence

L3 regressions in `tests/lifecycle_retry.rs` fail on the original implementation (`c6f699a0`):
HTTP retries/backoff are missing, and the broad handshake fallback also retries permanent failures
and failed stdio startup. Narrowing that fallback fixes the latter cases. Typed classification in
`client/retry.rs` initially could not classify status failures because pinned rmcp 3.3 flattened
ordinary non-success POST responses into a formatted string; `9e07746d` failed the three transient
cases. Valid JSON-RPC error bodies instead follow the upstream RPC path, also losing the HTTP status.
The frozen TypeScript client is the repository-local `@earendil-works/pi-mcp@1.6.0`. It exposes
HTTP status before decoding an error body but does not read HTTP `Retry-After`; the product uses
fixed 250/1000 ms setup delays and a 250 ms resource retry. The Rust patch preserves optional
`Retry-After` metadata without using it for scheduling. This is distinct from SSE `retry` fields
and makes no claim about the separate official TypeScript SDK.

@Suzuya approved a minimal, explicitly pinned compatibility patch after verifying that rmcp has no
public successful-response injection/decoding seam. Patch only the non-success response metadata;
keep successful bodies, SSE decoding, OAuth and session recovery unchanged. Do not parse error
strings, edit the Cargo cache, or add independent SDK retry policy. The source is vendored from the
checksum-verified locked `rmcp-3.3.0` crate; historical provenance, reviewed delta and removal
criterion remain in Git commit `f54ddeb649c93b82bcefdfe445d60f5c1ce022d0`. At that L3-only checkpoint, the approved supervisor and
L1/L2/L4–L6 were not yet implemented. L3 was implemented locally under the explicit validation
exception below; this historical checkpoint is not the current no-vendor delivery design.

The metadata patch now passes its red→green regression (`92f3d846` red, `cbbbd25d` SDK/product green),
13 focused SDK HTTP tests, all seven lifecycle product tests, and all 262 standalone SDK library tests
(`9d808103`). Tests cover typed status/Retry-After, valid/invalid/date values, bounded body omission,
unchanged JSON-RPC/SSE/redirect behavior, exact 250/1000 ms retries, auth/permanent/stdio exclusion,
cancellation, and anonymous/provider/stored-OAuth equivalence. The standalone SDK Clippy addition
reported 15 unchanged upstream findings (13 `result_large_err`, two `too_many_arguments`); a fresh
checksum-verified pristine archive reproduced the identical findings in `062bd85c`. No upstream
lint suppression or unrelated SDK edits were added; this is not a workspace-Clippy exception.

Package run `1bd852c3` passed all MCP tests, targeted all-target Clippy `-D warnings`, workspace fmt,
exact vendor-delta verification, and diff check. The narrower fallback exposed two existing malformed
legacy fixtures (`inventory.rs`, `tests/config_values.rs`): each now explicitly returns method-not-found
for `server/discover`, preserving its original no-tool/no-secret-execution assertions. Their owner
was unreachable through the supplied resume path/session search/message route. No production fallback
was broadened.

Final-input workspace run `fdd60a75-a1df-4ed5-8f01-0914d64aa335` passed strict workspace/all-target
Clippy, workspace formatting, exact vendor verification and diff check. Unfiltered
`cargo test --workspace --no-fail-fast` completed every other target but aborted Coding integration
in `codemode::depth_limit_store_values_survive_selected_branch_reload_and_disk_resume` with stack
overflow/SIGABRT—the same failure historically recorded for `1e2aeabd-2ad8-42de-8309-70eb04359c1b`;
that earlier run's clean-tree provenance is unverified as qualified above.
Supplemental run `e18a6801-7a2c-4182-8d82-637132767a23` passed all 73 remaining Coding integration
tests, excluding only that exact test. @Suzuya explicitly extended the existing baseline exception
to this L3-only local checkpoint. **Unfiltered workspace tests are not fully green.** No stack/depth
change, extra test skip, SDK lint suppression, Git rewrite, landing or push was performed.

The checksum verifier retains all published source bytes outside the two reviewed files. Git
whitespace exceptions cover only the original upstream fixture's trailing-space doc line and the
unified patch artifact's blank context lines; they do not disable checking for project code.
Checkpointing is authorized only through the managed tool; final whole-series metadata cleanup
and local landing remain held.

## Lifecycle integration tranche

L3's managed local checkpoint is `f54ddeb649c93b82bcefdfe445d60f5c1ce022d0`; its tree and both lockfiles
were verified after commit. The subsequent supervisor/selection checkpoint is authorized below;
it does not close lifecycle parity or the remaining L4/L5 contracts.
The transport seam now separates generation-owned `ConnectedServer` policy from a shareable
`McpConnection` plus raw inventory; synchronous cancellation remains available even while an old
view is retained. MCP package checks passed in `764f2173`. Configuration resolution now delegates
through a data-only `McpConnectionDefinition`, without moving exposure/timeout policy into background
work; config tests/strict targeted Clippy passed in `b6b06a0c` and formatting in `b76186f2`.

The initial `cfg(test)`-only supervisor has now been promoted into the supervised library/Coding
publication path. It owns transport-only inputs, configuration/attempt epochs,
immutable readiness snapshots, weak cache handles and abort-on-final-owner cleanup. Local `!command`
resolution runs inside the bounded attempt, not while inputs are created or identical candidates
are rebuilt. Nine owning-seam regressions passed in `942379e5`, along with strict MCP Clippy,
workspace formatting and diff check. They cover independent failures, bounded selective waits,
coalescing across cancelled waiters/rebuilds, changed trust/auth identity, removal/stale candidates,
stale completion rejection, retained-view cancellation, manual reconnect and uncached actual opens.
These component tests alone do not establish production publication, request-time lazy reconnect,
resource retry, list-change refresh or runtime-drop/Pi-v4 integration parity.

The approved local startup contract is concurrent starts with one total ten-second budget over
effective enabled direct servers (including tool overrides). Indirect-only generations do not wait;
partial pending/auth/error states preserve ready siblings. Publication remains whole-generation and
next-idle, never a tool-call-time registry mutation. The sole strong readiness lease is captured in
`GenerationFreshness`; executable views and a runtime-scoped factory cache retain weak handles.

The generic non-wire/non-persisted `LiveRuntimeScopeId` seam preserves identity for Reload/Refresh
and stages fresh identity for initial/New/Resume/Fork/Branch. Seven scope and thirteen replacement
regressions passed in `027e7133-7b9c-414b-8f6b-8b72dfefab3b`. Package tests and Clippy completed
successfully in `ac5044d6-9fde-40cc-bd66-9679fbd4be63`, whose enclosing process was then stopped at
Git's pager; it is not recorded as exit-zero evidence. Non-paged format/diff checks passed in
`51d9b5df-bbe3-4048-9d85-ecd1b2b1950e`.

Supervised preparation/lifecycle/status/prompt wiring has focused evidence: 139 MCP library and
14 selected integration tests passed before a now-repaired local Clippy finding in `7e8e148a`;
14 final supervised regressions, strict all-target MCP Clippy, format and diff passed in
`2cc37b34-733e-4380-9aa0-10d84282762e`. Sixteen supervisor/input/lease tests plus `cargo check -p
pi-coding` and diff check passed in `cc88681d-4458-4988-aa88-07839d174df4`.

Cross-layer validation is still in progress. Normal Coding test builds `859687bd` and `87160063`
failed because the selected pi-session artifact lacked the new API despite unconditional exports
in source and the successful check build. A scoped `cargo clean -p pi-session` repaired that artifact:
initial six product lifecycle tests passed in `b5e79a9c-b336-4028-941f-8666a369a248`. Coding credential
unit tests, strict library Clippy and diff checks passed in `9b1ca8ce-8bf4-4149-8a79-2a785d9d1c63`.
No source/ABI checks, stack settings or test expectations were weakened for the rebuild.

A distinct transaction gap was reproduced and repaired. The initial nine-test product run
`01833278-12b6-4731-86ea-165a124ece26` passed eight tests but failed
`mcp_lifecycle::changed_connection_candidate_failure_keeps_the_original_transport_usable`:
changing the server URL before a later duplicate-plugin rejection closed the old current tool's
connection. Unchanged-input lease survival is not sufficient rollback evidence. The regression also
requires candidate-exclusive cleanup and reuse after reverting the config. Both new timing cases
passed: real-product mid-turn freeze/eventual idle admission and a public-library revision-observed
ready-before-first-idle handoff. The latter's additional second-admission no-extra-refresh assertion,
integration-target Clippy, format and diff checks passed in
`205ac1b7-b2df-49b6-afdd-3d4219e41d64`. Those two timing seams are deliberately distinguished; a
server response being sent is not proof that private supervisor readiness has already committed.

The fix pins exact raw-slot snapshots in the opaque freshness source, uses a weak published-slot map
for guarded rollback, and cleans up candidate-exclusive slots on unpublished-source abandonment.
Owning red `8683d0ae-6938-4f31-b734-afaeb01500ed` was followed by owning green `7316d56b`
(17 tests). After the explicit source-file handoff, check-only formatting, all 23 supervisor tests
(including six additional pinning/fencing cases) and scoped diff checks passed in
`b47dd153-4c71-4a1a-9ddd-2e0f15633e59`. All nine unchanged product lifecycle tests passed in
`e8ac5d53-f672-443e-95e1-f9388390cd61`; its later Clippy suffix failed on a separate repaired
`let_and_return`. Final integration Clippy, formatting and diff checks passed independently in
`4af8d37f-4670-41eb-8497-a3b1bcd4f0a7`. Those initial runs did not establish full-workspace
acceptance; the final-input evidence and narrow disposition follow below.

## Selection and live inventory integration

The lifecycle tranche is still not a full-parity or workspace-green claim. Ordinary Reload now
intentionally reuses an equivalent transport and its last immutable tools/initialize metadata;
explicit reconnect or changed connection inputs obtain fresh initialize metadata. This divergence
was approved in `question-3cf6df2854ff4ffc819395ae3ee07b26`. Oracle anchors are
`core/agent-session.ts:3831` and MCP `index.ts:1250` (reload emits shutdown; MCP closes connections).
The existing transaction, direct-only shared startup budget and first-next-idle boundaries remain.

The initially migrated Coding suites produced 23 passes and one real pending-Resume selection loss,
not an obsolete assertion. The generic fix separates durable desired selection from effective
registered tools using existing Pi v4 records. Exact immutable classifications are `Denied`,
`TemporarilyUnavailable`, and `UnresolvedUntilInventory`; explicit map entries override convenience
allowances. Historical header/tree/lane/recovery selections are read-only candidates, not current
intent. Explicit deselection wins, including an earlier startup hook's clear. Coding provides a weak
live session reader and filters candidates through the current overlay ceiling; MCP resolves actual
qualified inventory, never guessed reverse-normalized identities. Ambiguous ready aliases are
quarantined after naming without changing their peers or raw inventory. Approvals are
`question-4ea19c615a184d0fbf0d833781af4f1c` and
`question-4905080d132e496095777b806930ea49`.

Selection evidence:

- Generic classification red `354d1110` (3 fail / 9 pass) became 12 focused passes in `762f4535`.
  Full `pi-session` tests (224 unit tests plus integration/doctests), strict all-target Clippy and
  scoped hygiene passed in `2d1df609-8fb3-4a86-8f15-afc18ed629b8`.
- MCP classification/quarantine/startup: 15 selection regressions and four `startup_` matches,
  including eight live-reader scenarios; check, strict all-target Clippy and scoped hygiene passed
  in `e96fb0e6`. No fallback to a stale static desired snapshot is permitted.
- Coding's expanded lifecycle/selection suite passed all 18 cases in
  `08ec3b19-6956-4cd0-9a7d-01216b80154a`, including passive capture/quit/reopen, explicit deselection,
  historical checkout, policy/grant revocation, failed-candidate rollback, mixed exposure, hashed
  and truncated names, normalization collision, overlapping namespaces, completed no-match and
  the real bounded setup timeout. Tool-call counts stay zero and withheld names never enter provider
  declarations. The timeout case observes production setup termination, not a test-only deadline.

L6 now refreshes through the existing transport with stable callbacks and native modern SDK
subscriptions established before initial listing. Weak-slot workers coalesce bursts and fence by
attempt plus raw connection identity. Full descriptor changes or meaningful error/recovery transitions
advance readiness; reordered/equivalent tools and repeated equivalent failures do not. Refresh failure
retains the last valid inventory and bounded connected-with-refresh-error status. Initialize-only
instructions never change on notification.

The initial `6cc1ddde` red was invalid: its fixture incorrectly answered modern discovery with stable
negotiation. Corrected red `cfab0169-bbd2-428f-b3ea-fc50e78fc83c` reached initialize/list once and
failed specifically to refresh when handler delivery was disabled. Deadline red `c4412356` established
that timing out the SDK convenience pager does not cancel its pending request. The refresh-only pager
therefore uses existing typed SDK cancellable requests, one 30-second data deadline across pages,
repeated-cursor rejection, explicit cancellation and at most three seconds of control cleanup. It
adds no protocol/auth/retry implementation or vendor change. Abort cleanup retains no source/slot.
A genuine update of a still-published historical slot can conservatively cause one extra idle rebuild;
its source drain stops that worker and does not itself stale the next baseline.

L6 evidence:

- Final MCP package tests passed in `af135b22`: 181 library and 36 integration tests, two live tests
  ignored. `f068dc1d` passed three snapshot tests, 39 supervisor tests (including 18 refresh cases),
  and strict all-target MCP Clippy. Its trailing Git pager was stopped; scoped formatting and
  nonpaged diff checks then independently exited zero in `0cad4a07`.
- Real-notification public-library/session integration passed in
  `9920bc27-ab06-4607-98bc-bb79078e0424`; integration-target Clippy/scoped hygiene passed in
  `b2334e7a-7bd3-4fbb-8ac6-ec2b9a279057`. It observes the real readiness commit and proves the first
  next-idle submission publishes once while the second does not; active/retained generations stay
  frozen across addition/removal/schema changes. Counts: initialize 1, helper 1, tools/list 2,
  provider requests 3, tool executions 0. Initialize-only instructions remain unchanged. This is
  explicitly public-library/session evidence, not private ProductSessionFactory revision observation.

Integrated affected-package run `fc415dfd-5ca6-4149-87f8-3dea13d12b25` exited zero: 116 Coding
library tests, all 44 Coding `mcp_` integration cases (including the original strict pending-Resume
selection assertion), seven CLI management tests, affected-package all-target Clippy with warnings
denied, workspace fmt, vendor verification, staged/unstaged whitespace checks, package typecheck and
all 14 release tests. The unchanged MCP and generic-session owning-package results above remain valid.

Final-input unfiltered workspace run `411b864b-3b5e-4ce6-a072-73b2d3c04e78` completed every target
with `--no-fail-fast`, with `WORKSPACE_GATE_STATUS tests=101 clippy=0 fmt=0 patch=0 diff=0`.
The sole failed target was Coding integration, aborting in the unchanged
`codemode::depth_limit_store_values_survive_selected_branch_reload_and_disk_resume` with stack
overflow/SIGABRT. No other target failed; strict workspace Clippy, formatting, exact vendor patch
verification and staged/unstaged diff checks passed independently. This is the previously recorded
failure (`1e2aeabd-2ad8-42de-8309-70eb04359c1b`), not an unfiltered workspace pass or renewed proof
of that earlier run's clean-tree provenance.
Supplemental `3b671dd2-6d14-4b98-a32e-c61adb321777` passed all 92 other Coding integration cases,
filtering exactly that one test; `RUST_MIN_STACK` was unset. The codemode-file diff changes only the
MCP readiness fixture, not the depth-512 test, its contract or the stack environment.

`question-8a61f7c6761c4383877711549b140924` explicitly authorizes a managed local checkpoint of
only released L1/L2/L6 and selection-intent work, retaining exactly the independently reproduced
depth-512 exception above. The first managed commit refused atomically because source handoff does
not transfer line-commit ownership. `question-2f774bbd23ee44458ba11e762edf7c83` therefore authorizes
sequential commits by the original owners followed by the final owned contribution, without source
edits, formatting or manual staging between them. These intermediate commits are ownership checkpoints,
not independently validated builds. The final combined tree must match the frozen 69-path blob manifest
exactly with clean index/worktree; every included helper file was released before final validation.
No stack setting, depth contract, test assertion or unrelated test was weakened. This is not a fully
green unfiltered workspace. No land, push, amend, rebase or metadata rewrite is authorized; any
managed-tool trailers remain for the planned identical-tree whole-series cleanup.

The completed local ownership sequence is `acbad60`, `c2e088d4`, `0867b64`, `5ce96a60`, then
`bd6646a99ab3a6a670925032e48b9b16318090a3`, all after `f54ddeb`. The final tree is
`2f9160b9794c79717b0e608621400322c6e461a2`: its net change contains exactly the frozen 69 paths,
with every recorded blob and mode matching. The managed tool attributed the entire newly created,
jointly written `library/supervised.rs` to @Stroheim because default ownership could not split it;
`question-e50de5628c12454ab783d5547463fcd3` approved retaining that disclosed attribution, not a
claim of narrower authorship. The last 45 test lines went through their original owner's managed
commit, not a manual staging workaround.

Managed commits preserved 18 stale index snapshots. After proving that HEAD matched every frozen
blob, `git diff HEAD` was empty, and all owners/processes were idle, the narrowly authorized
`question-4d54e174272a41b6935f9cc5900a7dc4` repair ran exactly `git reset --keep HEAD`.
Before/after receipts in ignored `target/evidence/mcp-lifecycle-checkpoint/` then verified unchanged
HEAD, branch and all refs, all 1,766 tracked-file SHA-256 values, all 1,262 then-existing evidence-file
hashes, an index identical to HEAD and clean status. Those raw receipts were subsequently lost in
the storage incident below; their historical verification is not a claim of current availability. No source/history movement, land, push or
metadata cleanup occurred. The combined-tree evidence above therefore remains applicable to this
checkpoint, with the same explicit depth-512 exception; it does not validate each intermediate commit.

L4/L5 request lifetime, E1–E3 and Q1/A3 remain open; these local regressions are not official-conformance
evidence. Neither this checkpoint nor its narrow exception marks #132 or lifecycle parity complete.

## Official-suite preparation: verified artifacts, execution blocked

Preparation alone was authorized by `question-b4e4afe9b63d4df8b41d8ebc1683395d`. The exact public
`@modelcontextprotocol/conformance@0.2.0-alpha.11` artifact and its runtime closure were preserved
during the completed audit under ignored `target/evidence/mcp-conformance-0.2.0-alpha.11/`;
the subsequent storage incident below made that evidence directory unavailable. No repository
dependency manifest or lock changed. The root archive SHA-256 is
`67d28b0d50d64458232945d9b3af75178add5d05819c748ec2c8b26e5cb038c5`.
The isolated Node 22.23.2/npm 10.9.8 resolution locked 119 package instances from 114 unique public
registry tarballs, all independently SRI/SHA-256 verified. Its lock SHA-256 is
`209cbd57a441ed5ab28e20c5760804272784fe2619630ea899b42b7e88ba8ede`. This records the current
caret-range resolution, not an assertion that the historical oracle used that dependency tree.

Source/publishing hooks in 37 packages remained inert. No preinstall/install/postinstall hook,
native-addon artifact or implicit native-build requirement was found. The clarified authorization
`question-44e93cbeb3364d4b9a83d23dabab924d` nevertheless required every advertised runtime
export/main/bin and import-graph artifact to exist before scriptless installation. The verified
`@modelcontextprotocol/sdk@1.32.2` tarball advertises but lacks both `dist/esm/index.js` and
`dist/cjs/index.js` root exports. The actual suite imports present SDK subpaths; that does not waive
the stricter prerequisite. Installation stopped without building/fabricating files or silently
selecting another version. No installed dependency tree or offline reinstall result is claimed.

A bounded static audit recorded 687 modules and 1,973 edges, with an optional `debug` import of
`supports-color` and computed Express template loading explicitly unresolved. Its literal traversal
is not proof of runtime containment. The actual local OS toy probe permits IPv4/IPv6 loopback and
denies outbound TCP to reserved documentation addresses with EPERM, but unsandboxed own-host
non-loopback IPv4/IPv6 clients exchanged payloads with wildcard and explicit-interface listeners,
including ordinary exec children. Thus the tested macOS sandbox **fails the required inbound
exposure boundary**. A tighter numeric-loopback profile was rejected by `sandbox-exec`; no bypass
or remote-host reachability claim followed.

`preparation-status.md`, the integrity receipts, `sdk-missing-root-exports.json`, static graph and
`isolation-probes/` recorded these results before their loss in the storage incident below. No package/suite/adapter entrypoint, lifecycle hook,
validator, official list command or scenario ran. There was no `pi.dev` request, real credential use,
sudo, firewall/global-setting change or suite/baseline patch. Draft `auth/offline-access-scope`
remains excluded. Installation needs a separately reviewed artifact/requirement decision; execution
also needs an approved boundary that contains non-loopback listeners and subsequent explicit run
authorization. **Q1/A3 remain unmeasured and open.**

### Subsequent validation-storage incident

On 2026-11-26, owning Cargo runs `e3151f77`, `6e090d22` and `9524b7a2` failed before test execution
because incremental/dependency object or metadata files disappeared. These are infrastructure
failures, not product test failures or successful reruns. At 03:05 UTC the coordinator independently
confirmed that `target/evidence/`, including both the lifecycle checkpoint receipts and official-suite
preparation directories, was absent; `target/debug` had meanwhile been recreated. No removal owner
was established. The active helpers reported no cleanup, target relocation or environment workaround.

The original `target/evidence` receipts and official-preparation bundle are gone. Only historical
verified tool/process summaries remain; **no raw bundle is currently restored**. HEAD still identified
`bd6646a`; no checkpoint reset was repeated, no missing receipts were fabricated, and no official
artifacts were reacquired. Cleanup ownership remains unknown, not attributed to an agent or daemon.

New evidence belongs under the already-ignored, non-Cargo root `.eval/mcp-evidence/<run-id>/`.
Post-loss observations and any later explicitly regenerated material must be labeled as such, never
as the missing originals. A guarded restart requires a quiet interval, unchanged target inode/birth
and disposable sentinel, then one smallest focused Cargo check. Subsequent Cargo runs are serialized
under one build owner, with target/sentinel identity captured before and after each run and evidence
manifests/hashes archived outside `target/`. No `cargo clean`, target-directory/environment workaround,
or concurrent helper build is authorized. Any renewed artifact loss or target/sentinel change returns
immediately to hold rather than repeated rebuilding. This incident waives neither behavioral
assertions nor the independently blocked official acquisition/execution boundary.

The guarded restart subsequently passed a 781.82-second copy-command-inclusive quiet interval
(after a separate 461.66-second observation). Target device/inode/birth, the sentinel and all 30,448
metadata entries stayed unchanged; ordinary resident macOS storage daemons were recorded separately,
without attribution. The sole smallest check, `cargo check -p pi-plugin-mcp` (`068da84e`), passed in
10.32 seconds with the same target identity and sentinel afterward. The serialized post-box auth
run `2cb24e9c` then passed all 18 `auth::refresh` cases and strict MCP all-target Clippy, with unchanged
source hashes and stable post-run identity/sentinel. These are new post-loss results, archived with
input/output/guard hashes under `.eval/mcp-evidence/target-guard-20261126T033557Z/`, not regenerated
original receipts. Expected build-tree additions do not mean that the quiet-period metadata hash
remained unchanged during Cargo. At that guarded restart, the full runtime recovery matrix and
HTTP/RPC opt-in had not yet run; this guarded resumption was not L4/L5 acceptance.

## Superseding no-vendor source migration and bounded validation window

Decision #173 requires unmodified crates.io rmcp 3.3.0, not the vendored SDK or its later
`PreserveHttpErrorStatus` prototype. Decision #188 releases only the exact scoped batch in
[the validation-window proposal](mcp-stock-validation-window.md). Its dependency transition removes
the path patch/exclude, vendor tree/nested lock/provenance, obsolete checker and attributes together,
and restores only the registry source/checksum in the root lock. Preimages and index state are
preserved outside the fresh batch target. Registry inputs are restored; compilation and validation
remain pending. Executable source stays frozen. No source repair, persistence implementation,
acquisition, official/prototype execution, target cleanup, commit, landing or push is released.

The public generic client seam supports `PiHttpClientError` through `StreamableHttpError::Client`;
there is no need to disguise ordinary HTTP failures as I/O errors. The approved source design gives
ordinary application/setup HTTP paths status-before-RPC handling, finite safe diagnostics with
private classification/challenge getters, a 16 MiB JSON success-body cap and request-versus-control
202/204 semantics. Method/protocol and nonempty owned session/bearer headers follow frozen precedence. Fixed setup/resource delays stay at
their existing owners; HTTP `Retry-After` is not consumed or retained by the new adapter. Only typed
Discover 400/404 with no owned or effective caller session header gets one fresh Initialize fallback;
this is Rust compatibility, not frozen Pi behavior. No general retry classifier is broadened.

The approved single private OAuth reaction coordinator preserves SDK AuthorizationManager/store
and existing Explicit versus supervised session-recovery ownership. Plain 403 has no callback;
insufficient-scope 401/403 observes a challenge and requires sign-in without refresh/replay. Other
401 may refresh once and replay after success even with unchanged token text. Proactive failure
falls back to the current stored token; static/token-provider authentication never reactive-refreshes.
Explicit/ACP uses a bounded local allowance; supervised application POST/resource requests use the
existing shared logical allowance and cancellation fences. Each transport-owned common or response
GET opening/resumption has its own at-most-one auth callback/replay, independent of the originating
application allowance. Recovery of a response stream must never reexecute its original RPC. This
approved contract correction does not change application POST/resource policy. No nested SDK
AuthClient retry loop is allowed. Frozen transport
`streamable-http.ts:194–197,353–403` and `oauth/auth-provider.ts:78–94` establish these distinctions;
product auth-readiness projection additionally depends on the actually attached auth provider
(`extensions/mcp/runtime.ts:319–324`). The approved projection makes coordinator-issued sign-in/step-up
actionable, or raw 401 with an actual token/OAuth provider attached. Static Authorization bypasses
that provider, so static/anonymous raw 401 is an ordinary safe failure; raw 403 is terminal without
coordinator step-up. This retires the old Rust static-401 pending-auth behavior. Attachment comes
from immutable effective dispatch, never merely `config.auth`, token presence or error text.

A separately approved terminal-cleanup exception follows frozen `streamable-http.ts:259–260,326–340,
377–380`: after closing ordinary dispatch, a one-second best-effort DELETE reuses that transport's
latest acquired token, even if logout/new login has since superseded it. There is no provider/token
lookup, refresh/retry, response interpretation, private wrapper/backend logging of tokens, headers
or outcomes, persistence or CAS/journal mutation. The cache must never enter a replacement transport
or ordinary GET/POST/replay. This is not a last-sent authority latch or a session-capture workaround.
At this earlier stock boundary its owning source/tests were not yet validated. The historical
**SDK-CLEANUP-TRACE** finding was **OPEN** under the lead's qualified private-silence ruling:
private cleanup discarded outcomes, but the stock HTTP worker emitted an info trace containing
the diagnostic-sensitive session ID, whose visibility depended on the subscriber. Later #204
removed that stock worker from the product path. The lead has now accepted #219's existing source
and capture evidence under original #183: this row is **CLOSED only for d47bf1ef's inputs and
DELETE 204/401/500/unanswered scenarios**, as detailed below. This is not global SDK silence or
full cleanup parity, and authorizes no subscriber/filter, fake-error, SDK or lifecycle workaround.

The following retain the stock-boundary findings; later delivery notes explicitly update their
status. Items without such an update remain **OPEN**, not shared non-support or accepted differences:

- **JSON-ARRAYS:** frozen `streamable-http.ts:315` emits every array member; the public SDK JSON
  response holds one message. **CURRENT PARTIAL DELIVERY:** the private POST envelope and worker
  now handle application/setup JSON arrays directly, including ordered prefix dispatch, correlated
  reply/tail separation and the narrow #205 Ping boundary. Five real-loopback tests pass below,
  including exact SDK setup-error handling; the corrected full affected-input run is also recorded.
  Explicit session-repair arrays and remaining cancellation/tail races still need coverage; do not
  close this row solely from application/setup tests. No JSON-to-SSE or fabricated RPC bridge exists.
- **SESSION-CAPTURE:** frozen `:296–297,392–395,527–528` captures nonempty session headers after
  successful applicable POST/GET, including controls, not rejected responses. **CURRENT PARTIAL
  DELIVERY:** private Accepted and GET envelopes now retain successful nonempty session headers
  before reply/content validation for the existing transport metadata paths; public SDK types are
  unchanged. Ordinary notification/common-GET/follow-up/cleanup and two successive resume GETs have
  focused evidence below. Successful POST metadata is now observed before missing-reply,
  content-type, JSON-body and JSON-RPC protocol validation, while rejected responses cannot replace
  the active or cleanup ID; sequentially repeated nonempty IDs are stable, and delayed completion of
  an older body cannot overwrite a newer header capture. Ordinary POST and GET dispatch re-read the
  active ID after asynchronous credential waits. A resume GET combines that latest dispatch-time ID
  with the originating logical stream's protocol version and cursor. Stream-origin bootstrap Pong
  and response-tail controls instead retain their opening-stream authority: a pre-initialize Pong
  uses the opening session ID but intentionally sends no version before negotiation, and a tail
  keeps its opening negotiated version after unrelated global rotation. Deterministic tests cover
  those rules plus invalid-GET capture. Session-expiry, candidate/cleanup, response-tail cancellation
  and broader resume/control races remain open. The current MCP package and strict all-target Clippy
  pass; the earlier CLI fixture failure remains historical evidence below.
- **SSE-SYNTAX:** **IMPLEMENTED/VALIDATED locally.** The private incremental parser accepts CR,
  LF and CRLF across arbitrary chunks; strips the initial BOM; performs nonfatal streaming UTF-8
  replacement without losing split valid scalars; tolerates duplicate, unknown and bare fields;
  dispatches the unterminated final event at EOF; and publishes each complete `id`/valid `retry`
  line immediately so an open stream updates its resume cursor. Metadata-only items update the
  transport cursor but never enter JSON-RPC dispatch. Deterministic parser and live-transport tests
  cover those boundaries; official fixture conformance remains under Q1.
- **SSE-ACCOUNTING:** **IMPLEMENTED/VALIDATED locally.** The event limit counts decoded UTF-8 bytes
  from `data:` values plus joining newlines, not field prefixes or metadata, while the separate
  unfinished-line bound prevents unbounded buffering. Replacement expansion cannot bypass either
  cap. Complete prefix events are delivered before an oversized suffix, then the unread response
  source is dropped before the typed terminal error is exposed. Chunk-partition, exact-bound,
  malformed-UTF-8 and source-drop regressions pass; Q1 and wider workspace acceptance remain open.
- **SSE-PREFIX-DELIVERY:** fixed in the main-worktree follow-up below, with focused regression
  evidence. Whole-chunk validation previously discarded complete earlier events when a later event
  overflowed. The private adapter now forwards through each frame boundary before inspecting the
  suffix. At that prefix-delivery checkpoint, stock fatal-reconnect behavior and raw-byte accounting
  were unchanged. The later private-worker fatality evidence is recorded below. The frozen unfinished-line
  cap also has chunk-sensitive behavior, so universal partition invariance is not claimed.
- **SSE-FATAL-OVERFLOW:** raw bounds alone do not reproduce the SDK's private fatal-overflow marker.
  POST can reconnect after an event ID; common GET can reconnect without one. Repeated successful
  HTTP reopens followed by overflow can cycle immediately because the failed-open retry budget
  does not govern that cycle. No hidden latch, fake stream or second session stack is approved.
  **CURRENT DELIVERY: validated for the #204 private worker**, whose typed classification already
  terminates overflow in both common and response streams. The three real-loopback regressions in
  the supplemental evidence below prove task completion, prefix preservation, body release and
  independent/admitted success. This resolves fatal reconnect eligibility, not the separately open
  exact parser syntax or size-accounting rows.
- **SSE-RESUME-CONTROL:** the corrected contract permits an independent bounded auth reaction for
  each GET opening/resumption, as frozen `authorizedFetch` does. A source-derived isolated success
  can have POST2/GET2/token4/callback2/exchange2; these counts assume no cancellation, overlapping
  refresh, token change or reuse and exclude separately counted startup/common traffic. The earlier
  no-second-refresh expectation tested a Rust contract, not Pi conformance. Real-SDK source-only
  fixtures/cases were initially source-only. **CURRENT COVERAGE:** the actual-slot OAuth cancellation,
  reconnect/retirement, sibling/common preservation and shutdown cases are included in the package
  gates; the additional provider-wait cases below validate cancellation and generation retirement
  while keeping a newer view on the exact same connection. Frozen detached resumption lacks the
  original sender abort: this stronger Rust isolation is intentional, not claimed as frozen behavior.
  Deadline, remaining supersession/control-registration/tail-lifetime races and complete common-stream
  observation remain open. No request/event-ID map, task local, hidden latch, fake SSE, wire metadata,
  SDK change or disabled GET is approved.
- **SSE-RECONNECT-ELIGIBILITY:** frozen resumption/common reopens stop on exhausted 401, ordinary
  403 or sign-in/step-up errors, while genuinely transient callback/network/408/429/5xx failures may
  reopen. Stock retries every failed reopen without classifying its error; the current production
  backoff has no intrinsic attempt cap, although external cancellation/timeout/shutdown can stop it.
  Initial common GET fails before the auto-reconnector and is not the same case: frozen Pi retries
  an initial common 503, whereas stock returns that failure without creating its reconnect loop.
  This initial-transient loss needs a separate oracle-desired regression from established-stream
  retry eligibility. A finite-count test override must not mask the production policy; count-only
  retry configuration is not parity. **CURRENT DELIVERY:** the #204 private worker replaces this
  stock behavior, with the 13 desired opening/reopening/completion regressions first passing in
  `48b8bd74` and included in the later complete package gates; the historical failures remain below.
- **SSE-RESPONSE-COMPLETION:** stock drains the reconnecting response stream for up to 50 ms after
  forwarding a response; retry:0/fast EOF can initiate another GET. Frozen checks its answered flag
  before reopening after that stream ends. Source-only characterization/desired-red cases must not
  encode this stock divergence as desired behavior or hide it behind a held final stream.
  **CURRENT DELIVERY:** the #204 worker ends resumption eligibility at the correlated reply and
  drains only the existing tail. Its desired final-reply regression is included in those same
  13-case and complete-package gates; remaining cancellation-registration/tail-lifetime races are
  separate ownership acceptance work.
- **CALLER-HEADER-ORDER/FOLD:** frozen caller records retain source order and coalesce case variants
  before owned setters. Rust's JSON-to-BTreeMap conversion previously lost that order; neither
  last-wins nor rejecting case duplicates is an accepted parity disposition. **CURRENT DELIVERY:**
  local configuration now retains a private, debug-redacted ordered header list alongside the public
  transport shape, resolves every late value separately in source order, then comma-folds ASCII-case
  variants before transport validation and owned setters. The unresolved ordered list participates
  in the connection reuse fingerprint, so a semantically significant reorder replaces rather than
  reuses the old input; policy overrides retain the defining list. Explicit Rust `BTreeMap` callers
  also fold case variants in their representable map iteration order instead of being rejected.
  Desired-red coverage first observed duplicate rejection at both config and transport seams and a
  rejected local definition; current focused regressions pass for source-order command resolution,
  transport folding and reload identity. On the same dirty snapshot, the full MCP package passes
  **335 library + 42 integration tests / 2 live ignored**, Coding's MCP filter passes **47**, CLI MCP
  management passes **7**, and strict MCP all-target Clippy plus workspace formatting pass. This is
  implementation evidence for this row, not a bare-commit, full-workspace, Node/PTY or official
  conformance result; the wider HTTP/SSE and authentication rows remain open.

Source review also found auth-specific blockers distinct from those SDK/parser limits. The frozen
product credential store is `legacy/pi/packages/coding-agent/src/extensions/mcp/oauth.ts`, backed by
`core/auth-storage.ts`; there is no `legacy/pi/packages/mcp/src/auth-store.ts`. Per-client refresh
coalescing plus save CAS does not serialize rotating-token consumption across separately
prepared clients/epochs sharing a credential key; store-scoped locking and post-lock reread require
a reviewed design. The lead requires legacy `proper-lockfile` interoperability, not an fs2-only
advisory divergence: exact server key/hash and logical lock path (`realpath:false`), atomic acquire,
initial acquisition plus 250 retries at 100 ms (nominal 25 seconds, not an absolute deadline),
20-second recorded-mtime staleness, compatible precision/10-second heartbeat and normal-release
cleanup, private 0700 lock directory, and an explicit path/symlink threat review.
Memory key locks remain scoped to their shared backend Arc. Implementation remains held pending
that no-new-dependency plan; later Rust↔Rust and Rust↔Node tests must establish cross-process claims.
The read-only investigation found additional identity/storage blockers: frozen refresh keys use
`mcp__<normalized-name>|String(new URL(url))`, including fragments, whereas current Rust credential
keys omit that prefix and strip fragments. Current connection/public OAuth validation already rejects
fragments: this is a storage/path/conformance constraint, not a demonstrated live-connection race.
Broader URL coverage must settle storage and lock identity together. Node also uses a proper-lockfile
document-lock directory and a top-level namespace state format; Rust uses an fs2 document-lock file,
atomic replacement and a `servers` wrapper. Refresh-lock protocol compatibility alone does not
establish credential-format, document-write or login/logout CAS interoperability. No key/schema or
URL-validation change or dual-lock workaround is released; protocol-only Node helper evidence must
not be called shared-state proof. Proper-lockfile stale removal/release is unfenced, and frozen MCP
uses a no-op compromise handler; compatibility is not unconditional exclusion during compromise.
Public `McpOAuthSignIn::refresh` also needs a separate cancellation/CAS scope review: adding a trait
guard alone does not provide the coordinator's detached persistence or protect newer login/logout.
Token/completed-generation snapshot consistency needs its own race regression; the proposed
closed-empty-watch outcome is a bounded coordination failure, not spin, repair or sign-in guidance.
These are design findings only; guard/snapshot implementation remains held. The lead requires one
complete persistence-boundary design first and withdraws refresh-lock-only interoperability as an
assumption. That design must preserve existing credentials through explicit read-both/migrate-once,
without silent overwrite or dual write; cover pending sign-in, old/new Rust and Node concurrency,
permissions/size, document-lock ordering, crash recovery and rollback. Temporary dual document locks
are a design question, not a release, and cannot alone fix old-format writers or in-flight refreshes.
Default agent-directory files may target full frozen interoperability; custom/memory stores must
state narrower guarantees where path/lock-directory identity is unknown.

Read-only persistence review found a concrete downgrade hazard: the current defaulted Rust
`servers` field accepts a Node document as an empty Rust document, and a later update (even a
no-op removal) can rewrite it without the Node entries. Dual document locks cannot repair that
writer. The proposed migration is explicit, quiescent and all-or-nothing, with preflight stopping
on ambiguous identities, unrepresentable metadata/pending states or mixed documents. A fixed,
non-map `servers` marker could make the inspected old Rust fail deserialization while surviving
the frozen Node store operations. This is a proposed reserved storage extension, not Node schema
identity: conflicting existing data and unknown marker versions must fail preflight, and it cannot
fence an old exchange already sent to the authorization server. Quiescence remains necessary.
Normal write/parser/size policies also need decisions. In-place writes match Node more closely
but retain truncation risk. Atomic replacement can share the canonical pathname lock for a stable
resolved target; supported links, metadata preservation and filesystem durability need explicit
limits. Neither strategy makes arbitrary hardlink aliases or all mixed writers atomic.

Frozen Node locks individual loads and saves but provides no matching conditional publication
across them; concurrent Node refresh or browser completion can undo logout. Rust content-digest
CAS is not an ABA-proof generation fence. Shared refresh locking therefore cannot establish
universal mixed-runtime latest-action-wins semantics. Decision #187 approves only the atomic
supported-subset design: canonical-path locking, stable resolved regular-file single-link targets,
faithfully preserved required metadata, and safe rejection before publication for unsupported
cases—never a silent fallback to truncating in-place writes. A versioned non-map marker is approved
in principle; encoding/collisions and the CLI settlement design remain to be settled. Original-byte
no-clobber backup, quiescent representability checks, unknown-field preservation or rejection, the
1 MiB/malformed boundary, and cooperative-only limitations remain mandatory. This releases no
migration, schema, guard, storage hook or production change. The independent frozen HTTP/SSE
tranche need not wait for persistence implementation or every parity gap: its single proposed
no-vendor/offline validation window is documented in [the release batch](mcp-stock-validation-window.md).
Decision #188 releases exactly that batch, not this persistence design or broader validation.

**PUBLIC-REFRESH-LIFETIME** remains open: detached exchange/save survives caller cancellation only
while its host runtime/process keeps running. The session-free Rust CLI cancellation/timeout path
does not drain detached token work before returning or `process::exit`. A graceful CLI guarantee
needs a separately reviewed app-owned settlement handoff through save and lease cleanup, with a
chosen drain budget and an explicit incomplete outcome. No hidden exit hook or blocking Drop is
approved. Frozen Node is friendlier to outstanding I/O but also provides no forced-exit durability.
**INTERACTIVE-REFRESH-FIRST** is a separate source gap: Rust shell login currently connect-probes,
then starts browser authorization when needed; it does not explicitly call the public refresh
route before browser start as frozen `startMcpSignIn` does. Neither gap is covered by in-runtime
refresh tests or by the current public-refresh API alone.

The authorized OAuth post-response gate fix and regressions are now source-frozen, uncompiled and
unrun: normal supersession may drain admitted results without relaxing cancellation, retirement or
new-dispatch fences. Terminal actionable application-auth failures also lack the frozen exact-client drop/NeedsAuth readiness
handoff; handshake-only projection does not close that cross-layer A3 gap. Challenge handling also
remains incomplete: actionable handshake headers reach a fresh-probe sign-in path, but there is no
lasting frozen `onChallenge` observation, reactive refresh does not carry challenge/resource-metadata
input, and explicit auth metadata bypasses challenge-scope recording. Projection/no-refresh tests
do not establish subsequent login or challenge-aware refresh behavior. These findings are static,
not executed failures, and the provisional source freezes are being revised before validation.

The later runtime run `3a652f7d` genuinely executed 20 cases with 14 passes and six failures. Subsequent
runtime/fixture corrections expanded the source matrix to 21 but were not rerun. Source review has
since added two real-slot OAuth drain cases (23 runtime cases) and a third carrier case; these are
also uncompiled/unrun. Ten additional real-SDK runtime SSE regressions are source-frozen and
read-only-reviewed under the approved source-only release. The original 23 runtime tests remain
unchanged, yielding 33 in that owning module. The separate auth-SSE subset now has 13 tests covering
23 scenarios: four response-stream tests/nine scenarios plus nine common-stream tests/14 scenarios.
Common initial/reopened terminal failures and their distinct transient-503 paths are written and
source-reviewed, not observed red/green. Review distinguishes local GET bounds, deliberate Rust
ownership, stock characterization and oracle-desired cases. Five-second bounds are per observation
or result, not whole multi-phase test durations. The old-epoch persistence fence is bounded
cooperative polling of actual saved state, not event-driven settlement or task quiescence.
No new regression has run or closed an SSE gap.
The obsolete SDK prototype printed 6/6 in `3d7d7894`, then failed the post-run
storage guard: target identity changed, its sentinel and SDK subdirectory were missing, and the
target root was empty. Its source/lock hashes remained unchanged, but this is exploratory output,
**not accepted migration validation**. No cause is attributed to a resident OS service. The incident receipts survived under
`.eval/mcp-evidence/target-guard-20261126T033557Z/`; historical lost originals were not reconstructed.
The second incident reinstated the build/layout hold.

The three-file Coding-eval prerequisite now reserves the `.eval` directory basename in its existing
private exclusions. Added generic and real-product regressions independently cover copy, populated
original snapshots, nested cache/evidence, preserved `.pi`/`.evaluation`/normal source, and source-only
changes. Its frozen patch SHA-256 is
`1d4508f68c3c6a991df293de00ca93bc4e235cf8bae705f20f88a58b9ba2e39d`.
Readback and targeted diff hygiene passed; tests and formatting are **unvalidated**. Generic defaults,
native fingerprint coverage and native fixture copying are unchanged. A checkout-root `.eval` cache
and evidence layout is approved only for the named decision #188 batch, not as an inherently durable
location or a replacement for lost historical receipts. Prior auth
18/18, product 2/2 and checkpoint gates do not validate the new adapter/auth policy. L4/L5, the named
HTTP gaps, E1–E3, Q1/A3 and the full parity task remain open.

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

The startup budget was clarified in `question-38e75452c9c24c3fbd547b4214ee7021`: start every eligible
server concurrently, but wait only for effective enabled **direct-exposure** servers, under one
shared per-generation **10-second total** budget. No-direct generations do not wait; indirect/codemode
servers never delay initial publication. Snapshot ready/pending/auth/error states without discarding
healthy siblings. Re-evaluate eligibility on policy reclassification; keep equivalent background
attempts alive, reject stale epochs, and publish later readiness only at next idle. Required integration
regressions include zero-wait indirect startup, multiple direct starts sharing one budget, mixed
ready/timeout/auth/error outcomes, policy reclassification and next-idle publication.

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

## Private transport replacement (decision #204, in progress)

The person approved replacing the SDK HTTP worker with one private product transport, while
retaining unmodified registry rmcp 3.3.0. This supersedes only the former thin-adapter boundary,
not historical HOLDs, persistence/migration restrictions or missing evidence. No agents, SDK
fork, extra worker stack, synthetic RPC failures, commit or push are part of this change.

`http_transport` now uses public rmcp `WorkerTransport` plus existing HTTP/auth clients.
It owns bounded concurrent POST admission, separate control dispatch, session headers, GET
eligibility/backoff, response tails, explicit-client repair and terminal cleanup. Product common
SSE tests enter this transport; independent stock-characterization tests still enter the stock
worker. Actual-slot tests enter the normal product connection factory without assertion changes.

Initial `cargo check --offline --locked -p pi-plugin-mcp --tests` passed in
`d430b25c-ccfd-46b1-a625-363e1cdacfab` after correcting a private SDK helper call and the client
Sync bound (first compile `6a24e9a8-eb41-4435-a7ac-94467ecfab03` failed). This is compilation
evidence only. Subsequent focused evidence:

- `48b8bd74-12b2-4152-bae3-a3f2accdb2d6`: **13/13 desired reconnect cases passed**, including
  all seven previously failing cases, with unchanged assertions/time bounds. This was before the
  later GET-fence/setup/header follow-ups below; the full package must be rerun on final inputs.
- `f2925c72-4934-409d-9dd5-b70fe5ab19a7`: **2/2 controlled-GET regressions passed**. Held real
  OAuth exchanges test cancellation, retirement and supersession before replay; token rotation
  persists, no extra GET is dispatched. A spent POST allowance does not consume the GET
  allowance. The new typed `StreamRequest` passes authority explicitly into each auth wait,
  without wire metadata, task-local storage or a request-ID lookup. The first compilation
  (`b2dc4016-b109-4b17-81aa-30c4dff9e689`) failed on a missing test `Duration` import.
- `317d8d71-c8c9-4dee-8d7d-8f7e6499849b`: **0/2 setup-SSE regressions passed**, both blocked
  by send/receive backpressure with a one-item receive channel. After acknowledging actual
  setup SSE headers before consumption, `5e11483d-8a87-49b6-bc0e-b20dff25805b` passed **2/2**.
  Sixty-four notifications before initialize no longer deadlock; missing-response EOF closes
  setup rather than leaving it pending. Application response streams retain typed send failures.
  A failure after setup-header acknowledgement closes the connection; it cannot retroactively
  replace the acknowledged send result and does not fabricate an RPC error.

- First complete package run after replacement, `6862b209-25e4-40bb-bf2b-68c4d57a2cdb`:
  **280 library passed; 36 integration passed / 1 failed; 2 live ignored** (316 passed total).
  All thirteen desired reconnect cases and the new GET/setup/header unit cases ran. Independent
  strict MCP Clippy, workspace format check and diff check passed; tests returned 101. The sole
  failure was provider auth accounting (5 resolver calls versus 4 recorded POSTs). Frozen legacy
  opens the common GET after initialized even without an owned session ID; the fixture now
  observes that GET explicitly and keeps the exact resolver/request equality assertion, adding
  an exact five-request assertion. `b4d63656-ffc0-4d82-8da0-7c356dd4657c` passed that focused case.
- Shared explicit repair regressions, `3ecf2cee-711d-4b84-9a7e-ea7a8277d5a4`: **1 passed / 2 failed**.
  Cancelling the initiating waiter and simultaneous failed repair each initialized three times
  instead of the expected two. Moving the repair task into the sole worker and sharing its
  original typed outcome made all **3/3 pass** in `3f1bb6ac-a51c-4077-a0ca-6ef40c25d2df`. The real
  rmcp cancellation API is used; old cancellation controls cannot enter the replacement session.
  `Arc<HttpError>` preserves the original failure rather than cloning error strings or producing
  synthetic RPC errors. These changes invalidate the preceding package-wide/static-gate inputs.

The worker now has its own private configuration rather than accepting an unused SDK retry
policy. `ff7e40b6-35d0-4952-8220-d65e810d8087` ran the transport-focused group: **8 passed / 1 failed**.
The modern header wire regression passed, proving validated tools reach the inventory and
Method/Name/base64-encoded parameter headers reach HTTP. The earlier fixture compile
`d634f2b9-dbb0-46c4-bab2-1e317ae6fe2b` omitted the Discover version field; the next run
`3f132e45-580a-4e40-92c1-19d136590630` was **7 passed / 2 failed**, with a malformed modern
discovery fixture (corrected using locked rmcp `DiscoverResult::new`) and the real ping failure.

**SDK-SETUP-PING, pre-#205 failure:** `setup_peer_ping_makes_progress_before_the_initialize_reply`
failed its unchanged two-second bound. The fixture sends a real ping on the initialize
SSE body and withholds the final initialization response until pong. rmcp 3.3.0
`service/client.rs:234–237` explicitly ignores that ping. Frozen Pi installs the ping handler
in the constructor and attaches message dispatch before initialize (`client.ts:174–247`).
Acknowledging SSE headers fixes channel backpressure but cannot make SDK bootstrap dispatch a
request it explicitly discards. No retry/timeout/assertion workaround is applied. A narrowly
scoped bootstrap ping responder needed a boundary decision beyond #204's retained SDK setup
lifecycle. The person approved that narrow exception in **#205**; it does not authorize generic
bootstrap dispatch replacement.

- `1cb5ea1b-280e-48dc-825c-f4196d2e0b9f` first failed to compile because rmcp's `EmptyResult`
  implements `From<()>`, not `Default`; the Pong now uses the real public constructor.
- `56225078-0b18-46aa-8b77-f8434be8c2f5`: **11 passed / 1 failed**. The original two-second
  initialization-Ping regression and explicit repair Ping passed; a held-Pong close regression
  exposed DELETE targeting the old session instead of the acquired repair candidate.
- After retaining candidate identity/version for terminal cleanup only,
  `5ec6d448-0566-491b-80a1-636d5430d0da` passed **13/13 transport regressions**. These include
  original string/numeric Ping IDs, static auth and candidate-session headers, bounded control
  timeout without application replay, close during a held Pong with candidate DELETE, all
  shared-repair regressions, and modern headers. A custom SDK Ping handler is invoked for a
  Ping after the initialize response on the same SSE tail, proving the exception does not
  replace ordinary SDK dispatch. No old assertions/deadlines were weakened.

The complete package/static rerun on the #205 tree,
`4afb3e58-511b-499f-9cda-fae8cea206ec`, passed: **289 library + 37 integration = 326 passed;
2 live tests explicitly ignored**. Independent exit codes were tests=0, strict MCP all-target
Clippy=0, workspace fmt=0 and diff=0. This is the first green complete package snapshot after
the replacement, not full workspace or full observable-parity acceptance.

Earlier static gates on the shared-repair/protocol-test tree,
`3c3487e6-c66e-420c-894a-bfee32a90462`: strict MCP all-target Clippy, workspace fmt check and
diff check all passed. The package run above supersedes those narrower inputs; full workspace
validation results follow.

The unfiltered workspace run `479e057a-9d2a-43e4-8998-ea7428ab1a9f` completed with
**tests=101, workspace Clippy=0, fmt=0, diff=0**. Cargo reported exactly two failed targets:

- CLI `mcp_list_opts_in_to_local_command_values_without_a_session` expected `needs-auth`
  for rejected command-produced static Authorization; the actual state was `failed`. Frozen
  `runtime.ts::usesOAuth` / `needsSignIn` confirms that caller-owned Authorization is not
  managed OAuth. The fixture now checks the correct failed-handshake state, the exact wire
  Authorization and one request, no MCP-login guidance/store materialization, plus the existing
  helper-count, redaction, disabled-server and no-session assertions. No production change
  was needed. The follow-up full CLI package in `3f6e825d-99db-430c-8196-8b8138962f60`
  passed **251 library + 7 integration = 258 tests**; targeted Clippy, fmt and diff passed.
- The known Coding integration depth-512 test again overflowed its stack and SIGABRTed
  before that 95-test executable could proceed. `RUST_MIN_STACK` was unset; no test was
  skipped or weakened. Other workspace targets continued under `--no-fail-fast`. A separate
  focused `mcp` integration run supplies evidence for the MCP cases that this abort prevented;
  it cannot make the failed unfiltered workspace run green.

The focused Coding MCP run in `3f6e825d-99db-430c-8196-8b8138962f60` exposed **35 passed /
11 failed**, 49 outside the filter. One configuration fixture had the same obsolete static-auth
status expectation (corrected without changing its helper counts, trust/ACP checks or deadlines).
Ten lifecycle/reload cases ran through real ephemeral Agents and failed with `AuthorityRetired`.
MCP incorrectly mapped typed `PluginContextError::Unbound` from intentionally unavailable parent
capabilities to retirement. The fix stays inside MCP request admission: only `Unbound` means
optional host inventory is absent; original scoped dispatch grants, run/generation/transport
authority and actual context retirement remain enforced. No generic runtime or native ABI change.

Two dedicated regressions first failed in `c0cc16e3-66b1-48ce-ad1f-b36502d69861` (**0/2**), then
the complete authority group passed **8/8** in `c9d818f7-ab08-4cf4-9176-1fbf35e89701`. They prove
denied preparation/execution sends nothing despite an identical advertised descriptor; a live
ephemeral invocation can recover once; retired contexts/generations and missing available host
inventory cannot dispatch; old admitted calls retire without killing a newer lease.

The follow-up `f6266d66-f828-40af-a125-4d5728cb8aba` completed with **291 library + 37 integration
= 328 MCP tests passed; 2 live tests explicitly ignored**. Focused Coding `mcp` integration passed
**46/46**, with 49 unrelated cases outside the filter, resolving all eleven failures above on
these inputs. Independent exit codes: MCP=0, Coding MCP=0, strict MCP/Coding all-target Clippy=0,
workspace fmt=0, diff=0. This is affected-package/focused integration evidence, not a replacement
for the failed unfiltered workspace run `479e057a` or full-parity acceptance.

Failed repair currently retains one result per epoch until reconnect; readiness after failure,
held-POST draining and non-Ping recovery-time requests still need validation/completion. The
new candidate-close proof covers acquired JSON/SSE POST identities, not complete session-header
capture for every response kind or all possible shutdown races. The application terminal-auth
handoff tranche below addresses one A3 seam, not the entire auth/conformance row. A renewed
frozen-source read corrects the earlier
common-stream wording: `McpClient.connect` reports transport errors without closing pending requests;
Coding `McpServerConnection` does not subscribe to `onError`. Only `withClient` request failures
invoke `needsSignIn`/`markNeedsAuth`; an OAuth `onChallenge` observation can update the cached
challenge independently. A common-stream failure alone is therefore not oracle evidence for a
NeedsAuth transition. Common error/challenge observation remains incomplete; do not synthesize an
application failure or retire unrelated admitted requests to fill it. Cleanup tracing,
repeated-successful-response session capture, JSON arrays and exact SSE syntax/accounting remain
open. The package snapshot above is green on its recorded inputs; no full-parity claim.

### Application terminal-auth handoff (A3 subset)

`client/request.rs` now classifies completed application failures using the setup classifier's
actual-provider/coordinator evidence. Typed guidance retains the original HTTP error and private
challenge while Display/Debug remain redacted. `supervisor/recovery.rs` checks the admitted
connection against its weak exact slot before setting auth-failed readiness and bumping freshness.
It neither looks up a replacement by server name nor reconnects. The next idle generation projects
NeedsAuth and removes unavailable executable tools; old registries remain immutable. New calls are
fenced immediately, but unrelated admitted JSON/SSE successes drain before cleanup. This preserves
the approved Rust ownership guarantee instead of copying legacy's eager cancellation.

New local regressions in `client/recovery_tests/authorization.rs` reproduced the gap in
`218d06cc-7c73-4eee-a6d6-162cf698c645`: **1 passed / 4 failed**. After implementation,
`433f5d9f-cf2c-4e1b-8c99-256dc4d89b69` passed **5/5**. They cover exhausted OAuth 401, scope
step-up, provider 401 with/without a token, static/nonactionable 403 exclusion, unchanged frozen
catalogs, no further dispatch/eager opening, late old-transport failure after replacement,
admitted JSON/SSE success draining, and aggregate-page failure without partial-data leakage.
The first expanded run, `a868a024-11eb-4fbe-a7d8-49fb97a1b800`, failed: MCP **294 library + 37
integration = 331 passed / 2 failed / 2 live ignored**; Coding MCP **46 passed / 1 failed** (49
outside the filter). Formatting, strict MCP/Coding Clippy, fmt check and diff check all exited 0.
Both MCP failures were unchanged terminal response-resume tests: the new auth handoff incorrectly
fenced their independent follow-up/common-stream probes. Frozen `consumeResponseStream` instead
reports a local RPC stream error, not an application POST authentication rejection. A further
unspent-budget GET-404 regression in `f81f6fc9-06ae-4b60-8076-2b14aacc77e9` failed **0/1**: it
observed two original echo POSTs instead of one. This exposes an earlier replay bug at the same
missing-provenance boundary, not an acceptable retry.

The correction wraps real post-acceptance response-stream failures in a private typed phase that
retains the original HTTP cause but grants neither application retry nor auth/readiness authority.
Cancellation/retirement controls remain typed. No fabricated RPC, status-string classification,
request-ID map, wire metadata or SDK change is introduced. Existing resume tests and deadlines are
unchanged; new regressions cover GET-404 non-reexecution and underlying-error/control preservation.
The Coding failure was in the new fixture's assumed admission point: registered `/mcp status`
commands are handled before freshness admission by `runtime/run.rs`. The fixture now verifies
command-only use remains unsaved, then uses an ordinary idle prompt to verify replacement, actual
provider-tool removal, status guidance, separate-live-session isolation and first-response journal
materialization without leaking the earlier ephemeral tool call.

Focused run `08f4e402-c193-4d4c-8d0b-eae051e78814` passed **40 recovery tests**, **2/2 typed
error/control tests**, and **1/1 Coding cross-layer test**. One new GET-404 case still failed after
its no-reexecution assertions: its fixture helper expected a refreshed bearer despite this case
intentionally having zero refreshes. The helper now explicitly represents generation zero as the
original bearer while retaining exact nonzero-generation, session and cursor assertions.
`451674a2-b082-4dcb-ba4b-a9ec5616a3b8` then passed that **1/1** GET-404 case, including a healthy
independent probe/common-stream progress and unchanged initialization/execution counts. No original
SSE deadline or retry expectation changed.

Complete affected-package follow-up `29902372-9ec7-48f2-81d2-47fbba7a3ec3` exited **0** on these
final tranche inputs: MCP **299 library + 37 integration = 336 passed**, with **2 live tests explicitly
ignored**; focused Coding MCP **47 passed / 0 failed**, with 49 unrelated cases outside the filter.
Independent exits were **MCP=0, Coding MCP=0, strict MCP/Coding all-target Clippy=0, workspace fmt=0,
diff=0**. This includes the unchanged resume regressions, A3 exact-slot/draining tests, GET-404
non-reexecution, error/control preservation and Coding next-idle/separate-live-session coverage.
The earlier 328/46 result remains evidence for the preceding ephemeral fix; every intervening red
receipt remains recorded. The unfiltered workspace `479e057a` depth-512 SIGABRT is not superseded.
No full-workspace, official-conformance, credential-interoperability or complete A3/parity claim is
made; common challenge/error observation and the other open rows still require separate work.

### Supplemental private-worker fatal-overflow evidence

The next smallest authorized follow-up added tests only; production stream classification and
retry policy were not changed. Frozen `consumeSseStream` throws an ordinary non-retryable Error
on overflow and cancels its reader; `consumeResponseStream`/`runGetStream` stop rather than reopen.
The #204 private worker already implements that terminal classification for its own typed body-limit
error; it does not need to impersonate rmcp's private marker.

`http_transport/tests/overflow.rs` uses loopback HTTP bodies that remain open after a complete
cursor-bearing progress event, `retry: 0`, and an oversized suffix. Three tests separately establish:

- the actual production common-stream future **returns its terminal typed error**, preserves the
  complete prefix and issues exactly one GET (not merely a bounded absence observation);
- an application stream failure retains typed overflow provenance, drops/cancels the still-open
  server body, does not resume or replay the RPC, and leaves an independent SDK call usable;
- common-stream overflow releases its body without cancelling an already-admitted held call;
  that call and an independent probe both succeed, with one initialization and terminal DELETE.

First run `fa8692de-aaf1-40d6-aac3-5c5af012c029` retained **2 passes / 1 fixture failure**: its prefix
assertion compared serialized SDK `Number(1.0)` with integer `1`. The correction checks the exact
numeric progress value `Some(1.0)`, with no timing or wire-count changes. Run
`2175dc3b-7d84-405d-8719-ee2cd9b26193` passed **3/3**; strict MCP all-target Clippy, workspace fmt and
diff checks also exited 0. No previous full package/Coding gates were rerun: `29902372` remains the
336/2-live-ignored and Coding-47 evidence for unchanged product code, supplemented by these three
new tests, not a newly executed full-suite total. Exact SSE syntax/accounting, common diagnostic
observation and the other open rows remain distinct. No new transport, observation framework,
dependency, credential persistence, SDK change, network suite or Git delivery was introduced.

### Supplemental provider-wait authority evidence

`client/recovery_tests/sse/provider_wait.rs` extends the existing real HTTP fixture through the
actual supervised slot, service, provider-auth wrapper and resumed GET path. The original echo
executes once, emits its real priming progress and ends its response stream; only then does the
fixture suspend the next provider token callback, before GET dispatch. One case cancels the logical
request. The other publishes an equivalent generation on the **same `Arc<McpConnection>`**, drops
only the original generation lease, and verifies retirement without closing/reconnecting the
transport as a substitute for the generation fence.

Both cases positively observe the suspended provider future being dropped, then release the gate
and run an independent probe with real common-stream progress through the newer view. They retain
one initialization/echo POST/execution, no response GET, no OAuth exchange, Ready slot state and
no provider lookup from terminal DELETE. These tests strengthen the existing Rust ownership
contract; they do not claim legacy carries the original sender cancellation into detached resumes.

Initial run `d1e5da03-6137-4951-acd5-310b55ba81c0` failed test compilation and Clippy (both 101)
for missing test imports `AtomicUsize`/`Semaphore`; no test bodies ran, fmt/diff were 0. After adding
only those imports, `108cc549-1fb2-4324-bbcd-dabc6740334d` passed **2/2**, strict MCP all-target Clippy,
workspace fmt and diff checks (all 0). Product code was unchanged. Together with the three overflow
cases, these are **five supplemental focused passes**, not a rerun or replacement of `29902372`.

### Private JSON array response delivery

The frozen transport parses the whole JSON document, then calls `emitMessage(parseJsonRpcMessage(item))`
in array order. A malformed member stops later members but cannot retract the valid prefix;
`client.ts:441–443` catches a send error against the still-pending request only, so a previously
correlated result stays complete. The stock public JSON response holds only one message.

`http_client/post.rs` supplies a private `PostRequest` / `PostResponse` envelope, used by the
existing static, provider and OAuth clients. Credential acquisition, auth budget and dispatch fences
remain in their existing owners; the public SDK adapter retains its single-message contract.
The backend still bounds/parses the complete real JSON document; the worker lazily validates and
forwards each real member. A setup batch acknowledges that real document before receive-channel
backpressure. Only pre-final genuine Ping uses #205; post-final requests go to rmcp. At the exact
correlated response the worker acknowledges the original send and drains remaining JSON members
under the existing cancellation-registration lifetime, as it does for an SSE tail. A malformed tail
stops its suffix without turning an already completed RPC into failure. No fake RPC/SSE, second
HTTP worker, error-string classification or SDK changes were added.

Red run `747d52ed-0784-4e71-bcfa-f03922dfb96f` produced **0/3**: application/setup arrays were
rejected as invalid JSON responses and the valid malformed-array prefix was never delivered.
`3e7db19d-8dad-4c97-b2a8-8dda92492f52` then passed **4/4**, adding the completed-result/invalid-tail
case. The tests retain the 2 s bounds; setup has receive capacity 1, 64 preceding notifications and
real Pings on either side of the initialize result, with standard empty Pong only before it and an
exact SDK-handler error after it. Strict MCP all-target Clippy, workspace fmt and diff checks all
exited 0. Explicit repair still consumes a single JSON initialization object; arrays and general
candidate-time dispatch in that separate repair path remain open rather than silently discarding
members.

Expanded run `edb8fad2-75fa-47b4-b92e-da52e2afcc86` failed: MCP library **305 passed / 3 failed**;
its integration targets were not reached. Coding MCP **47 passed / 49 filtered**, CLI MCP **7 passed**,
and strict three-package Clippy/fmt/diff all exited 0 (mcp=101; all other independent gates=0).
The three failures were `collision_names_remain_stable_when_list_order_or_exposure_changes`,
`resource_only_servers_connect_without_a_tools_list_request` and
`falls_back_to_legacy_initialize_over_streamable_http`. All exposed the same new JSON-end check:
it replaced a real, uncorrelated setup error already forwarded to rmcp with a send-side
`UnexpectedEndOfStream`, preventing the SDK's typed correlation error from reaching the existing
outer connection fallback policy. The correction removes that extra JSON-end failure, matching the
frozen transport's return after document emission; it does not relax IDs or modify fallback policy.
Empty/unrelated documents leave the existing caller deadline in charge. A new real-loopback test
asserts the exact SDK `UncorrelatedErrorResponse` for both single and array documents.

Focused `ff7dae7d-fa4d-481f-8dbd-63eac43b84cf` passed JSON **5**, client contracts **6** and legacy
fallback **1**. Its Clippy failed only on the new test's `.err().expect()` (101); fmt/diff were 0.
That call was changed to `expect_err`, without an allow or changed assertion. The start-call response
was interrupted by Sikemux's background update; workspace inspection identified this already-running
execution, so it was not duplicated. Corrected full-input run
`a3b2f94b-37fb-4d01-aa0c-681a5cff8b10` passed **309 library + 37 integration = 346 MCP tests**,
with **2 live tests explicitly ignored**; Coding MCP **47 passed / 49 filtered** and CLI MCP **7 passed**.
Independent mcp/coding_mcp/cli_mcp/clippy/fmt/diff exits were all 0. This is current evidence for that
JSON application/setup input snapshot, including the five preceding supplemental tests—not the sum
of historical runs. `29902372` remains historical evidence for its earlier snapshot. Explicit
repair/other open rows, official conformance, persistence and the failed unfiltered workspace gate
are not resolved by this affected-package result.

### Explicit-repair response and control-ownership blockers

The next two loopback regressions (`http_transport/tests/repair_messages.rs`) failed **0/2** in
`5a060a1a-5c3d-4614-9744-c329e5e0c87a`: JSON repair still rejects a real array, while SSE repair
treats a preceding progress notification as the initialization result. Both fixtures also require
one genuine pre-final Pong and an SDK-owned tail Ping on the candidate session, with receive
capacity 1, a 2 s bound, exactly two initializations and one rejected/one executed application POST.
No in-place repair patch was used to bypass those failures; the later #215 owner replacement is
described below.

A further real-service regression (`http_transport/tests/repair_controls.rs`) established a more
fundamental ownership gap. A common-stream Ping enters the SDK handler on `old-session` and waits.
A definitive application 404 publishes `replacement-session`; releasing the old handler then sends
its genuine error reply **with the replacement session header**. Run
`005e5f7d-5e0d-4528-9214-05745953aaf3` failed **0/3** (including the preceding two cases), with the
positive assertion `old SDK callback crossed the session epoch on the wire`. This is not merely
a timeout or absence-of-traffic inference. The tests were active and red at that snapshot; their
later owner-boundary relocation is recorded below. `a3b2f94b` remains evidence for its earlier
inputs, not full green for the extended inputs.

Stock rmcp 3.3.0 `transport/worker.rs:159–165,304–318,345–350` explicitly captures
`control_generation` when the outbound send future is created, **not** when the inbound handler
starts. Advancing that number fences previously created/queued sends only. It cannot by itself
bind a delayed SDK handler result to its old session. The frozen client instead captures its
transport before awaiting the handler (`client.ts:494–522`); Coding detaches the expired client
and opens a fresh one, without closing unrelated in-flight calls (`runtime.ts:290–295`).

The person resolved that choice in **#215**: create a fresh SDK service/transport under the existing
connection owner, not an in-place quiescence barrier. Explicit pools now own an existing
`ConnectionSlot` directly, while supervised generation views retain their weak authority pins.
They share the same attempt, frozen-grant, retry-budget, admitted-use and draining machinery.
The obsolete private worker repair module, flight/channel, saved initialize replay, recovery
configuration and transport-local replacement epoch/control-generation path have now been removed.
A raw HTTP worker cannot open or publish a replacement service. No request-ID side map, hidden
latch/task-local recovery channel, wire metadata, SDK fork, fabricated reply, or cancellation of
unrelated admitted success is an acceptable shortcut.

The first owner iteration `c194d95a-0307-41eb-a2c1-26b3dc1ca905` ran five new tests: **4 passed /
1 failed**, with MCP Clippy, formatting and diff checks 0. Fresh connection identity, old-service
shutdown isolation, frozen grants, cancelled-waiter isolation and the shared OAuth/expiry budget
passed. Both admitted JSON/SSE calls also succeeded, but the new execution-total assertion exposed
that the existing fixture incremented its execution counter for held JSON and not held SSE. The
fixture now counts the actual emitted SSE success too; the total assertion stays exactly three.
That failed run is not reclassified as a pass.

The original three repair regressions are relocated (not ignored) to
`client/recovery_tests/{repair_messages,repair_controls}.rs`, using the actual exact-slot owner.
The original callback case retains positive cancellation-or-wire observation when no application
call remains. An additional case retains a real old admitted SSE success and requires actual
old-session and replacement-session callback replies with distinct SDK peer metadata: cancellation
is not accepted as a substitute for draining in that case. Receive capacity 1, 2 s bounds, original Ping
correlation, ordered progress, application replay limits and candidate DELETE remain asserted.
Per-fixture callback observers are `cfg(test)` only, absent from production ownership/control.
Run `dcea8809-073b-44b7-8d19-53688cd8f7a6` failed compilation (two E0609 accesses to optional SDK
server metadata); no test bodies ran. The fixture now explicitly unwraps that expected metadata.
Run `8346a2fa-ef48-4f0b-8e28-bb436df45b74` was **1 passed / 2 failed**: the callback-plus-admitted
success case passed, while the two message cases still assumed an already-negotiated version on
the fresh candidate's pre-final Pong. Frozen `client.ts:239–243` and HTTP transport headers at
`streamable-http.ts:388–393` instead establish the version only after initialize succeeds. The
relocated assertions now require **no** pre-final version header, exact `2025-03-26` post-final,
and the candidate session/auth header on both; this does not permit stale version inheritance.
Run `87dccd94-ba48-4b93-99b5-9bcc193faaa2` remained **1 passed / 2 failed**, now positively observing
progress **2 before 1** because stock rmcp's handshake consumes/drops non-logging notifications.
Neither failed run is reclassified.

A response-local private queue now retains genuine non-logging setup notifications until the
correlated response can hand them to the ordinary SDK dispatcher, in their order before the tail.
It neither answers an RPC nor implements another handshake/recovery owner; setup logging remains
in the SDK's live handshake path. Deferral until a successful handshake is a deliberate Rust
timing boundary, not a claim of identical pre-handshake callback timing or cross-log ordering.
The queue is bounded to 1024 notifications and the configured message-byte limit; bound failure
has no HTTP/auth/network retry authority. The original progress-order assertions remain, and the
64-notification/capacity-1 JSON setup test now checks every delivered progress value. Two focused
unit regressions cover both bounds, safe failure and unchanged logging/request/error forwarding.
Focused run `6b1cf999-9573-4fcc-a9a8-5f75d1c1f09e` completed with recovery **51 passed / 1 failed**,
transport **23 passed**, and strict MCP Clippy/workspace format/diff checks all 0. The relocated
JSON/SSE prefix cases, both delayed-handler variants, all five explicit-owner cases, the
64-notification setup case and both prefix bounds passed. The retained failure
`explicit_closed_connection_keeps_existing_disconnect_diagnostic` exposed a genuine policy
regression: a plain closed explicit service was silently reopened by the reused slot. The original
diagnostic/no-wire assertions remain; acquisition now distinguishes a definitively expired raw
connection already in that exact slot's draining list from a plain close, under the slot lock.
Only the former may open for explicit pools; supervised lazy reconnect remains unchanged. The
explicit replacement test also checks a later plain close, without another initialization. Run
`d1408dae-8c52-46e9-b87e-cf11378a1cd9` then passed **all 52 recovery tests** and strict MCP Clippy.
That focused result does not validate the subsequent obsolete-worker removal or establish current
full-package green.

The remaining six in-place repair/bootstrap tests are migrated to
`client/recovery_tests/{shared_repair,repair_bootstrap}.rs`, not deleted or ignored: concurrent
rejections share one fresh setup, waiter cancellation does not cancel it or cross old controls,
failed setup retains a shared typed auth requirement with its private challenge, and numeric
pre-final Pong/auth/candidate identity, 50 ms control deadline, no replay and actual candidate
DELETE remain checked. A fresh setup error now belongs to the owner rather than a fabricated
application reply on the old peer. Separate raw transport regressions retain exact typed SDK
cancellation and `ControlRequestTimeout` evidence at their owning seams. Incomplete fresh
handshake cleanup is verified by positive DELETE observations within the same 2 s bound, not a
quiet interval; synchronous cleanup-at-owner-return is not claimed for SDK-owned cancelled setup.
A new test checks the original 5 s explicit recovery setup bound; only that new timing test has a
6 s watchdog, while all migrated cases retain 2 s. Explicit owner input is bounded to 5 s rather
than silently inheriting supervised opening's 30 s budget. Removal run
`774a9752-c0ee-4c69-895c-e25bacf4e38f` passed **59 recovery tests / 19 transport tests**, with
independent format/recovery/transport/Clippy/workspace-format/diff exits all **0**. This includes
the six migrated cases, both raw typed-control cases, the 5 s bound and both delayed callbacks.

Follow-up ownership inspection found a collect-then-retire window in explicit shutdown. The slot
now collects strong cleanup references and retires current/draining state under the same lock;
the generic silent-drop/revision behavior is preserved. A new integration regression keeps both
an old admitted JSON request and a replacement SSE request active, then requires authoritative
shutdown to cancel both and complete both exact session DELETEs, with no new call/reconnect or
duplicate DELETE on repeated shutdown, inside 2 s. Expanded run
`36246c6e-0649-42e8-96a4-7b1b35227696` passed that exact regression, then recorded MCP library
**323 passed / 1 failed** (MCP integration targets not reached), Coding MCP **47 passed / 49
outside the filter**, CLI MCP **7 passed**, and strict three-package Clippy/workspace-format/diff
all **0**. Its independent six exits were **101/0/0/0/0/0**. The retained failure was
`plugin::tests::diagnostics::disconnected_status_keeps_frozen_catalog_and_shows_safe_stderr_tail`: the
new binding reported only `Connection closed`, dropping the already-redacted stdio diagnostic.
The existing test and its secret/no-reload/frozen-catalog assertions are unchanged. Weak slot
observation now returns typed readiness; the client binding renders the captured tail from that
slot's actual disconnected connection, never the frozen view's original/replaced transport.
Run `1da73945-2273-4daf-8816-6fdc2310f10f` passed that unchanged diagnostic regression and all
**324 MCP library tests**, then **17 integration tests / 2 live ignored** before
`explicit_oauth_replays_an_unchanged_token_once_without_supervised_resource_recovery` failed: the
explicit resource 503 path emitted **2 reads instead of 1**. Later MCP integration targets were
not reached. Coding MCP **47 / 49 outside filter**, CLI MCP **7** and all targeted static checks
passed; independent exits again were **101/0/0/0/0/0**. This is a real policy regression from
generalizing `supervision` into `binding`, not permission to change the test's one-read assertion.
Transient resource retry is now explicitly limited to supervised ownership; explicit pools share
only definitive-expiry replacement and the existing combined application allowance. The test's
obsolete SDK-in-place comment is updated, with all assertions unchanged. The adjacent terminal
auth handoff had the same over-broad binding condition: new public integration
`explicit_terminal_auth_does_not_inherit_supervised_needs_auth_state` failed in
`fe02fd4a-04b0-4a07-8a2a-e3e53a7367ff` (old resource regression passed / new test failed), with
`MCP connection is unavailable; retry after the next idle boundary` on a separate logical call.
The frozen standalone client only rejects that operation; Coding runtime `withClient` owns the
needs-auth lifecycle. A typed `supervised_slot` accessor now gates both policies. Explicit
application auth still returns typed guidance/cause, but neither retires its service nor starts
replacement setup merely for that auth failure. Its next independent accepted call must use
a distinct request ID on the same session, preserve the frozen catalog and keep the original
refresh count at one. Run `c3290bab-664f-47f5-923b-f8350c4acdc3` passed **both public integration
tests**, including unchanged one-resource-read/replay/header/refresh assertions. Final run
`e210afe6-bbdb-4d78-a49e-c0ea66cabec7` then validated these same corrected inputs: MCP
**324 library + 38 integration = 362 passed / 2 live ignored**, Coding MCP **47 passed / 49
outside the filter**, CLI MCP **7 passed**, strict three-package Clippy, workspace-format and
diff checks all **0** (six independent gate exits **0/0/0/0/0/0**). This includes supervised
auth retirement, shared retry budgets, authority checks, both callback variants, acquired-service
shutdown cleanup and the retained diagnostic assertions. Decision **#215** is complete at this
tested scope; the obsolete in-place coordinator is removed, not merely disabled. Earlier
failures remain historical failures. This is not unfiltered workspace green or full MCP parity: the
known depth-512 workspace SIGABRT remains unresolved, general candidate-time dispatch and remaining
control/tail races remain open, and goal **#124** plus the other ledger rows are not closed.

### Native cleanup output observation (pre-subscriber evidence)

Post-#204 source ownership differs from the original stock-boundary finding above:
`client.rs` binds every production HTTP auth path to `PiHttpTransport`, and private
`http_transport.rs` performs bounded DELETE without the stock HTTP worker's session-ID success
trace. The generic rmcp `WorkerTransport` still emits its own worker-exit diagnostics. The native
CLI does not install a tracing subscriber; source inspection alone does not prove event visibility
or silence for an embedding that installs one. No subscriber/filter or logging workaround was added.

Evidence-only `apps/pi-cli/tests/mcp_cleanup_output.rs` executes the real native CLI against
loopback fixtures for DELETE **204, 401, 500 and an unanswered request**. Each case positively
observes exactly one DELETE with the actual session, negotiated version and fake Authorization,
then requires child exit within 2 s of that observation. Output remains valid connected JSON,
stderr is empty, session/token/body markers are absent, setup/list counts stay one, and no journal
or auth file is created. `RUST_LOG=trace` is supplied without assuming it installs a subscriber.
Run `f616f695-ea52-4dee-a1be-62949d5fb8d1` passed **4/4**, with package format, targeted CLI strict
Clippy, workspace-format and diff checks all **0**. This new CLI-test receipt does not replace or
expand `e210afe6`'s #215 input scope; MCP/Coding sources and their already-validated tests did not
change, and those gates were not repeated.

This proves bounded cleanup and absent sensitive native stdout/stderr for these cases, **not**
absence of SDK tracing events or coverage of caller-installed subscribers. At this stage,
`SDK-CLEANUP-TRACE` therefore remained **OPEN**. The subsequent #219 observation below adds an
explicitly scoped, all-events test subscriber over the actual private-worker path, with a positive
SDK-event control and separate event ownership/capture reporting. No global filter, suppression, hidden error, SDK
patch, production subscriber or additional control dispatch is authorized by this observation.

### Scoped cleanup subscriber evidence (#219)

The person approved the bounded observation proposal in #219. Only a dev-dependency edge to
already-locked `tracing 0.1.44` was added to `pi-plugin-mcp`, plus that package's corresponding
lock dependency row. It uses `default-features = false, features = ["std"]`; stock rmcp already
enables tracing/std. No versions or production features were upgraded. Offline/locked dependency
inspection accepted this graph. There is no production subscriber, global default/filter, SDK
patch, error suppression, cleanup change or expanded control dispatch.

`tests/cleanup_trace.rs` drives the public `McpToolSet` through the actual private HTTP worker.
A thread-local `with_default` subscriber admits every level/event and captures span fields and
updates; the dedicated current-thread client runtime stays inside that scope through teardown.
The fixture server runs on a separate runtime/thread so server traces cannot masquerade as client
observations. Each DELETE 204/401/500/unanswered case requires exact wire session/auth/version,
one initialize/list/DELETE, a 2 s shutdown bound and exactly one real
`rmcp::transport::worker` DEBUG `worker quit` event during shutdown as a positive capture control.
The cleanup phase label means after shutdown invocation, not that every concurrent event is
caused by DELETE. Source metadata remains attached to event records.

Run `57bc90c0-e12c-4b5f-a351-66fa5431c0ab` passed **4/4** plus MCP strict Clippy, workspace-format
and diff checks, all **0**. Each case captured **38 setup events**, then **8 shutdown events**
for 204/401/500 or **6** for the unanswered request. Every case captured the real generic SDK
worker-exit event at `rmcp-3.3.0/src/transport/worker.rs:253`; there were **0 records containing
the fake session, complete Authorization or body markers** across event/span captures. Full
managed output is preserved in `.eval/project-checks/cleanup-trace-57bc90c0.log`. This demonstrates
that safe generic SDK tracing still occurs; it does not claim absolute SDK silence. The specific
stock HTTP-worker session-ID trace is no longer on the product path.

The final observer assertion searches the **bare token value**, not only the complete
Authorization header. Run `d47bf1ef-a202-4bc4-9d06-7c547d2d950a` passed that focused **4/4**
with the same positive SDK-event controls and **0 sensitive records**, then ran the unfiltered
workspace with `--no-fail-fast`. Its actual MCP targets passed **324 library + 42 integration =
366 tests / 2 live ignored**, including the four new captures; the prerequisite focused run is
not counted a second time. Native CLI **251 unit, 4 cleanup-output and 7 management tests**
passed in this same workspace run. These are current-input results, not sums of old receipts.

Independent final gate exits were **workspace tests=101 / workspace all-target Clippy=0 /
workspace fmt=0 / diff=0** (overall 1). Cargo reported exactly one failed target,
`pi-coding --test integration`: the unchanged
`codemode::depth_limit_store_values_survive_selected_branch_reload_and_disk_resume` again
aborted with stack overflow / SIGABRT. That target announced 96 tests and aborted before
reporting MCP-case results; this run supplies **no new Coding MCP 47-pass claim**. No test was
filtered to avoid that failure and no stack-limit workaround was applied. The complete managed
plain-text log and final metadata are retained in
`.eval/project-checks/cleanup-dependency-d47bf1ef.log`; the failed workspace gate remains failed.

#219's scoped observation is **COMPLETE**, and the lead has accepted the existing evidence under
original #183: **SDK-CLEANUP-TRACE is CLOSED for d47bf1ef's inputs and these four scenarios**.
The accepted rule is that private/backend cleanup does not log tokens, headers, bodies, statuses
or errors, inspect DELETE outcomes or reacquire credentials. The old stock HTTP-worker
`session_id` event is no longer in the product path; safe generic SDK events need no suppression.
Neither zero fixture markers nor this acceptance implies absolute silence across the entire SDK.
No repair, production logging change or further suite run was needed for acceptance.

Accepted cross-reference anchors (paths relative to `plugins/features/pi-plugin-mcp/`):

- Product entry: `src/client.rs:13–14,1029–1047` selects `PiHttpTransport` for the HTTP auth paths;
  `src/http_transport.rs:49–56` spawns the private `HttpWorker` on generic `WorkerTransport`, not
  the stock HTTP worker that emits `delete session success` with `session_id` at upstream
  `rmcp-3.3.0/src/transport/streamable_http_client.rs:1786–1789`.
- Terminal path: `src/http_transport.rs:245–264` aborts/joins tasks and discards bounded DELETE
  results; `src/http_client.rs:325–350` sends once, discards response/error without body/status
  interpretation or logging, and returns success. `src/auth.rs:1024–1042` and
  `src/auth/refresh.rs:538–555` only reuse their transport's acquired-token cache;
  `src/client/operation/http.rs:76–83` directly delegates cleanup.
- Capture: `tests/cleanup_trace/capture.rs:94–142` retains event/span fields and updates with all
  levels enabled; `:21–32` identifies the actual SDK worker-exit target/source/message.
  `tests/cleanup_trace.rs:121–140` positively observes exact DELETE headers; `:144–168` scopes
  the current-thread client runtime through teardown and bounds shutdown; `:172–209` checks
  wire counts, the genuine SDK positive control, and session/bare-token/body markers.

Wider credential, lifecycle and session-capture coverage, #124, and the known depth-512
workspace failure remain separate. Historical failures, the aborted Coding integration target
and its absence of a new 47-pass result are unchanged; full cleanup parity is not waived.

### Independent current-snapshot Node/RPC project check

The preserved original `.git/sikemux/check-logs/202eb9a-full.log` contains only `tsc --noEmit`
and TS2688 (the `node` type library was not found); the Rust tail of its `&&` command did not run.
Its execution cwd/dependency snapshot cannot be recovered from the log or the inspected supported
workspace metadata: historical execution domain and root cause remain **unknown**. Current read-only
inspection instead found declared/locked/installed `@types/node` 24.13.3 and TypeScript 5.9.2, a
readable real `index.d.ts`, and a valid local `tsc` symlink. No dependency restoration, configuration
change, installation, rollback or source change was performed to make the check pass.

After the MCP processes exited, separately approved run `9ee180c0-a8bf-4a11-9e8c-77b2b2c15ad0`
recorded cwd `/Users/bytedance/Documents/pi-rs`, branch `main`, HEAD
`202eb9a0b5524ee1a23f8016191db1ea70a59f47`, the current dirty status and eleven input hashes.
Tools were Node **22.23.2**, npm **10.9.8**, cargo/rustc **1.98.0**. Exactly one
`npm --prefix packages/pi run check` passed, followed serially by
`cargo check --offline --locked -p pi-rpc`; independent exits were **node=0 / rpc=0**.
The managed log is copied intact with run metadata to
`.eval/project-checks/current-main-9ee180c0.log`, independently of the untouched historical log.
This is a project-check pass for that **current dirty snapshot only**, not acceptance of the bare
commit, proof of the historical TS2688 cause, a resolution of the three new MCP regressions, or
a replacement for the failed depth-512 unfiltered workspace gate.

### SSE malformed UTF-8 replacement (partial SSE-SYNTAX delivery)

Frozen `legacy/pi/packages/mcp/src/transports/streamable-http.ts:42–144` uses nonfatal streaming
`TextDecoder`, including its EOF flush. The private adapter previously passed bytes directly to
`sse-stream`'s strict UTF-8 parser, making a malformed byte inside an otherwise valid JSON text
field fail the actual SDK request. Desired-red run `4f7aa7f8-83df-4456-9530-b3f2226af546` compiled:
**0 passed / 3 failed** — strict metadata UTF-8 parsing, missing typed replacement-expansion bounds,
and the loopback SDK tool reply (`Client(MCP response stream failed)`). These failures are retained.

`http_client/sse/utf8.rs` now performs nonfatal streaming replacement before the unchanged public
SSE parser. Only an incomplete scalar (at most three bytes) is retained across chunks; clean EOF
flushes it, while source errors retain their original error rather than flushing a success tail.
The existing raw frame-boundary check remains, and the same conservative line/event bound is
also checked on normalized bytes. Complete earlier frames still reach the parser before a later
bound failure, which drops the source and unread suffix. This is not a new parser/control owner,
a synthetic RPC/SSE response, an SDK patch, or a change to retry/authority/lifecycle policy.

`3209827d-69ec-4761-9d31-a7343c411379` passed **8 parser tests and 16 private-transport tests**,
with strict MCP all-target Clippy, workspace format and diff checks all **0**. Coverage includes
BOM plus malformed data/ID bytes, valid multi-byte Unicode over six chunk sizes, complete prefix
and suffix events, positive source Drop on replacement expansion, incomplete-scalar EOF decoding,
and an actual authenticated private-worker/SDK tool reply followed by an independent successful
request: exactly one initialize, the two named calls once each, and one DELETE under original
two-second bounds. HTTP chunk writes may be coalesced; deterministic chunk-partition claims come
from the byte-adapter tests, not the network fixture.

Final affected-input run `cf83ba1f-27d5-419f-af63-29949031aaf3` completed with all six independent
gates **0**: MCP **328 library + 42 integration = 370 passed / 2 live ignored**, Coding MCP
**47 passed / 49 outside the filter**, CLI MCP management **7 passed**, strict all-target Clippy
for the three affected packages, workspace format and diff checks. The complete managed log and
run metadata are retained at `.eval/project-checks/sse-utf8-cf83ba1f.log`. The completion trigger
reported a timeout, but the collected process receipt is completed/exit 0; no duplicate run was
started. These are this input's counts, not a sum of historical focused runs. Full workspace,
Node/native-bridge/PTY and official-suite gates were not rerun for this private MCP change.

This delivery is conformance for the tested UTF-8 subcase within the retained conservative bounds.
Decoder EOF flushing is not evidence of SSE unfinished-line/event EOF dispatch parity. Duplicate,
unknown and bare fields, general EOF/tail dispatch, line-time cursor/retry updates and exact decoded
payload accounting remain open. No Node, official networking, depth-512 reproduction/repair, new
agent or Git delivery is part of this step; the failed unfiltered workspace gate is not superseded.

### Accepted control session headers (partial SESSION-CAPTURE delivery)

Frozen `streamable-http.ts::send` checks HTTP status and captures nonempty session headers before
ignoring a non-request body; initialized then opens its common GET with that metadata. The private
backend previously returned a fieldless Accepted before extracting the header. Desired-red run
`1beeef0e-cb04-4ea9-883c-65675c467532` compiled: **0 passed / 2 failed**. The actual common GET
used `initial` instead of `ready`, and a client-response POST lost its returned session header.

The private `PostResponse::Accepted(Option<String>)` now carries the same successful/nonempty
header metadata as JSON/SSE responses. Existing `Shared::post` cleanup capture and ordinary
`Shared::send` capture consume it; no new state, coordinator, dispatch owner or authority is added.
`into_sdk` still returns the unchanged public Accepted variant. Rejected HTTP responses still fail
before extraction; non-request bodies are ignored, and request 202/204 keeps its missing-reply
failure. No RPC response is manufactured. An ordinary control header uses the current transport
metadata path, not generation/connection publication or a new SDK service.

The first focused follow-up, `5f531ee4-d22c-4bd0-9c1e-069fadc0f04e`, ran **21 transport passes /
1 failure**, with **11 backend passes** and Clippy/fmt/diff all **0**. The new notification case
incorrectly downcast the SDK error to bare `HttpError`; the worker actually preserves
`Arc<HttpError>`. Only the fixture was corrected to use the existing typed `transport_error`
helper, retaining the exact 403 assertion. `f2460699-febe-4802-866e-3d773d10081f` then passed
**2/2 capture regressions**, with strict MCP all-target Clippy, workspace fmt and diff all **0**.

The SDK fixture awaits real notification ACKs, then verifies exact ordered method/session headers
on five independent Ping calls and four notifications: 200 updates the ID, 204 empty ID does not
clear it, 403 cannot capture its rejected ID, and absent headers retain it. It positively observes
the first common GET using the initialized-202 header, exactly one initialize, no replay, fake
Authorization/protocol headers and one DELETE using the final captured ID, under two-second
bounds. The lower backend fixture proves a client RPC response retains headers for 200/202/204
while ignoring invalid JSON bodies and preserving the public SDK Accepted result; it is not a
full delayed SDK-callback test.

Final affected-input run `3b724e0f-dd53-458d-9bcd-e502b640f491` passed all six independent gates:
MCP **330 library + 42 integration = 372 passed / 2 live ignored**, Coding MCP **47 passed /
49 outside the filter**, CLI MCP management **7 passed**, strict all-target Clippy for the three
affected packages, workspace fmt and diff **0**. Complete managed output and metadata are retained
at `.eval/project-checks/accepted-capture-3b724e0f.log`. These are this input's actual counts;
previous focused counts are not added. Full workspace, Node/native-bridge/PTY and official-suite
gates were not rerun, and the previously failed unfiltered depth-512 gate remains unresolved.

This closes only the tested ordinary non-request POST subcase. GET response-header capture was
still open at this checkpoint; the valid-SSE subset is addressed in the separate follow-up below.
Successful-HTTP errors while decoding request bodies or missing replies, repeated IDs, remaining
rotation/resume interactions and bootstrap-Pong active/protocol propagation remain open.
Accepted metadata reaches the existing cleanup-only hook too, but these tests do not establish
all candidate/cleanup races. #205 remains genuine pre-final Ping only; no broader dispatch, SDK
patch, persistence, Node, new agent, Git delivery or depth-512 repair is part of this step.

### Successful GET session headers (partial SESSION-CAPTURE delivery)

Frozen `streamable-http.ts::openSseStream` ignores unsupported GET 405, checks other statuses,
captures a nonempty session header and returns the SSE body. `buildHeaders` reads the transport
session ID for following traffic. The private GET result previously discarded all response
headers, and successive resume attempts kept the originating POST's old ID snapshot.

Desired-red run `481ee67e-e353-4366-ad2e-826a90044997` compiled and failed **0 passed / 2 failed**
with positive wire evidence: after a real common-stream notification, the independent SDK Ping
still sent `base` instead of `common-session`; the second response resume GET still sent `base`
instead of the first resume response's `resume-session`. Both actual SDK flows completed their
responses; these were exact header-trace failures, not silence or fabricated RPC failures.

`http_client/stream.rs::StreamResponse` now carries the actual body and optional session header
through the existing static/provider/OAuth private client seam. The public SDK adapter continues
returning only its original stream type. Existing dispatch/completion checks, local GET auth
allowance, status/content-type handling and bounded SSE decoding are retained. The worker uses
its existing cleanup and active metadata capture before consuming the returned body, and each
new opening reads this same transport's current ID while retaining the originating protocol
context and RequestControl. No new state, side map, latch, recovery coordinator, wire metadata,
SDK patch or control owner is introduced; changing metadata does not publish a replacement or
expand a callable grant. The test OAuth observer still wraps the actual stream lifetime while
preserving its returned header.

Focused run `e3395123-edb1-4ba3-b91e-46881a1691b6` passes **24 private transport tests and 11
backend tests**, MCP strict all-target Clippy, workspace fmt and diff checks (all six command
statuses **0**, including package formatting). The two new real-SDK fixtures retain two-second
bounds, exact auth/version and ordered POST/GET/DELETE traces, one Initialize, an independent
Ping, and no original RPC replay. The resumed case observes both real progress prefixes and
the final correlated tool reply across two cursor-bearing GETs; its unsupported common GET
with a nonempty rejected header cannot replace the current ID. The complete desired-red/focused
managed outputs are retained in `.eval/project-checks/get-capture-focused-e3395123.log`.

Affected-input run `ef08e296-9b3d-45b4-afa2-d66adeae93ec` completed with aggregate exit **1**:
MCP **332 library + 42 integration = 374 passed / 2 live ignored**, Coding MCP **47 passed /
49 outside the filter**, and CLI MCP management **7 passed**. Workspace fmt and diff returned
**0**. The sole recorded failed gate is three-package all-target Clippy (**101**): `E0063` at
`apps/pi-cli/src/config.rs:726`, where the test's `Cli` literal lacks `no_tools` and `offline`.
Read-only inspection found those new fields in unrelated uncommitted CLI changes; this MCP step
has not modified that file. `who_wrote` returned historical owners, not an identified writer of
that current diff, so no authorship or historical-causality inference is made. The complete
managed output and metadata are retained at `.eval/project-checks/get-capture-ef08e296.log`.
These are actual independent results, not accumulated focused counts. Gate closure is held for
the CLI change's ownership/resolution; no automatic source repair or duplicate run was started.

That checkpoint covered the tested successful GETs accepted as SSE, not all SESSION-CAPTURE
behavior. The later main-worktree follow-up below covers successful POST metadata before
content-type, JSON-body, missing-reply and JSON-RPC protocol failures. Repeated IDs, concurrent
rotation and metadata changes during credential waits, other resume/candidate/cleanup races, and
bootstrap-Pong active-session/protocol propagation remain open. IDs are selected when an opening is
created, not newly resolved after every asynchronous credential wait. The previous
unfiltered depth-512 failure remains unresolved; no Node/native bridge/PTY/official networking,
new agent, Git delivery or depth repair is included.

### Remaining authorization boundary

Work still within #204/#205 includes private HTTP response handling (JSON arrays and successful
session capture), caller-header folding, exact SSE syntax/accounting, and remaining bounded
repair/control/authority tests. Common challenge/diagnostic observation must retain its independent
meaning, never synthesize application failure or NeedsAuth. These rows remain open; the completed
supplements do not justify a full-parity claim.

Credential migration/persistence implementation, official network-suite execution or artifact
reacquisition, new agents, vendoring/forking and commit/land/push remain excluded. Credential
interoperability waits for completed, reviewed supported-subset identity/encoding/locking/conversion
design and implementation authority. Official conformance waits for approved artifact acquisition
and containment resolution. SDK cleanup trace is accepted/closed only at #219's d47bf1ef input
and four-scenario scope under #183; no global subscriber/filter/suppression workaround is
authorized. The known unfiltered workspace depth-512 SIGABRT remains a failed gate, not an
excuse to change stack limits or weaken coverage.

## Main-worktree follow-up after integration

The following evidence is from `/Users/bytedance/Documents/pi-rs`, starting at
`202eb9a0b5524ee1a23f8016191db1ea70a59f47`, not from a revived historical validation window.
Prior failed runs, HOLDs and missing-evidence limitations above remain historical facts.

- The 11 `http_client::tests::` HTTP/TLS cases passed on integrated main with offline/locked Cargo,
  loopback fixtures, fake tokens and proxy variables cleared.
- Initial `--lib sse` run `bdd31691-9847-4dd9-9aee-4b460b2d0a74` finished **22 passed / 8 failed**.
  The failures were the four common-stream eligibility cases, three terminal response-resumption
  cases, and the JSON/SSE connection test
  `client::tests::streamable_http_discovers_and_calls_over_json_and_sse`.
- Unlike the earlier ambiguous assertions, this main run emitted `forbidden-wire-observed` for
  all three terminal response cases: target response GET counts were 3 versus expected 2 (401),
  and 2 versus expected 1 (plain/scope 403). Each diagnostic still observed one target execution
  and a pending tool task. Cleanup/post-terminal probe assertions were not reached. This is
  current-run extra-GET evidence, not a retrospective explanation of older failures.
- The connection test had an obsolete generic-handshake expectation: frozen
  `runtime.ts:usesOAuth` attaches OAuth by default even without stored credentials. It now checks
  typed sign-in guidance for that path, ordinary handshake failure for rejected static headers,
  and redaction in both cases. No production authentication policy was loosened.
- `http_client/sse.rs` now preserves complete frames before an oversized suffix, while dropping
  the byte source and unread suffix before delivering the overflow error. A deterministic test
  first failed at the original whole-chunk boundary in `9f81a219-2f48-4fb1-acae-9246861aa6f5`.
  Focused run `82bed7dc-3db4-472b-9053-8bc60c45305c` then passed all five parser-boundary tests
  plus the corrected JSON/SSE connection test. New cases cover prefix delivery under several
  chunk partitions, split CRLF with empty chunks, and immediate source drop on overflow.
- First package-wide run `3dcb49c3-01d6-4967-babf-d2632d2b9516` completed with **264 library
  passes / 9 failures**, **35 integration passes / 2 failures**, and two explicitly ignored live
  tests. In addition to the seven reconnect failures, it exposed stale configuration/static-auth
  expectations and two lost-disconnect-diagnostic failures. These failures were not filtered out.
- `client/request.rs` now sends both SDK send failures and dropped-response-channel failures
  through the same typed diagnostic conversion. A dropped sender no longer exits through `?`
  before bounded stderr settlement; typed transport closure does not depend on the peer's closed
  flag winning a race. HTTP/auth/control errors retain their typed recovery information. Existing
  stdio fixtures prove the redacted tail, disconnected management state, frozen catalog and no
  implicit log/session files. Configuration tests now preserve caller transport headers and
  distinguish rejected static headers from actionable OAuth, matching the frozen oracle.
- Follow-up `4c86769c-2e96-4e73-b47d-7e89046d808e` passed **13 focused cases**: four config
  parser cases (including a new header-preservation test), one manager disconnect case, three
  configuration-value cases and five diagnostics integration cases.
- The workspace formatting check initially failed in 20 pre-existing files. Standard rustfmt
  formatting was applied without changing those files' behavior; rerun
  `bb3bbe40-68ee-4117-9443-71a6d11c1f45` passed formatting and `git diff --check`.
- Final affected-package run `296e6f48-12ce-4faa-8092-fdae1daea5f5` completed with **267 library
  passes / 7 failures**, **37 integration passes / 0 failures**, and two ignored live tests
  (**304 passed / 7 failed / 2 ignored** in total). Every remaining failure is one of the seven
  named SSE reconnect cases above. Cargo tests exited 101; strict MCP all-targets Clippy,
  workspace formatting, and `git diff --check` each exited 0. The combined check therefore
  correctly failed. Full-workspace/native/Node gates were not run for this private MCP patch;
  no wider pass claim or baseline-stack-overflow waiver follows.
- The seven reconnect failures remain open; neither the prefix/disconnect fixes nor corrected
  diagnostic assertions establish full HTTP/SSE parity. No SDK fork, second session worker,
  credential migration or Git promotion is part of this patch.

## Main-worktree recovery, parser and CLI-disable checkpoint

This checkpoint is from the same shared dirty checkout and does not convert local evidence into a
bare-commit or full-parity claim. It supersedes the stale OPEN implementation status for L4/L5/E3
and the stale partial parser descriptions above; Q1, authentication interoperability and E1/E2
were still open at this checkpoint.

- L4 now has product-owned typed recovery in `client/request.rs`. Each logical operation has one
  non-rearmable recovery allowance. Supervised resource read/list operations retry once only for
  typed 408/429/eligible 5xx failures, while a typed session-expiry 404 can replace the exact
  connection for either resource or tool requests. Tool calls never replay for ambiguous HTTP,
  network, auth or JSON-RPC failures. Aggregate resource listings discard the failed attempt's
  pages and cursor and restart from the first page. Deterministic regressions include
  `tools_never_retry_ambiguous_http_rpc_or_network_failures_even_with_readonly_hint`,
  `resources_retry_only_classified_statuses_and_at_most_once`,
  `aggregate_restart_discards_partial_pages_and_cursor_across_transient_or_expiry`, and
  `repeated_expiry_exhausts_without_eager_third_setup_then_next_call_opens_lazily`.
- L5 now replaces an expired/disconnected exact slot lazily and coalesces concurrent waiters. An
  admitted request retains its detached transport until its real JSON/SSE response finishes;
  replacement does not cancel unrelated in-flight work. Frozen callable grants are rechecked
  against the replacement inventory, so a descriptor change refuses execution before wire dispatch
  and waits for normal whole-generation publication rather than mutating a live registry. Explicit
  reconnect, cancellation of one waiter, generation retirement, failed replacement, shutdown and
  terminal session cleanup are covered by the recovery, supervisor and cleanup-trace fixtures.
- The custom SSE parser now matches the frozen parser's syntax and accounting boundaries described
  above. Its regressions cover all input partitions for complete-prefix/overflow ordering, split
  CRLF with empty chunks, immediate source release, malformed UTF-8 expansion, duplicate metadata,
  unknown/bare fields, EOF dispatch, decoded-data limits and cursor updates while a stream remains
  open. The private worker classifies the typed limit as terminal and does not impersonate an SDK
  private marker.
- E3 is wired from Clap to generation configuration. `--no-mcp` leaves automatic JS extension
  discovery and explicit `-e` sources intact while skipping configured MCP. `--no-extensions`
  disables automatic extension discovery and built-in MCP while still preserving explicit `-e`.
  ACP applies an additional `load_mcp_config = false` ceiling and continues to own only its
  transient per-session MCP list. The built-in option table prevents the Node extension adapter
  from stealing `--no-mcp`. CLI parser/config tests cover both flags and explicit extensions; the
  Coding disabled-generation fixture starts with an invalid `mcp.json`, executes no value helper,
  and publishes no `/mcp` command.
- Current affected evidence: the full locked MCP package run passed all 380 executed tests with two
  opt-in live tests ignored; the locked CLI library run passed 252; package typecheck and Node tests
  passed. After the latest parser/CLI changes, strict all-target Clippy for `pi-plugin-mcp` and
  `pi-cli`, workspace formatting, and staged/unstaged diff checks all exited 0. Full-workspace
  tests/Clippy, Node/native RPC or PTY, and official conformance were not run for this checkpoint,
  so lifecycle acceptance and Q1 remain open.

## JavaScript extension MCP loading and late-mutation checkpoint

This later checkpoint from the same shared dirty checkout first established the loading-time E1
path, then added a generation-safe late register/unregister transition and replacement-generation
observer delivery. It is not a claim that formal concurrent lifecycle behavior,
authentication-store compatibility, official conformance, or the full workspace is accepted.

- `packages/pi` now exposes typed `registerMcpServer`, `unregisterMcpServer` and `getMcpServers`
  APIs. Its generation-local registry validates stdio/Streamable HTTP, OAuth, provider auth,
  exposure and JSON serialization; preserves registration order; permits same-owner replacement;
  rejects exact and normalized namespace conflicts; ignores non-owner removal; and returns defensive
  clones. The initial loading checkpoint rejected runtime calls explicitly; the late-mutation
  transition described below supersedes that temporary restriction.
- The strict JS generation manifest carries MCP registrations with a default empty list for older
  hosts. `pi-js-plugin` retains them only after validating a nonempty source/name, object config and
  a matching agent-plugin owner. MCP registration `Debug` output redacts the entire config so
  headers, OAuth secrets, environment values, arguments and credential-bearing URLs cannot leak
  through manifest diagnostics.
- Coding prepares the JS manifest before the MCP candidate, then projects registrations through
  `McpExtensionServerDefinition`. Rust reparses each object with the authoritative MCP parser,
  rejects normalized collisions, preserves the extension path as resolution/source provenance and
  keeps extension entries out of disk management views. Configured global/project namespaces take
  precedence and produce an explicit override notice. Extension rows allow inspection, tool view,
  OAuth sign-in/out and reconnect, but do not offer persistence-backed enable/disable or exposure
  edits.
- Extension registrations participate in credential watching, supervised preparation, direct-tool
  publication and generation reload. A lifecycle-only empty candidate is retained when a JS host is
  present, without exposing `/mcp`, so removing the final registration publishes an empty snapshot
  and sends HTTP session cleanup instead of stranding the prior connection. A deterministic Coding
  fixture proves an extension HTTP tool connects while configured-file loading is disabled, an
  invalid `mcp.json` remains unread, reload removes the command/tool, and the old session receives
  cleanup.
- Current focused evidence: the locked MCP package run passed 382 tests with two opt-in live tests
  ignored; the locked `pi-js-plugin` run passed 49; the Coding library run passed 117 plus three
  builtin-contract tests before the known depth-512 integration stack overflow aborted the full
  integration binary. Targeted MCP configuration, reload and lifecycle integration runs then passed
  31 tests. `packages/pi` typecheck and five extension-MCP tests passed, as did workspace formatting.
  Full Node tests, affected-package Clippy, diff checks, full-workspace tests/Clippy, Node/native RPC,
  PTY and official conformance are not yet evidence for this checkpoint.

A follow-up hardening pass remains part of the same uncommitted checkpoint. Extension OAuth manager
actions now resolve generation entries rather than the disk-only management view, while persistent
edits remain unavailable. Tests cover the read-only extension action set and true source path,
OAuth identity lookup without persistence, stdio relative-cwd resolution against the session cwd,
and retirement of the prepared JavaScript generation when Rust rejects a hostile/older host's MCP
config. Node validation now rejects non-finite timeouts, whitespace provider names, and credential-
or fragment-bearing transport/callback/metadata URLs without echoing their secret markers. The MCP
package subsequently passed **385 executed / 2 live ignored**, `pi-js-plugin` passed **49**, and the
Coding library passed **118**. Targeted package typecheck and all five extension-MCP Node tests
passed; strict all-target Clippy for MCP, JS plugin, Coding and CLI and workspace formatting passed.
Affected-file staged and unstaged diff checks passed. The repository-wide diff check is still not a
clean receipt because the shared checkout has an unrelated trailing blank line in
`scripts/install-local-pi.sh`. The full package Node run was attempted and passed 133 tests but failed
four release tests because its then-current npm version did not match Cargo's workspace version; the
shared unstaged package version changed again afterward, so that run is retained as a failure, not
silently upgraded. Full workspace gates, RPC, PTY and official conformance remain open.

The late-mutation follow-up routes post-load Node register/unregister notifications through typed
session capabilities into `DynamicMcpRegistry`. Whole-session preparation reloads extension
factories, replays the ordered source-owned candidate, and runs the resulting manifest through the
same native MCP validation and generation transaction. The committed overlay is live-runtime-scope
local, survives ordinary reload, stays out of sibling sessions and is released with its scope.
Candidate commit/rejection, mutations that arrive during preparation, removed-source filtering and
config Debug redaction have focused unit coverage. Coding product tests prove automatic registration
publishes `/mcp`, ordinary reload retains it, a sibling omits it, automatic removal retires it, and an
invalid candidate preserves the published generation without replaying the rejected mutation. The
Node test proves runtime notification shape and next-generation replay. A subsequent observer slice
delivers the complete extension-server snapshot after replacement activation and suppresses false
events on ordinary reloads; its generation-bound timing is an intentional divergence from legacy's
retiring-generation callback. After both follow-ups, the Coding library passed **126**,
`pi-js-plugin` passed **50**, and `pi-session` passed **224** tests; the five focused Node
extension-MCP tests, affected-package all-target Clippy including `pi-napi`, and a rebuilt
Node/NAPI/RPC bridge run passed **42** tests with **2** intentional skips. The bridge run explicitly
removed an inherited `PI_RS_NATIVE_BINDING` override pointing at installed 1.0.0 bits; a preceding
targeted attempt using that stale binary failed on the expected unknown notification variant and is
not counted as current evidence. Connector-less reporting is an intentional non-applicable legacy
state because the native connector is generation-invariant: even with configured MCP disabled, the
existing Coding fixture connects an extension registration without an observer. A later lifecycle
slice expanded the dynamic-registry suite to **8** tests: dropping a prepared candidate preserves
only mutations staged after its revision, and closing the last live-scope lease before commit prevents
a late candidate from resurrecting overlay state. The focused Node suite now passes **6** tests and
proves retiring a replacement generation aborts an active MCP observer, blocks its post-abort context
operation, and invalidates the old callback ID. Generic session activation tests already establish
commit-before-`session_start` ordering. A product RPC regression that closed stdin while the
replacement observer was active first timed out after **15 seconds**, exposing a real lifecycle-gate
cycle: shutdown waited behind reload while only retirement could abort the callback. Manager shutdown
now marks admission closed and signals the in-flight replacement before awaiting the write gate;
dropping the unpublished candidate retires the JavaScript generation. A focused `pi-session` test
proves blocked reload preparation is dropped and returns manager `Closed`, and the same RPC regression
now exits in about two seconds without a post-retirement observer notification. A second core race
test proves per-session close aborts an unpublished reload with session `Closed`, removes only that
slot, and leaves the manager able to create and close another session. On these final lifecycle
inputs, the complete `pi-session` library passed **226** tests. The Coding library passed **128**
and builtin contracts passed **3** before the full integration binary again hit the already-known
codemode depth-512 stack overflow; focused MCP configuration/reload/lifecycle runs separately passed
**2 + 10 + 19** tests. All-target Clippy for `pi-session`, Coding, and MCP passed. After rebuilding
the local NAPI binding and unsetting the inherited `PI_RS_NATIVE_BINDING`, the complete bridge run
passed **43** tests with **2** intentional skips, including the EOF/observer regression. A subsequent
product RPC race holds the replacement observer in a UI confirmation, queues `new_session`, proves
there is no early response, then releases the observer and verifies that the queued replacement
publishes a fresh scope without inheriting the prior runtime MCP overlay. After adding that case and
rebuilding the then-current native and TypeScript artifacts, the complete bridge passed **44** tests
with **2** intentional skips; the earlier 43-test receipt remains the historical pre-case checkpoint.
That checkpoint also had a **226**-test `pi-session` receipt. It was invalidated by two later close
races and is retained as history rather than described as the latest session source.

The first later desired-red held a grandchild replacement while closing its owner and timed out with
`Elapsed(())`: marking only the root slot closing could still wait forever behind a descendant that
held the manager-wide lifecycle guard. Close now recursively marks and cancels the currently owned
isolated tree before waiting for that guard, then re-walks the tree under the guard before shutdown.
The regression proves the descendant reload returns session `Closed`, the owner/child/grandchild are
all removed, and an unrelated sibling can still be created and closed. A second regression pauses a
candidate after activation but before `session_start` publication and proves close cancels it without
publishing over the old closed generation. On those later inputs the complete `pi-session` library
passed **228** tests. The Coding library again passed **128** tests; its runtime-extension MCP
regression now separately stages an overlay before `new_session`, resume and fork and proves every
fresh replacement allocates a scope without importing that overlay. Combined all-target Clippy for
`pi-session`, Coding and MCP, workspace formatting and the unstaged repository diff check passed.
After rebuilding native and TypeScript artifacts again, the complete bridge still passed **44** tests
with **2** intentional skips. This is current dirty-snapshot evidence, not a bare-commit or
full-workspace acceptance. Broader concurrent resume/fork, isolated attach/detach and CLI/ACP
lifecycle matrices remain open.

A further SESSION-CAPTURE follow-up keeps the public SDK response/error types unchanged while giving
the private POST envelope a response-header observer. The desired-red regression first retained
`previous` instead of the successful missing-reply response's `missing` ID. The observer now updates
the active and cleanup identities immediately after successful status and header validation, before
content-type, bounded JSON-body, missing-reply or JSON-RPC protocol checks; rejected status responses
with a session header remain non-authoritative, and repeating the current nonempty ID is stable. The
full locked MCP package then passed **388 passed / 2 live ignored**, its **23** private transport
tests passed, and MCP all-target Clippy plus workspace formatting passed. The concurrent fixture
also proves delayed completion of an older malformed body cannot overwrite a newer header capture.
A deterministic credential-wait wrapper proves ordinary POST and GET select the latest active ID at
actual dispatch; the same fixture verifies the POST protocol header and captures a successful GET
session header before rejecting its content type.

The stream-affinity follow-up separates those late-bound ordinary dispatches from controls caused by
an already-open body. `post_on_stream` binds bootstrap Pong to that body's opening session and
protocol context. The first direct fixture expected the initialize request's proposed version on the
pre-final Pong and failed because the header was absent. A temporary propagation of that proposed
version then failed **5** existing recovery cases. This preserved historical failure evidence and
confirmed the frozen contract: the version is negotiated only after initialize succeeds. The final
regression rotates the global snapshot to an unrelated session and newer version while Pong waits,
then proves Pong uses the opening session ID and leaks no unnegotiated or unrelated protocol header.
The existing setup fixture and repaired-session bootstrap tests pass with that rule. A resume opening
uses the latest session ID after its credential wait but retains the originating stream's protocol
version and event cursor. Response-tail forwarding likewise retains the opening negotiated protocol
after global rotation while still observing the returned tool schema and parameter headers.

On those transport inputs, the complete locked MCP package passed **391 tests with 2 live tests
ignored** (**349** library tests); the private HTTP transport module passed **24** tests, its SSE
submodule passed **2**, and the complete recovery module passed **60**. That receipt remains the
historical checkpoint immediately before the two follow-ups below.

Response-tail cancellation now has a deterministic post-primary-reply regression. Once the SDK has
correlated the primary tool result, cancelling the request sends `notifications/cancelled` with the
opening session authority, drops the blocked remainder of that response body, and prevents its
progress notification from escaping. A second independent request succeeds on the same initialized
service, proving cancellation neither poisons the service nor triggers another initialize. The
private HTTP transport module then passed **25** tests. Session-expiry coverage now also holds an
admitted JSON body and replacement SSE body across retirement: no DELETE occurs before both drain,
the retired transport cleans only its captured session, and owner close subsequently cleans the
replacement in exact `session-1`, `session-2` order. Repeated expiry cleans the first two sessions,
does not eagerly open a third setup after the logical allowance is spent, and opens `session-3` only
for the next logical call. The complete focused recovery module remained **60** tests.

The exact-current locked MCP package now passes **392 executed tests with 2 opt-in live tests
ignored**, including **350** library tests. Combined all-target Clippy for `pi-session`, Coding and
MCP, workspace formatting, and the unstaged repository diff check pass on these inputs. After
rebuilding the current native and TypeScript artifacts, the complete Node/NAPI bridge, with the
inherited installed-binding override removed, passes **44 tests with 2 intentional skips**; it now
includes `--no-mcp --no-extensions --extension <explicit>` and proves
that both automatic/config disable ceilings preserve explicit runtime MCP registration, whole-
generation reload, and removal. The native build emitted only the known macOS linker warning that
`__eh_frame` is too large for compact-unwind offsets.

The Node host's legacy offline propagation is now bounded explicitly. A command or eval host
normalizes `PI_OFFLINE` and `PI_SKIP_VERSION_CHECK` only for its run and restores both on success or
failure. Because these variables are process-wide, starting an offline host while any other
`PiNodeHost` is active, or starting another host while an offline host is active, fails before the
native binding runs instead of leaking mode across sessions. The guard does not serialize online
hosts or claim broader multi-host safety for unrelated process globals such as RPC stdout routing.
Three focused tests cover command/eval success, native failure and both overlap directions. Package
typecheck and those tests pass. The full package test run records **137 passes / 4 failures**: all
three new tests pass, while the same four release tests reject the shared Cargo `0.8.6` versus npm
`2.0.0` version mismatch. That run remains a failure and is not upgraded to a package-wide pass.
`--offline` still suppresses selected automatic startup network operations; it is not a network
sandbox and remains independent of `--no-mcp`.

A subsequent isolated-tree acceptance regression pauses a child acquisition in `session_start`,
starts owner close while the child is still absent from the ownership registry, then lets acquisition
register. Close remains bounded, its under-gate second walk finds and closes that newly registered
child, the manager has no orphan session, and an unrelated sibling can still be created and closed.
The complete `pi-session` library now passes **229** tests. Combined all-target Clippy for
`pi-session`, Coding and MCP, workspace formatting and the unstaged diff check were refreshed after
this test addition and pass.

All results in this section are dirty-snapshot receipts, not bare-commit, full-workspace, PTY or
official-conformance acceptance. Broader session-expiry/candidate cleanup and resume/control races,
formal concurrent lifecycle and L4/L5 acceptance, OAuth-store interoperability, and Q1 remain open.
