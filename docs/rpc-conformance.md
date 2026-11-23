# RPC conformance: implementation and acceptance matrix

## Scope and evidence

This is task #15 / implementation #18, separate from prompt/tool alignment. Implementation starts
from Rust source baseline `a04d6779` (formerly `88e16a424c2e20e84c18b600ebeca5aec7e352e0`);
current clean parent is `97876b3f54f0de71666a48c24403628153b01412` (formerly `7c648d99`).
A user-authorized metadata-only rewrite removed internal commit trailers; all corresponding trees
and working edits stayed byte-identical, so the rewrite does not invalidate source test evidence.
The oracle is local Pi commit
`05fd3fbda0bbb9a1c7a76e4bf92ab58ed77c8d94`. Command-name coverage (33/33) is not behavioral parity.

Evidence levels used below:

- **S**: inspected current source, not executed equivalence.
- **O**: actual legacy function executed to generate an oracle fixture.
- **R**: Rust/Node owning-seam regression against that fixture, including real-host field checks;
  not a complete cross-product wire comparison.
- **D**: complete real RPC endpoints driven with the same scenario and compared. The current
  harness compares pinned legacy separately with the standalone Rust and Node-hosted Rust endpoints.
- **P**: deliberate product divergence, never normalized away.
- **Pending**: planned acceptance, no passing claim.

Unless explicitly upgraded in the validation log, rows below are **S / pending**. No installed
user binary, global package, live credentials, paid provider, or user session is part of this work.

## Current delivery boundary

Automatic opening/migration of legacy v3 history has been explicitly deferred to backlog #30.
This task does not add v3 in-place writes or automatic v3→v4 switching. Unsupported v3 files must
produce a clear error without changing the source or current session. The existing explicit import
feature is retained, but is not claimed as a proven lossless substitute. Shared compaction metadata,
current v4 replay, command/event behavior and tool-result fixes remain in scope.

## Latest bounded evidence

The usage/history candidate `0m8owkhu/mffvir39` was tested once per scenario against the pinned
oracle through the standalone Rust and Node/NAPI entrypoints. The first three cancellation
scenarios passed before the remaining 25 ran on the same immutable products. Across all 28
controlled schedules, every complete normalized JSON record agrees and all 84 individual
endpoint behavior checks pass. The unchanged strict comparison reports **24 passed, 1 failed,
3 blocked**.

| Scenario group | Strict D result | Observed behavior |
| --- | --- | --- |
| `ui-abort`, `abort-before-headers`, `ui-client-cancel` | All passed; 30 / 17 / 7 records | Streaming usage, actual history, terminal fields and cancellation agree. |
| Other 21 passing scenarios | All passed | Includes `queue-order` (72 records), `abort-continues-queue` (50), `queue-lifecycle-only` (46), and `stream-batched` (20); the earlier usage/history differences are resolved. |
| `backpressure-term` | Failed | Three complete records agree; all exits are 143. Incomplete UTF-8 tails remain 131,072 legacy bytes versus 458,752 for each Rust endpoint. |
| `ui-eof`, `ui-hup`, `ui-term` | Blocked | Three complete records and exits 0 / 129 / 143 agree. Candidates observe `ui-released(false)`; legacy has no release marker at held shutdown or final capture. |

Initial tool selection now lives in optional v4 header metadata, preserving absent, explicitly
empty and restricted selections without an initial tools-change tree entry. Subsequent actual
changes keep precedence. Provider lookahead exposes only already-observed response metadata/usage;
content and terminal events advance at FIFO handoff. The real JS bridge regression verifies an
empty first content seed followed by text/tool deltas applied exactly once.

The stable implementation passed full workspace tests, all-target Clippy with warnings denied,
formatting and diff checks (`13d22748`), Node package checks, and all 43 native bridge smoke tests
with no skips. The actual host build traced newly compiled Agent/Session/RPC objects in NAPI and
their reuse by the CLI. Independent acceptance checked 176 source inputs, 171 candidate files,
runtime/launch pins and directory guards. A cross-run raw index change received a separate semantic
and current-input verification; the original pins and both raw index snapshots remain preserved.

The first-stage manifest SHA-256 is
`7a7708fb460dd739e3e238f956ae75dfaf58e71c2b4d5242b1d79b05be922fdf` (`332e4eb4`), and the
remaining-stage manifest is
`dead18388ad9299d2680aba561fba861ca3bb3a79b5b0a44ee8eb4eacb68f529` (`1b9d204e`). The owner
independently verified all 1,101 original evidence files across these two stages. The final
offline readback recomputed the remaining 75 normalized streams from raw bytes with the unchanged
comparator (`0f4bb4c1`, receipt SHA-256
`c158dc59b9c2d020d5f85e960d4b2789ef9b992eb3d8aee3854ed80e32de8ea2`).

### Termination and dialog observation limits

Both implementations await disposal and skip the final stdout drain on SIGTERM. The backpressure
driver resumes reading immediately after signal submission, without acknowledging that the child
has handled it. Node's callback-driven stdout and Rust's blocking writer therefore have no shared
byte-level stopping point. The recorded tails also differ in model-object key order starting at
byte 94. Their exact lengths are observations, not a transport guarantee. Moreover, the fixed
legacy capture itself has an incomplete UTF-8 error; the strict predicate rejects either endpoint's
parse errors, so even an identical candidate tail would leave this comparison failed. The raw
failure remains recorded. EOF/HUP backpressure scenarios require and receive the complete response.

