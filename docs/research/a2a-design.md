# Standard A2A integration — design proposal

Status: proposal, not implemented. This is a Rust product extension, not legacy Pi conformance.

## Recommendation

Start with outbound delegation as an optional feature plugin. Add inbound serving only when we need other agents to invoke Pi. Keep the local subagent protocol unchanged: a remote task is neither an owned child session nor a local model turn.

Use the A2A-project-listed Rust SDK after a pinned compatibility evaluation. Do not build another protocol/client abstraction merely to wrap that SDK.

```text
Outbound
Pi Agent -> pi-plugin-a2a -> upstream A2A client -> remote agent

Inbound (subsequent delivery)
remote agent -> pi-a2a -> MultiSessionManager / PiSession
                         ^ constructed by pi-sdk and passed by host
```

- `plugins/features/pi-plugin-a2a`: configured remote catalog, credential references, model tools, outbound attempts/task references, observation and generation lifecycle. Upstream client/types are implementation dependencies.
- `crates/pi-a2a`: inbound protocol Adapter, authentication/authorization, task admission/state/artifacts, durable task ledger and Pi session coordination. Accepts `MultiSessionManager` and owner-defined options, like `pi-acp`; does not depend on `pi-sdk`.
- `pi-sdk`: selects and wires the outbound plugin through factory-backed generations. The application constructs the inbound Adapter and owns listener/startup/shutdown configuration.
- `pi-core`, `pi-plugin`, `pi-agent`: no A2A wire types, task registry, network policy or vendor switches. No new provider lifecycle, generic agent-backend trait or workflow language.

Only create modules needed for the chosen delivery. If outbound is the first delivery, there is no empty inbound crate or speculative server dependency.

## Verified standard baseline

The latest release inspected is **A2A v1.0.1**; negotiated wire version is **1.0**. Pin implementation fixtures to the release, not mutable latest documentation. [S, P, R]

- Agent Card discovery: `/.well-known/agent-card.json`; select a compatible entry from `supportedInterfaces`, including its endpoint and tenant. Explicit configured discovery URLs are also valid.
- Start with one advertised standard binding, **JSON-RPC over HTTPS**. Use `A2A-Version: 1.0`; never silently fall back to 0.3. Optional SSE is an observation mechanism.
- V1 methods include `SendMessage`, `SendStreamingMessage`, `GetTask`, `ListTasks`, `CancelTask`, `SubscribeToTask`. V0.3 names such as `message/send` and `tasks/resubscribe` are not v1 names.
- V1 uses ProtoJSON, including uppercase enum strings and the current Part/oneof representations. Do not copy v0.3 hand-written JSON fixtures.
- Send returns a response containing **Task or Message**. A direct Message has no task lifecycle to poll.
- Configure `returnImmediately: true` explicitly for asynchronous outbound sends. The schema defaults false to waiting for a terminal/interrupted state; some surrounding upstream prose contradicts that specific rule.
- The current official SDK listing includes `a2aproject/a2a-rs`. Its inspected source claims v1 support; this investigation did not establish exact patch conformance, security maturity or TCK certification. Library crate versions are not protocol versions. [SDK]

## Outbound Interface

Proposed model-facing tools, not A2A wire method names:

| Tool | Responsibility |
| --- | --- |
| `a2a_list_agents` | Bounded descriptions and capabilities of host-configured agents |
| `a2a_send` | Send a new request or a reply for an existing nonterminal task |
| `a2a_get_task` | Reconcile a task with the remote authority |
| `a2a_wait` | Bounded wait for changed state, required input or completion |
| `a2a_cancel` | Explicitly request remote task cancellation |

The model chooses a configured agent alias, not an arbitrary URL or credential. Agent Card skills describe advertised capabilities; they are not MCP tool schemas and do not automatically become one tool per skill.

A local opaque handle resolves to remote identity, endpoint/tenant, authorization binding, context ID and task ID. Neither credentials nor arbitrary session-control authority appear in tool arguments. A handle for an existing operation must not silently retarget when a configuration alias changes.

Send outcome is explicitly one of a direct message, a remote task reference, or an uncertain delivery result. Persist an attempt/message ID before sending and persist the remote task ID as soon as known. Remote status remains separate from local observation status such as disconnected or delivery-unknown.

Only explicitly selected request content leaves Pi. Parent history, system prompts, filesystem paths, tool output and credentials are not implicitly exported. Remote descriptions and results remain untrusted data in ordinary tool results, never system instructions.

### Lifecycle and state

Use separate identities:

