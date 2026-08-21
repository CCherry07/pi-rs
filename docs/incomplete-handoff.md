# Implementation status

Status: the scoped plugin-first Agent Core MVP is implemented and all current quality gates pass.

## Implemented

- Workspace crates:
  - `crates/pi-core`
  - `crates/pi-agent`
  - `crates/pi-runtime`
  - `crates/pi-session`
  - `plugins/providers/pi-plugin-faux-provider`
  - `plugins/tools/pi-plugin-test-tools`
- Core message, stream, tool, provider, registry, plugin-driver, and cancellation contracts.
- Registration-ordered static Rust plugins with duplicate plugin/tool/provider rejection.
- Frozen provider/tool registries with plugin ownership metadata.
- `StreamAssembler` with transition validation, partial snapshots, strict final tool-argument parsing, and error normalization.
- Stateless `AgentLoop` with text turns, tool turns, steering, follow-up, bounded tool iterations, abort, and balanced lifecycle events.
- `ToolScheduler` with source-ordered preparation, sequential override, bounded parallel execution, update draining, completion-ordered end events, and source-ordered tool-result messages.
- Cloneable stateful `Agent` façade with prompt/continue, abort, wait-for-idle, queues, state reduction, subscriptions, and settlement semantics.
- `PiRuntime` builder that registers plugins and constructs the frozen runtime.
- Pi v4 session tree with shared mutation sequence, lanes, operation/usage records, facts,
  context projection, in-memory and JSONL repositories, branch/tree forks, pure recovery reduction,
  and runtime restore.
- Scripted Faux Provider and deterministic echo/delay/fail/update/abort test tools.
- Updated `docs/architecture.md` matching the implementation.

## Covered behavior

- Parallel tools finish out of order while transcript results remain source ordered.
- A sequential tool forces the full batch to execute sequentially.
- Unknown and failed tools become error tool-result messages.
- Plugins can block calls and patch executed results.
- Blocked/immediate outcomes do not run after-tool hooks.
- Provider and tool execution cancellation settle and restore idle state.
- Steering and follow-up keep an otherwise settled run alive.
- Agent remains running until awaited `agent_end` listeners settle.
- Malformed provider streams end with a complete error lifecycle.
- Tool updates accepted before settlement precede `tool_execution_end`.
- Maximum tool-iteration exit has balanced turn/agent lifecycle events.
- Stream assembler rejects malformed transitions, duplicate start, events after Done, missing Done, and invalid final tool JSON.

## Validation

All passed:

```text
cargo test --workspace
cargo check --workspace --all-targets
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Current workspace test total: 112 passed, 1 ignored (the opt-in real-provider test).

## Explicitly out of scope for this milestone

- Real HTTP/SSE providers
- Production tools
- Harness-level crash/deferred operation replay and execution wiring
- CLI/RPC/TUI
- QuickJS/WASM
- Native dynamic plugin loader and proc macro
- Auth and model catalogs

## Next starting position

The next vertical slice should be SSE parsing plus one real provider plugin, or production tool plugins. Keep `pi-core` runtime-neutral and preserve the event/order contracts documented in `docs/architecture.md`.

## Git state

`/Users/cherry/Documents/pi_rs` has no `.git` directory. No commit was possible, and no repository was initialized.
