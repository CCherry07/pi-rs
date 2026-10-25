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

Interactive Node/NAPI sessions with an installed frontend bridge expose `ctx.hasUI = true` and use
the shared Rust component runtime. TUI mode without a bridge reports false. `@pi-rs/cli` exports
the declarative UI types and supports Pi `custom(factory)` components through `PiComponentAdapter`.
The host pins `@earendil-works/pi-tui` and aliases both supported Pi scopes to it. It does not start
the upstream terminal loop or install the Coding Agent npm runtime. Node 22.19+ is required.

| UI capability | Interactive support |
| --- | --- |
| `select`, `confirm`, `input`, `editor` | Shared compact panels with titles, padding, selection markers and keyboard hints; width is capped at 60 cells and the terminal width. Native Rust input, prefill, cancellation, timeout and AbortSignal work during `session_start` too. Editor Enter inserts a newline; Ctrl+Enter submits. |
| `mount`, `custom` | Semantic trees, stable node IDs, update/unmount/focus and slots. `custom(factory)` also accepts Pi components and defaults to the editor region. Both custom forms share completion and cancellation. |
| Overlays | Static/dynamic options, width/minWidth/maxHeight, anchors, absolute/percentage positions, margins, offsets and visibility predicates. Handles support hide/show, focus/release, focus queries and actual Rust-rendered bounds. Noncapturing overlays leave existing input focus intact. |
| Layout and controls | Text, Link, Markdown, Column, Row, Panel, Spacer, Scroll, Select, Input, Editor, Button, Progress and Image. Rust owns measurement, clipping, focus, scrolling, edits and completions. |
| Rich selection | Searchable/filterable/sortable multi-selection is a shared semantic node exposed by `ctx.ui.multiSelect`; native Rust uses the same node through `UiContext::multi_select`. |
| Widgets/header/footer/status | Semantic nodes and persistent Pi component factories; widgets also accept string arrays. Replacement/clearing/retirement disposes components. Footer factories receive Git branch subscriptions, extension statuses and available provider count. |
| Input/shortcuts | Rust handles semantic control input and registered shortcuts. Focused Custom nodes receive reconstructed keyboard/paste events, with component-local listeners and focus in JS. Terminal control and global interception remain unavailable. |
| Mouse callbacks | Mounted Pi components and widget/header/footer factories receive `handleMouse`: press, release, move, drag, click and vertical wheel, with nested targets, capture, focus and render results. Rust owns hit testing, clipping, gestures and global focus. Transcript-only renderers remain noninteractive. |
| Themes | Shared built-in light/dark/system palettes; Pi formatting and editor/selection/Markdown theme helpers use terminal palette colors. `getSettingsListTheme` supplies the theme contract for upstream `SettingsList` components. Complete upstream themes, JSON discovery/watchers and custom keybinding files remain unsupported. |
| Transcript renderers | Message, entry and tool callbacks return nodes or Pi components; Markdown transformers return text. Tool metadata retains arguments, details, shared state, separate call/result `lastComponent`, output padding, image policy and duration. |
| System operations | Rust-owned title, progress/program status, clipboard, suspend and external editor. Image data is decoded/encoded by Rust for Kitty/iTerm; Link becomes a clipped terminal hyperlink. |
| Editor/working state | Read/replace/paste composer text, tool expansion, working message/visibility and hidden-thinking labels. Custom editor UI uses the shared Editor node. |

Stable node IDs preserve Rust editing state across description changes. Semantic controls edit
locally; Pi components process their internal state in JS. Removed/replaced nodes cannot receive queued events; retained nodes keep valid queued
submissions. Unmount, cancellation, host failure and generation retirement settle pending dialogs
and release callbacks. Failed generation preparation preserves live UI. Print/JSON/RPC use inactive
UI defaults (notifications remain active). The CLI TUI is the current renderer; Desktop reports UI
unavailable until it provides its own semantic-surface adapter.

This adapter is a compatibility subset. Custom editor subclasses,
complete theme configuration and arbitrary terminal operations are not supported. Use the existing
semantic surfaces and host operations where available. Dynamic overlay option functions are
reevaluated on refresh/resize; local upstream currently evaluates them only when opening.
An overlay's `hide()` removes its region without settling `custom`; completion/cancellation or
generation retirement still owns disposal. Focus and bounds queries use the latest frontend
snapshot. Native plugins must be rebuilt for UI ABI 33.

Mouse coordinates are zero-based terminal cells, with component-local and screen coordinates;
captured local coordinates may be negative. `capture` and `focus` imply `handled`. A handled press
retains its component until release, with a click emitted only when the pointer did not move.
The JS adapter retains the concrete nested mouse target and preserves keyboard-delegating
containers. `render: false` suppresses callback-driven rendering, while explicit requests and host
refreshes remain effective. Mouse-only slots do not take keyboard focus on mount.

The host waits for mouse results asynchronously and preserves subsequent input order. A gesture
retains its original coordinate transform across ordinary content reflow, including a settings
description changing height on press. The rest of the Rust terminal loop keeps running. Hiding/removing a component, resize,
focus loss or generation retirement cancels its gesture. A two-second reply timeout or the bounded
input queue overflowing also cancels it; Ctrl+C/Ctrl+D remain available. Cancelled/failed requests
do not click through to app controls, and replies from replaced mounts cannot steal focus. These
deadline/cancellation rules are host additions to Pi's synchronous mouse callback contract.

See [the shared plugin UI contract](plugin-ui.md) for usage and deliberate API changes.

The configuration-form provider registration surface supports `name`, `baseUrl`, `apiKey`, `api`,
`headers`, `authHeader`, and `models`. Supplying `models` replaces the provider catalog; omitting it
preserves lower-layer models, and unregistering restores the built-in/`models.json` layers on the
next safe generation. The full `Provider` object overload and the `streamSimple`, `refreshModels`,
and OAuth callbacks are recognized and inactive.

Message/Markdown/entry renderer registrations, tool call/result renderers and global tool renderer
registration are active in interactive mode. Extension flags are active; boolean flags use `--name` and string
flags accept both `--name value` and `--name=value`; first registration wins for duplicate names,
and unregistered flags fail generation construction. Tool
`prepareArguments` runs before Rust validation through its own asynchronous adapter callback using
the same runtime-generation native context as tool execution, and
registered tools can publish partial content/details through their streaming update callback. The
generation-local `pi.events` bus is active, isolates listener failures, and drops all listeners on
retirement.

## Module compatibility

The host provides runtime-neutral SDK aliases for both
`@earendil-works` and `@mariozechner`: `pi-coding-agent`, `pi-agent-core`, `pi-ai`,
`pi-ai/compat`, `pi-ai/oauth`, and `pi-ai/providers/all` use the one host compatibility
module. `@pi-rs/cli` resolves to the same host SDK. Both `pi-tui` namespaces resolve to the pinned
`@earendil-works/pi-tui` dependency. The host also maps `typebox` and `@sinclair/typebox`, including supported subpaths
such as `/compile` and `/value`, to the single TypeBox runtime bundled by the host. Subpath aliases
take precedence over package-root aliases so resolution cannot produce paths such as
`build/index.mjs/value`.

Module resolution coverage does not imply that every runtime export from the upstream JavaScript
Agent, provider, or TUI implementations exists in pi-rs. The compatibility module exposes the
runtime-neutral helpers documented by this matrix; type-only imports disappear during transpilation,
and unsupported runtime capabilities remain explicit product gaps.
