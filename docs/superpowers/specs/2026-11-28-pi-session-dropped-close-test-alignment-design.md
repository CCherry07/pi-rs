# Pi Session Dropped-Close Test Alignment Design

## Intent

Restore a green `pi-session` lifecycle suite without changing production behavior. Two lifecycle tests predate the manager's complete-on-drop close transaction and still assume that dropping a manager-close waiter cancels that transaction.

## Goals

- Prove that dropping an admitted `MultiSessionManager::close_session` waiter does not cancel owner/child cleanup.
- Keep the direct `AgentSession::shutdown` cancellation regression focused on direct runtime shutdown rather than manager close.
- Preserve prompt settlement, aborted child outcome, session-registry removal, plugin-context retirement, checkpoint persistence, and manager shutdown.
- Preserve current `UnknownSession` policy and all production interfaces.

## Non-goals

- Do not make duplicate `close_session` calls idempotent or coalesced.
- Do not change `MultiSessionManager`, `CompleteOnDrop`, isolated-session registry code, or architecture documentation.
- Do not change legacy Pi compatibility claims; legacy has no equivalent managed isolated-session tree.

## Constraints

- Modify only `crates/pi-session/tests/isolated_lifecycle.rs` and `crates/pi-session/tests/shutdown_state.rs` for executable behavior.
- Keep the existing explicit concurrent-close regression in `management/replacement/tests.rs` unchanged.
- Retain the two-second bounded waits so a lost cleanup still fails deterministically.
- Keep this correction separate from the MCP implementation commit.

## Approaches

1. **Align the stale test with complete-on-drop semantics — selected.** Drop the first close waiter, release the blocked settlement, and directly wait for background cleanup evidence: the settlement finishes, the child reports an aborted outcome, and the manager registry becomes empty. This changes no production behavior.
2. **Make production close idempotent/coalesced — rejected for this slice.** This is an observable lifecycle change on a GitNexus HIGH-risk symbol and conflicts with the existing duplicate-close regression.
3. **Restore cancellable close — rejected.** This violates the documented Rust cancellation-safety guarantee and can strand a closing registered session.

## Test Design

In `closing_an_isolated_owner_drains_prompt_cleanup`:

1. Launch an isolated child and prove its waiter is pending.
2. Start owner close and prove it blocks in `agent_settled` cleanup.
3. Drop that close waiter.
4. Release settlement.
5. Within the existing bound, yield until both `probe.finished` is true and `manager.sessions()` is empty.
6. Assert the retained child waiter resolves to an aborted outcome.
7. Assert normal manager shutdown still succeeds.

The test must not issue a second close request: duplicate-close policy is already owned by the focused replacement regression.

In `cancelled_shutdown_retires_the_plugin_context_after_its_final_write`:

1. Start `AgentSession::shutdown` directly and wait until the shutdown plugin has written its checkpoint and blocked.
2. Prove ordinary external writes are rejected during shutdown.
3. Abort the direct shutdown task, preserving the original cancellation-under-test.
4. Assert the session is closed and a retained plugin context cannot append another entry.
5. Assert the checkpoint was written exactly once and manager shutdown remains successful.

The test must not route through `MultiSessionManager::close_session`: manager close is complete-on-drop, while direct runtime shutdown remains cancellation-sensitive and is the seam this regression intends to cover.

## Validation

- The old isolated test fails with `UnknownSession` at its second close unwrap.
- The old shutdown-state test fails because aborting a manager-close waiter no longer cancels the detached transaction.
- Both rewritten lifecycle tests pass.
- The explicit duplicate-close replacement test remains green.
- The full `pi-session` package suite passes before returning to the MCP plan's workspace gates.
