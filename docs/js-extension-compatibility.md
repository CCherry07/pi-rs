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
| Agent | `turn_start`, `turn_end` | Active; both carry a zero-based per-run `turnIndex`, and `turn_start` carries `timestamp`. |
| Agent | `message_start`, `message_update`, `message_end` | Active; role-preserving `message_end` replacements chain and become the live and persisted message. |
| Agent | `tool_execution_start`, `tool_execution_update`, `tool_execution_end` | Active. |
| Agent | `context` | Active; message replacement is supported. |
| Agent | `tool_call` | Active; in-place input mutation, blocking, reason, and terminate are supported. |
| Agent | `tool_result` | Active; content, details, usage, and error patches are supported. |
| Provider | `before_provider_request` | Active; payload replacements chain in registration order. |
| Session | `session_start`, `session_info_changed`, `session_before_switch`, `session_before_fork`, `session_before_compact`, `session_compact`, `session_compact_failed`, `session_shutdown`, `session_before_tree`, `session_tree` | Active through the session plugin lifecycle. |
| Product/UI | `project_trust`, `resources_discover`, `before_provider_headers`, `after_provider_response`, `model_select`, `thinking_level_select`, `user_bash` | Recognized and inactive. |

Supported callbacks run in registration order. A rejected callback, malformed patch, or invalid
cross-role `message_end` replacement is recorded in the generation's diagnostics and later
callbacks continue from the last valid value. `tool_call` is the deliberate exception: it remains
fail-closed for the affected tool call, matching Pi's runner.

## Context capabilities

Base contexts support lazy reads for cwd, project trust, current model/thinking level, available
models, idle/queue state, context usage, the effective system prompt, and the read-only
`sessionManager` tree/branch/identity surface. `abort`, background compaction, and graceful product
shutdown are native notifications. `modelRegistry` currently provides read-only
`getAll`, `getAvailable`, `find`, `hasConfiguredAuth`, and `getProviderDisplayName`; provider auth,
completion, refresh, and dynamic registration remain inactive.

During `before_agent_start`, `ctx.getSystemPrompt()` is invocation-local and returns the same
chained value as `event.systemPrompt`; it does not fall back to the reusable base prompt queried
from the current session.

Command contexts additionally support `getSystemPromptOptions`, `waitForIdle`, `newSession`, `fork`,
non-summarizing `navigateTree`, `switchSession`, and `reload`. `withSession` runs against the newly
published `PiSession`. `newSession({ parentSession, setup })`, summarized tree navigation, and the
replacement-only `sendMessage` / `sendUserMessage` surface are not implemented yet and fail at the
point of use rather than corrupting session state.

## UI and registrations

JavaScript extensions never own terminal rendering. `ctx.hasUI` is always false and `ctx.ui` is an
explicit NoOp object: selection/input/editor/custom calls resolve `undefined`, confirmation resolves
`false`, getters return empty values, and setters/notifications do nothing. Factories supplied to UI
registration methods are not executed.

If an extension cannot be imported solely because the host intentionally does not provide the
`@earendil-works/pi-tui` or `@mariozechner/pi-tui` peer, that extension entry is skipped with an
inactive diagnostic and the rest of the generation continues. Missing non-UI dependencies remain
fatal so broken packages and installation mistakes are not hidden.

The following registration surfaces are recognized and inactive: shortcuts, flags, providers,
message/Markdown/entry renderers, and tool `prepareArguments`. Extension-level send/append/session
mutation helpers are also inactive. Their query-shaped counterparts return safe empty/default
values, and `exec` returns a non-zero inactive result instead of throwing. Registered tools receive
no streaming update callback; their final result remains supported. The generation-local
`pi.events` bus is active, isolates listener failures, and drops all listeners on retirement.

## Module compatibility

The host covers every module specifier in Pi's extension-loader alias table for both
`@earendil-works` and `@mariozechner`: `pi-coding-agent`, `pi-agent-core`, `pi-ai`,
`pi-ai/compat`, `pi-ai/oauth`, and `pi-ai/providers/all` resolve to the one host compatibility
module. `pi-tui` is the deliberate inactive exception described above. The host also maps `typebox`
and `@sinclair/typebox`, including supported subpaths such as `/compile` and `/value`, to the single
TypeBox runtime bundled by the host. Subpath aliases take precedence over package-root aliases so
resolution cannot produce paths such as `build/index.mjs/value`.

Module resolution coverage does not imply that every runtime export from the upstream JavaScript
Agent, provider, or TUI implementations exists in pi-rs. The compatibility module exposes the
runtime-neutral helpers documented by this matrix; type-only imports disappear during transpilation,
and unsupported runtime capabilities remain explicit product gaps.
