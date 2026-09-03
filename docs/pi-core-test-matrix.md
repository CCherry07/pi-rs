# Pi core test conformance matrix

## Purpose

`legacy/pi/packages/agent` is the behavior oracle for Pi's agent and v4 harness core. At the
currently vendored `0.84.2` snapshot it contains 23 test files and 238 static `it` / `test`
declarations; parameterized cases expand beyond that number at runtime.

The Rust layout deliberately does not mirror those files. A behavior is tested at the Module that
owns it:

```text
contracts -> stream assembly -> agent/tool loop -> runtime generation -> session v4
```

Provider protocols, filesystem tools, skills, and resource formatting are included where an
upstream core test crosses those seams. TUI rendering, package/release tooling, live network
providers, and JavaScript-runtime mechanics are not core unit tests.

Run the complete Rust core set with:

```bash
./scripts/test-core.sh
```

The script uses `cargo test --locked` and selects the core crates plus the production skills,
shell, truncation, and filesystem-tool adapters. `cargo test --workspace` remains the superset and
the required CI gate.

## Status meanings

- **Covered**: an executable Rust regression proves the corresponding behavior family.
- **Redistributed**: covered, but split across deeper Rust Modules instead of copied one-for-one.
- **Divergence**: pi-rs intentionally uses a different contract and tests that contract.
- **Gap**: the upstream capability is not implemented, so no passing test claims compatibility.
- **Adapter-specific**: the test exercises Node/JavaScript mechanics that do not exist in the Rust
  Interface; equivalent product behavior may still be covered by a Rust Adapter.

## Oracle-to-Rust mapping

