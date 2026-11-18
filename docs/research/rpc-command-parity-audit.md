# RPC command parity audit: current legacy Pi vs pi-rs

## Scope and result

Read-only source audit, recorded 2026-11-18. Inspected the **working-tree sources**, not just committed HEAD. Repository HEAD was `2863458`; the local legacy Pi checkout HEAD was `05fd3fbda`. Many Rust/Node implementation changes already existed and were not modified. This report is the only file created by this audit.

**Conclusion: command-name coverage is complete (33/33), but full behavioral/wire compatibility is false.** Three confirmed command-level counterexamples are below. These are static source proofs, not newly executed end-to-end tests. No Cargo, provider calls, package installation, network access, commits, or full validation gates were run. Existing prompt/tool conformance results do not prove RPC conformance.

Scope: command names, declared inputs, response fields, id/errors, session/model/compact/fork/tree/history. Event streaming, extension UI, transport/backpressure, and test execution are being audited separately by the lead.

Primary sources (paths relative to repository root):

- **L-mode**: `legacy/pi/packages/coding-agent/src/modes/rpc/rpc-mode.ts`
- **L-types**: `legacy/pi/packages/coding-agent/src/modes/rpc/rpc-types.ts`
- **L-doc**: `legacy/pi/packages/coding-agent/docs/rpc-commands.md` (there is no root `docs/rpc-commands.md`)
- **R-mode**: `crates/pi-rpc/src/rpc.rs`
- **R-wire**: `crates/pi-rpc/src/json_wire.rs`

## Three confirmed differences with user impact

### 1. `switch_session` cannot directly open the current legacy Pi session format

**Input scenario:** an existing, otherwise valid session written by current legacy Pi, beginning with `{"type":"session","version":3,...}`, is supplied to `{"type":"switch_session","sessionPath":"/absolute/path/to/legacy-session.jsonl"}`. Assume no extension cancels the switch and the session cwd exists.

Legacy opens its native v3 file. Rust's RPC command opens only its native v4 journal; it does **not** route this command through legacy import.

Proof chain:

| Layer | legacy Pi | pi-rs |
|---|---|---|
| RPC handler | L-mode **624–627** calls `runtimeHost.switchSession(command.sessionPath)` | R-mode **751–757** calls `resume_session(session_path)` |
| Session replacement | `legacy/pi/packages/coding-agent/src/core/agent-session-runtime.ts` **215–242**, especially **229**: `SessionManager.open(...)` | `crates/pi-session/src/management/session.rs` **437–443** creates a Resume request; `crates/pi-session/src/management/replacement.rs` **604–617** chooses `SessionTarget::open(&path)` for another path |
| File open | `legacy/pi/packages/coding-agent/src/core/session-manager.ts` **42–52** defines native `type: "session"` and version 3; **1784–1795** opens/loads that file | `crates/pi-session/src/management/replacement.rs` **409–412** calls `SessionStore::open_handle` directly |
| Decoder | legacy uses its own `loadEntriesFromFile`, **628 onward**, through `SessionManager.open` | `crates/pi-session/src/history/journal/jsonl.rs` **133–136**, **1032–1034**, **1057–1087** read bytes and immediately validate the header |
| Rejection | v3 header is the current native format | same Rust file **1185–1194** requires `kind == "header"` and native schema; `crates/pi-session/src/history/types.rs` **12** sets schema to 4. The legacy `type: "session"` header fails before entry decoding |

**Automatic-import exclusion:** the complete RPC-to-decoder path above contains no format detection/conversion. Import is a separate `/import` feature: `plugins/features/pi-plugin-session-transfer/src/lib.rs` **105–159** parses an import command, requests confirmation, stages `import_session_file`, and only then switches to the converted destination. The decomposition is documented at `docs/architecture.md` **3361–3371**; native v4 storage is explicitly deliberate at **2422–2430**.