The background UI fixture supplies neither a cancellation signal nor a timeout. Pinned Pi settles
that dialog on a matching response, a supplied signal, or a timeout; its RPC shutdown/disposal path
does not settle the pending dialog map. Rust connection closure resolves pending dialogs as
cancelled, preserving its generation/disconnect cleanup guarantee. Code after the awaited dialog
is observable to an extension, so this is a lifecycle compatibility boundary. Missing legacy
markers establish neither successful settlement nor a leak after process exit. All three strict
comparisons remain blocked.

The demonstrated scope is complete normalized records and the individual behavior assertions in
these 28 schedules. Exact TERM suffix bytes, matching background-dialog callback side effects before
exit, arbitrary-schedule equivalence and full RPC parity remain outside that demonstrated scope.
Historical strict failures and blocked observations below retain their original conclusions.

## Earlier bounded evidence

The transcript/queue freeze `0rfq2gpx` was tested against the pinned oracle through all three real
entrypoints. Its complete ordered stdout comparisons remain immutable:

| Scenario group | Strict D result | Scope |
| --- | --- | --- |
| `queue-lifecycle-only` | Passed; 46 records per endpoint | Abort retains the pending follow-up; a new prompt reaches the third provider request; queue consumption precedes user start/end. No history-query inputs. |
| Remaining 27 scenarios | 17 passed, 7 failed, 3 blocked | All 81 individual target behavior checks passed without timeouts; that does **not** turn failed or unobserved cross-product comparisons into passes. |

The queue-only result does not supersede the original 50-record `abort-continues-queue` history
failure or establish general RPC parity. In the 27-scenario run:

- `abort-continues-queue`, `clear-then-abort` and `abort-before-headers` differ only in history
  containing the native `pi-rs.active_tools_change` checkpoint. Native v4 history is preserved;
  the extra record remains a strict difference, including its effect on tree identities.
- `queue-order` and `stream-batched` expose intermediate legacy live-reference content/usage
  timing, plus history differences. Event order and final values do not justify backfilling future
  data into earlier Rust snapshots or asserting arbitrary-schedule equality.
- `backpressure-term` preserves unequal incomplete UTF-8 tails (131,072 legacy bytes versus
  458,752 for each Rust endpoint), despite the same exit code 143. EOF/HUP backpressure passed.
- `ui-eof`, `ui-hup` and `ui-term` have equal stdout and expected exits, but the legacy fixture
  supplies no waiter-resumption marker. Comparative waiter cleanup is unobserved, not an inferred
  leak or success. `ui-client-cancel` passed.
- `ui-abort` has equal 30-record event ordering, tool identifiers/arguments and final tool-call
  assistant data, and all three endpoints observe `ui-released(false)`. The full stream still
  fails: besides intermediate usage and history, it exposed a missing nested `toolCall.type`,
  omitted scheduler-error `details: {}`, and incorrect final pre-request abort assistant metadata.

Those three concrete `ui-abort` field defects now have owning-seam red/green regressions and
repairs. The projector retains the tagged content block; only scheduler-generated errors receive
empty details; pre-request cancellation obtains API/model/thinking/timing from the current run,
not RPC decoration. Tool-returned absent/null/empty details and the distinct already-started
provider cancellation paths remain covered. Full workspace tests, all-target Clippy, formatting
and diff checks passed on the repaired code (`471506cd`, receipt SHA-256
`7bf80280d441115e7e315480d401f07d4a6c823c992f3be912b855c4e7ce79cd`).
The subsequent freeze `eyulzpjw` nevertheless produced **2 failed / 1 passed** strict narrow D
results: `ui-abort` and `abort-before-headers` failed; `ui-client-cancel` passed. All three repaired
fields were still wrong in the actual `ui-abort` products. Dependency traces then demonstrated
that the explicit-target CLI/NAPI host graph had reused stale Agent/RPC objects, unlike the
owning-test graph. Source hashes, passing tests and SDK compatibility alone had not proved that
the repaired implementation was linked. The origin of the earlier source mtimes remains unknown.

After preserving the old artifacts and advancing only the three demonstrated-stale inputs'
mtimes, the actual NAPI host graph recompiled both owning crates and the CLI reused those proved-new
objects. A strengthened, explicitly registered-model bridge regression failed against the immutable
old binding and passed against the rebuilt one. The rebuilt standalone CLI/native fixture and
Node endpoint then each passed field checks over all 30 raw `ui-abort` records, using the unchanged
acceptance driver and its real confirmation barrier. The checks cover the tagged tool call,
scheduler details and all four terminal/history surfaces, current API/provider/model/thinking/error,
empty aborted content, zero usage/cost, and present nonnegative response duration. No unexecuted-tool
duration or extra provider request was introduced. Current Node typechecking, 131 package tests and
42 bridge smoke tests (both opt-ins enabled, no skips) passed.

The new immutable freeze `txrkmgiq` subsequently completed an independent three-scenario D run
(`84df9fd9`, manifest SHA-256
`0dfac3a09a2325f4c68bcbb979a7db6de3c3995085ba1800d3c0ea59da69bfc6`). The three repaired
field contracts passed on both Rust endpoints across the actual terminal/history surfaces. All nine
target behavior checks passed with clean exits and complete streams. Full-stream strict results are:

