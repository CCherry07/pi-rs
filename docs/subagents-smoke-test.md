# Agent collaboration smoke test

Use this read-only playbook with a real tool-capable model to verify the first-party dynamic agent
tree. The feature intentionally has no `subagent` compatibility tool or static workflow language.

## Start

From the repository root:

```bash
PI_SUBAGENT_MAX_DEPTH=4 cargo run -p pi-cli --
```

Accept project trust when prompted, then record the primary path with `/session`.

## Procedure

Ask the model to perform these steps:

1. In one assistant response, call `spawn_agent` twice:
   - a `scout` to read `plugins/features/pi-plugin-subagents/src/runtime.rs` and report the default
     depth, cumulative spawn limit, and active-agent limit;
   - a `reviewer` to inspect `plugins/features/pi-plugin-subagents/src/tool.rs` and list the six
     registered tool names.
2. Call `wait_agent` with both exact ids and `mode: "any"`. Verify it returns after at least one
   child settles without cancelling the other.
3. Call `wait_agent` again with both ids and `mode: "all"`. Expected limits are depth `4`, spawns
   `64`, and active agents `8`; expected tools are `spawn_agent`, `send_message`, `followup_task`,
   `wait_agent`, `interrupt_agent`, and `list_agents`.
4. Send the reviewer one non-starting message with `send_message`, then call `followup_task` on the
   same exact id and ask it to confirm the message. Wait for that id and verify `childSessionId`
   did not change.
5. Spawn a longer read-only scout, immediately call `interrupt_agent`, wait until its state is
   `interrupted`, then reuse it with `followup_task`. Verify the new turn can settle as `idle`.
6. Call `list_agents` and verify it returns the descendant tree with exact ids, parent ids, current
   state, last turn result, and cumulative usage.

Ask for this final report:

```text
AGENT_COLLABORATION_SMOKE_TEST
parallel_race: PASS|FAIL - <evidence>
barrier: PASS|FAIL - <evidence>
message_followup_reuse: PASS|FAIL - <evidence>
interrupt_reuse: PASS|FAIL - <evidence>
tree_snapshot: PASS|FAIL - <evidence>
workspace: unchanged
```

## Nested message check

Create a trusted project agent definition with `allowNestedSubagents: true` and an explicit tool
set containing the six collaboration tools. Ask it to spawn a leaf, wait for it, and use
`send_message { target: "parent", ... }` for one meaningful progress update. Verify the parent
receives the message and the child continues in the same turn.

Exact ownership is intentional: a parent controls direct children by full id; prefixes and foreign
ids fail. `wait_agent` timeout is a normal snapshot, not cancellation. Live trees and mailboxes are
process-local; closing or reloading the owner interrupts them, and restarting does not reattach
them.

## Human checks

1. `/session` still reports the original primary path.
2. Desktop shows each `spawn_agent` as a selectable child task; the other tools appear as ordinary
   tool calls.
3. Isolated child JSONL files are below `<parent-stem>/isolated/<uuid>.jsonl` and absent from the
   top-level `/resume` list.
4. No `subagent`, `subagent_workflow`, `contact_supervisor`, `subagent_supervisor`, or `bg_wait`
   tool is registered.

Deterministic coverage:

```bash
cargo test -p pi-plugin-subagents
cargo test -p pi-session isolated_session_reuses_identity_across_turns_and_reports_usage_deltas
```