**Impact:** swapping the CLI implementation under a client that stores and reopens existing Pi `sessionFile` paths does not work transparently. This is a known architectural/storage boundary, not a missing command name. A separate import flow can mitigate it, but cannot establish identical `switch_session` semantics.

### 2. `compact` success drops `estimatedTokensAfter`

**Input scenario:** a session eligible for compaction; `{"type":"compact"}` completes successfully (including extension-supplied compaction content).

- Legacy L-mode **554–557** returns the complete result from `session.compact`.
- `legacy/pi/packages/coding-agent/src/core/agent-session.ts` **3021–3024** rebuilds context and computes `estimatedTokensAfter`; **3041–3048**, **3059** put it in the returned result. Computation is after the custom/default compactor branches, so this is not restricted to one provider.
- The field is documented in L-doc **386–404** and declared in `legacy/pi/packages/coding-agent/src/core/compaction/compaction.ts` **105–114**.
- Rust R-mode **673–695** runs compaction, then reconstructs its response from the last persisted compaction record using `compaction_result_json`.
- R-wire **280–301** emits only `summary`, `firstKeptEntryId`, `tokensBefore`, optional `details`, and optional `usage`. It never inserts `estimatedTokensAfter`.

**Impact:** a client displaying reclaimed context capacity after manual compaction loses a field legacy actually returns. The fact that the TypeScript interface marks the field optional does not make observed success responses equal. The event projection reuses this helper, but event-level consequences are outside this sub-audit.

### 3. Model responses cannot preserve all current model metadata

**Input scenario:** a configured chat model has `promptCache` or `samplingParamsByThinkingLevel` metadata; the client calls `get_available_models` / selects it with `set_model`, or reads it via `get_state` / `cycle_model`.

- Current legacy model contracts include `promptCache` and `samplingParamsByThinkingLevel`: `legacy/pi/packages/ai/src/types.ts` **864–888**.
- These are real configuration inputs, not merely unused interface members: `legacy/pi/packages/coding-agent/src/core/model-config.ts` **59–67**, **79–85**; `legacy/pi/packages/coding-agent/src/core/provider-composer.ts` **198–206**, **243–247** preserve/merge them into model objects.
- `legacy/pi/packages/coding-agent/src/core/model-runtime.ts` **508–510** returns the actual available snapshot. L-mode **469–483**, **491–512** exposes these model objects, not a smaller field projection.
- Rust's `ModelSpec` in `crates/pi-core/src/model.rs` **140–159** has neither field, nor a flattened unknown-field map. R-mode **998–1010** serializes only that struct (and fills `baseUrl` if absent).
- That serializer is used by R-mode **591–631**, **968–988** for the four corresponding response surfaces.

**Impact:** clients cannot inspect the same full configured model definition. This is a model-contract gap exposed through RPC, not necessarily an RPC-adapter-only fix. The claim is conditional on models carrying these fields; it does not assert every default catalog model currently contains them, nor claim a provider request behavior from this metadata audit.

## Additional confirmed boundaries, and false positives excluded

### Compaction entries omit boundary metadata

For `get_entries` / `get_tree`, legacy returns actual persisted entries (L-mode **648–664**). `legacy/pi/packages/coding-agent/src/core/session-manager.ts` **1280–1303** saves `fromHook` and, when present, `systemMessage` in compaction entries. R-wire **236–242**, **280–301** reconstructs compaction entries through the compact-result helper, which never emits either field. This is a separate history-payload difference from missing `estimatedTokensAfter` in a compact command response. It does not establish whether Rust's internal replay retains equivalent semantics.

### Malformed input validation is stricter/different, not a valid-input equivalence proof

Rust uses serde tagged command decoding (R-mode **24–169**, **480–510**). Legacy parses JSON then casts to `RpcCommand`, without a general runtime schema validator (L-mode **759–810**).

Concrete examples from the dispatch paths:

