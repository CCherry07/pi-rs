# Pi Session Dropped-Close Test Alignment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Align two stale session-lifecycle tests with the documented split between complete-on-drop manager close and cancellation-sensitive direct runtime shutdown, without modifying production behavior.

**Architecture:** Keep `MultiSessionManager::close_session`, `CompleteOnDrop`, direct `AgentSession::shutdown`, and duplicate-close policy unchanged. The isolated-owner test observes detached manager-close completion directly; the shutdown-state test invokes direct runtime shutdown so aborting its task continues to exercise the intended cancellation seam.

**Tech Stack:** Rust, Tokio, Cargo test

**Spec:** `docs/superpowers/specs/2026-11-28-pi-session-dropped-close-test-alignment-design.md`

## Global Constraints

- Modify only `crates/pi-session/tests/isolated_lifecycle.rs` and `crates/pi-session/tests/shutdown_state.rs` for executable behavior.
- Do not change any production Rust source or public interface.
- Preserve current `UnknownSession` and complete-on-drop semantics.
- Keep the correction separate from the MCP implementation commit.
- Use `CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0` because worktree disk space is constrained.

## Review Focus

- Dropping the admitted close waiter must not cancel prompt cleanup or leave sessions registered; the rewritten isolated lifecycle test proves both terminal conditions.
- The child waiter must still resolve to an aborted outcome after background cleanup; the rewritten isolated lifecycle test retains that assertion.
- Duplicate concurrent close policy must remain unchanged; the existing replacement regression must stay green.
- A background cleanup hang must fail within two seconds; all new waiting remains inside `bounded`.
- Direct runtime-shutdown cancellation must still retire retained plugin contexts after the final checkpoint write; the shutdown-state regression invokes that seam directly.
- Final manager shutdown after detached or direct cleanup must remain successful; both rewritten tests retain shutdown assertions.

---

### Task 1: Align the dropped-close lifecycle regression

**Files:**
- Modify: `crates/pi-session/tests/isolated_lifecycle.rs:370-407`
- Test: `crates/pi-session/tests/isolated_lifecycle.rs`

**Interfaces:**
- Consumes: Existing `bounded`, `LifecycleProbe`, `MultiSessionManager::sessions`, and the documented complete-on-drop close contract.
- Produces: A regression that proves detached close cleanup without changing any production interface.

- [x] **Step 1: Preserve RED evidence**

Run:

```bash
CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test -p pi-session --test isolated_lifecycle \
  closing_an_isolated_owner_drains_prompt_cleanup -- --exact
```

Expected: FAIL at the second close unwrap with `UnknownSession`.

- [x] **Step 2: Rewrite the stale assertion path**

After `drop(close)`, notify `probe.release_settled`, then use `bounded(async { ... })` to yield until `probe.finished` is true and `manager.sessions().is_empty()`. Retain the aborted child outcome and successful manager shutdown assertions. Remove the second `close_session` request and its pending poll.

- [x] **Step 3: Verify the aligned and neighboring contracts**

Run:

```bash
CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test -p pi-session --test isolated_lifecycle \
  closing_an_isolated_owner_drains_prompt_cleanup -- --exact

CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test -p pi-session \
  management::replacement::tests::close_session_cancels_an_unpublished_reload_without_closing_the_manager \
  -- --exact
```

Expected: both PASS.

### Task 2: Restore the direct runtime-shutdown cancellation regression

**Files:**
- Modify: `crates/pi-session/tests/shutdown_state.rs:196-244`
- Test: `crates/pi-session/tests/shutdown_state.rs`

**Interfaces:**
- Consumes: Existing `AgentSession::shutdown`, `CheckpointPlugin`, retained command context, and manager cleanup.
- Produces: A regression that exercises direct runtime-shutdown cancellation without routing through complete-on-drop manager close.

- [x] **Step 1: Preserve RED evidence**

Run:

```bash
CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test -p pi-session --test shutdown_state \
  cancelled_shutdown_retires_the_plugin_context_after_its_final_write \
  -- --exact --nocapture
```

Expected: FAIL at `assert!(session.is_closed())` because aborting a manager-close waiter does not cancel its detached transaction.

- [x] **Step 2: Route the cancellation test through direct runtime shutdown**

Spawn `session.shutdown()` using an `Arc::clone(&session)` instead of spawning `manager.close_session(&owner)`. Keep the checkpoint-write, rejected external/retained writes, closed-session, single checkpoint, and final manager-shutdown assertions unchanged.

- [x] **Step 3: Verify the focused shutdown-state contract**

Run:

```bash
CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test -p pi-session --test shutdown_state \
  cancelled_shutdown_retires_the_plugin_context_after_its_final_write \
  -- --exact
```

Expected: PASS.

- [x] **Step 4: Verify the owning package**

Run:

```bash
CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p pi-session
```

Expected: all `pi-session` tests PASS.

- [x] **Step 5: Run structural checks**

Run:

```bash
cargo fmt --all -- --check
git diff --check
node /Users/bytedance/Documents/pi-rs/.gitnexus/run.cjs detect-changes --scope all --repo pi-rs
```

Expected: format and diff checks exit 0; GitNexus reports the two intended test symbols (plus the parent MCP worktree changes) and is not partial/truncated.

- [x] **Step 6: Commit separately after the parent MCP plan's required gates pass**

```bash
git add \
  crates/pi-session/tests/isolated_lifecycle.rs \
  crates/pi-session/tests/shutdown_state.rs
git add -f \
  docs/superpowers/specs/2026-11-28-pi-session-dropped-close-test-alignment-design.md \
  docs/superpowers/plans/2026-11-28-pi-session-dropped-close-test-alignment.md
git commit -m "test(pi-session): align dropped close lifecycle expectations"
```

Do not include MCP source files or the MCP Slice design/plan documents in this test-only commit. If the parent plan requires one final combined commit instead, retain the test corrections and their two documents as a separately reviewable staged diff and record that deviation.