| Scenario | Strict D result | Remaining differences (zero-based record indices) |
| --- | --- | --- |
| `ui-abort` | Failed; 30 records per endpoint | Records 9/10 contain different intermediate usage; record 28 contains the native v4 history checkpoint. The repaired terminal fields match. |
| `abort-before-headers` | Failed; 17 records per endpoint | Only record 16 differs, in history containing the native v4 checkpoint. |
| `ui-client-cancel` | Passed; 7 records per endpoint | Complete ordered streams match. |

Rust and Node normalized streams match each other in all three cases. The owner independently
verified the recorded raw streams, repaired fields, launch/product pins, 169 source inputs and
164 candidate files. These bounded results preserve the full-stream failures and the earlier
matrix's unobserved cleanup cases; they do not establish complete RPC parity.

For provenance, the queue manifest SHA-256 is
`9c2fd3535ac9d66bd4cc2137e7029e9d04f88980d45bea8ce8420eb929485e67`; the 27-scenario
manifest SHA-256 is `963283272830b0669c644fc3eac69633abc794d29811ade84df359e949de2499`.
Both include source/build/SDK/launch pins and complete stdout/stderr. The fixture descriptor
fingerprint was `a9d83174d1378400` (ABI 36), matched by both hosts; matching ABI alone is insufficient.
No full-protocol approval, commit/land, release or user installation is claimed by these results.

## Entry points and source keys

All paths below are relative to the respective checkout, not implicit dependencies on a particular
machine's main worktree.

| Key | Source |
| --- | --- |
| L-mode | `legacy/packages/coding-agent/src/modes/rpc/rpc-mode.ts` (`runRpcMode`) |
| L-types | `legacy/packages/coding-agent/src/modes/rpc/rpc-types.ts` (`RpcCommand`, `RpcResponse`) |
| L-json | `legacy/packages/coding-agent/src/modes/json-event.ts` (`toJsonEvent`) |
| L-session | `legacy/packages/coding-agent/src/core/agent-session.ts` |
| L-runtime | `legacy/packages/coding-agent/src/core/agent-session-runtime.ts` |
| L-history | `legacy/packages/coding-agent/src/core/session-manager.ts` |
| L-agent | `legacy/packages/agent/src/agent-loop.ts` |
| R-mode | `crates/pi-rpc/src/rpc.rs` (`dispatch`, `run_connection_with_shutdown`) |
| R-json | `crates/pi-rpc/src/json_wire.rs` |
| R-session | `crates/pi-session/src/runtime/` |
| R-history | `crates/pi-session/src/history/` |
| R-management | `crates/pi-session/src/management/` |
| R-cli | `apps/pi-cli/src/lib.rs` RPC mode wiring |
| R-node | `packages/pi/src/index.ts` → `bindings/pi-napi/src/lib.rs` → R-cli |
| R-ui | `packages/pi/src/extension-ui.ts` and `crates/pi-rpc/src/ui.rs` |

Both R-cli and R-node use R-mode. Only R-node hosts JavaScript extensions. Each command row refers
to its corresponding switch arm in L-mode and R-mode; downstream owners are given explicitly.

## Universal command acceptance pack

Apply to **every row**, not only one representative command:

1. Required parameters, minimal valid request, optional parameters omitted and supplied, empty
   strings/arrays where accepted, extra fields, and explicit `null` separately from omission.
2. Absent, empty and ordinary string `id`; one correlated authoritative response, no extra `data`
   on no-data success, explicit `data: null` where the operation returns null.
3. Missing required parameters, wrong types, unknown commands, invalid enum values and numeric/null
   IDs: record actual parser/handler failures and resulting state. Rust's strict typed decoding
   versus legacy's unchecked JavaScript cast is a separate invalid-input class, not evidence of
   equivalence for supported requests. Do not remove typed validation merely to copy an accidental JS crash.
4. Applicable idle, preflight, streaming, tool, compaction, retry and replacement states; on rejected
   mutations verify the old state remains usable. Compare event and response order in one mixed stream.

## All 33 commands