- `{"id":"bad","type":"set_model","provider":"p"}`: Rust rejects the missing required `modelId` as `Invalid command: ...`; legacy enters the handler and returns `Model not found: p/undefined` (L-mode **491–497**). These differ even though both fail.
- `{"id":7,"type":"get_state"}` is outside the declared string-id protocol. Legacy echoes the supplied id through `command.id` and its success object (L-mode **405**, **65–74**); Rust rejects `Option<String>` deserialization and the fallback id uses only `Value::as_str` (R-mode **485–510**), so the error cannot echo numeric `7`.
- `get_entries` with `since: null` is also outside the declared input type: legacy tests `since !== undefined` and fails to find id `null` (L-mode **652–655**); Rust `Option<String>` interprets null as absent and returns all entries (R-mode **146–149**, **814–823**).

These are confirmed error/validation differences, not the highest-impact evidence against compatibility for valid clients. Exact parser-library error text was not executed/recorded.

### Things that should NOT be reported as Rust-only defects

- **33 command names exist in both implementations.** `clone`, `get_entries`, `get_tree`, and `get_available_thinking_levels` are not missing.
- **Absent last assistant text:** L-types **226–233** and L-doc say `text: null`, but actual legacy code returns `undefined` from `getLastAssistantText` (`legacy/pi/packages/coding-agent/src/core/agent-session.ts` **4526–4550**) and JSON-stringifies `{text: undefined}` into `{}`. Rust R-mode **842–844**, **1189–1220** also omits the field. This is source-vs-documentation drift shared by both, not evidence of a Rust-only regression.
- **`set_session_name` history is not left as an opaque Rust custom entry.** `crates/pi-session/src/runtime/configuration.rs` **269–292** writes `set_name_with_entry`; `crates/pi-session/src/history/journal/jsonl.rs` **626–633**, **649–691** stores an opaque legacy `session_info` record; R-wire **196–204** unwraps it back to `type: "session_info"`. Both normalize CR/LF runs to spaces (legacy `session-manager.ts` **1322–1333**; Rust configuration **273–290**).
- **Fork selector scans all entries, not just active branch, on both sides.** L-mode **643–645** → legacy `agent-session.ts` **4340–4354**; Rust R-mode **790–805**. The prose description of active-branch forking must not override the current implementation.
- **Stats aggregate history, not only current context, on both sides.** Legacy `agent-session.ts` **4362–4413** and Rust R-mode **1063–1125** iterate stored entries. Full tool/summary usage aggregation correctness remains unverified here.
- **Known matching envelopes:** string ids are preserved, absent ids and no-data success fields are omitted, unknown string commands retain the original command/id and fail. Legacy L-mode **65–78**, **725–728**; Rust R-mode **485–503**, **1037–1061**. Both deliberately return `data: null` when no model/thinking cycle is possible (L-mode **501–528**, R-mode **608–621**, **640–654**).

## Complete command-name coverage and bounded semantic inspection

A local read-only Python structural comparison extracted command literals from L-types, handler names from R-mode `RpcCommand::name`, and L-doc headings: **33 / 33 / 33**, missing set empty, extra set empty. Legacy mode also has all 33 corresponding cases. `extension_ui_response` is separate from `RpcCommand` in both and is excluded from this count.

The table records owning dispatch lines, not a claim that each downstream implementation passed equivalence tests. “Surface match” means corresponding input and response envelope/operation are present; downstream behavior remains unverified unless specifically discussed above.