- configured remote agent: discovery and routing identity;
- remote `contextId`: related conversation/tasks;
- remote `taskId`: one unit of work, possibly spanning multiple messages;
- `messageId`: individual message identity, not guaranteed idempotency;
- local Pi session/run: the caller's execution, not remote lifecycle ownership.

Protocol states, shown as readable labels rather than exact wire enum values:

```text
submitted -> working -> completed | failed | canceled | rejected
                 |
                 +-> input-required / auth-required -> working ...
```

Input/auth-required are nonterminal. Surface them immediately rather than polling forever. Replies use the existing task ID; terminal tasks cannot restart, so additional work uses a new task, optionally in the same context. Credentials required by a remote task go through a trusted out-of-band flow, not chat text. Unknown states are not success. [S §§3, 7.6; P TaskState]

Artifacts are task outputs with stable per-task artifact IDs. Aggregate stream chunks using artifact identity and append/lastChunk semantics; lastChunk does not complete the task. Bound outputs entering model context and retain structured metadata in tool details.

### Failure and reload rules

- Send deduplication by messageId is optional in the standard. Never automatically resubmit an ambiguously acknowledged mutating send. If the task ID is known, query it; otherwise expose uncertainty unless negotiated deduplication makes retry safe.
- SubscribeToTask begins with the current task snapshot; it is not lossless historical replay and has no standard resume cursor. Reconcile snapshots instead of blindly appending replayed artifact chunks. A terminal task is read with GetTask rather than subscribed to.
- Local timeout, cancellation of a wait, dropped SSE connection, and generation reload do not mean the remote task was canceled. Only an explicit CancelTask requests that operation; its result still needs inspection and implies no rollback.
- Save outbound ownership/references through existing session custom entries. For model-invoked tools, ordinary assistant-message persistence precedes tool dispatch; verify that handoff before claiming a send attempt is durable. Custom entries alone do not force an otherwise unsaved session to disk. Any future command/background send before materialization needs its own durable attempt store or must reject that entry path. Plugin reload rebuilds clients and can reconcile saved tasks without repeating sends. New generations cannot silently reuse credentials for a different endpoint. Disabling/removing the feature stops local observers and leaves remote work explicitly unresolved, not falsely canceled.
- No automatic detached completion injection or new scheduler is needed initially; explicit wait/get tools suffice.

## Inbound task ownership

Recommended initial product policy, not an A2A requirement:

```text
(authenticated principal, contextId) -> dedicated managed PiSession
                                 taskId -> durable task state
                                   task -> one or more Pi runs
```

One context has at most one nonterminal task; distinct contexts may run concurrently subject to quotas. Reject conflicting new tasks explicitly rather than allowing Pi's active-run steer behavior to mix tasks. Permit continuation at input-required only when that flow is implemented. A2A allows but does not require additional messages during working. If auth-required is implemented, also support its prescribed negotiation/out-of-band behavior. Task/context pairs must match, and every operation checks caller ownership. [S §§3.3–3.4, 7.6]

Completed tasks remain immutable and queryable for a documented retention period. New work in the same context creates a new task. Cancellation is tied to the exact current task execution under the context gate, not whichever run a mutable session happens to have later.

### Managed input and completion

Current code has an important distinction:

- `AgentSession::submit` interprets registered slash commands and active submissions can steer an existing run.
- `AgentSession::prompt_messages` is a managed literal message path, but deliberately bypasses both command dispatch and input hooks.
- Runtime hook-only preparation is not itself a managed run Interface and must not be invoked directly by the Adapter.

For a text-only inbound first delivery, use the structured managed path with explicitly documented input-hook bypass and a dedicated restricted generation. If input hooks are required, add one protocol-neutral literal-submission policy in `pi-session`, preserving the same policy for queues; do not add an A2A-specific runtime entry or stitch preprocessing/execution across generations.

Derive task completion from the authoritative complete run outcome, including retries and errors, not any assistant message_end or low-level agent_end. Internal tool-call cycles are not separate external tasks. Default external projection includes only deliberate answer/artifact/status data, not thinking, raw tools, system prompts or native session history.

There is currently no durable managed input-required facility. A UI confirmation and a question in assistant prose are not a suspended A2A task. An ask tool with terminate=true also is not sufficient: tool termination requires all results in the batch to terminate, and queued follow-ups can continue execution. Inbound input-required must be a separately tested explicit suspension workflow; omit it from the initial behavior rather than pretending text questions implement it.

### Durable task ledger

The inbound Module needs a private task ledger independent of the Pi transcript. Commit authenticated ownership, accepted request identity, context/session mapping and admission state before acknowledging acceptance. Persist authoritative status and final artifacts there.

This is external task bookkeeping, not another agent/session runtime. Native Pi JSONL still materializes only after the first assistant response. Do not force early session persistence merely to make an accepted network task queryable.