| # | Command / parameters | Exact operation acceptance / known difference | Legacy downstream → Rust owner |
| --- | --- | --- | --- |
| 1 | `prompt(message, images?, streamingBehavior?)` | Text/images/empty; started/queued/handled preflight; preflight failure responds once; post-accept stream failure never adds a second response; source=rpc | L-session.prompt → R-session admission/delivery/run |
| 2 | `steer(message, images?)` | Idle/running; transform/handled/throw input hooks; source=rpc; images preserved; ordered queue | L-session.steer → R-session activity/queue |
| 3 | `follow_up(message, images?)` | Same input cases; steer precedence, one-at-a-time/all draining, continuation before settlement | L-session.followUp → R-session activity/queue/run |
| 4 | `abort` | Idle idempotence; preflight, stream, tool and before-settle cancellation; await idle before response | L-session.abort → R-session activity |
| 5 | `clear_queue` | Empty, multiple and duplicate-text items; ordered steering/followUp arrays; no later ghost input | L-session.clearQueue → R-session activity |
| 6 | `new_session(parentSession?)` | Absent/empty/nonempty parent metadata (no file existence requirement); cancelled hook and failed preparation preserve old session; startup ordering | L-runtime.newSession → R-management replacement |
| 7 | `get_state` | Fresh/stream/retry/compact/replaced; optional session fields, counts, full model metadata | L-session getters → R-mode state/R-session views |
| 8 | `set_model(provider, modelId)` | Existing/missing/same/scoped models; thinking clamp, selection hooks and state persistence; metadata gap | L-session.setModel → R-session configuration + model catalog |
| 9 | `cycle_model` | Zero/one/multiple/scoped candidates; single already-selected candidate gives null, single candidate with different current model selects it; metadata gap | L-session.cycleModel → R-session configuration |
| 10 | `get_available_models` | Ordering, scope/auth visibility using dummy credentials only, full metadata | L-session.modelRuntime → model catalog + R-mode model_json |
| 11 | `set_thinking_level(level)` | Every valid level, non-reasoning models, clamp and selection hooks | L-session setter → R-session configuration |
| 12 | `cycle_thinking_level` | Null or supported cycle in exact order | L-session cycle → R-session configuration |
| 13 | `get_available_thinking_levels` | Per-model available levels and ordering | L-session getter → model/configuration owner |
| 14 | `set_steering_mode(mode)` | Both enum values, no-data success, resulting drain behavior | L-session setter → R-session queue |
| 15 | `set_follow_up_mode(mode)` | Both enum values, no-data success, resulting drain behavior | L-session setter → R-session queue |
| 16 | `compact(customInstructions?)` | Absent/empty/text instructions; default/hook/cancel/failure/retain-none; runtime estimatedTokensAfter separate from entry metadata | L-session.compact → R-session compaction + R-history |
| 17 | `set_auto_compaction(enabled)` | False/true then threshold/overflow behavior | L-session setter → R-session compaction |
| 18 | `set_auto_retry(enabled)` | False/true, retryable/nonretryable failures and exhaustion | L-session setter → R-session run |
| 19 | `abort_retry` | Idle idempotence, abort delay, no later provider request | L-session.abortRetry → R-session activity/run |
| 20 | `bash(command, excludeFromContext?)` | Empty/success/nonzero/Unicode/large output; absent exclude=false vs true; user_bash override; request ID | L-session executeBash → R-session shell |
| 21 | `abort_bash` | Idle/active/exited-but-output-open/disconnect; process cleanup | L-session.abortBash → R-session shell |
| 22 | `get_session_stats` | Whole tree, assistant/tool/summary/usage totals; changing branch does not erase billed history | L-session stats → R-history + R-mode aggregation |
| 23 | `export_html(outputPath?)` | Default/explicit/unwritable path; returned path and rendered contents | L-session exporter → injected app exporter |
| 24 | `switch_session(sessionPath)` | Native v4, missing/invalid/cancel/prepare-failure/repeated source; v3 direct opening remains an explicit unsupported boundary, with source/current session unchanged | L-runtime switch → R-management; automatic legacy import deferred |
| 25 | `fork(entryId)` | Before selected user; unknown/non-user/cross-branch/unsaved/cancelled; cancelled hook omits undefined selected text, never substitutes empty text | L-runtime fork → R-management |
| 26 | `clone` | Empty leaf error, current leaf at-position, cancellation | L-runtime fork → R-management |
| 27 | `get_fork_messages` | All-tree users, entry IDs, truthy text filtering without trim (whitespace text remains) | L-session getter → R-history |
| 28 | `get_entries(since?)` | Absent/first/middle/last/unknown/null cursor; append order, leafId and every entry kind | L-history entries → R-history/R-json |
| 29 | `get_tree` | Branches, labels, null roots, deep trees, orphan/error handling; exact child ordering | L-history tree → R-history/R-mode tree |
| 30 | `get_last_assistant_text` | Empty/whitespace → {}; trim joined blocks; error/aborted nonempty text returned; normal stop without text does not fall back | L-session.getLastAssistantText → R-mode helper |
| 31 | `set_session_name(name)` | Reject blank, normalize CR/LF runs, trim, restore through session_info | L-session/L-history → R-session configuration/R-history |
| 32 | `get_messages` | Live/replay/custom/context_edit/compaction, role and unknown message-wire extension preservation | L-session.messages → R-history projection |
| 33 | `get_commands` | Extension/prompt/skill ordering; collisions/invocationName/sourceInfo; description omission | L-mode catalogs → R-mode catalogs |

## Events, transport and extension UI

