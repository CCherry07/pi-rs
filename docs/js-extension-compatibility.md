# JavaScript extension compatibility

This matrix records the observable JavaScript extension surface implemented by the Node/NAPI host.
`legacy/pi` remains the behavior oracle; “inactive” means the host recognizes the Pi facility but
does not register or execute it. Inactive registration is non-fatal and appears in the generation's
diagnostics. Unknown names still fail construction.

## Hooks

| Lifecycle | Hook | Status |
| --- | --- | --- |
| Agent | `input` | Active; receives `text`, `images`, `source`, and optional `streamingBehavior`. Continue, handled, and chained text/image transforms are supported. |
| Agent | `before_agent_start` | Active; Pi prompt, images, chained system prompt, and system-prompt options are passed. Both `systemPrompt` replacement and persistent custom-message injection are supported and chain in registration order. |
| Agent | `agent_start`, `agent_end`, `agent_settled` | Active; `agent_settled` fires after session-owned retry, compaction, and queued continuation have finished and before the product settled event. |
| Agent | `turn_start`, `turn_end` | Active; both carry `turnIndex`. Session `turn_end` supports typed journal proposals, persisted message/tool-result IDs, context previews and `continue`. |
| Agent | `agent_before_settle` | Active; proposes journal entries and another response after retries, compaction and queued work. |
| Agent | `message_start`, `message_update`, `message_end` | Active; role-preserving `message_end` replacements chain and become the live and persisted message. |
| Agent | `tool_execution_start`, `tool_execution_update`, `tool_execution_end` | Active. |
| Agent | `context` | Active; message replacement is supported. |
| Agent | `tool_call` | Active; in-place input mutation, blocking, reason, and terminate are supported. |
| Agent | `tool_result` | Active; content, details, usage, `addedToolNames`, and error patches are supported. |
| Provider | `before_provider_request` | Active; payload replacements chain in registration order. |
| Provider | `before_provider_headers` | Active after a built-in HTTP provider assembles its final headers and immediately before transport. Handlers mutate the shared header object in registration order, return values are ignored, and `null` deletes a header. |
| Provider | `after_provider_response` | Active after an HTTP response arrives and before its body stream is consumed; status and response headers are observable. |
| Session | `session_start`, `session_info_changed`, `session_before_switch`, `session_before_fork`, `session_before_compact`, `session_compact`, `session_compact_failed`, `session_shutdown`, `session_before_tree`, `session_tree` | Active through the session plugin lifecycle. |
| Product/UI | `project_trust`, `resources_discover`, `model_select`, `thinking_level_select`, `user_bash` | Recognized and inactive. |

Supported callbacks run in registration order. A rejected callback, malformed patch, or invalid
cross-role `message_end` replacement is recorded in the generation's diagnostics and later
callbacks continue from the last valid value. `tool_call` is the deliberate exception: it remains
fail-closed for the affected tool call, matching Pi's runner.

Actionable `turn_end` and `agent_before_settle` callbacks receive a refreshed preview after every
handler and atomically commit valid final drafts. See [the session boundary guide](../crates/pi-session/README.md).

Agent observer hooks are dispatched as one generation batch per event. Handlers still await
sequentially, share the same event object, and isolate failures. `message_update` uses a compact
stream wire: Rust sends the initial assistant message once, then only a stream ID and typed delta.
Node retains chunk arrays and exposes lazy `message` and `assistantMessageEvent` snapshot getters.
An empty hook never copies the accumulated text; if any handler reads a snapshot, the whole batch
shares that single detached materialization.

Provider header and response hook failures follow the same diagnostic isolation rule. Header
deletion tombstones remain visible to later hooks and are removed only when the final map crosses
the transport seam. Response observers run for successful and error HTTP statuses before any
provider parses or buffers the body.

Native `Plugin` tool hooks receive an `Arc<AgentContext>` batch snapshot. The JavaScript
Adapter intentionally does not add that field to extension events because current Pi extension
`tool_call` / `tool_result` payloads expose only call/result data; the full context belongs to Pi's
lower-level before/after-tool callback contract.

## Context capabilities

Base contexts support lazy reads for cwd, project trust, current model/thinking level, available
models (including the resolved `enabledModels` scope), idle/queue state, context usage, the effective
system prompt, and the read-only `sessionManager` tree/branch/identity surface. `abort`, background
compaction, and graceful product shutdown are explicit non-replacing controls. `modelRegistry`
provides read-only
`getAll`, `getAvailable`, `find`, `hasConfiguredAuth`, and `getProviderDisplayName`; provider auth,
completion, and refresh remain inactive. Configuration-form `pi.registerProvider(name, config)` and
`pi.unregisterProvider(name)` are active. A command's immediately following `pi.setModel` flushes a
pending provider generation first; calls from active hooks become visible after that run settles so
one run never mixes immutable generations.

The generation-local `pi` API also exposes the active/all tool and command catalogs, model and
thinking selection, session name/labels, custom context messages, user messages, durable custom
entries, and active-tool mutation through the same native session capability. These mutations use
`AgentSession` rather than editing JSONL in Node, so queued custom messages are durable before the
agent observes them and no-turn messages are reflected in both live and resumed context. `pi.exec`
matches Pi's direct-spawn contract, including cwd override, abort, timeout, captured output, exit
code, and killed status.