| Legacy suite (static declarations) | Rust ownership and executable evidence | Status |
| --- | --- | --- |
| `agent-loop.test.ts` (23) | [`crates/pi-agent/src/agent_loop/tests.rs`](../crates/pi-agent/src/agent_loop/tests.rs), with additional callback/stream/scheduler/runtime regressions | **Covered with 23 corresponding unit tests.** Uses the oracle's done-only response inputs and directly asserts configured default-stream fallback, independent conversion/transform callbacks, messages, tool usage patches, truncated calls, argument preparation, hook mutations without revalidation, parallel completion/source ordering, sequential overrides, queues, turn preparation, stopping, termination, continuation, exact lifecycle sequences, and empty-context error text. `should_stop_after_turn` precedes queue polling; `prepare_next_turn` runs only before a continuation. The default stream is scoped to a Rust runtime generation; typed tool patches express Pi's argument mutations. |
| `agent.test.ts` (23) | [`crates/pi-agent/src/conformance_tests.rs`](../crates/pi-agent/src/conformance_tests.rs), `pending_queue.rs`, and runtime lifecycle tests | **Covered for the Rust Agent Interface.** Includes state/restore, subscribe/unsubscribe, async settlement, active abort signals, late updates (including a parallel batch), running guards, queueing, assistant-tail steering/follow-up continuation, pre-turn abort closure, exceptional failure lifecycle, mutable session ID, and run-local turn-control replacements that do not mutate Agent configuration. `FnTurnControl` covers ordinary async-closure use without exposing future boxing. |
| `e2e.test.ts` (10) | `pi-agent` thinking/continue regressions, `pi-runtime` scripted-provider tests, and [`e2e/tests/runtime_agent.rs`](../e2e/tests/runtime_agent.rs) | **Covered.** Basic text, tools, pending state, abort, lifecycle, multi-turn context, thinking blocks, and valid/invalid continuation tails are deterministic and offline. |
| `harness/agent-harness-scaffold.test.ts` (4) | `pi-session` reducer, durable queue recovery, deferred JSONL, and `agent_session::tests::open_reconciles_interrupted_run_from_reducer_without_replaying_side_effects` | **Divergence, stronger restore.** The current TypeScript scaffold explicitly rejects every recorded-session restore. Rust reducer-reconciles accepted deferred writes, missing initial input, run queues, and interrupted-operation closure idempotently. Open deliberately performs no provider I/O or blind tool side-effect replay. |
| `harness/branch-summarization.test.ts` (2) | `agent_session::tests::tree_preparation_collects_only_the_abandoned_branch_in_chronological_order` | **Covered.** Includes the chronological abandoned side, common ancestor, and no-previous-leaf case. |
| `harness/compaction.test.ts` (22) | `pi-session::compaction::tests` plus manual, threshold, overflow, abort, retained-tail, and persistence tests in `agent_session` | **Redistributed.** Token accounting, thresholds, cut points, roles, previous summary/tail, split turns, serialization, prompts, errors, usage, file details, active-model reasoning gating, and summary-token clamping to the model output cap are covered. |
| `harness/events.test.ts` (2) | `pi-session::event::tests` and ordered async Agent listener tests | **Covered.** Snapshot revision plus subsequent live delivery and dropped-listener behavior are tested without a terminal dependency. |
| `harness/nodejs-env.test.ts` (27) | `pi-shell`, `pi-tool-support`, and production tool-plugin tests | **Adapter-specific.** Rust covers filesystem/shell results, cancellation, timeout, truncation, path semantics, and full-output persistence. Node `Buffer`, WSL spawn transport, and JavaScript callback mechanics are not Rust Interfaces. |
| `harness/prompt-templates.test.ts` (5) | `pi-plugin-prompts::tests` | **Covered.** The feature plugin owns non-recursive discovery, source provenance, symlink-visible names, frontmatter/fallback descriptions, collision diagnostics, command registration, quoted arguments, and every positional/rest/range expansion form. |
| `harness/reducer.test.ts` (22) | `pi-session::reducer::tests::{reduces_configuration_queues_writes_and_unfinished_step,reduces_tool_batch_deferred_handle_and_terminal_failure,rejects_every_record_log_contradiction_class}` | **Covered.** The Rust contradiction test is table-driven across the record classes, so its three function names represent many cases. |
| `harness/resource-formatting.test.ts` (2) | `pi-plugin-skills::tests::explicit_skill_invocation_formats_location_references_and_task` and `pi-plugin-prompts::tests::registered_template_command_expands_before_agent_input` | **Covered.** Skill and prompt-template invocations are formatted at their owning plugin seams. |
| `harness/session/context.test.ts` (3) | `pi-session::context::tests` | **Covered.** Compaction, custom/bash/branch projection, transforms, and dangling tool-call repair are tested. |
| `harness/session/jsonl-codec.test.ts` (10) | `pi-session::types::tests` and `jsonl::tests` | **Redistributed.** Exact v4 serde shapes, TypeScript user-string compatibility, optional-null rejection, and unknown wire-extension retention are covered. |
| `harness/session/jsonl-storage.test.ts` (5) | `pi-session::jsonl::tests::{repairs_only_a_syntactically_torn_final_append,deferred_materialization_never_overwrites_a_racing_file,...}` | **Covered.** Atomic/deferred materialization and final-append repair rules are exercised. |
| `harness/session/jsonl.test.ts` (26) | `pi-session::{session,jsonl,repo}::tests` | **Redistributed.** Shared sequence, trees/lanes, queries, records/facts, forks, layout, and reopen/replay are covered. |
| `harness/session/memory.test.ts` (2) | `memory::tests::memory_backend_shares_sequence_and_forks_only_the_selected_branch` and generic `Session` tests | **Covered.** The shared backend contract supplies the remaining behavior. |
| `harness/session/search.test.ts` (4) | `session::tests::filtered_branch_queries_and_signed_usage_ledger_match_pi` and record/query tests | **Covered.** Ordering, filtering, limits, branch scope, and usage queries share one storage contract. |
| `harness/skills.test.ts` (6) | `pi-plugin-skills::tests` | **Covered.** Discovery, symlink-following path semantics, metadata/diagnostics, model visibility, trust, collisions, reload, exact invocation, XML escaping, caller-owned sourced provenance, and silent ignoring of undeclared root documentation are owned by the skills plugin. |
| `harness/system-prompt.test.ts` (3) | `pi-plugin-skills::tests::{before_agent_start_contributes_visible_catalog_only_with_read,model_visible_catalog_escapes_every_xml_field_and_preserves_order}` | **Covered.** Visible ordering, disabled skills, and escaping are explicit. |
| `harness/telemetry.test.ts` (4) | `pi-telemetry::tests`, `pi-agent::conformance_tests::provider_requests_emit_typed_ai_telemetry_lifecycle`, and product `submit` run-span wiring | **Covered.** Both exact span vocabularies are strongly typed, unknown/missing attributes are unrepresentable through the marker API, child contexts preserve parentage, no-op/in-memory sinks are tested, provider requests emit response lifecycle data, and product runs emit harness lifecycle spans. |
| `harness/tools.test.ts` (23) | Independent `pi-plugin-{read,write,edit,bash}` tests, `pi-shell`, and Agent tool-update/order tests | **Redistributed.** Core read/write/edit/bash behavior, abort, output, UTF-8/line truncation, update settlement, strict sequential event order, and dynamic `addedToolNames` projection are covered. Node execution-environment injection and its mutation-queue implementation are Adapter-specific. |
| `harness/truncate.test.ts` (9) | `pi-shell::tests`, `pi-plugin-read::tests`, and production grep/find/ls truncation tests | **Covered for valid UTF-8.** Byte/line boundaries, trailing newlines, oversized single lines, and deterministic multi-byte tails are tested. Lone UTF-16 surrogate cases cannot inhabit a Rust `str` and are Adapter-specific. |
| `proxy.test.ts` (1) | `ProviderCallContext`, immutable registries, model runtime, and provider-plugin hook tests | **Adapter-specific.** JavaScript `Proxy` identity mechanics are not a Rust contract; request metadata and generation affinity are directly typed and tested. |