| Acceptance group | Oracle → owner | Regression requirement / known gap |
| --- | --- | --- |
| Text/thinking/tool-call stream | L-agent + L-json → R-json | Exact delta payload and usage; message/turn/agent lifecycle, no reordered mixed response stream |
| Tool scheduling | L-agent → pi-agent ToolScheduler | Controlled barriers force reproducible sequential/parallel completion; durationMs on actual execution (including throw), absent on immediate prepare/unknown/blocked paths; duration also in toolResult messages |
| Structured tool output | L-agent + L-json → R-json | Preserve missing vs null/false/empty/array/object structuredContent in partial and end results; do not parse text as substitute |
| Compaction | L-session.compact/autoCompact → R-session compaction/R-json | estimatedTokensAfter on runtime result/event; fromHook and systemMessage in stored/history entries; not interchangeable projections |
| Model metadata | legacy ai/types + model-config/provider-composer → pi-core ModelSpec/model plugins | promptCache and samplingParamsByThinkingLevel must survive catalog, selection and RPC serialization |
| Extension failures | L-mode onError → plugin diagnostics/session events/R-json | Exact extensionPath/event/error, one occurrence, failures isolated and later hooks still called; malformed patches too |
| Startup/replacement | L-mode rebind before bindExtensions → generation binding | session_start emits more than bounded capacity before connection-ready: startup completes and no event is lost; do not subscribe only after activation |
| Backpressure | L-mode agent subscriber + output-guard → session-owned RPC delivery | More than 512 agent events with stopped reader then recovery: no gap or missing transient notices; await actual writer progress. Synchronous extension bursts use legacy-style ordered queuing and are not claimed hard memory-bounded |
| Cancellation/lifetime | L-mode shutdown → R-cli/R-node/R-mode | No-subscriber does not block; old generation simultaneously has an await-ack waiter and pending UI during retire/cancel/disconnect; both released without startup/retirement cycle |
| Process framing | legacy rpc/jsonl → R-mode/ui/output | LF/CRLF/final no LF, Unicode separators and partial UTF-8, malformed records, slow reader, intermediate disconnect |
| Process signals | L-mode signal handlers → R-cli rpc_signals/R-node | Actual subprocess EOF/dispose; SIGHUP flush and 129; SIGTERM may omit flush and 143; repeated signal forced exit; test both pressured stdout and pending UI |
| Dialog UI | L-mode createExtensionUIContext → R-ui | select/confirm/input/editor success/empty/cancel/timeout/pre-abort/wrong ID/duplicate reply/retirement; notification/dialog/response order |
| UI notifications | L-mode UI → R-ui | notify/status/widget/title/editor-text exact wire; widget only array/undefined; missing vs empty vs null |
| RPC-only UI defaults | L-mode UI → Node RPC facade | getAllThemes=[], getTheme=undefined, onTerminalInput returns no-op unsubscribe, setTheme failure object for any theme, editor/tools defaults and no-op terminal-only methods |

## Entry notification provenance (implemented, with owning regressions)

At the pinned oracle, persisting an entry does **not** imply emitting `entry_appended`. This is
independent of whether an entry has a model-change, custom, or compaction payload. Do not delete
history records or filter purely by payload type to hide extra events.

| Legacy source in L-session | Persists entry | Emits entry_appended | Current Rust publication owner |
| --- | --- | --- | --- |
| Agent `message_end` persistence, 1216–1240 | Standard/custom messages | No; normal message lifecycle already emitted | runtime/bridge.rs |
| `setModel` / model cycling, 2667 / 2730 / 2761 | Model change (even same model) | No | runtime/configuration.rs + events.publish_configuration |
| Thinking selection / active-tool configuration | Configuration/system state | No extra entry notification | runtime/configuration.rs and context initialization |
| `setSessionName`, 4116–4121 | session_info | No; emits session_info_changed | runtime/configuration.rs:set_name_locked |
| Extension setLabel, 3590–3591 | label | No | runtime/context.rs:set_label; navigation label |
| `_appendCustomMessage`, 2513–2523 | custom_message | No; emits message_start/message_end | runtime/context_updates / delivery |
| Manual/automatic compact, 3021 / 3347 | compaction | No; emits compaction lifecycle | runtime/compaction + context_updates |
| User shell persistence, 4071 / 4103 | bashExecution message | No | runtime/shell + context_updates |
| Accepted boundary drafts, 1020–1092 | custom/custom_message/context_edit/compaction | Yes, each committed draft | boundary/context_updates |
| Extension appendEntry, 3577–3583 | custom | Yes | runtime/context.rs:append_custom_record_from_plugin |
| Failed-attempt omission, 1335–1338 | context_edit | Yes | runtime/context.rs:omit_failed_assistant |
| Virtual-model state update, 895–899; cache warmer, 533 | custom state / usage | Yes | Native equivalent not established by this matrix |

Transient `EntryAppendOrigin` now records frontend-neutral publication provenance, preserving
native history-update events and v4 storage. Owning regressions jointly check that history still
appends, get_entries can read it, native consumers receive updates, RPC omits only redundant
notifications, and explicit extension/boundary notifications remain visible. This does not erase
native-only checkpoint records from history or convert the history differences above into D passes.

## Synchronous publication lock audit (review S1)

The initial observer implementation released only the hub lock; that was insufficient. A queue
mutation could still own the activity mutex. S1 was reproduced with an outer OS-thread watchdog:
a QueueUpdate callback that only reads has_pending_messages deadlocked even empty clear_queue.

| Publication family | Outer ownership at publication | Treatment |
| --- | --- | --- |
| activity enqueue / activate_idle_queue / clear_queue | Activity state mutex, plus Agent queue transactions before return | A dispatch-deferral guard is declared **before** taking state. Snapshot/revision/native publication and Agent enqueue retain their original atomic order; callback draining starts only when owner/Agent guards have left scope. Nested/concurrent deferrals postpone queue draining, not mutation ordering. |
| activity begin_queued / edit_queue | Same activity state mutex and queue transaction | Same deferral, including early returns; no guard crosses await or external execution. |
| activity finish_run | Activity mutex is confined to an inner block | Already releases it before publish_queue; no mechanical unlock/reorder. |
| Agent bridge (message commit / queue / initialized context) | message_ended returns owned records/snapshots; agent state is a value | Store/activity/agent-state locks have returned before publication. |
| Context append/batch/reconcile/navigation | Store commits and runtime context replacements return owned values before publication | No journal/state mutex spans callbacks. Operation-admission gate may remain held; read queries do not acquire it and conflicting synchronous mutations fail Busy rather than blocking. |
| Configuration/model/name/label | Prepared configuration commits consume their guard; journal methods return records | No prepared-runtime or journal mutex remains at publication; model/name/label provenance stays transient. |
| Shell lifecycle/output | Async activity admission, no activity-state mutex spans output callback | Snapshot mutation is unlocked before observers; cancellation registration has finished before streaming. |
| Notices / fallback | Direct notice; fallback mutex is taken/taken-from in a completed statement | No owner lock across publication. |