During `before_agent_start`, `ctx.getSystemPrompt()` is invocation-local and returns the same
chained value as `event.systemPrompt`; it does not fall back to the reusable base prompt queried
from the current session.

Command contexts additionally support `getSystemPromptOptions`, `waitForIdle`, `newSession`, `fork`,
summarizing and non-summarizing `navigateTree`, `switchSession`, and `reload`. `parentSession` is
preserved on the new Pi session header; `setup`, `withSession`, and the replacement-only
`sendMessage` / `sendUserMessage` surface run against the newly published `PiSession`. Unlike Pi,
the JavaScript `setup` callback runs after the replacement session's `session_start` hook because
the Rust generation is prepared and activated atomically before the old command resumes. `reload`
also advances the callback's native capability before resolving, so the same JavaScript command
context can immediately query and operate on the reloaded generation.

## UI and registrations

Interactive Node/NAPI sessions expose `ctx.hasUI = true`. Both Pi TUI package names resolve to
real `pi-tui` **1.3.0** components, pinned under the private dependency alias
`@pi-rs/pi-tui-runtime`. This requires Node **22.19+**. The alias prevents Jiti's compatibility
mapping from recursively resolving its own implementation. Other missing dependencies still fail.

The generation owns JavaScript component objects; `apps/pi-cli` owns input and terminal rendering.
`render(width)` ANSI output is decoded into typed text/style spans before crossing NAPI, and
`CURSOR_MARKER` becomes a frontend cursor position. No component frame executes terminal escape
sequences. Frames are cached by Ratatui and refreshed through an asynchronous exchange; keyboard
and bracketed-paste events return to the component without blocking the session event loop.

| UI capability | Interactive support |
| --- | --- |
| `select`, `confirm`, `input`, `editor` | Selection, cancellation, prefill, multiline editing, timeout countdown and `AbortSignal`; also while awaiting `session_start`. |
| `custom` | Inline editor replacement or overlay, async factories, keyboard focus, `done(result)`, nested prompts, resize, refresh requests and disposal. |
| `setWidget`, `setHeader`, `setFooter` | String widgets and actual component factories; above/below editor slots, replacement and disposal. Footer statuses are available; Git-branch data is not yet supplied. |
| Editor state | Read, replace, paste, custom editor factories; `CustomEditor` uses real Pi editor behavior with Rust submit/escape/exit handoffs. Additional autocomplete providers and the full app action keymap remain gaps. |
| Notifications/status/title | Product notices, keyed footer statuses, terminal title. |
| Input/shortcuts | Input listeners and registered keyboard shortcuts. Input is reconstructed from Crossterm events; exact original terminal bytes, mouse routing and key-release events are not provided. |
| Theme/tools | A dark extension theme, text styling helpers and tool-expansion getter/setter. Theme discovery/switching beyond this theme remains unsupported. |

Component disposal runs on replacement, close, frontend shutdown and generation retirement.
Pending dialogs resolve as cancelled; old owners cannot remount after retirement. A failed generation
preparation preserves the existing UI. A renderer exception produces a product notice and closes
that generation's component host. Frame transport is transient and does not change session JSONL.

Working-indicator customization, hidden-thinking labels, external-editor launch, image protocols,
full overlay focus/geometry options and arbitrary terminal operations remain unsupported. These
limits mean compatibility with every upstream UI extension is not claimed. Print/JSON/RPC still
use inactive UI defaults (notifications remain active); Desktop legacy JS UI is outside this adapter.

The configuration-form provider registration surface supports `name`, `baseUrl`, `apiKey`, `api`,
`headers`, `authHeader`, and `models`. Supplying `models` replaces the provider catalog; omitting it
preserves lower-layer models, and unregistering restores the built-in/`models.json` layers on the
next safe generation. The full `Provider` object overload and the `streamSimple`, `refreshModels`,
and OAuth callbacks are recognized and inactive.

Message/Markdown/entry renderer registrations remain recognized and inactive. Tool/shell render
callbacks and global tool renderer registration are not implemented. Extension flags are active; boolean flags use `--name` and string
flags accept both `--name value` and `--name=value`; first registration wins for duplicate names,
and unregistered flags fail generation construction. Tool
`prepareArguments` runs before Rust validation through its own asynchronous adapter callback using
the same runtime-generation native context as tool execution, and
registered tools can publish partial content/details through their streaming update callback. The
generation-local `pi.events` bus is active, isolates listener failures, and drops all listeners on
retirement.

## Module compatibility

The host covers every module specifier in Pi's extension-loader alias table for both
`@earendil-works` and `@mariozechner`: `pi-coding-agent`, `pi-agent-core`, `pi-ai`,
`pi-ai/compat`, `pi-ai/oauth`, and `pi-ai/providers/all` resolve to the one host compatibility
module. Both `pi-tui` namespaces resolve to the real component compatibility module
described above. The host also maps `typebox` and `@sinclair/typebox`, including supported subpaths
such as `/compile` and `/value`, to the single TypeBox runtime bundled by the host. Subpath aliases
take precedence over package-root aliases so resolution cannot produce paths such as
`build/index.mjs/value`.

Module resolution coverage does not imply that every runtime export from the upstream JavaScript
Agent, provider, or TUI implementations exists in pi-rs. The compatibility module exposes the
runtime-neutral helpers documented by this matrix; type-only imports disappear during transpilation,
and unsupported runtime capabilities remain explicit product gaps.