## Closed gaps and explicit compatibility boundaries

The seven previously listed items are now resolved as follows:

1. `prepareNextTurn` and `shouldStopAfterTurn` have a typed `AgentTurnControl` seam, a closure
   Adapter, shared copy-on-write snapshots, and ordering/isolation regressions.
2. Prompt-template discovery and positional expansion are owned by `pi-plugin-prompts`.
3. `pi-telemetry` owns the exact typed schemas; Agent provider calls and product runs emit spans.
4. Recorded-session recovery is reducer-driven and idempotent. Blind deferred/provider or tool
   side-effect replay is deliberately excluded from session open; the current TypeScript scaffold
   does not implement restore at all.
5. Compaction derives reasoning support and maximum output tokens from the active `ModelSpec`.
6. The skill loader preserves generic caller provenance and matches the root-document ignore rule.
7. Node `Buffer`, WSL spawn transport, JavaScript callback mutation queues, and `Proxy` identity are
   Adapter-specific rather than missing Rust Interfaces. Rust directly tests the corresponding
   UTF-8/path/process behavior, settled update queue, immutable registries, and generation-affine
   `ProviderCallContext`.

Most upstream suites remain distributed across owning Rust modules. The AgentLoop suite now maps
each declaration to a named unit test, with a configured stream callback used when the explicit
callback is absent. Additional regressions cover explicit overrides, missing defaults, AgentOptions
callback composition on every turn, default filtering of custom messages without changing history,
runtime-generation defaults, streamed metadata, and cancellation. Session-level custom-message
projection remains explicit, preserves caller overrides, and survives resume and ephemeral history
inheritance.
Prepared tool arguments are validated before `tool_call`; hook
replacements execute without revalidation, matching Pi. Session open still refuses to execute
external work implicitly. These boundaries are documented in `docs/architecture.md` and covered
by focused regressions.

The earlier AgentLoop parity audit also closed five observable gaps:

1. Stream assembly and the Anthropic, Google, OpenAI Completions, OpenAI Responses, and scripted
   adapters preserve every response/content extension they can observe; failures retain partial
   content and metadata.
2. Sequential tools complete their full event/result lifecycle before the next call begins.
3. Continuing from an assistant tail consumes steering first, skips only the duplicate initial
   poll, and falls back to follow-up only when steering is empty.
4. Abort and escaping loop errors emit Pi-shaped terminal assistant, turn, and agent events; Rust
   still reports the originating exceptional error to its caller.
5. `addedToolNames` survives tool execution, hook patches, JavaScript adaptation, and transcript
   messages, while native before/after tool hooks receive the batch `AgentContext` snapshot. This
   trait-surface change was introduced in native ABI 2; ABI 3 added the required hook-interest
   contract, ABI 4 adds provider header/response lifecycle hooks, ABI 5 adds the shared
   generation-bound Pi product context (including tool argument preparation), ABI 6 shares
   cumulative assistant messages, ABI 7 replaces per-update cumulative messages with a shared
   live stream handle plus typed delta and requires reload to hand off fresh generation
   capabilities, and ABI 8 adds semantic UI confirmation. Older artifacts are rejected before
   loading.

## Updating this matrix

When the vendored Pi oracle changes:

1. Recount test declarations under `legacy/pi/packages/agent/test` and inspect changed source.
2. Map behavior to the deepest Rust owner; do not duplicate a session invariant in an E2E test.
3. Add a focused regression before changing behavior.
4. Change a **Gap** only after the capability and its test both exist.
5. Run `./scripts/test-core.sh` followed by the full workspace quality gates.