An already executing callback on another thread can naturally wait for a concurrent mutation's
mutex; the mutation never waits for that callback while holding its locks. Crucially, no drainer
can take a publication queued within a deferred owner scope until that scope releases its locks.
Normal retirement queue preservation is reviewed separately as S3, not asserted by this audit.

## Architectural choices

1. Keep native v4 storage, shared mutation ordering, lazy first-assistant materialization and
   generation factories. No conversion of the normal journal codec to v3 and no parallel runtime.
2. **Current P / backlog #30:** automatic v3 opening/migration is deferred. `switch_session` remains
   v4-only; do not normalize that unsupported-file difference away or describe explicit import as
   identical behavior. No automatic-conversion glue or in-place v3 writing has been added.
3. If automatic import is revisited, the existing `pi-plugin-session-transfer` owner must preserve
   fromHook/systemMessage semantics, legacy message wire extensions and trees. systemMessage is a full
   SystemMessage object (including sections/toolsAdded/toolsRemoved/timestamp), not a string; a legal
   empty-content fixture has `content: ""`. It projects before the compaction summary. Empty branch
   summaries do not enter context, but empty compaction summaries still do. Unsupported known-entry
   extensions without a lossless representation must be rejected before writing/switching.
   Never silently swallow fields. Old v4 records remain valid with optional/default-compatible fields.
4. Future import work must not overwrite an existing/concurrent target. Cleanup guards may remove
   only owned files. Existing import target-race concerns remain explicitly unvalidated/deferred;
   the current shared-field parser update is not a claim that this broader import boundary is fixed.
5. Lossless delivery uses a session-owned synchronous observer and one ordered RPC output path;
   agent/awaitable asynchronous execution waits for actual writer backpressure, not just enqueue.
   TUI keeps its broadcast/snapshot reset semantics. Install before startup hooks and preserve
   generation ownership. **Approved resource boundary:** arbitrary synchronous extension/startup
   bursts retain legacy-style queuing and are not hard memory-bounded. Do not replace the old
   broadcast with an unbounded replay cache, add disk spill, or require block_in_place/multithread.
   Release snapshot/dispatch locks before callbacks; support reentrancy, current-thread runtimes,
   and subscription disposal. Ordinary abort must not silently discard frames on a valid connection;
   connection EOF/closure and SIGTERM follow their separate legacy flush/drop contracts.
6. Fix transforming semantics in their deepest owner. R-json only serializes existing semantic data;
   it must not invent duration or recompute a compaction result from a later unrelated session state.

## Reproducible validation environment

- [`sikemux.json`](../sikemux.json) declares locked Node dependency setup for new worktrees,
  quick TypeScript/RPC checks, and final Rust, Node and native-host checks.
- The product is a CLI; no server/database port is fabricated. Current validation uses checkout-local
  `packages/pi/node_modules` and the guarded `.build/rpc-validation/cargo` target, selected per command
  with `CARGO_TARGET_DIR`. A missing or replaced dedicated directory stops the run rather than
  silently recreating it. External setup drafts are not part of the delivery candidate. Actual
  two-checkout **platform setup_check is pending**; local validation is not a substitute.
