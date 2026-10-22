# Session context editing and continuation boundaries

`AgentSession` supports the `context_edit`, `turn_end`, and `agent_before_settle` behavior from
`legacy/pi/packages/coding-agent`. Native plugins use the same session transaction as JavaScript
extensions. These features require a session; a standalone `Agent` has no durable journal and its
`turn_end` callback remains observational.

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
