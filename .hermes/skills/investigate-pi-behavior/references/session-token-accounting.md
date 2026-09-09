# Session token accounting

Use this when explaining why a displayed Pi/pi-rs session token total differs from a recursive scan of a session and its isolated children.

## Distinguish the requested metric

1. **Current/root session product total**: load the root `SessionDocument` and apply `aggregate_document_usage`. This includes usage-bearing tree entries plus explicit usage-adjustment records in that document.
2. **Per-file provider-call total**: sum assistant `message.usage` within one JSONL file, plus explicit detached adjustments only when reproducing the product total. Do not assume `usage.totalTokens` is the only canonical source; product aggregation recomputes total from input + output + cacheRead + cacheWrite.
3. **Recursive physical work across stored sessions**: enumerate the root JSONL and its `isolated/*.jsonl` children and total each file. This answers how much work all sessions performed, but may not equal the root UI total.

## Avoid double counting

Modern subagent completion can attribute child usage back to the parent through `record_usage`, producing a parent usage adjustment. If that adjustment exists, adding both the parent product total and the child JSONL total counts the child twice. Inspect adjustment `details.source` / task metadata and child session IDs before recursive addition.

Background review usage is commonly represented as a root `type: "usage"`, `cause: "adjustment"` record rather than a visible assistant message. Include it for the root product total.

## Diagnostic workflow

- Compute the root document total using the same aggregation path as the UI.
- Separately list each isolated child file and its usage.
- List root adjustment records with their source/task and child session ID.
- Reconcile the displayed value with rounding (`{:.1}m` means decimal millions).
- Report both scopes explicitly when useful: “current session/product-attributed” versus “recursive physical work.”

## Relevant pi-rs seams

- `crates/pi-session/src/usage.rs`: entry and document aggregation.
- `apps/pi-cli/src/tui.rs`: initializes `session_tokens` from `aggregate_document_usage`.
- `apps/pi-cli/src/tui/view.rs`: decimal `k`/`m` display formatting.
- `plugins/features/pi-plugin-subagents/src/child_run.rs`: attributes completed child usage to its owner.
- `plugins/features/pi-plugin-subagents/src/coordination.rs`: commits usage through the current owner session handle.

When examining historical sessions, do not infer current behavior solely from absent adjustments: the session may predate the attribution implementation or may record a failed attribution warning.