- Obtain the exact legacy commit explicitly into an isolated checkout (currently under the
  validation checkout's ignored `.build/rpc-validation/oracle/`); record its source repository, commit, tree and
  lockfile digest. Never silently consume uncommitted oracle changes or rely on main-worktree paths.
- Install legacy dependencies with its committed lockfile in that isolated environment. Run its
  actual `src/cli.ts` through its own `experimental/source-resolver.ts`; use actual process cwd and
  supported environment/CLI flags. A real `get_state` + EOF launch preflight must succeed before a
  two-process comparison is credited. Preserve full stdout/stderr, not just the last 500 characters.
- Prior audit legacy launch failed with ERR_MODULE_NOT_FOUND. Its precise missing module was not
  retained; this is not a passing differential and must be fixed locally without changing oracle code.
- Tests use isolated HOME/PI_AGENT_DIR/settings and deterministic scripted provider/tools only.
  Parallel tools use explicit barriers. No user credentials, network model calls or global install.
- Map generated IDs bijectively and replace only nondeterministic time values while preserving field
  presence/types. Preserve ordering and full payloads. Do not normalize missing fields, null/false/empty,
  dropped events, or the approved v3→v4 path divergence away.
- Small red/green owning-seam tests first. Public event/session/native changes require all workspace
  gates; Node changes add typecheck, Node tests and fresh native bridge. PTY only for a changed terminal seam.

## Validation log

**Artifact availability:** the original owner/main `target` trees disappeared outside this task;
removal is not attributed. The target-based paths below are historical and currently unavailable,
not reconstructed files. Source WIP and generated corpus files remain. Authorized tool receipts and
new immutable source snapshots were preserved in an explicitly supplied, private evidence directory
outside build caches. New preparation/preflight/differential commands require `--evidence-dir`;
D runs additionally require the ready preparation manifest. Host-specific evidence paths are not
hardcoded into these scripts. The following log retains historical stage-specific verdicts; later
isolated preparation, builds and bounded acceptance are summarized above, not retroactively applied
to old artifacts or failures.

- Baseline sync: `88e16a424c2e20e84c18b600ebeca5aec7e352e0`; clean before this task wrote files.
- **O (method extraction + real JSONL serializer, not full handler/CLI):** getLastAssistantText at
  pinned legacy commit: empty/whitespace → `data:{}`; `" answer "` → `data:{text:"answer"}`.
  Process `390f36cc-4051-43bd-a69b-d5261c9d63c2`, exit 0.
- **O (same level):** after earlier `" earlier "`, aborted nonempty → `"partial"`, error nonempty →
  `"failure partial"`; normal stop with no text or whitespace → `{}` (no fallback); aborted no text
  or error whitespace → `"earlier"`. Process `732766d0-64cc-4749-8655-6fdb0d8fa913`, exit 0.
- **O/R:** `scripts/rpc/generate-oracle.mjs --legacy <explicit-checkout>` verifies pinned commit and
  unchanged oracle files. It executes the actual extracted RPC UI factory (19 cases), actual toJsonEvent
  (12 structured-result cases), and actual last-text method + JSONL serializer (9 cases). `--check`
  compares regenerated complete fixtures. Source: `packages/pi/test/fixtures/rpc-ui-oracle.json` and
  `crates/pi-rpc/tests/fixtures/rpc-{tool-events,last-text}.json`.
- UI red: `6e554eed-e0a3-4b1c-a4f6-53384fd66839` failed on missing getAllThemes; green
  `57278a42-9809-4669-93bd-c1911deb8a79`: 25 UI tests plus Node typecheck passed.
- Tool projection red: `431e66ee-83b3-41ab-8282-bfcc58fd62c6` failed on missing explicit null
  structuredContent; green `4674f024-0e4d-4a62-9217-5d09cdf73d3b`: 12 cases plus 4 old tests passed.
  Earlier two compile attempts only repaired test fixture construction; they were not behavioral reds.
- Last-text fixture against public RPC dispatch: `c118bd8b-556d-43ad-9bd0-7575cc8f6cea`, all nine
  cases passed without a production change. Do not report the old checkout's behavior as a new defect.
- **Real legacy CLI preflight, not D:** `scripts/rpc/prepare-oracle.mjs --source <local-repository> --evidence-dir <persistent-directory>`
  creates an independent pinned clone, installs the lockfile and follows the oracle's nix recipe to
  hydrate ignored model data from its hash-verified catalog revision. It never modifies oracle source.
  `scripts/rpc/preflight.mjs --legacy target/rpc-oracle/legacy --evidence-dir <persistent-directory>`
  now retains full launch command/stdout/stderr in its exclusive evidence child. First attempt found missing generated `.manifest.json`; after
  hydration, `904bcf92-df43-41da-93d2-a879f3cfb486` passed get_state + EOF (no provider call), evidence
  `target/rpc-evidence/preflight-Z2lhxu`.
- **D executed, comparison FAILED:** actual pinned legacy CLI vs fresh standalone Rust CLI,
  15 sequential idle/no-provider requests, all mixed stdout records compared with no normalization.
  `2f1b12d1-1c2e-4dc3-b3ac-c55f82a23d79`, evidence `target/rpc-evidence/idle-differential-QkfirI`:
  clear_queue's queue_update precedes its response in legacy, but follows it in Rust. A separate
  single-command causal run (`c7690e4c-0f4b-457a-8dda-cd35195cbcd5`, `idle-differential-aksLPg`)
  reproduces the same order with no cross-command ambiguity. Do not sort the stream to make it pass.
- **R:** ModelSpec metadata roundtrip red `a10f180d-fb6d-4933-a08c-b91ece5e7eda`, green
  `a8f9cdf6-efaa-4bad-9e25-33bedfb82d64`; public ModelsPlugin registration/override red
  `faf0d47a-b1e1-4fd9-8ec1-9cd8d03b3169`, green `81a85d66-c343-46f4-a039-212bbf73db1b`
  (2 tests); all four model response surfaces via public RPC dispatch passed
  `52304537-ccdd-4d24-875b-206ae55e897d`. This covers catalog metadata preservation, not cache warming.
- **D executed, comparison FAILED:** models scenario, `02887ab1-71f0-40cd-a968-3f8dba5155be`,
  evidence `target/rpc-evidence/idle-differential-1jGraw`: full model metadata agrees, but Rust
  additionally emits entry_appended(model_change). Only generated sessionId values were mapped
  bijectively; the extra event remains a failure. The provenance table above identifies its owner.
- `scripts/rpc/differential.mjs --legacy <checkout> --rust <fresh-binary> --preparation <ready-manifest> --evidence-dir <persistent-directory> --scenario idle|clear-queue|models`
  retains both complete stdout/stderr, commands, exit statuses, actual HEAD/base tree, tracked-diff
  digest, input/fixture digests, binary SHA and normalization mappings. These runs do **not** cover
  the Node/NAPI entrypoint. Oracle regeneration from the isolated clone passed `--check`
  (`8259ed7f-5462-4415-931b-1db6e5c51040`).
- **R:** duration wire red `78f6323c-0a91-47a7-9c4a-11b6d77722a4` / green
  `23d3f97e-bad6-4894-bbc8-d1a6c9118f7a`; execution distinction red
  `c1932493-2e9c-4d56-899c-0b11e71dfe0e` / green `fd6384bb-f200-4f1b-bfbe-82e42ea68ac7`.
  Real execution timing now reaches tool_result messages, end events and plugin end callbacks.
  Success/throw are timed; unknown/invalid/blocked/pre-aborted immediate outcomes are not. End and
  message values agree in sequential/parallel schedules. Combined duration/oracle run
  `c7ff0cc2-51d5-4039-bfc1-9a5ba766ed2c` passed; tool-event fixture now has 15 cases (12 structured,
  3 explicit duration values). All 86 pi-agent library regressions passed `c6dedda6-cd0f-4775-a6e4-72a968ce4dac`.
- **R:** JS complete result decoding previously collapsed explicit null structuredContent to missing;
  red `eef96ada-6a21-4959-8e40-61abc73ac71e`, green `3cc828ef-5952-4519-b3e1-189380f7c13a`
  (two tests). This strengthens the JS wire handoff; fresh native-bridge evidence is still pending.
- **D passed for a bounded standalone subset:** `a90683ba-c750-4966-9d8c-b00bc6d675fa` rebuilt
  the standalone CLI and passed the single clear_queue causal scenario (2 mixed records) and
  the 15-command idle scenario (16 mixed records), with no normalization. Evidence directories:
  `idle-differential-7zPagG` and `idle-differential-JZ1xnk`. This does not establish Node/NAPI,
  streaming, model mutation, compaction or full-protocol parity.
- **Independent static review:** immutable snapshot `ordered-delivery-1794999426` produced three
  findings, not an independent dynamic pass. S1: owner activity mutex remained held during queue
  callbacks. S2: UI cancellation was not observed while awaiting writer acknowledgement. S3:
  normal retirement discarded queued old-generation publications. Author regressions:
  - S1 red `7b5149c0-ef5a-4c1a-a1b4-bec63742b69f`: outer-thread watchdog observed a 5-second
    deadlock. Deferral guards now retain original mutation/revision/enqueue ordering while keeping
    callbacks outside owner locks. Expanded queue enqueue/activation/move/cancel/begin/clear
    reentry and monotonic revisions passed in `2f9ccc11-3604-40d0-8a09-9cc08274cda6`.
  - S2 red `c3eb67a8-2c53-4452-864d-8e60b7b0182a`: UI waiter timed out with one byte of its frame
    written and stdout still open. Green `2f9ccc11-3604-40d0-8a09-9cc08274cda6`: EOF and abort
    release the waiter before draining output, preserve the already queued frame and reject stale replies.
  - S3 red `8b1ff3e4-6a26-4d1b-979f-8c3c19616d01`: a controlled external-thread gate caused old
    shutdown-tail to disappear before new startup. Green `b3cf66d4-6cc4-4851-a2c1-2716bdd90d2c`:
    normal retirement freezes new publications and asynchronously fences accepted synchronous
    projections before closing leases; no stdout flush or blocking Drop is introduced.
- **R combined review regressions:** `392e3f40-8e02-4fab-b635-2c7a2d289c6f` passed 8 connection,
  5 observer and 11 command tests. Direct shutdown future cancellation releases pending acks;
  EOF during a managed replacement is followed by bounded disposal of the existing
  complete-on-drop transaction. That disposal expectation is a **Rust internal R-level contract**,
  not a claim of legacy EOF equivalence. Real EOF/signals remain separate D acceptance.
- Prompt preflight order also gained a focused red (`3620e858-b02b-4a56-bbf7-c5bcf20fde9a`,
  agent_start incorrectly preceded the receipt) and green (`4620e86d-b3d1-4e32-ba84-7eb0ccb30749`).
  The transport now queues the one authoritative response in the existing preflight callback;
  accepted stream errors do not create a second response. Public command-only dispatch is retained.
- **R request behavior:** sampling defaults red `d7945160-2622-4064-9ba1-6684c52555e2`, green
  `e1616aad-8f07-459f-9960-f70393da1c55`: the real provider serializer into a mock HttpTransport
  verifies base < clamped Pi level < explicit request values, with level lookup before provider
  mapping. Unmapped XHigh and nonreasoning Off cases, null/false/empty values and shallow override
  are covered. This is owning-seam integration, not a dual-CLI D claim. All models-package tests,
  targeted Clippy and core metadata roundtrip passed `3a8c5cd9-9900-4b6f-8b68-f06584f6ebc5`.
- **R N1 observer rotation:** old-cohort red `602e241c-76d5-47c3-9c7e-fa661dcb5089` and late-first-poll
  red `1580a122-432e-49bf-bf52-1322b79eff5a`; final `17ca8a8e-2a6d-4694-aca0-cde0de5a1c44`
  passed 10 observer and 8 connection tests. Checkpoints capture only subscribers with `since < target`,
  and wait on their surviving obligations, not replacement subscribers. Partial survivors and genuine
  new publications still require delivery; no producer abort, fake cursor or rescuing event is used.
  Independent immutable-snapshot review closed the N1 static finding, not overall A or dynamic acceptance.
- The compaction oracle in `scripts/rpc/compaction-oracle.mjs` subsequently executed successfully
  (`compaction-oracle-E4zFaS`): actual pinned in-memory SessionManager, system-message fold, entry
  projection and token estimator, without a provider request. This is function-level O, not D.
- The latest repaired code passed full workspace gates, as recorded above. Broader equivalence is
  still bounded by the strict failures and missing observations; a green regression suite is not a
  replacement for fresh product evidence. No local commit, land, release or user installation yet.
