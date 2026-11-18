# Session history, runtime and management

`AgentSession` supports the `context_edit`, `turn_end`, and `agent_before_settle` behavior from
`legacy/pi/packages/coding-agent`. Native plugins use the same session transaction as JavaScript
extensions. These features require a session; a standalone `Agent` has no durable journal and its
`turn_end` callback remains observational.

## Module ownership

| Module | Location | Responsibility |
| --- | --- | --- |
| Session History | [`src/history/`](src/history/README.md) | durable v4 values, JSONL, tree/lanes, projection and pure recovery reduction |
| Session Runtime | `src/runtime/` | `AgentSession`, activity, delivery, configuration, context, compaction and events |
| Session Management | `src/management/` | `MultiSessionManager` / `PiSession`, replacement, child lifetime and observation |
| Plugin Adapter | `src/plugin/` | `PiPluginContext`, generation binding and model/session/UI capabilities |

These modules live inside the same `pi-session` crate. Runtime depends on History and executable
`pi-plugin` contracts. Management composes Runtime;
the Plugin Adapter translates capabilities to Runtime/Management operations. Runtime does not
import either Management or the concrete adapter. The actual dependency diagram and invariants
are in [the architecture guide](../../docs/architecture.md#session-modules).

`pi_session::SessionStore` owns a session's recorded history. `SessionHistory` provides its read-only
view, while `SessionDocument` holds a complete history snapshot. Direct storage operations return
`HistoryError`; product operations retain `SessionError`. Typed callers can use
`SessionError::from` to cross that seam. `ForkPosition` now lives in `pi-core` and remains available
through its plugin/session re-exports. Neither change alters serialized v4 or plugin messages.

## Session entry points

Product hosts create sessions with `MultiSessionManager::create_session(CreateSessionOptions)`
and open them with `open_session(source, OpenSessionOptions)`. Creation options combine the
workspace, journal path, optional ID/header metadata, and transient generation overlay. Opening
accepts a path or `SessionSource::Store` for an already-replayed journal. An already-active path
returns its existing handle; opening options apply only when constructing a new one.
The Manager owns generation preparation, capability binding, activation, and replacement.

Use `session.prompt(text)` or `session.prompt(SessionInput::new(text).with_images(images))` for
user input. Commands and input hooks run once; the result is `PromptOutcome::Agent`, `Handled`,
or `Queued`. While a durable run accepts messages, unhandled input is queued as steering.
`session.run_messages(messages)` accepts structured messages, skips text preprocessing, and
waits for the full `AgentLoopOutcome`, including settled callbacks. `steer` and `follow_up`
retain their explicit delivery timing.

Independent runtime embeddings can use `AgentSession::create(runtime, path, options)` or
`AgentSession::open(runtime, path, options)`; both return activated sessions. Deferred
preparation and activation are private. Generation factories use
`PreparedSessionGeneration::bind_session` when capabilities must be ready before `session_start`.

## Session ownership and read views

Long-lived frontends use `PiSession::observe()` for the initial snapshot and subsequent
`SessionUpdate` values. `Event` carries a product event and `(epoch, revision)`;
`Reset` supplies an authoritative snapshot after replacement or lag. Reload increments the epoch
even when the session ID stays the same. Observation handles subscription switching and revision
filtering; frontends own their presentation. `snapshot()` on the observation is the baseline from
subscription or the last reset, not a continuously reduced view. A fresh read remains available
through `observation.current().snapshot()`. `recv()` is cancellation-safe and returns `None` after
final closure, once queued final events have been drained. Lag resets state without replaying
missed transient events.

Run-scoped consumers use `AgentSession::snapshot()` / `subscribe()` and remain pinned to that
generation. For this low-level subscription, refresh on lag and ignore events at or below the
snapshot revision. Historical messages are shared through `Arc<Vec<Message>>`; retaining a snapshot
preserves completed history while new chunks update the separate stream handle.

`session.catalog()` exposes models, tools, diagnostics, prompt/resource metadata, and workspace
queries. `session.history()` exposes immutable journal records, context, and branch queries.
Read the current tool selection from `session.snapshot().agent.active_tools` and registered tool
definitions from `session.catalog().tool_specs()`.
These concrete views cannot configure, execute, reload, or mutate the journal. Route changes
through Session methods and reload through `PiSession`; plugin adapters use
`session.plugin_context(scope)` for generation-bound capabilities. Standalone `Agent`, `PiRuntime`,
and `SessionStore` remain available when no product session is needed.

`session.wait_for_idle().await` covers input preparation, Agent execution, retries, compaction,
`agent_before_settle`, and the final settled callbacks/event. It uses the same state as
`session.is_idle()`. A plugin may still deliver to the durable run during `agent_before_settle`;
that delivery window closes before settled callbacks begin. The low-level Agent's idle state
does not imply that the Session is idle.

Configuration setters prepare and validate the entire change, commit the journal batch, then
apply memory changes and publish the updated snapshot. A failed validation or deterministic
write does not partially change the live configuration. A model switch and its necessary
thinking clamp are recorded together, in `ModelChange` then `ThinkingLevelChange` order.
Batching retains the existing Pi v4 JSONL failure recovery rules and lazy first-response
persistence; it does not add cross-line power-loss atomicity.

## Edit model context

Append an edit with `AgentSession::edit_context(target_id, replacement)` while idle, or propose a
`context_edit` draft in either boundary hook while a run is active:

```ts
pi.on("turn_end", (event) => ({
  entries: [
    ...event.entries,
    {
      type: "context_edit",
      targetId: event.messageEntryId,
      replacement: { content: "A shorter version of the assistant response." },
    },
  ],
}));
```

`replacement: null` omits the target from model context. A content replacement changes only
content: role, usage, tool identity, timestamps and unknown message fields are preserved. Text
replacements for assistant/tool-result messages become text blocks. Targets must be earlier
user, assistant, tool-result or custom-message entries on the selected branch. The latest edit
wins. Original records remain in history, and checking out a point before an edit restores the
previous projection.

Edits persist in the v4 JSONL journal and survive resume. Compaction snapshots carry optional
`retainedEntryIds`, so later edits can address retained original entries even after repeated
compaction. Older v4 snapshots use an ordered message-match fallback. Importing Pi v1-v3 sessions
also preserves context edits and retain-none compactions.

## Request another response before settlement

```ts
export default function (pi) {
  // Generation-local example state: verify once in each run started by a user.
  let verificationRequested = false;
  pi.on("before_agent_start", () => { verificationRequested = false; });
  pi.on("agent_before_settle", (event) => {
    if (event.outcome !== "completed" || verificationRequested) return;
    verificationRequested = true;
    return {
      entries: [
        ...event.entries,
        {
          type: "custom_message",
          customType: "verify-result",
          content: "Verify the result against the original request and report any remaining gaps.",
          display: false,
        },
      ],
      continue: true,
    };
  });
}
```

Both hooks receive `entries`, `continue`, `outcome` and `context`. Context contains typed projected
entries with provenance, agent messages, provider messages, pending messages and `canContinue`.
JavaScript receives the corresponding camelCase Pi wire fields. `turn_end` additionally receives
`messageEntryId`, `toolResultEntryIds`, the response/tool results and `turnIndex`.

Handlers run in registration order. Returning `entries` replaces the accumulated draft list;
include `...event.entries` to keep prior proposals. A new preview is built after every handler.
Invalid drafts remain visible for a later handler to repair; a final invalid batch is discarded
without partial journal writes. Handler failures are diagnostic events. JavaScript in-place
changes to the entries array survive a thrown handler, matching Pi.

Supported draft types are `custom`, `custom_message`, `context_edit`, and
`compaction: { summary, firstKeptEntryId, details?, usage? }`. A null `firstKeptEntryId` retains
no old messages. Preview construction validates edits and computes the projected context without
writing the journal. Final proposals are revalidated and committed as one batch.

The assistant and its tool results are persisted before `turn_end`. Valid drafts are applied
before the next provider request. Explicit continuation defers follow-up messages while steering
retains its normal delivery priority. `agent_before_settle` runs after post-run retry, compaction
and queued continuations; `agent_settled` fires only after this boundary finishes without another
run. Abort and the existing turn controller's stop decision take precedence over continuation.
A request to continue without runnable context produces a diagnostic rather than an empty loop.

Automatic compaction ignores usage checkpoints from before a context edit and preserves newly
appended or replaced input that has not reached a provider, including when metadata follows it.

Native plugins return `BoundaryResult` from `Plugin::turn_end` or
`Plugin::agent_before_settle`. `TurnEndEvent::boundary` is populated for sessions. The contract is
native ABI 26; rebuild native plugins with the current SDK and constructor symbols.