Implement inbound duplicate-message admission explicitly, scoped by caller and context, with payload mismatch detection and a documented retention window. This reduces duplicate admission to our server; it does not create an exactly-once guarantee for external tools.

After restart, recover queryable task records. Uncertain in-progress execution becomes a clear failed/interrupted diagnostic in the existing standard state vocabulary; no implicit submit or external side-effect replay. Persisting acceptance, running Pi and updating the task ledger is not one atomic transaction.

## Security and deployment

- Trust-gate project remote configuration through the existing project trust decision. That decision is not remote caller authorization or an execution sandbox.
- Credential lookup is request-time through host-controlled references. Never put credential material in Agent Cards, prompts, session/task records or logs.
- Discovery cards, redirected endpoints and artifact URLs all pass outbound URL/origin policy; prevent unintended private-network access and cross-origin credential forwarding. Local/private endpoints require explicit host permission. Disable automatic artifact URL fetching initially.
- Inbound host configuration binds authenticated callers to allowed workspaces, tool/plugin selections and quotas. Remote callers cannot supply arbitrary cwd, native plugins, filesystem paths or host credentials.
- Authentication alone is not sufficient for safely exposing a coding agent. Use a dedicated restricted process/user/container where isolation is required; normal filesystem tools remain governed by OS permissions.
- Limit task concurrency, request/response/file sizes, stream buffers, lifetime, model budget and retention. Cancel/inspect/list/subscribe all enforce the same resource authorization.
- Defer push callbacks; they introduce inbound hosting, duplicate delivery, callback authentication and SSRF risk without being necessary for send/poll/SSE use.

## Delivery and verification

1. Evaluate pinned upstream SDK client against v1 fixtures: discovery/version/tenant, direct Message vs Task, returnImmediately, state enums and errors. Adopt only required transports.
2. Deliver outbound text requests, configured auth, send/get/wait/cancel, persisted attempt/task references and bounded outputs. Polling is sufficient initially; SSE can follow with snapshot reconciliation.
3. Add inbound serving only when needed: Agent Card, text task execution, durable admission/query/cancel, principal/context isolation and final text artifacts. Implement required nonoptional operations for the binding; advertise optional features only when supported.
4. Add explicit inbound input-required, richer file/data artifacts, extra bindings and optional push delivery as separate validated increments.

Deterministic fixtures must cover message-only responses; terminal and input/auth-required results; lost send acknowledgments without duplicate send; cancel/completion races; stream reconnect and artifact replacement; reload without resend; duplicate inbound admission; crash before first assistant persistence; cross-principal denial; malicious URLs; literal slash-prefixed text; and complete-run rather than message-end completion. Run repository Rust quality gates when implementation begins.

## Local evidence

- `plugins/features/pi-plugin-subagents/src/runtime.rs`: owned parent-child collaboration; not a remote transport.
- `crates/pi-acp/src/lib.rs`: sibling external Adapter accepting MultiSessionManager.
- `plugins/features/pi-plugin-mcp/src/lib.rs` and `src/plugin.rs`: feature-owned external integration and management tools.
- `crates/pi-session/src/agent_session.rs`: submit, prompt_messages, queues and complete-run settlement.
- `crates/pi-session/src/event.rs`: race-free snapshot/subscription, AgentEnd retry flag and AgentSettled.
- `crates/pi-runtime/src/lib.rs`: structured submission's intentional command/input-hook bypass.
- `crates/pi-agent/src/tool_scheduler.rs` and `src/agent_loop.rs`: tool termination and continuation behavior.
- `docs/architecture.md`: inward dependencies, generations, workspace/trust and lazy persistence invariants.

## Primary references

- [R] [A2A v1.0.1 release](https://github.com/a2aproject/A2A/releases/tag/v1.0.1), tag commit `3303592588e388e62e0f69f701af531d2f4e3991`.
- [S] [Pinned specification](https://github.com/a2aproject/A2A/blob/v1.0.1/docs/specification.md): §§3 task operations/lifecycle, 7 security, 8 discovery, 9–11 bindings, 13 security requirements.
- [P] [Pinned authoritative schema](https://github.com/a2aproject/A2A/blob/v1.0.1/specification/a2a.proto).
- [SDK] [Official SDK listing](https://a2a-protocol.org/latest/sdk/) and [Rust SDK inspected source](https://github.com/a2aproject/a2a-rs/tree/ae3019fa5cea2d93b62e79316258f92e0c0da45a). Evaluate actual published packages before dependency selection; upstream workspace aliases and crate release versions are not protocol version guarantees.