| Command | L-mode line | R-mode line | Inspection result |
|---|---:|---:|---|
| `prompt` | 412 | 527 | Text/images/streamingBehavior; receipt disposition surface matches; live ordering/expansion not proven here |
| `steer` | 440 | 537 | Text/images and queued/handled disposition surface; queue semantics unverified |
| `follow_up` | 445 | 549 | Same bounded conclusion as steer |
| `abort` | 450 | 561 | Both await an abort/idle operation; race semantics unverified |
| `clear_queue` | 455 | 565 | Returns steering/followUp text arrays; persistence/recovery unverified |
| `new_session` | 459 | 575 | Optional parentSession, cancelled response; generation/persistence policy unverified |
| `get_state` | 469 | 591 | Same state field names; model metadata difference #3 |
| `set_model` | 491 | 592 | provider/modelId lookup and response present; metadata difference #3; availability/hooks unverified |
| `cycle_model` | 501 | 608 | model/thinkingLevel/isScoped or null; metadata difference #3; selection policy unverified |
| `get_available_models` | 509 | 623 | models array present; metadata difference #3; catalog/credential visibility unverified |
| `set_thinking_level` | 518 | 633 | level input/no-data success; supported-level clamping not independently proven |
| `cycle_thinking_level` | 523 | 640 | level or null; per-model enumeration not independently proven |
| `get_available_thinking_levels` | 531 | 656 | levels array; catalog-dependent values unverified |
| `set_steering_mode` | 540 | 659 | all/one-at-a-time; serde stricter on invalid input; live behavior unverified |
| `set_follow_up_mode` | 545 | 666 | Same bounded conclusion as steering mode |
| `compact` | 554 | 673 | Confirmed missing estimatedTokensAfter (#2); boundary/summary behavior unverified |
| `set_auto_compaction` | 559 | 697 | enabled boolean/no-data success; thresholds/persistence unverified |
| `set_auto_retry` | 568 | 704 | enabled boolean/no-data success; retry policy unverified |
| `abort_retry` | 573 | 711 | no-data success; race/settlement unverified |
| `bash` | 582 | 715 | command/excludeFromContext/id and final result fields present; shell/hooks/events unverified |
| `abort_bash` | 605 | 741 | no-data success; process cancellation unverified |
| `get_session_stats` | 614 | 745 | same response field names/full-history aggregation intent; exact totals unverified |
| `export_html` | 619 | 746 | optional outputPath/path response; Rust injected exporter content unverified |
| `switch_session` | 624 | 751 | Confirmed direct legacy-v3 incompatibility (#1) |
| `fork` | 629 | 760 | before-user target/text/cancelled surface; before-fork hook exists; unsaved-target edge behavior not proven |
| `clone` | 634 | 776 | current leaf/at position/cancelled; same no-leaf error; storage/lifecycle edges unverified |
| `get_fork_messages` | 643 | 790 | both scan all stored user entries and omit textless ones |
| `get_entries` | 648 | 807 | append-order/since cursor/leafId; confirmed compaction payload omission; malformed null-since differs |
| `get_tree` | 661 | 831 | tree/leafId and labels present; compaction payload omission; malformed/deep-tree behavior unverified |
| `get_last_assistant_text` | 666 | 842 | same reverse scan/text trimming and actual empty-result omission; docs claim null |
| `set_session_name` | 671 | 845 | trim/reject blank; same newline normalization and restored session_info projection |
| `get_messages` | 684 | 856 | messages response present; replay/context edit/provider projection equivalence unverified |
| `get_commands` | 692 | 859 | source/sourceInfo/name/description; full catalog ordering, collisions and optional description behavior unverified |

## Unverified items and validation limits

- No client was driven against both running processes. No scripted-provider integration or malformed-input process test ran.
- No claim of full session wire interchange: current Rust storage intentionally differs from legacy; RPC entry projections alone do not establish session-file compatibility.
- `get_tree` implementation differs structurally: legacy `session-manager.ts` **1547–1594** explicitly roots orphan/self-parent entries and traverses iteratively; Rust R-mode **1128–1159** recursively visits only null-rooted parent chains. Whether malformed trees can reach a live Rust RPC session, and practical stack-depth thresholds, were not proven; do not present those as reproduced RPC failures.
- Exact model availability, scoped selection, model clamping, credential effects, compaction retained boundaries, fork/clone cancellation ordering, resume recovery and custom message replay need focused cross-implementation fixtures.
- Source-level similarity or an existing Rust-only test is not a differential oracle. Existing prompt/tools and build gates remain valid only for their stated scope.
- Report validation: local name-set extraction and local report path/reference checks only. No Rust source file, Node source file, AGENTS file, existing research report, or implementation test was changed.
