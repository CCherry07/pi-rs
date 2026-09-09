# pi-rs subagent execution model

Use this note when explaining whether subagents are implemented, what “detached” means, or whether an OS process is required.

## Verified architecture

- `plugins/features/pi-plugin-subagents/src/tool.rs` launches a real isolated child through `SessionContext::launch_isolated_session`, then starts a feature-owned `ChildRun` monitor with `tokio::spawn`.
- `crates/pi-session/src/multi_session_manager.rs` creates the child as an independently managed `PiSession` with `SessionExecutionOrigin::Subagent`.
- `crates/pi-session/src/isolated_session.rs` runs the child prompt in a Tokio task and retains its session/result in `IsolatedSessionRegistry`.
- `plugins/features/pi-plugin-subagents/src/coordination.rs` owns process-local run receipts, supervisor mailboxes, abort handles, oneshot replies, and watch notifications.
- `plugins/features/pi-plugin-subagents/src/child_run.rs` owns completion, timeout, and abort after the initiating tool wait yields.
- `plugins/features/pi-plugin-subagents/src/supervisor_tools.rs` implements blocking `bg_wait`; wait timeout leaves the child running, while `nonBlocking: true` is rejected.

## Terminology

“Detached” means detached from the initiating `subagent` tool wait. The child remains owned by the current pi-rs process and session manager. Call it an **in-process background task**, not a background process or daemon.

The isolated `PiSession` is the logical state/capability boundary. The Tokio task is the concurrency boundary. This provides independent context, prompt, model/tool selection, cancellation, result observation, and session behavior without an OS-process boundary.

## Trade-off framing

For coding-agent work dominated by provider HTTP waits, filesystem I/O, and tool subprocesses, Tokio supplies the needed concurrency cheaply. The in-process design also preserves strong types and directly reuses runtime generations, plugins, credentials, and session semantics.

An independent worker process is justified by product requirements rather than by parallelism alone: survival after the Pi process exits, crash/OOM isolation, OS-level resource controls, cross-client/remote execution, or restart reattachment. It adds IPC schemas, serialization, leases, deduplication, credential transfer, version skew, and daemon lifecycle concerns.

Durable non-blocking subscription is a separate concern from process isolation. A process-local non-blocking notification can be built on the existing coordination/custom-message machinery; cross-restart subscription requires a durable event/run store but does not inherently require one OS process per subagent.

## Lifecycle investigation checklist

When hardening Tokio-backed delegation, inspect both the prompt task and its monitor:

1. Reproduce abnormal exit at launch/wait/shutdown seams. A dropped JoinHandle detaches; check who retains and joins it, and who publishes a terminal receipt on panic or pre-first-poll cancellation.
2. Check drop safety before replacing cooperative cancellation with task abort. Dropping a session prompt may bypass persistence and agent-settled cleanup; explicit shutdown should signal all children before awaiting any one.
3. Acquire generation-bound waits before returning detached control. Test immediate parent reload followed by reply/wait, and reload followed by logical session replacement. Keep current control handles available until child cancellation finishes.
4. Draw the ownership graph through task futures and real plugin adapters. Use weak runtime references where tasks would own their own registry. Capture only an owned outcome receiver before awaiting; manager/run/session temporaries can accidentally preserve task ownership. Rust 2024 opaque futures may require explicit `use<>` lifetime capture.
5. Test notification acceptance separately from task completion. First terminal wins; rebind during an in-flight attempt must not duplicate accepted delivery, and adapter panic must restore a retryable state. Remove/close must prevent stale acknowledgements from reviving notifications.
6. Read the production message adapter before claiming delivery guarantees. A synchronous `Ok` around fire-and-forget prompt launch is not a durable acceptance acknowledgement. Verify actual replacement/queue behavior or state this boundary explicitly.
7. Use deterministic first-poll, barrier, or Notify fixtures rather than sleeps. Test real PiPluginContext plus manager drop, not only fake wait providers. Limit cancelled-shutdown guarantees to the phases actually tested.

## Claims to keep precise

- Implemented: real isolated subagent sessions, concurrent execution, foreground-to-detached handoff, supervisor request/reply, repeated blocking waits, timeout/abort, and completion notification within the current process lifetime.
- Not implied: survival after process exit, process-restart reattachment, fault isolation from native/plugin failure, or durable subscription cursors.
- Do not describe lack of a worker process as “subagent is not implemented.”
- Do not recommend `Command::spawn("pi", ...)` merely for more parallelism; first identify a lifecycle, isolation, or durability requirement.
