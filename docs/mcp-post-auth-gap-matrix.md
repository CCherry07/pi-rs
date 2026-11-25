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
| L4 | `runtime.ts:request`: one transient HTTP retry for resource reads/listings, never tool calls; expired-session 404 allows one request retry on a new session without cancelling other in-flight requests | No product-owned retry/session-expiry contract over rmcp has been established | **OPEN**. Local POST/GET counts; mutating tool not repeated on ambiguous failure; expired-session concurrent calls share reconnect safely |
| L5 | `runtime.ts:getClient/handleClientClose/reconnect`: disconnected state, lazy reconnect on next call, coalesced connect, explicit reconnect and shutdown lifetime | Published `McpToolSet` holds fixed services; manual reconnect currently probes then reloads | **OPEN**. Transport replacement behind frozen descriptors; schema changes need a new generation, no silent live registry mutation; cleanup/parallel callers/failed retry |
| L6 | `runtime.ts:refreshTools` + `index.ts:registerTools`: tools/list_changed adds/updates/retracts tools, retaining old inventory on failed refresh | Client handler does not observe list-changed notifications | **IMPLEMENTED/VALIDATED for supervised Coding; lifecycle acceptance OPEN**. Coalesced stable/modern refresh, whole-generation schema publication, frozen active views and retained inventory on error. See checkpoint evidence below |
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
  Focused reproduction `41ade74f` also overflowed. @Suzuya reproduced the identical failure with the
  exact focused command on clean `main@f176fc33c` in `1e2aeabd-2ad8-42de-8309-70eb04359c1b`
  (exit 101/SIGABRT), confirming a pre-existing baseline/toolchain blocker rather than diagnostics
  spillover. No depth contract, stack environment or unrelated test source was changed. The lead
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
non-success POST responses into a formatted string; `9e07746d` failed the three transient cases.

@Suzuya approved a minimal, explicitly pinned compatibility patch after verifying that rmcp has no
public successful-response injection/decoding seam. Patch only the non-success response metadata;
keep successful bodies, SSE decoding, OAuth and session recovery unchanged. Do not parse error
strings, edit the Cargo cache, or add independent SDK retry policy. The source is vendored from the
checksum-verified locked `rmcp-3.3.0` crate; provenance, reviewed delta and removal criterion belong
in `vendor/rmcp-3.3.0/PI-PATCH.md`. The approved supervisor and L1/L2/L4–L6 are not yet implemented.
L3 is implemented locally under the explicit validation exception below; no landing/full-parity claim.

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
overflow/SIGABRT—the same clean-main baseline reproduced by `1e2aeabd-2ad8-42de-8309-70eb04359c1b`.
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
verification and staged/unstaged diff checks passed independently. This is the previously reproduced
clean-main failure (`1e2aeabd-2ad8-42de-8309-70eb04359c1b`), not an unfiltered workspace pass.
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

L4/L5 request lifetime, E1–E3 and Q1/A3 remain open; these local regressions are not official-conformance
evidence. Neither this checkpoint nor its narrow exception marks #132 or lifecycle parity complete.

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
