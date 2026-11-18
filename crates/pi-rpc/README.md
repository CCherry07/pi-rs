# Pi RPC adapter

`pi-rpc` implements the stdin/stdout protocol in the checked-in
[`legacy/pi`](../../legacy/pi/packages/coding-agent/src/modes/rpc/rpc-mode.ts).
The adapter is shared by the standalone CLI and the Node extension host.

The wire consists of LF-delimited JSON commands, correlated responses, session events and
extension UI records. CRLF input and a final record without a newline are accepted; Unicode
line and paragraph separators inside strings remain part of that record. The Node host sends
extension logging to stderr so stdout contains protocol records.

## Command surface

| Family | Commands |
| --- | --- |
| Input | `prompt`, `steer`, `follow_up`, `abort`, `clear_queue` |
| State | `get_state`, `get_messages`, `get_commands` |
| Models | `set_model`, `cycle_model`, `get_available_models` |
| Thinking | `set_thinking_level`, `cycle_thinking_level`, `get_available_thinking_levels` |
| Queues | `set_steering_mode`, `set_follow_up_mode` |
| Compaction | `compact`, `set_auto_compaction` |
| Retry | `set_auto_retry`, `abort_retry` |
| Shell | `bash`, `abort_bash` |
| History | `new_session`, `switch_session`, `fork`, `clone`, `get_fork_messages`, `get_entries`, `get_tree`, `get_last_assistant_text`, `set_session_name`, `get_session_stats`, `export_html` |

Prompt responses acknowledge preparation with `started`, `queued` or `handled`. A handled input
has no associated agent run. Clients consume `agent_settled` for final settlement and its
`aborted` flag for cancellation. `abort` waits for idleness before responding. Input hooks receive
the `rpc` source; extension commands and input interception precede the streaming admission check.

Explicit steering and follow-up commands can queue while idle. Model cycling respects the scoped
model list and per-model thinking preferences. Command discovery retains extension, prompt and
skill provenance. Shell output repeats a command ID only when the client supplied one.

## Extension UI and shutdown

Supported UI methods are `select`, `confirm`, `input`, `editor`, `notify`, `setStatus`, text
`setWidget`, `setTitle` and `set_editor_text`. Dialog replies use the request ID. Unknown and
retired IDs are ignored; cancellation, timeout, generation retirement and disconnect release
pending dialogs. Optional values preserve omission separately from explicitly empty text.
Arbitrary terminal components use the TUI frontend.

Extension-requested shutdown completes outstanding commands and the active run before disposing
the session. Stdin EOF starts disposal. On Unix, SIGTERM and SIGHUP start orderly disposal; one
signal after EOF preserves that cleanup, while a repeated signal exits immediately.

Native persistence retains the v4 journal and lazy first-assistant materialization. History
queries project the Pi entry format through `json_wire`; clients should use RPC commands to
inspect and manipulate the session.

## Validation

`cargo test -p pi-rpc` checks wire projection, UI correlation, stdout backpressure and public
command dispatch. Session admission, queue, cancellation and summarization retry regressions
live at their owners in `pi-session` and `pi-runtime`.

The Node bridge fixture in
[`rpc-interop.smoke.ts`](../../packages/pi/test/rpc-interop.smoke.ts) uses a local fake provider,
temporary settings and real JS extensions. Build the matching native library and Node launcher
before running it:

```sh
cargo build -p pi-napi
npm --prefix packages/pi run build
PI_RS_NATIVE_BINDING="$PWD/target/debug/libpi_napi.dylib" \
  packages/pi/node_modules/.bin/tsx --test packages/pi/test/rpc-interop.smoke.ts
```

Use the host's `.so` or `.dll` library on other platforms. These tests require no provider
credentials or external model requests. Repository-wide completion gates remain documented in
[`AGENTS.md`](../../AGENTS.md).
