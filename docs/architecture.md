# Pi Rust core architecture

## Scope

The current product is a plugin-first Rust coding agent with one `MultiSessionManager` / `PiSession`
Interface behind interactive TUI, print, Pi-compatible NDJSON, and stdin/stdout RPC modes. Both the
standalone binary and the Node extension host delegate interactive terminal ownership to the
Ratatui frontend in `apps/pi-cli`.
Its core implements the deterministic:

```text
prompt -> provider stream -> assistant message -> tool calls -> tool results -> next turn
```

HTTP providers, production tools, model/resource discovery, resumable sessions, compaction,
project trust, native dynamic-library loading, and terminal presentation are outer modules around
that core. A Node/NAPI host adds Pi-compatible JavaScript and TypeScript extensions while Rust owns
session/runtime and terminal presentation. Interrupted operations are reducer-reconciled without
implicitly replaying external side effects. Signed/OCI native-plugin distribution remains an open
product seam.

## Workspace

```text
crates/pi-core                  contracts, plugin-facing product capabilities, registries, plugin drivers,
                                ModelRuntime
crates/pi-agent                 Agent façade, AgentLoop, StreamAssembler, ToolScheduler
crates/pi-runtime               plugin registration and Agent construction
crates/pi-provider              vendor-neutral HTTP transport and SSE framing
crates/pi-media                 multimodal byte processing; image detection, conversion and inline limits
crates/pi-prompt                pure Pi-style system prompt assembly
crates/pi-resources             generic system/append prompts and project context discovery
crates/pi-session               Pi v4 storage/runtime plus plugin contracts under plugin/ and types/
crates/pi-settings              current-format settings documents, snapshots, and safe writes
crates/pi-sdk                   headless product composition shared by CLI, desktop, and embedded adapters
crates/pi-telemetry             typed Pi AI/harness span schemas and sink adapters
crates/pi-rpc                   Pi JSON projector and stdin/stdout RPC adapter
crates/pi-mcp                   protocol-neutral MCP client, tool projection, and process ownership
crates/pi-acp                   official stable-v1 ACP adapter and ACP session policy
apps/pi-desktop                experimental Tauri/React shell with a Pi-to-thread event Adapter
apps/pi-cli/src/markdown       TUI-owned Markdown parsing, streaming repair, highlighting, and Ratatui rendering
crates/pi-plugin-sdk            native plugin author interface and descriptor types
crates/pi-plugin-macros         static plugin preparation, agent hook-interest derivation, and native exports
crates/pi-plugin-loader         manifest discovery, compatibility checks, and factory adapters
crates/pi-memory-loader         memory.json loading and provider construction Interface
crates/pi-plugin-manager        package intent/lock, Registry resolution, CAS, and activation
crates/pi-plugin-tools          native author scaffolding, Cargo builds, release verification and publication
crates/pi-js-package-manager     Pi-compatible JS discovery and npm/git orchestration
crates/pi-js-plugin             JS wire DTOs plus three Rust lifecycle adapters
bindings/pi-napi                NAPI-RS boundary between Node callbacks and the Rust product
packages/pi                     Node launcher, jiti extension loader, and callback host
crates/pi-test-support          deterministic scripted providers and tools for tests
plugins/providers/pi-plugin-openai        OpenAI provider plus reusable Responses wire support
plugins/providers/pi-plugin-anthropic     Anthropic Messages, Claude Code mode, provider, and catalog
plugins/providers/pi-plugin-xai           xAI Responses provider and Grok catalog
plugins/providers/pi-plugin-google        Google Generative AI provider and Gemini catalog
plugins/providers/pi-plugin-models        models.json catalog, routing, and request-time config
plugins/features/pi-plugin-{prompts,skills}
                                generation-local prompt-template and skill discovery/commands
plugins/features/pi-plugin-session-transfer
                                first-party export/import/share commands
plugins/features/pi-plugin-subagents
                                profiled delegation and bounded recursive child-session policy
plugins/features/pi-plugin-schedule
                                persistent schedules, claim locks, isolated runs, history, lifecycle cleanup
plugins/features/pi-plugin-memory-hermes
                                default curated USER.md/MEMORY.md provider with frozen prompt snapshots
crates/pi-tool-support           shared path validation, argument, and truncation helpers
plugins/tools/pi-plugin-{read,write,edit,hashline-edit,bash,grep,find,ls}
                                one production tool per plugin crate
e2e/                            in-process runtime acceptance and frontend example resources
crates/pi-sdk/src/project_trust.rs product trust policy and persistence
apps/pi-cli/src/tui.rs           terminal project-trust prompt Adapter
```

Dependencies point inward:

```text
pi-agent             -> pi-core
pi-provider          -> pi-core
pi-media             -> pi-core + image codecs
pi-prompt            -> standard library only
pi-resources         -> pi-prompt
pi-session           -> pi-core + pi-prompt + pi-resources + pi-runtime
pi-settings          -> serde JSON + filesystem persistence only
pi-sdk               -> pi-session + pi-settings + pi-runtime + product providers, tools, resources,
                        memory, skills, subagents, and plugin loaders
pi-rpc               -> pi-agent + pi-core + pi-session
pi-mcp               -> pi-core + rmcp
pi-acp               -> pi-agent + pi-core + pi-mcp + pi-session + official ACP SDK
pi-plugin-openai     -> pi-core + pi-provider
pi-plugin-anthropic  -> pi-core + pi-provider
pi-plugin-xai        -> pi-core + pi-provider + pi-plugin-openai::responses
pi-plugin-google     -> pi-core + pi-provider
pi-tool-support      -> pi-core
production tools     -> pi-core + pi-tool-support
pi-plugin-read       -> pi-media (shared image normalization)
plugins/features/pi-plugin-skills
                     -> pi-core (skill discovery, prompt contribution, explicit invocation)
pi-memory-loader     -> pi-core + pi-session
                        (provider selection, opaque configuration, and construction)
plugins/features/pi-plugin-memory-hermes
                     -> pi-memory-loader + filesystem locking
                        (default bounded, file-backed curated memory provider)
plugins/providers/pi-plugin-models
                     -> pi-core + pi-plugin-openai (credential-blind catalog and routing)
other plugins/*      -> pi-core
pi-runtime           -> pi-core + pi-agent + pi-prompt
apps/pi-cli          -> pi-sdk + pi-rpc + pi-acp + terminal and Markdown adapters
                        + pi-media (attachments) + pi-tool-support (Pi read-path semantics)
apps/pi-desktop      -> pi-sdk + pi-session + Tauri
pi-plugin-manager    -> HTTP + filesystem package source adapters
pi-plugin-tools      -> pi-plugin-manager release format + pi-plugin-loader + Cargo/GitHub CLI adapters
pi-js-package-manager -> filesystem + npm/git process adapters (no Node dependency)
pi-js-plugin         -> pi-core + pi-session (no Node or terminal dependency)
bindings/pi-napi     -> pi-js-plugin + apps/pi-cli + NAPI-RS
packages/pi          -> Node + jiti + platform pi-napi artifact
```

`apps/pi-cli/src/markdown` is a private frontend module rather than a reusable crate. It owns the
Ratatui-specific Markdown adapter used by `pi-cli`; no crate under `crates/` or plugin may depend on
it. Keeping parsing, streaming repair, highlighting, and rendering behind its small `render`
interface gives the supported TUI locality without moving terminal presentation into the core
layers.

The standalone Rust TUI deliberately adopts the Codex CLI 0.149 interaction shell while retaining
Pi runtime and command semantics. Its composer is persistent application state; command and skill
completion renders directly below it, queued input previews render above it, and focused model or
resume selectors temporarily replace the composer through a bottom-pane view stack. Closing a view
restores the unchanged draft. The frontend follows a tui-realm-style model/message/module split:
`tui.rs` owns the application model and event loop, semantic runtime notifications pass through
`tui/message.rs` and `App::update`, `tui/controller.rs` owns input routing and effects, `tui/view.rs`
owns layout/rendering, and `tui/components` owns stateful input/selector adapters. This is an internal
terminal-presentation seam, not a new core/runtime message protocol. This is a presentation and input-
routing choice, not a claim that Pi implements Codex-only accounts, usage limits, permissions, or
remote services. Terminal styling, view routing, and selector state remain owned by `apps/pi-cli`.

Fullscreen text selection is a TUI-owned deep Module. `ScreenTextSurface::capture(buffer)` accepts
only the complete final Ratatui buffer, hiding cell-width normalization, wide-character continuation
cells, coordinate clamping, text extraction, and highlight painting behind that Interface. The view
paints selection after every other widget, including focused bottom views and the project-trust
screen, so no caller can accidentally restrict copying to the transcript. The controller routes
fullscreen mouse gestures through this Module and emits a clipboard write when a non-empty drag is
released. This avoids depending on `Cmd+C` / `Ctrl+Shift+C`, which terminal emulators commonly
consume instead of forwarding to fullscreen applications; forwarded shortcuts remain available for
re-copy and stay distinct from `Ctrl+C`. Animation ticks pause while a selection is active.
Main-screen mode never enables mouse capture and leaves visible-text selection to the terminal.
Semantic `/copy` remains independent and copies the last completed assistant message.

`pi-media` owns provider-neutral multimodal byte processing. Its first Module, `image`, owns
magic-byte detection, PNG conversion, dimension/encoded-size limits, and processing hints behind
`process_image(bytes, policy)`. `pi-core::ImageContent` remains the message contract. The read tool
and CLI both use that Interface on blocking workers; filesystem paths, settings, clipboard I/O,
and terminal state remain with their owning Adapters. Video/audio Modules and message contracts
are future work. The existing Rust PNG/Lanczos resize algorithm is retained, so processing limits
and hints follow Pi intent without claiming byte-for-byte encoder equivalence.

CLI startup prepares text and image attachments into `SessionInput`, preserved through TUI,
print and NDJSON submission. As in current Pi's `cli/file-processor.ts` and `cli/initial-message.ts`,
stdin precedes file markup and the prompt; image detection uses bytes rather than extensions,
empty files are skipped, and processing failures become inline omission notices. File paths use
Pi's shared read-path resolution. Startup resolves project trust before reading effective
`images.autoResize`; `images.blockImages` remains request-time runtime policy.

The clipboard Module retains its small `ClipboardWriter::set_text` Interface. Its system Adapter
uses the native clipboard first for local sessions, PowerShell as the WSL fallback, and tmux then
bounded OSC 52 for terminal-mediated copying. SSH deliberately skips the remote native clipboard so
copied text reaches the user's local terminal.
Its separate asynchronous read Interface supports local native image/text paste through `Ctrl+V`.
Matching current Pi's interactive mode, RGBA pixels become a temporary PNG and its path is inserted
at the composer cursor; later tool reads process that image through `pi-media`. These files remain
available after exit for saved path references. Pending reads never insert into a replaced session,
changed draft or focused selector. SSH has no native-read Adapter; terminal text/path paste and
startup `@file` remain available. Native clipboard availability is platform/session dependent.

`packages/pi` does not own a terminal frontend. Its executable creates the JavaScript extension host
and invokes the NAPI `runPi` entry; interactive, print, JSON, RPC, piped-input, and plugin-management
arguments are forwarded unchanged. This keeps extension callbacks in Node without allowing Node and
Rust to compete for raw mode, stdout, editor state, or transcript projection.

`scripts/pi-dev` is the source-checkout Adapter for that same Interface. It incrementally builds
`pi-napi` for Rust's current host target and passes the resulting absolute library path through
`PI_RS_NATIVE_BINDING` before starting the TypeScript host. It never relies on copied package
artifacts, so an older `packages/pi/pi-napi.*.node` cannot shadow the current Rust generation. The
standalone Rust Adapter remains native-only. Before constructing a session, it asks the
`pi-js-package-manager` Module whether the trusted/discovered configuration can require JavaScript;
an active requirement fails with an actionable launcher message unless the user explicitly disabled
discovery. This probe is read-only and never installs or updates a package.

All frontend adapters enter the product through `pi-sdk`, whose `Pi` Module owns the shared
product composition and exposes the session manager. Beneath that interface, two public session
Modules remain responsible for session behavior. `MultiSessionManager`
owns the runtime factory, manager shutdown, and a private table of active handles. `PiSession` is the
cloneable per-frontend handle for current-session events and new/resume/fork/reload transitions.
There is deliberately no public `SessionRegistry`: duplicate-path checks and handle bookkeeping are
implementation details of `MultiSessionManager`. `AgentSessionRuntime` remains the lower-level replacement
transaction used inside each `PiSession`, rather than a type frontend adapters coordinate directly.
The print and NDJSON Adapters pin `PiSession::current()` for one invocation; the longer-lived TUI and
RPC adapters watch the handle's replacement stream. This keeps generation changes behind the same
Interface while preventing a single in-flight submission from crossing generations.

`apps/pi-desktop` is an experimental desktop Adapter that provides workspace, thread, Git, file,
terminal, and conversation presentation on top of the in-process `pi-sdk` / `MultiSessionManager`
/ `PiSession` Interface. CLI and desktop do not assemble independent provider, tool, skill, or
prompt runtimes; those product defaults belong to `pi-sdk`.
`PiRuntimeState` owns native sessions, `SessionStore` owns create/open/fork/archive/unarchive/delete
discovery, and one projection Module translates Pi lifecycle events into the UI's `thread/*`,
`turn/*`, and `item/*` vocabulary. The Adapter emits that vocabulary through its own `pi-event`
channel; there is no external app-server protocol or child-process boundary. Registered
workspaces are immediately usable by path; the Adapter has no per-workspace connection lifecycle.
Direct subagent calls and Rust's native `subagent_workflow` extension project through the same
multi-receiver collaboration item vocabulary. Workflow progress remains one parent tool item, while
each node that acquires an `isolatedSessionId` starts one observed child thread keyed by its stable
`runId`; the workflow node key is presentation metadata and the agent profile remains its role.
Nested node status and usage update that item without replaying the workflow's deliberately silent
per-child tool updates. Completed workflow receipts retain child links through their persisted
`sessionId` values. This is Desktop projection policy for the Rust workflow extension, not a new Pi
session or provider wire format.
Desktop session creation delegates path construction to `JsonlSessionRepo`, so its project-scoped
`<timestamp>_<session-id>.jsonl` filename, v4 header ID, and projected frontend thread ID share one
identity just as they do in Pi.
Archive and unarchive preserve that project-relative path and companion session tree. An unarchive
event includes a freshly projected summary from the restored JSONL so the desktop rebuilds the
title, model, message count, and timestamps instead of publishing an empty thread placeholder.
The model selector reads the generation-local catalogue from `pi-sdk` and sends the exact
provider/model identity back to `AgentSession`; thinking selection is constrained by that model's
declared levels and persisted through the session configuration entries. The Adapter exposes no
synthetic read-only, on-request, or full-access mode because filesystem and shell tools follow Pi's
operating-system permission boundary.
AI commit-message generation runs through a disposable Pi session and removes that session after
collecting the response. The desktop has no account-login, hosted-usage, approval, review,
remote-daemon, or external agent-configuration compatibility layer. Its only application-level
infrastructure integrations are the app-owned updater/release-notes flow and Sentry.
Desktop thread snapshots include `command_specs()` from the selected `PiSession::current()`;
command/skill discovery never constructs a throwaway session. On entering a workspace without a
selected thread, Desktop prepares one managed in-memory draft via `pi_start_thread(prepareOnly)`.
`SessionStore` serializes preparation per workspace and reuses that handle for command, skill,
and model discovery. Preparation returns a snapshot without publishing `thread/started` or
selecting a visible thread. The first normal `pi_start_thread` claims the prepared handle, so
completion and execution use the same generation; background prompt sessions remain separate.
Both the workspace home input and the conversation composer receive its registered commands
and skills. The existing first-assistant-response persistence rule keeps an unused draft out of
resume history. Closing/reopening the app rebuilds drafts through the normal runtime factory;
no new runtime lifecycle or command registry is introduced. The Desktop Adapter retains case-insensitive exact-name priority for its six UI actions
(`/compact`, `/fast`, `/fork`, `/new`, `/resume`, `/status`) and reserves `/prompts:*` for its
custom-prompt expansion. Completion and dispatch share that priority, hiding conflicting runtime
names while the composer displays a visible collision warning. These UI actions are intentional Desktop divergences (notably `/resume` refreshes the
thread and `/fast` remains a UI-only setting rather than a native provider service tier).
All other runtime commands pass through `AgentSession.submit` once, including transformation and
input hooks; Tauri returns the real handled/queued/completed receipt rather than fabricating an
in-progress turn. Product events continue streaming while the IPC call awaits completion. Handled
receipts do not require AgentStart/Settled and cannot stall local queue draining. Plugin notices
are transient severity-bearing Desktop transcript entries, not persisted/provider messages.
Long-lived Desktop forwarders use the existing `PiSession::subscribe()` latest-value watch.
After a replacement they subscribe to the latest `AgentSession` and publish `thread/replaced`
with that subscription's snapshot and the same session's command catalog. The frontend refreshes
history, status, commands, and selected identity together, including reloads that preserve the
JSONL ID. Initial create/resume snapshots also carry commands; no separate catalog IPC or
catalog event listener is needed. This is a deliberate Rust Desktop observation policy, not a Pi
wire/schema change: intermediate generations may be coalesced, and transient plugin notices sent
before the new event subscription is installed are not replayed. Displayed notices survive a
same-ID reload. Failed preparation publishes no change and leaves the old subscription intact.
There is no generation FIFO, activation observer bookkeeping, or detached replacement commit
introduced for Desktop. Dropping the managed handle closes its watch and stops the forwarder;
no liveness polling is needed. Snapshot revisions delimit subsequent events so history captured
at subscription time is not displayed twice. When subscription starts during an active response,
the forwarder also refreshes the completed snapshot on settlement so a missed message-start event
cannot hide that response's final content.
Desktop submission receipts only reconcile optimistic status while their synchronous status
revision is still owned; later lifecycle events or submissions win even before React renders.
Submitted attachments are consumed before awaiting IPC, so a completed turn cannot clear the
next draft's images. Store/forwarder keys
identify managed handles separately from replaceable JSONL IDs: an old saved ID can be reopened,
but is never silently routed to a different current session. Failed replacement publishes no swap
and keeps the old subscription/catalog. Only native registered commands are supported; Desktop
keeps JS extension discovery/hosting disabled and does not emulate JS widgets, argument completion
callbacks, interactive dialogs, or CLI-only frontend commands.
Desktop image attachments are decoded from data URLs by the Adapter and enter the same
`SessionInput` Interface as text; submit, steer, and follow-up processing preserve those images
through input hooks, durable queue records, and provider projection.

`pi-rpc` is one external protocol-adapter Module above this session Interface. Its Pi JSON
projector is the single erasure seam for both `--json` and Pi RPC: it emits the coding-agent v3
header, delta-only assistant updates, exact tool metadata/results, committed v4 entry identity, and
optional-field omission without exposing Rust revision envelopes or `Debug` strings. The RPC
adapter owns strict LF-delimited input, command correlation, synchronized stdout, session
replacement subscription, queue/model/thinking/compaction/retry/shell/session commands, and a
narrow injected HTML-export callback.

`pi-acp` is a sibling external adapter rather than an RPC submodule. It uses the official ACP SDK's
stable-v1 schema and maps ACP connection/session capability negotiation onto
`MultiSessionManager`/`PiSession`. Its multi-session ownership, asynchronous prompt responders,
cancel notifications, transcript replay, and model/thinking configuration stay independent of Pi
RPC commands and wire types. `pi-mcp` is deeper and protocol-neutral: it owns stdio MCP process
lifetime, discovery, qualified tool names, invocation, result projection, and cancellation. ACP
converts per-session `mcpServers` into `pi-mcp` configuration and injects the resulting plugin by a
`SessionGenerationOverlay`. The overlay also carries typed `SessionExecutionOrigin` provenance
(`User` or `Subagent`). Both are carried across live new/resume/fork/reload generation replacement
but are never serialized; reopening a session requires the caller to provide its transient
configuration again. This preserves Pi v4 storage and keeps ACP, MCP, and review policy out of
`pi-core` and `pi-session`.

Pi's experimental framed-CBOR server/client protocol is intentionally outside the pi-rs product
surface. Process integrations use the supported Pi stdin/stdout RPC or ACP adapters rather than a
second proprietary multi-session transport with no CLI entry point.

The TUI startup card reads immutable `SessionRuntimeInventory` metadata from the prepared
`AgentSession`. `pi-js-package-manager::Resolution` retains the source identity already attached to
each resolved entry: effective package entries use their original `settings.json` source, while
explicit files and automatic extensions retain their resolved path identity. `ProductSessionFactory`
turns those identities into stable labels only after the Node generation prepares successfully, so
the TUI never guesses package names from install-layout segments such as `dist/index.ts`. The
Rust-plugin list is intentionally narrower: it is the ordered intersection of `plugins.json`
reconciliation results and successfully loaded native descriptors. Built-in Rust plugins and
explicit `--plugin` paths remain active but are not presented as configured Rust plugins. Replacing
a session or running `/reload` rebuilds this inventory with the same transaction as the runtime and
session plugin generations, so the frontend never reconstructs registration state from
configuration files or plugin ID conventions.

## Plugin-first rules

1. Tools and commands are registered through `AgentPlugin`. `ProviderPlugin` contributes provider implementations, routing overlays, model catalog entries, and provider request hooks. Agent code has no provider/tool name switches.
2. Agent plugin hooks, provider plugin registration, and provider request hooks each execute in builder order. There is no numeric priority. Statically linked Rust plugin impls use one lifecycle attribute—`#[pi_core::agent_plugin]`, `#[pi_core::provider_plugin]`, or `#[pi_session::session_plugin]`—which expands async callbacks without a companion `#[async_trait]`. The agent attribute also derives hook interests. Native plugins get the corresponding behavior from `#[pi_plugin_sdk::{agent,provider,session}]`, while JavaScript agent adapters derive interests from the validated `pi.on(...)` manifest. `PluginDriver` snapshots an immutable per-hook route when a generation is built. Registration still visits every agent plugin; runtime hooks visit only their exact route, with no catch-all interest or `ALL` fallback.
3. Duplicate IDs are rejected within each plugin system; duplicate tool, command, provider, or model IDs fail runtime construction.
4. Registries are mutable only during registration and frozen before Agent construction.
5. `tool_call` runs in order after prepared provider arguments are validated, chains argument patches without revalidation (matching Pi), and lets the first block decision win. It is the intentional fail-closed exception: a hook error fails that tool call. Every typed `AgentPlugin` callback receives the same `Arc<AgentContext>` snapshot for the batch, including the current system prompt, transcript with the requesting assistant message, and active-tool names. The JavaScript extension Adapter projects Pi's narrower extension event and does not serialize this native context into Node.
6. `input` receives text, images, source, and optional streaming behavior. Text/image replacements chain in registration order, `Handled` stops the submission, and a hook error is recorded as a generation-local plugin diagnostic before later hooks continue.
7. `before_agent_start` runs once per prompt/continue invocation in registration order; prompt replacements chain and injected messages are accumulated for that run only. Hook errors are diagnosed and skipped without discarding earlier replacements.
8. `context` runs before every provider request and chains message replacements without mutating the persisted transcript. Hook errors are diagnosed and later hooks still run.
9. `tool_result` receives that same batch context and chains content, details, usage, `added_tool_names`, and error patches. Hook errors are diagnosed and skipped; they do not rewrite a successfully executed tool result into a failure. `added_tool_names` then survives the tool-result message and session/provider projection. Legacy before/after tool hooks remain compatible.
10. Lifecycle events are delivered through independent plugin methods (`agent_start/end/settled`, `turn_start/end`, `message_start/update/end`, and `tool_execution_start/update/end`) in registration order. `agent_start`/`agent_end` belong to each low-level run, while session orchestration emits `agent_settled` once no automatic retry, compaction, or queued continuation remains and before publishing the product settled event. Turns use a zero-based per-run index and `turn_start` also carries its millisecond timestamp. `message_end` may replace a message while preserving its role; each valid replacement becomes the next hook's input and the final message is used by Agent state, listeners, provider context, tool scheduling, and persistence. A validated plugin context may append extension state while the owning prompt operation still holds its outer gate, including during `agent_settled`; direct idle-time callers retain the ordinary busy exclusion. Observer errors and invalid cross-role replacements are diagnosed and skipped without failing the run.
11. A native plugin is trusted in-process code; the loader and trait interfaces are not a sandbox.
12. Registered slash commands own both their `CommandSpec` and execution. A `TransformInput` result then passes through `input` hooks in registration order before the agent run; `Handled` stops the submission. Text preprocessing retains both the product-facing submitted text and the effective model text rather than requiring a frontend to reverse an expansion.
13. `before_provider_request` runs after a concrete provider has serialized its final wire payload and before transport. Replacements chain in provider-plugin order; hook errors are diagnosed and skipped so later provider hooks still receive the last valid payload.
14. Built-in HTTP providers cross the shared `post_json_with_provider_hooks` Interface after assembling their final headers. `before_provider_headers` chains a header map in provider-plugin order, preserves `null` deletion tombstones until all hooks finish, and only then produces transport-ready strings. `after_provider_response` observes status and decoded response headers before the body stream can be consumed. Both hooks isolate failures as diagnostics and continue in registration order.

## Runtime generations and reload

`PiRuntime` keeps a reusable blueprint and publishes immutable runtime generations. A generation contains the agent and provider plugin drivers, the frozen `ModelRuntime`/registries, and the assembled base prompt that must move together. Agent plugins may contribute tools, commands, input processing, and lifecycle hooks; provider plugins have a narrow surface for provider/catalog registration and provider request lifecycle hooks.

`reload()` prepares the complete next generation off to the side, validates it against the current provider and active-tool selection, waits for the active run to settle, and then swaps one `Arc<AgentRuntime>`. A failed factory, duplicate registration, or incompatible provider/tool selection leaves the prior generation untouched. Each agent run captures one generation before invoking hooks or resolving providers and tools, so a run cannot observe a mixture of old and new plugin state. Hook-interest routes are rebuilt with the candidate generation and never mutated after publication.

Use `agent_plugin_factory` / `try_agent_plugin_factory` for reloadable agent plugins and `provider_plugin_factory` / `try_provider_plugin_factory` for providers, catalogs, routing overlays, and request hooks. Their pinned `agent_plugin` / `provider_plugin` and `*_arc` forms intentionally reuse an instance, primarily for stateless plugins and externally observed fixtures. `pi-plugin-loader` adapts version-locked dynamic libraries through the existing type-erased fallible factory seams and never mutates live registries in place.

Product wiring installs agent, provider, and session plugins through their three independent factory
seams. Each `PiSession` uses `AgentSessionRuntime` for cross-system atomicity: its factory prepares
the complete runtime and session plugin generations before shutting down or replacing the current
session. The prepared session carries registration inventory as generation-local metadata; it is
not persisted into Pi v4 session data and does not introduce another plugin lifecycle.
`pi-plugin-loader` discovers global manifests and trusted project manifests, resolves explicit
`--plugin` paths, verifies a C-layout descriptor before resolving an exact-build Rust constructor,
and partitions packages into separately ordered agent, provider, and session factories. It snapshots
each dynamic library by content hash before loading, so rebuilt artifacts receive a distinct path
while unchanged content reuses one process-pinned handle. Libraries remain pinned for the process
lifetime because plugin code may retain worker threads. Package metadata and artifact lifetime are
loader concerns rather than a fourth lifecycle or cross-lifecycle bundle.

Native ABI 16 adds aggregate usage/provider-call results to ephemeral Agents and
`SessionContext::record_usage` for detached parent-session accounting. Native ABI 15 replaces the
core tool-run state container with `ToolContext::run_id()` and explicit invocation-private
Agent-plugin attachments. ABI 14 added optional detached compaction to
`SessionContext::run_ephemeral`, typed execution origin inspection, and typed tool state (now
removed from core). ABI 13 added inherited effective
prompt/history, optional bounded history replay, per-invocation tool observations, and aggregate
input budgets to the ABI 12 ephemeral entry. `pi-core` owns the request/outcome contract, `pi-runtime` executes the
normal Agent loop, and `PiPluginContext` forwards without the multi-session manager or parent
operation/reload gate. The call pins a generation and reuses providers and request-time auth.
Model/thinking default to the parent. All parent-active schemas remain advertised; only the
requested subset may prepare arguments or execute. Tools receive the same run identity as plugin
hooks, but no parent-session/model-control/UI capabilities. Core stores no plugin state or tool
observations. `EphemeralSessionRequest.plugins` explicitly attaches private `AgentPlugin`
instances without a separate hook allowlist. The ordinary Agent driver awaits interested plugins
in registration order, including prompt/context, Agent/turn/message, and tool hooks. Prompt and
context patches affect only the private Agent. Tool blocking, initial argument validation, and result
patching remain intact. Duplicate IDs fail before the provider runs. Private plugin registration
is not invoked, so published generation registries remain immutable. This is reuse of the existing
AgentPlugin driver, not a fourth plugin lifecycle. Parent agent/session hooks do not run. The bare
Agent entry does not run the product `input` pipeline or emit `agent_settled` or `SessionPlugin`
lifecycle events. This is a capability boundary, not a sandbox for trusted native code.

The fork has no SessionLog, managed handle, frontend forwarding, or external worker. Completion,
timeout, cancellation, and dropping the future release its private plugin instances and abort its
provider/tool signal. Stateful attachments are fresh per invocation, not shared through request clones.
Cleanup uses plugin-owned RAII, since cancellation or dropping the future can bypass `agent_end`.
Completed tool side effects are not rolled back. It remains usable inside awaited compaction and
shutdown hooks before generation retirement. Ordinary managed subagents still use launch/wait/abort.
Optional compaction runs only between completed tool iterations, after the first full replay. It
reuses the pinned provider path for tool-free summaries, applies caller-supplied retention/token
limits, and replaces only the fork's private context. Summary input/cache usage counts toward the
same aggregate budget; timeout, cancellation, and invalid summaries cannot commit to the parent.
The ephemeral turn controller settles compaction before its stop decision so a failed summary or
exhausted summary budget prevents another request; it applies a successful context replacement
only when `prepare_next_turn` is called for a continuation.

Native ABI 11 added `SessionContext::complete`: a tool-free, transcript-free provider completion
for bounded, tool-free plugin side work. Direct completions pin the immutable
runtime generation and resolved model for their entire retry loop, rather than acquiring the
parent prompt's reload mutex. This lets lifecycle hooks await them without self-deadlock. An
in-flight standalone completion may finish against its pinned old generation after a runtime
reload; that generation's plugin context remains alive until its last owner is released.
Native ABI 10 adds inherited model,
thinking, and active-tool selection to fresh isolated sessions.
Native ABI 9 added fresh isolated-session launch, wait, and abort to the generation-bound
`SessionContext`. Native ABI 8 added asynchronous semantic confirmation to the `UiContext`, allowing
trusted Rust plugins to request a yes/no decision without owning terminal input or rendering.
Native ABI 7 replaced cumulative `message_update` ownership with one shared read-only
`AssistantStream` handle plus the current `StreamEvent` delta. A native hook can clone the handle
in constant time and calls `snapshot()` only when it actually needs the cumulative assistant
message. It also makes command-session reload return the fresh `ReplacedSessionContext` instead of
silently retiring the caller with no continuation handle. ABI 7 retains the generation-bound
`PluginContext` added in ABI 5 for agent, input, tool, command, provider, and session callbacks. The
public Rust interface exposes three explicit domain capabilities on each typed callback context:
`context.session`, `context.models`, and `context.ui`. It has neither a
pass-through `context.pi()` namespace nor a generic `context.runtime` bucket, and it does not use
implicit `Deref` to hide method ownership. Ordinary callbacks receive read-only model/catalogue and
session-inspection capabilities plus the non-replacing `abort`, background `compact`, product
`shutdown`, direct-completion, ephemeral tool-loop controls, and detached usage attribution. A
plugin calls `SessionContext::record_usage` to append a signed adjustment record with typed usage
and optional task details; it does not create a message or alter provider context. The capability
remains valid through an awaited shutdown hook and closes when that generation retires. A direct
completion carries an explicit system
prompt and message list, uses the active model and request-time credentials, has no tools, and does
not mutate the parent transcript or emit agent lifecycle events. Command callbacks additionally receive awaited message delivery and stronger
session/model capabilities for replacement, navigation, reload, and selection. Tool argument
preparation and execution receive the same
generation-bound `ToolContext`, so validation shims cannot escape the product capability lifetime.
The `plugin::capabilities` module in `pi-core` owns only dependency-inward domain interfaces,
typed capabilities, and `PluginContextEpoch`. `SessionContextAccess`, `ModelsContextAccess`, and
`UiContextAccess` keep the internal seam aligned with the public capability fields;
`PluginContext` is only their aggregate marker. `PluginContextHandle` enforces generation and
command scope but does not duplicate the domain method surface. `pi-session::PiPluginContext`
implements those interfaces against the actual `AgentSession`, `PiSession`, and generation-local
`PiRuntime`. It hides weak session links, command scope checks, queue policy, replacement
transactions, semantic UI notices, and the injected frontend confirmation bridge. Retained
contexts fail with `Retired` after their generation is replaced. `SessionContext` exposes typed
active-tool, tool-catalogue, and command-catalogue reads in addition to coherent snapshots.
Successful create, fork, switch, and reload operations resolve a fresh
`ReplacedSessionContext` from the newly active runtime generation rather than rebinding the old
capability. ABI 4 added
`ProviderPlugin` header/response lifecycle hooks, ABI 3 added the required `AgentPlugin`
hook-interest contract, and ABI 2 added the `AgentContext`/`added_tool_names` surface. The native
agent export macro derives its contract from the callback methods in the annotated impl, so authors
do not maintain a second hook list. The loader reads the stable C descriptor first and rejects older
ABIs before resolving any v16 Rust constructor symbol, preventing a stale in-process plugin from
crossing the changed trait boundary.

Callback metadata is read-only and exposed through accessors such as `plugin_id()`, `run_id()`,
`cwd()`, and `signal()`; only the typed `session`, `models`, and `ui` capability fields remain public.
Host-created callback contexts have no public `new()` constructor. Explicit `standalone()` tool and
command contexts advertise that product capabilities are unavailable. For coherent history reads,
`SessionContext::snapshot()` captures session identity, current branch, entries, leaf, labels, and
the raw extensible wire values at one revision. `SessionEntryView` types stable metadata while
preserving unknown fields through `raw()`, so native plugin ergonomics do not weaken v4 replay
compatibility.

Native package distribution is a separate deep module at `pi-plugin-manager`. Editable
`plugins.json` contains ordered intent; target-specific `plugins.lock` is the exact resolution and
durable installation record. Its intent digest lets startup and `/reload` avoid remote work when
the editable intent is unchanged, while local package roots are also checked for rebuilt artifacts
or changed manifest defaults. Local packages, direct HTTP/GitHub Release manifests, and a static
HTTP Registry resolve into immutable `plugins/store/sha256/<digest>` blob files. Automatic reconcile
reuses versions already selected by the lock; changing intent does not implicitly upgrade unrelated
packages. The manager verifies artifact SHA-256, selects the exact Rust target triple, preserves the
explicit `plugins.json` array order, and replaces a generated `plugins/installed` activation view.
Native plugin manifests do not declare runtime plugin dependencies: Rust crate dependencies remain
build-time concerns, and hook registration order remains consumer policy rather than a package graph.

`ProductSessionFactory` in `crates/pi-sdk/src/session_factory.rs` is the production Adapter at the
session-construction Seam. It prepares global package state and, after trust resolution, trusted
project package state before native discovery. The manager holds a package-state guard and retains
the previous lock and activation view until the complete runtime and session generation prepares
successfully. Failed native loading or plugin initialization therefore rolls package activation
back together with the generation; success commits the prepared package state. The same factory is
used for initial sessions and `/reload`. The loader consumes only the local activation view and does
not know about networks, semver, registries, install commands, or package transactions. This
distribution layer does not introduce a fourth plugin lifecycle or mutable runtime registries.

The current static Registry is signed-data-ready transport only: SHA-256 proves downloaded content
integrity but not publisher identity. Publisher signatures, Git repository and OCI adapters,
package update/rollback commands, and store garbage collection remain explicit package-manager
milestones.

## Native plugin authoring

Native plugin authoring is an outer Module at pi-plugin-tools, exposed through the CLI's
plugin new/package/verify/merge/publish/registry-entry commands. This is a deliberate Rust product
extension to Pi's npm/git author workflow. Cargo metadata and compiler messages locate the
cdylib, while the existing loader owns descriptor/ABI/fingerprint checks. Packaging generates
both the local manifest and the installer's shared version-one ReleaseManifest; it does not
create another runtime registry or installation record. Outputs never replace an existing
directory, and source files or credentials are not included in release assets.

Each platform builds and validates on a native runner. Merge checks immutable snapshots of all
input files and emits a deterministic multi-target release manifest, with no claim that foreign
binaries have been loaded. Normal verify additionally requires this host's artifact to pass the
loader; integrity-only verification explicitly omits that check. GitHub publication snapshots
the checked files before creating a draft and uploading only the listed manifest/artifacts;
existing releases are not overwritten and publication failures retain their draft.

Scaffolds pin a full SDK Git revision (or use an explicit local checkout), the host's Rust
toolchain and a seeded dependency lock. Independent crates.io publication remains open because
the SDK fingerprint currently consumes the complete workspace source/lock layout. The author
tools preserve the current ABI and the existing exact-build compatibility policy.

## End-to-end validation

The in-process Rust test in `e2e/tests/runtime_agent.rs` covers prompt assembly, plugin hooks,
production filesystem tools, agent loops, and session persistence through a deterministic scripted
provider. It is included in `cargo test --workspace`, but bypasses argument parsing and
`ProductSessionFactory` and is not black-box product coverage.

The dedicated black-box CLI/Node product scenarios and their local SSE/process harness have been
removed to reduce test infrastructure and fixture maintenance. No replacement product-E2E layer
is implied: complete process-to-provider tool-loop coverage is now an explicit gap. The frontend
example retains the project resources used by Rust acceptance and Node extension-host tests, but
not the unrelated copied skill collection; npm is its sole maintained frontend package manager.

Focused Node host and Node/NAPI bridge tests remain in `packages/pi/test`. CI runs the bridge
checks against a freshly built native binding, and release builds retain their native bridge smoke
checks. Bridge smoke cases launch a bounded child process with explicit stdin EOF and isolated
configuration rather than calling the native CLI inside a node:test worker whose input pipe stays
open. This is test-owned process isolation, not a change to the CLI's piped-input contract.
These narrower tests do not prove the removed end-to-end scenarios. Real-provider checks
remain a future opt-in layer, and fullscreen interaction requires a future PTY Adapter.

## Product packaging and release

Product distribution is an outer Release Module and does not add a runtime, session, frontend, or
transport layer. `packages/pi/scripts/release.ts` is its command-line Interface: it validates the
single product version, emits the CI matrix, builds one native target, assembles npm packages, and
publishes already-verified tarballs before checking their exact registry metadata and integrity.
`.github/workflows/release.yml` is a thin Adapter that supplies native runners and an npm OIDC
identity to that Interface; target naming, package layout, checksums, publication order, and smoke
tests do not live in workflow YAML.

Release Please is a version/changelog Adapter, not a second Release Module. Its release PR updates
the authoritative Cargo version, the npm manifest and lockfile, and `CHANGELOG.md` as one change.
Cargo's generated lockfile does not have a stable key that Release Please's generic TOML updater can
address, so the Release Module discovers every workspace member that inherits the workspace version
through Cargo metadata and synchronizes only those lock entries before each `--locked` native build;
third-party dependency resolution remains unchanged. CI performs the same normalization
before its locked test and lint gates. Merging the release PR creates a forced `v<version>` tag and
a draft GitHub Release, then `.github/workflows/release-please.yml` explicitly dispatches the native
release workflow. The dispatch derives the version tag from the merged manifest, verifies that the
tag targets the triggering commit, and skips an existing run. This both avoids duplicates and
recovers when Release Please creates a tag but fails before exposing its action outputs. The
explicit dispatch is required because a tag created with `GITHUB_TOKEN` does not recursively start
another workflow. The publish command polls the public registry with bounded backoff after each
package and verifies its exact metadata and integrity before continuing. Platform packages therefore
become verifiably public before the root package is published, and the draft GitHub Release becomes
public only after the root package passes the same check. The standalone verification command
remains available for manual audit and release recovery.

`packages/pi/src/native-target.ts` is the one target vocabulary shared by release tooling and the
Node loader. Supported artifacts currently cover macOS arm64/x64, Linux glibc arm64/x64, and
Windows MSVC arm64/x64. A target is publishable only when the matrix builds it on a matching host
and runs both the standalone binary smoke and the Node -> NAPI -> Rust smoke. Linux musl is a
separate future target rather than an alias for glibc.

One tag produces two delivery adapters from the same Rust product:

- GitHub Release archives contain the standalone `pi` binary. They support TUI, print, NDJSON, RPC,
  and native plugins, but no JavaScript VM or JS/TS extensions.
- The `@pi-rs/cli` npm root contains only JavaScript and declarations. Exact-version optional
  packages such as `@pi-rs/cli-darwin-arm64` and `@pi-rs/cli-linux-x64-gnu` each contain one NAPI
  artifact selected by OS, CPU, and libc. Platform packages publish first and the root package
  publishes last, so an incomplete native matrix is never advertised by a new root version.

`[workspace.package].version` is authoritative for the Rust product; every product crate that
declares `version.workspace = true` inherits it, and the Release Module rejects a mismatching npm
version or `v<version>` tag. Generated
npm staging is distinct from the private source package, preventing a development `npm publish`
from bypassing matrix validation. The protected workflow uses npm Trusted Publishing directly;
there is no long-lived npm token, and npm attaches provenance to the OIDC publication. Application
archives and NAPI artifacts receive SHA-256 files, the assembled sets receive `SHA256SUMS`, and npm
registry `dist.integrity` must equal the SHA-512 of each locally verified tarball.
Linux glibc artifacts use Ubuntu 24.04 runners for both x64 and arm64; this is the native dependency
and system-library compatibility baseline for those release targets.
Developer-ID/Authenticode signing and notarization remain release-hardening work rather than
runtime concerns.

## NAPI-hosted Pi extensions

The JavaScript extension path deliberately has one Rust product runtime, not a Node sidecar protocol
and not a fourth plugin lifecycle. Node is the executable launcher and JavaScript VM. It loads one
platform `.node` artifact and invokes `runPi` for Ratatui, print, JSON, RPC, piped-input, and management
modes. Extension callback generations remain in Node; provider, tool, trust, session, and terminal
authority remain in Rust. `crates/pi-js-plugin` contains only semantic wire values and adapters and
therefore has no NAPI, Node, Jiti, or terminal dependency.

`packages/pi` itself is authored in TypeScript, executed directly with `tsx` for development and
compiled by `tsc` into publishable JavaScript plus declarations under `packages/pi/dist`. Zod owns
runtime validation at the untyped Node seams: Rust host operations, generation manifests, dynamic
extension registrations/results, and native binding exports.
TypeScript protocol types are inferred from those schemas so runtime checks and static interfaces
cannot drift independently.

The callback boundary uses seven generation-scoped operations encoded as JSON: `prepareGeneration`,
`invoke`, `invokeHookBatch`, `invokeStreamHookBatch`, `releaseStream`, `cancel`, and
`retireGeneration`. Every invocation also receives a NAPI class instance named
`NativeExtensionContext`; it is a direct native capability rather than another process-global
callback broker. Its deliberately small JavaScript Interface has three methods: `query` for
synchronous reads, `notify` for non-blocking commands, and `request` for awaited commands. Those
operation tags live only in `pi-js-plugin` and the NAPI Adapter translates them into typed Rust
context calls. Native Rust plugins never pass through this JSON protocol. The capability object
itself is passed as the second threadsafe-function argument. Before `prepareGeneration`, `ProductSessionFactory`
calls the deep Rust Module `pi-js-package-manager` through its
`resolve(request) -> resolution` Interface. Its side-effect-free
`requires_javascript_host() -> bool` query is also used by the native-only startup Adapter after
construction from the same request.
The Module merges explicit `-e` local/npm/git sources first,
then trusted project settings entries, trusted project auto-discovery, user settings entries, user
auto-discovery, and configured package resources in current Pi precedence. A package may contribute
extensions, skills, and prompt templates from its current manifest/convention layout; the same
`autoload` and per-resource include/exclude filters are applied before each resource is handed to
its owning generation plugin. Package manifests and
filters, ignore files, canonical-path deduplication, managed npm/git installation, custom
`npmCommand`, and `PI_OFFLINE` are hidden behind that Interface. The Node Adapter receives only the
ordered `extensionPaths` load list and loads TS/JS with Jiti `moduleCache: false`; it has no settings,
source, installation, filtering, or precedence policy. JavaScript functions stay in a Node-owned
callback table; Rust stores only opaque generation and callback IDs. `invoke` crosses a weak NAPI
threadsafe function and awaits the JavaScript Promise without blocking either the Node event loop or Tokio.
Agent observer hooks cross the same Seam through `invokeHookBatch`: the JavaScript
generation Adapter routes all observer callbacks through its first agent adapter while the remaining
adapters retain their plugin identities and continue to own tools, commands, and chained/mutating
hooks. One batch carries the shared event once plus a small callback/context list. Node validates and
projects the event once, then awaits callbacks in registration order against the same event object;
a rejected callback becomes a callback-scoped diagnostic and does not suppress later observers.
High-frequency `message_update` uses the separate typed `invokeStreamHookBatch` wire. Rust sends an
initial message once and thereafter serializes only `streamId` plus the current `StreamEvent`; no
`serde_json::Value` message tree is built per delta. Node retains text, thinking, and tool-argument
chunks, applies metadata patches in place, and exposes lazy `message` plus
`assistantMessageEvent.partial`/`message`/`error` getters. Empty hooks never join the cumulative text;
the first snapshot read materializes once for the whole callback batch, and that detached snapshot
keeps plugin mutation out of canonical stream state. `releaseStream`, terminal `done`, and generation
retirement bound the accumulator lifetime.
Rust aborts send `cancel`, which aborts the callback's `AbortController`; retirement aborts all
remaining work and drops every callback for that generation. The native context is guarded by the
same generation epoch, so a context retained by extension code fails with a retired-context error
after its generation is gone. `JsPluginGeneration` does not construct or own another context epoch:
every JS invocation receives the handle already attached to its Rust callback.
`PluginContextBinding` connects each prepared generation to its concrete `AgentSession` before
`session_start`, then to the stable outer `PiSession` after initial
startup. Reads therefore remain generation-correct during activation and follow successful
new/resume/fork/reload replacements afterward. Both links are non-owning (`Weak<AgentSession>` and
`WeakPiSession`), because the concrete session owns the plugin generation; a strong context-to-
session edge would form a cycle and prevent generation retirement. The weak TSFN lets Node exit
once the exported `runPi` Promise settles.

Session-replacing requests are an explicit capability transition rather than an exception to
retirement. `newSession`, `fork`, `switchSession`, and `reload` capture the `PiPluginContext`, await
the replacement, and return a `PluginContextHandle` from the newly published runtime. The NAPI
callback object advances to that handle before JavaScript runs `setup` or `withSession`, and before
the `reload` Promise resolves; unrelated retained contexts from the retired generation still fail
with `Retired`.

The callback contract and generation epoch live in `pi-core`; the real product implementation lives
in `pi-session`. `PiPluginContext` is constructed by the app composition root for native-only
and JavaScript-enabled runs alike, and invokes `AgentSession`, `PiSession`, and `PiRuntime` directly.
Rust callbacks call its typed interface.
Only the NAPI Adapter projects JavaScript's `query`/`notify`/`request` wire operations onto that
same interface. Both paths therefore observe the same session selection, trust decision, model
state, queues, compaction, replacement ordering, and retirement rule without a parallel context
state machine.

That core contract reuses the owning semantic types instead of maintaining adapter copies:
`CustomMessageContent`/`CustomMessageInput` come from the message Module, `ToolExecutionMode` from
the tool Module, and `PresentationMode` plus `ForkPosition` are the canonical cross-layer contract
types consumed by JavaScript hosting and session storage. Adapter-only JavaScript operation DTOs
remain in `pi-js-plugin`; they are not exported by `pi-core` or used by native plugins.

Node builds Pi's lazy `ExtensionContext` and `ExtensionCommandContext` facades over that native
capability. Ordinary hooks and tools receive only base context operations. Registered commands also
receive `getSystemPromptOptions`, `waitForIdle`, and session replacement/navigation operations.
The native side returns typed v4 records; the Node compatibility facade removes journal-only
sequence fields, converts millisecond timestamps to Pi extension timestamps, and projects the
header into the current read-only `SessionManager` shape. Durable storage itself remains v4.
Command dispatch occurs before acquiring the old `AgentSession` operation gate; a command can
therefore await `newSession`, `fork`, `switchSession`, or `reload` without deadlocking on the
submission that invoked it. The native generation context falls back to `PiSession::current()` once
its prepared `AgentSession` closes, which lets Pi-compatible `withSession` callbacks observe the
replacement before the old JavaScript command returns. There is no `JsContextBroker`: lifetime and
authority are explicit in the callback argument, while orchestration remains in
`MultiSessionManager` / `PiSession`.

The top-level JavaScript `pi` object retains a small generation-local `ExtensionRuntime` Adapter.
Each callback rebinds that Adapter to the latest native context handle; the handle remains valid for
the generation but resolves the current `PiSession` after new/resume/fork/reload. Process execution
stays Node-owned because it is a process primitive, while messages, durable custom entries,
metadata, tool/model/thinking selection, and replacement setup flow through typed native
operations into `AgentSession`. Node never edits a session file or reconstructs queue policy.

Configuration-form `pi.registerProvider(name, config)` and `pi.unregisterProvider(name)` cross the
same native capability without mutating a published registry. Load-time registrations are validated
as part of the JavaScript manifest. Runtime calls stage ordered mutations in the generation-external
`DynamicProviderOverlay`; `ProductSessionFactory` combines load-time registrations and staged
mutations, prepares the complete replacement session/runtime generation, and commits the overlay
only after preparation succeeds. Failure drops the staged batch and preserves the current
generation. Calls made during an active run take effect at the next whole-run safe point, while a
command's immediately following `setModel` acts as a flush barrier. Extension provider state is
executable runtime state and is never serialized into Pi v4 JSONL.

Tool argument preparation is an asynchronous method on the core `Tool` Interface. This preserves
Pi ordering for JavaScript tools: prepare, validate, `tool_call` hooks, execute. Tool execution gets
an invocation-local update sink on the NAPI context so partial results cross the Adapter directly
into the existing semantic tool-update stream without adding a Node-owned event channel.

Managed npm installation deliberately uses npm's legacy peer-dependency mode, matching current Pi:
Pi extensions commonly declare the Pi SDK and TypeBox as peers, but those modules belong to the
running host rather than each managed package root. Before Jiti imports an extension, the Node host
mirrors Pi's complete extension-loader module table for both the `@earendil-works` and
`@mariozechner` namespaces. `pi-coding-agent`, `pi-agent-core`, `pi-ai`, `pi-ai/compat`,
`pi-ai/oauth`, and `pi-ai/providers/all` alias to one compatibility API; `typebox` and
`@sinclair/typebox` roots and subpaths alias to the bundled TypeBox runtime. This is module-resolution
coverage, not a claim that every upstream JavaScript runtime export is reimplemented. It keeps one
host ABI in a generation and prevents npm from installing a second Pi product runtime beside an
extension. `@earendil-works/pi-tui` and `@mariozechner/pi-tui` alias to a separate terminal-inert
compatibility module. Its pure text helpers and inert component classes allow mixed extension
modules to evaluate and retain non-UI registrations, while renderer/widget/shortcut registrations
remain inactive and terminal ownership stays in `apps/pi-cli`. All other missing modules remain
fatal.

The same Module exposes one management dispatcher,
`manage(Install | Remove | Update | List) -> ManageResult`. Top-level CLI commands adapt to this
Interface without duplicating npm/git parsing or settings policy. Install performs the physical
operation before persisting intent; remove performs physical cleanup before deleting matching
intent; update skips exact npm versions, batches mutable npm sources per scope, and reconciles git
refs or upstream branches; list reports user entries before trusted project entries. Project-scope
writes require the Rust-owned trust decision. Settings persistence merges the `packages` field into
the latest JSON object and preserves unrelated fields.

`pi-js-package-manager` is intentionally separate from the native `pi-plugin-manager`. JavaScript
packages use mutable npm projects and git checkouts, Pi settings filters, and Jiti entry points;
native packages use version-locked manifests, verified CAS blobs, and an activation view. JavaScript
discovery runs while a candidate session generation is prepared, so resolution or installation
failure preserves the published generation. As in current Pi, successful npm/git filesystem writes
are durable package-manager side effects and are not rolled back if later extension import or
runtime validation fails.

One JavaScript source may contribute to all three systems, but the manifest partitions it into
separate agent, provider, and session plugin adapters. No `PluginBundle` is reintroduced. Manifest
validation rejects unknown hooks, duplicate lifecycle IDs, duplicate callback IDs, invalid tool
schemas, and later ordinary registry collisions before a candidate is published. Tool prompt
metadata and execution mode become ordinary `ToolSpec` fields; hook replacement and cancellation
semantics continue through the existing typed drivers.

Supported JavaScript hooks use the same typed driver semantics as native plugins rather than a
second compatibility path. `input` transports and chains text plus images with Pi's source and
streaming-behavior fields; `turn_start`/`turn_end` expose the per-run turn index and start timestamp;
and `message_end` replacements flow back into the live Agent and persisted session, subject to the
same-role invariant. Callback rejection, malformed results, and invalid replacements become
generation-local diagnostics and do not suppress later callbacks. `tool_call` remains intentionally
fail-closed, matching Pi's tool runner. The validated manifest is also the JavaScript adapter's
hook-interest source, so an extension is never invoked for an event it did not register. Provider
payload callbacks use the same isolated chaining rule as the Rust `ProviderPluginDriver`.

`ProductSessionFactory` asks the Node host for a fresh callback generation on initial construction,
new/resumed sessions, and `/reload`, alongside native plugins, resources, models, and session
plugins. The existing whole-session transaction prepares all of them before swap. Except for the
recognized facilities that deliberately register as inactive, a failed import, factory, manifest,
or registry build drops the candidate (retiring its Node callbacks) and keeps the old Rust and
JavaScript generations active.
Executable JavaScript is never serialized into Pi v4 sessions.

This is compatibility by explicit capability, not an unsafe claim that every Pi TUI API is already
portable. The current bridge supports registered tools and commands, the Rust agent lifecycle
hooks, `before_provider_request`, the ten session hooks, session/model inspection, the explicit
non-replacing controls, and the command-safe session operations described above. UI is an intentional
product divergence: every
JavaScript context reports `hasUI = false` and exposes one explicit inert UI object with
Pi-compatible default return values. Its `notify` method crosses the native context as a transient
`AgentSessionEvent::PluginNotice`; each Rust frontend owns presentation, and nothing is persisted
to the v4 session log. UI registrations, renderers, and resource discovery,
and other recognized-but-inactive facilities do not fail generation construction; they produce an
`inactive` generation diagnostic and contribute no runtime callback. A hook name known to current
Pi but not implemented follows the same inactive policy, while an unknown hook name remains a hard
extension error so typos are not hidden. Unsupported result fields on an otherwise supported hook
are ignored rather than failing the callback. The maintained capability matrix is
[`docs/js-extension-compatibility.md`](js-extension-compatibility.md). JavaScript extensions are
trusted in-process Node code and share the process and OS authority of the product.

`ModelsPlugin` is a provider plugin loaded after the base protocol provider. It loads one immutable,
credential-blind `models.json` snapshot per generation and composes layers in Pi order: built-in
catalog, provider `baseUrl`/`compat`, custom-model upsert, JavaScript provider registration, then one
explicit model override. A JavaScript registration with `models` replaces that provider's complete
catalog; a registration without `models` preserves the lower catalog and may override routing,
credentials, and headers. The registry exposes a construction-only whole-catalog replacement
Interface for this case; frozen generations still have no removal or mutation surface. Its narrow
construction-time provider/model seams reject duplicate overrides, and
missing override targets fail candidate construction instead of mutating a published registry.
Full model overrides preserve partial cost rates, tier replacement, input modalities, context and
output limits, thinking maps, per-key sampling parameters, request headers, and Pi's four special
nested `compat` merges.

`ModelSpec` owns the ordered thinking-level capability and clamp policy shared by product startup,
sessions, TUI, desktop, extensions, and RPC. The order is `off`, `minimal`, `low`, `medium`, `high`,
`xhigh`, `max`; unsupported requests search upward first and then downward, matching current Pi.
New product sessions default to `medium` unless CLI/settings selects another level, while the
lower-level reusable `Agent` remains `off`. Model changes clamp before the next provider request and
persist a thinking entry only when the effective level changes.

Custom routes dispatch by their declared wire API. `openai-completions`, `openai-responses`,
`azure-openai-responses`, `mistral-conversations`, `anthropic-messages`,
`google-generative-ai`, `google-vertex`, and `bedrock-converse-stream` reuse the same protocol
modules as their built-in providers. The effective `ModelSpec` and session id travel with each
semantic `ProviderRequest`, so protocol modules apply merged compat, model limits, thinking
controls, prompt caching, routing, deferred tools, and session affinity at serialization time. The
provider overlay resolves credentials and headers only when sending the request and decorates
terminal usage with Pi-compatible per-million and tiered cost calculation.
Protocol serialization remains outside `ModelsPlugin`. `models_json_schema()` exposes the same
strict compat and catalog definitions for editors and tooling. A failed parse, validation, or
active-provider compatibility check prevents publication of the new generation, so `/reload`
retains the complete prior provider/catalog pair.

Radius is intentionally not projected through this static routing path. `oauth: "radius"` requires
one provider-owned OAuth, persisted remote-catalog, and `pi-messages` lifecycle; until that deep
module exists, generation construction rejects Radius configuration explicitly.

The independent `pi-plugin-anthropic` provider owns Anthropic Messages projection/SSE parsing, credential precedence, standard configurable routing, Claude Code request mode, browser PKCE authorization-code exchange/refresh, and its Claude catalog as one deep vendor Module. `ModelsPlugin` reuses its standard route for Anthropic-compatible custom providers without importing built-in Anthropic credential or OAuth policy. Claude Code mode preserves thinking signatures and applies OAuth identity headers, the required system identity, and bidirectional canonical tool-name mapping. `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, and Pi-compatible `<agent-dir>/auth.json` credentials are supported; explicit CLI credentials win, followed by environment credentials and stored credentials. The CLI owns secure credential persistence through `pi auth login/logout/status`: writes use a sibling lock, atomic replacement, and Unix mode `0600`, while status never emits secrets.

The built-in `openai-codex` plugin is installed in every product generation rather than only when Codex is initially selected. It owns the explicit Pi-compatible model catalog, Codex Device OAuth/refresh, and a Codex Responses Adapter. Reusable OpenAI Responses message/tool projection and SSE adaptation live directly in `pi-plugin-openai::responses`; the separate `pi-plugin-xai` provider depends on that plugin crate while retaining its own lifecycle, credentials, headers, payload policy, OAuth device flow, refresh, and Grok catalog. xAI exposes the current Grok 4.5/4.6 Responses models, resolves `XAI_API_KEY` or Pi-compatible stored credentials when rebuilding a generation, supports explicit xAI Device OAuth through `pi auth login xai --oauth`, and proactively refreshes stored xAI OAuth credentials before application startup. The built-in `pi-plugin-google` provider owns the Google Generative AI projection and current Gemini catalog, resolves `GEMINI_API_KEY` before Pi-compatible stored credentials, and exposes API-key login as `pi auth login google`; Google OAuth identities are not part of this provider. Anthropic and OpenAI Codex stored OAuth credentials use the same startup refresh transaction. `pi auth login` without a provider builds its selector from built-ins, validated JSONC `models.json` provider IDs, and existing stored credentials; unknown third-party providers receive API-key auth unless a future provider-owned OAuth capability declares otherwise. Device authorization validates xAI HTTPS verification URLs before invoking the platform browser Adapter; token polling and refresh remain provider-owned while locked atomic `auth.json` persistence remains CLI-owned. At generation construction the Codex plugin credential-blindly probes Codex CLI credentials from `~/.codex/auth.json` and `~/.config/codex/auth.json`; a valid access-token JWT with `chatgpt_account_id` makes the provider selectable. Requests use the ChatGPT Codex Responses endpoint and its required bearer, account, beta, originator, and user-agent headers. Its generation-local transport policy supports SSE, WebSocket, cached WebSocket continuation, and Pi's automatic preference: a WebSocket is reused by session/account, `previous_response_id` sends only a verified context suffix, and a transport failure before the first provider event activates SSE fallback for that session. Connect and per-frame idle waits remain abortable. An explicit HTTP proxy forces the proxied SSE path because the Rust WebSocket client has no proxy-tunnelling Adapter and must not bypass product proxy policy. This reuse does not write or refresh Codex CLI credentials and is not Pi's `/login` flow. The catalog supplies context windows, output limits, input modalities, reasoning support, and costs; it is not remote discovery. `ModelRuntime` keeps the complete registered catalog distinct from its credential-blind available view and exposes provider availability diagnostics. Providers report whether the current immutable generation has enough configuration to be selectable without resolving secret values. Initial selection and `/model` consume the available view, while restore and diagnostics can still inspect registered models. `AgentSession` derives compaction limits from the active generation's current `ModelSpec`, so model switches immediately change threshold and overflow decisions. An explicit session context-window option remains an embedding override.

The built-in Codex catalog includes Pi-derived thinking maps. Every current Codex model exposes
`xhigh`; GPT-5.6 maps semantic `minimal` to provider effort `low` and additionally exposes `max`.
GPT-6 Astra supports only `low` through `max`; unsupported `off` and `minimal` requests clamp to
`low` before provider dispatch.

Dedicated provider crates keep vendor behavior behind the same immutable generation boundary.
Mistral owns its native chat projection and stream parser; Azure OpenAI Responses owns deployment
and `api-version` endpoint construction; Google Vertex owns express API-key and
project/location-based application-default credential routing; Amazon Bedrock owns Converse Stream,
AWS EventStream decoding, bearer authentication, and SigV4 credential resolution. OpenRouter owns
its curated catalog and browser PKCE key exchange. GitHub Copilot owns device authorization,
short-lived Copilot token mint/refresh, account `/models` filtering, enterprise-domain routing, and
dispatch across Anthropic, OpenAI Completions, and OpenAI Responses projections. These product
plugins are registered in every generation even when unavailable, so diagnostics retain their
catalogs while `/model` exposes only providers with usable credential configuration. Their curated
catalogs are explicit generation data rather than a claim of complete remote Pi catalog parity.

Initial selection is a separate product policy in `pi-session`. `ModelRuntimeServices` adapts the
model portion of an assembled `PiRuntime` generation, while `InitialModelResolver` resolves an
explicit request, a restorable session model, the catalog default, or the runtime fallback in that
order. The resolver never reads `models.json`, resolves credentials, or registers providers. This
keeps file/routing mechanics inside `ModelsPlugin`, immutable catalog lookup inside `ModelRuntime`,
and new/resumed session policy above both. A removed session model falls back to the current catalog
with a diagnostic instead of silently restoring an unregistered route.

## Settings generations

`pi-settings::SettingsManager` owns only the current `settings.json` shape. It deliberately has no
legacy aliases or migration pass. Global `<agent-dir>/settings.json` is always eligible; project
`<cwd>/.pi/settings.json` is not even read until the product trust service approves that cwd. A
load retains each raw JSON object, recursively merges project objects over global objects while
replacing arrays/scalars, validates the fields consumed by non-UI Rust modules, and publishes one
immutable `SettingsSnapshot` for candidate generation construction. Unknown and UI-only fields
remain in the raw documents so package writes do not erase settings owned by another frontend.
Malformed reloads diagnose the scope and retain its last valid in-process document; field-level
validation failures are localized rather than rejecting unrelated current settings.

Settings persistence is a narrow field patch, not whole-object serialization. The package manager
locks a stable sibling lock file, rereads the latest on-disk object, refuses to overwrite malformed
JSON, replaces only `packages`, and commits through a synced temporary file and atomic rename.
Project writes require the same trust decision used for reads. This lets the JS package manager,
trust bootstrap, and runtime factory share one settings authority without making `pi-core` depend
on filesystem or product policy.

`ProductSessionFactory` resolves trust first and then maps the snapshot into one complete runtime
candidate. Non-UI mappings include initial provider/model/thinking, thinking budgets, active tools,
steering/follow-up queues, compaction and branch-summary budgets, assistant/standalone-completion
retry, shell configuration, session storage, image blocking/resizing, skills/prompts/packages,
provider timeout/retry, HTTP proxy, and Codex transport. The proxy is bootstrap/global policy and
is intentionally not project-overridable. UI-only theme and selector settings are retained but do
not create a second renderer settings implementation.
Memory follows the same generation publication boundary but not the `settings.json` schema.
The global `<agent-dir>/memory.json` document selects one registered memory provider and owns the
host recall limits; project settings never merge into it or redirect durable memory. The bundled
factory id is `local`, and automatic capture remains off.
Built-in provider credential loading, selected-provider overrides, and factory registration sit
behind `BuiltinProviderSet`; the session-construction Adapter supplies one transport and does not
contain provider-name construction branches.

## Project trust

Project trust is a product-level service in `pi-cli`, not a `pi-core` policy. It follows current Pi
behavior: `<agent-dir>/trust.json` stores canonical absolute paths, the nearest cwd/ancestor entry
wins, and writes are locked and key-sorted. Resolution order is an explicit
`--approve`/`--no-approve` override, the absence of trust-requiring resources, the in-process cwd
decision cache, the persisted nearest-ancestor decision, global `defaultProjectTrust`, then the
interactive selector. Non-interactive `ask` resolves to untrusted.

Trust-requiring resources are the current cwd's `.pi/settings.json`, `extensions`, `plugins`,
`plugins.json`, `plugins.lock`, `skills`, `prompts`, `themes`, `SYSTEM.md`, and
`APPEND_SYSTEM.md`, plus `.agents/skills` found from cwd toward
the repository root and `.hermes/skills` at
the repository root. The user-level `~/.agents/skills` root is always trusted. The current runtime
uses the decision to gate project `.pi` prompt files, project skill roots, project JavaScript
settings/packages/extensions, and native plugin manifests. The Rust JavaScript PackageManager is
called only after this decision and does not introduce a second trust store or prompt. As in Pi,
`HERMES.md`, `AGENTS.override.md`, `AGENTS.md`, and `CLAUDE.md` context discovery is not gated by
project trust, and trust is not a tool sandbox. Within one directory `HERMES.md` wins the context
candidate selection, matching the Hermes project-context boundary.

Filesystem tool paths follow Pi's `resolveToCwd` behavior. Relative paths resolve from the active
cwd, while absolute paths, `~` paths, `file://` URLs, and parent-relative paths may address files
outside it. Tool access is bounded by the process and operating-system permissions, not by a
`readable_roots` registry. The read tool additionally preserves Pi's macOS filename fallbacks for
Unicode normalization, screenshot AM/PM spacing, and curly apostrophes.

Startup resolves trust before constructing the first runtime generation. A session switch to a
different cwd sends a semantic trust request to the TUI and waits before constructing that
generation, so no project resource can be loaded before the decision. `/trust` updates persisted
policy for the current cwd; generation rebuild/restart applies the changed resource set.

`SkillsPlugin` is an example of the intended deep-plugin seam: it owns skill root configuration,
discovery, frontmatter parsing, collision policy, catalog formatting, `/skill:name` command
registration/expansion, and its generation-local diagnostics. The generic sourced loader keeps the
caller's source value attached to both successful skills and diagnostics. A direct root document
that does not declare valid skill metadata is silently ignored, while a declared `SKILL.md` remains
diagnostic on invalid metadata. Each registered `SkillCommand` owns its metadata and execution, so
command discovery, duplicate validation, and dispatch share one source of truth. Generic resource
loading and prompt assembly contain no skill-specific policy. The plugin contributes its catalog
through `before_agent_start`, which makes a separate `PromptContributor` trait unnecessary.

`PromptTemplatesPlugin` owns the parallel prompt-template seam. It discovers trusted project,
user, and explicit Markdown sources non-recursively; preserves visible symlink names and caller
provenance; parses frontmatter; applies deterministic first-name-wins collision policy; and
registers one slash command per template. Its argument parser and expansion own Pi's quoted tokens,
`$N`, `$@`, `$ARGUMENTS`, `${@:N}`, and `${@:N:L}` forms. The CLI only supplies trusted roots and
registers the plugin factory in each generation.

## Core contracts

- `Provider`: accepts semantic `ProviderRequest` data plus a generation-local `ProviderCallContext`, invokes wire hooks when its final payload exists, and returns `Stream<Item = Result<StreamEvent, ProviderError>>`.
- `Tool`: publishes `ToolSpec` and executes validated JSON arguments with an `AbortSignal` and `ToolUpdateSink`.
- `AgentPlugin`: registers tools/commands and participates in input, lifecycle, context, and tool hooks; its required hook-interest value is macro- or manifest-derived rather than author-maintained.
- `ProviderPlugin`: registers providers, routing overlays, and model metadata and may implement `before_provider_request` without implementing a provider.
- `SessionPlugin`: participates only in session lifecycle hooks and is rebuilt by `SessionPlugins`.
- `PluginDriver`: is the only component that invokes plugin hooks.
- `ProviderPluginDriver`: validates, registers, and invokes the ordered provider plugin set for one runtime generation.
- `SessionPluginDriver`: validates and invokes the ordered session plugin set for one session generation.
- `ModelRuntime`: is the immutable, generation-local model catalog and provider resolver.
- `TelemetryContext`: starts schema-typed spans through an injected sink; the no-op and in-memory
  sinks are adapters, not alternate event systems.
- `AbortHandle` / `AbortSignal`: provide cooperative cancellation without exposing Tokio types in public signatures.

## Agent layering

`Agent` is the stateful façade. It owns transcript state, active-run cancellation, steering/follow-up queues, subscriptions, and low-level run idleness. It is an `Arc`-backed cloneable handle so another task can call `abort`, `steer`, or `follow_up` while a prompt is running. Continuing from an assistant tail first consumes one steering batch as the new prompt and suppresses only the loop's initial duplicate steering poll; later steering remains queued until the first response completes. Follow-up is consumed only when no steering is available. `AgentSession` owns product settlement because only it can establish that retry, compaction, and queued-continuation policy has also finished; it dispatches the generation's `agent_settled` plugin hook before publishing `AgentSessionEvent::AgentSettled`.

`AgentLoop` is a stateless single-run engine over an `AgentContext` snapshot. It emits lifecycle
events, invokes a provider, delegates stream assembly and tool execution, polls steering after each
turn, polls follow-up before settlement, and returns the final context plus messages added by that
invocation. `AgentTurnControl` owns Pi's between-turn callbacks. After `turn_end`,
`should_stop_after_turn` runs before queue polling. Only when another turn will run does
`prepare_next_turn` receive that completed turn and optionally replace the run-local
context/provider/model/thinking values, before the next `turn_start`. If no steering was already
drained, the loop polls again after preparation to include messages queued during it. Settled runs
and runs stopped by `should_stop_after_turn` do not invoke preparation. During construction, an
owning product may permanently compose exactly one controller over the configured controller; the
Agent keeps that composition alive for its lifetime. Controllers that refer back to their owner use
a weak reference so session/runtime ownership cannot form a reference cycle. These replacements
never mutate the reusable Agent configuration. `FnTurnControl` is the closure Adapter for this
single Interface; it hides async future boxing rather than introducing a second callback lifecycle.
Turn callbacks receive `Arc`-backed immutable snapshots, while the live
loop mutates its context and per-run transcript with copy-on-write. The normal sequential callback
path therefore shares the same snapshot without cloning the transcript; retaining a snapshot
beyond its callback is safe but may make a later mutation clone the retained data.

`AgentOptions` and `AgentLoopConfig` expose `TransformContext`, `ConvertToLlm`, and `StreamFn`
closure adapters. Every provider request applies the generation's ordered `context` plugin hooks,
then the optional run-local context transform, then message conversion, then image filtering.
Transform and conversion operate on owned message snapshots without rewriting the transcript;
conversion is awaited exactly once per request, including tool continuations. As in Pi's base
Agent, the default converter retains only user, assistant and tool-result messages. Custom messages
remain in agent state and events but are omitted from provider input unless a caller supplies a
converter. Conversion can be configured between runs through `AgentConfigurationPatch` and is
captured once for each run.

`pi-session` owns the coding-agent projection. Session creation, open and reuse install its
runtime-message converter when the caller has not supplied one. It maps typed custom messages to
user content at the provider boundary, while session context construction handles summaries, bash
output and unknown wire roles. Explicit caller converters take precedence. Ephemeral agents inherit
the parent's converter so inherited session context has the same provider meaning. Configuration
and runtime-generation changes preserve the converter without changing stored messages.

An explicit `stream_fn` overrides `AgentLoopServices::default_stream_fn`. The loop resolves this
fallback once per run. `AgentRuntime` supplies a registry-backed default and can be built with a
different default stream callback. The default moves with its immutable runtime generation and
survives system-prompt configuration. This is the scoped Rust equivalent of Pi's configured
fallback; there is no mutable process-global stream registry. The callback receives the same
request and generation-bound `ProviderCallContext` as a registered provider, and can return either
a provider event stream or `AssistantResponse::Complete` for a terminal response without deltas.
Complete responses preserve assistant metadata, usage, tool calls, and message-end hook processing,
and emit only `message_start` and `message_end` for the assistant message.

Each assistant stream owns one mutable assembler state behind a read-only `AssistantStream` view.
`message_update` carries that constant-size shared handle and exactly one `StreamEvent`; the Agent
reducer, ordered native hooks, and listeners therefore do not clone cumulative content. Consumers
that require a full message call `snapshot()` explicitly, while `message_end` and `turn_end` share
the completed immutable assistant message. These hook fields were introduced in native ABI 7;
ABI 19 adds typed fork points for deferred isolated-session forks. ABI 18 adds aggregate managed-isolated-session usage outcomes. ABI 17 adds fresh/fork isolated-context initialization. ABI 16 adds detached usage attribution and ephemeral usage/call outcomes. ABI 8 adds UI
confirmation, ABI 9 adds isolated-session control, ABI 10 adds isolated-session
initial runtime selection, ABI 11 adds direct tool-free completion, ABI 12 adds ephemeral tool loops,
ABI 13 adds inherited prompt/history, guarded dispatch and invocation observations, and ABI 14 adds
detached compaction, execution origin and typed run-local state. Older native artifacts are rejected
before constructor resolution.

Cancellation and exceptional termination close the same observable lifecycle as Pi. A cancellation
that races with turn entry first commits the prompt and already-drained steering, then emits an
aborted assistant `message_start`/`message_end`, `turn_end`, and `agent_end`. Errors escaping the
loop trigger the equivalent error assistant sequence on a best-effort basis while the Rust API
still returns the originating `Err`; recovery emission continues past listener failures so state
reducers can observe as much of the terminal sequence as possible.

Provider streaming starts a typed `pi.ai.request` span and records normalized response metadata,
usage, chunk count, first-chunk latency, error status, and terminal reason. Product `submit` owns the
outer `pi.harness.run` span. `pi-telemetry` also exposes the complete exact harness span vocabulary
for compaction, navigation, checkpoints, turns, steps, tools, hooks, sleeps, event handlers, and
session writes so new orchestration can remain compile-time constrained instead of emitting
unstructured maps.

`StreamAssembler` is the sole owner of provider-stream assembly:

```text
StreamEvent -> shared live stream state -> final AssistantMessage
```

It validates Start/Delta/End/Done transitions, preserves content-index order, and parses tool argument JSON only after the tool-call block ends. Separate response/content metadata events preserve resolved response model and ID, diagnostics, deferred handles, raw stop reason, `end_turn`, redacted-thinking markers, and tool namespaces as providers discover them. Stream errors and aborts finalize the accumulated partial blocks and observed metadata instead of replacing them with an empty assistant message.

As in Pi, `Done` finalizes usage and stop reason without emitting a `message_update`; consumers
observe terminal metadata through `message_end`. Content events continue to emit stream updates.

`ToolScheduler` selects sequential execution when requested globally or when any resolved call is
declared sequential. Its two observable schedules are:

1. **Sequential:** each call completes `start -> prepare/validate -> tool_call -> execute -> tool_result -> end -> result message` before the next call starts. Cancellation after a result leaves later calls unstarted.
2. **Parallel:** start and preparation remain source ordered, ready executions are bounded by `max_parallel_tools`, end events use completion order, and result messages use assistant source order.

Unknown, invalid, blocked, and truncated tool calls produce error tool-result messages rather than disappearing from provider history. Final result messages preserve non-empty `added_tool_names`; tool-call and tool-result hooks share one immutable `AgentContext` batch snapshot.

## Session persistence and restore

`pi-session` follows the v4 implementation in
`legacy/pi/packages/agent/src/harness/session`. A JSONL file starts with the exact v4 header and is
followed by four mutation kinds: tree entries, lane records, lane pointers, and global facts. Every
mutation consumes one shared, consecutive `seq`. Entry and record IDs share one namespace.
Top-level entry and lane-record timestamps serialize as Pi-compatible ISO 8601 strings. The decoder
also accepts the millisecond-number representation emitted by earlier pi-rs v4 builds; nested agent
message timestamps keep their millisecond wire shape.

Entries form an immutable parent-linked tree. Named lanes are durable pointers into that tree and
remain available after moving away from their prior leaves. Lane records hold operation starts and
finishes, abort requests, step/tool attempts, queues, deferred writes, and attributed usage. Global
latest-value facts hold the session name and entry labels. Statistics are reconstructed from message
entries and the signed usage ledger across all lanes.

Product-facing billed totals deliberately aggregate every usage-bearing tree entry, including
assistant messages, metered tool results, compactions, and branch summaries. The scope is the whole
session tree rather than only the selected branch, so resume and navigation do not erase work that
was already billed. The signed v4 usage ledger remains the storage-level statistics contract for
operation-attributed usage and adjustments.

`Session<Storage>` is the small public façade shared by `InMemorySession` and `SessionLog`. It owns
ID provisioning, validation, global queries, branch queries, lane views, records, and facts. The two
repositories implement create/open/list/delete and branch/tree fork semantics. A branch fork copies
only the selected message path and applicable facts; a tree fork copies all entries, lanes, and
applicable facts. Neither copies operation records.

`JsonlSessionRepo::resolve_exact_id` owns the CLI's project-scoped session identity lookup and Pi
directory/filename layout. Resolution is read-only: an existing ID yields its metadata, while a
missing ID yields a timestamped `..._<id>.jsonl` target without creating a directory. The chosen ID
then travels through `AgentSessionRuntimeTarget` and `ProductSessionFactory` into the new session
header, keeping the adapter thin and deferred persistence intact.

`SessionDocument::context()` derives model, thinking level, and active tools from the entire selected
path. Its default transform starts at the latest compaction; that compaction contributes its summary
and persisted `retainedTail`, followed by later entries. Deferred assistant handles are omitted.
Callers may apply additional entry transforms and register projectors keyed by `customType`. Because
Pi's agent-level message union is extensible, `SessionContext` preserves both standard and custom
roles losslessly, including unknown wire fields. `provider_messages()` applies the same projection as
Pi's `convertToLlm`, including branch/compaction wrappers and bash/custom messages.
Interactive bash execution keeps the last 2,000 lines or 50KB in the session message. When that
tail is truncated, `pi-shell` streams the complete combined output to a temporary file and persists
its `fullOutputPath`, so restored context and NDJSON frontends can expose the same continuation
handle without placing the complete output in provider context.

`validate_record_log()` and `reduce_lane_state()` mirror the newest Harness recovery reducer. They
reject contradictory operation, attempt, queue, tool, provisioned-entry, and deferred-handle logs,
then reconstruct pending input, deferred writes, unfinished steps, tool batches, effective
configuration, structural targets, overflow state, and terminal-failure provenance without mutating
storage. Opening a product session feeds the main lane through that reducer before restoring the
runtime. Accepted but unapplied deferred writes are committed, an initial user message that never
reached `message_end` and undelivered run queues become durable `next_run` items, and the interrupted
operation is closed as aborted. This reconciliation is idempotent across repeated opens.

Opening a session never performs provider I/O or blindly re-executes a tool. The current TypeScript
Harness scaffold rejects every recorded-session restore, while pi-rs deliberately provides the
stronger fail-closed reconciliation above. Automatic replay would be unsafe for a tool whose
external side effect completed before its result was persisted; a future opt-in replay adapter must
therefore require both a provider deferred-redemption capability and an explicit safe tool replay
policy rather than weakening open semantics.

JSONL loading repairs only a syntactically torn final append, using a sibling temporary file and
atomic rename. A complete schema-invalid final line and every malformed middle line are hard errors.
A valid final line missing its newline receives the newline before further appends.

New agent sessions use a deferred `SessionLog`: the header and exact encoded mutations accumulate
in memory until the first assistant `message_end`, then are written together as one JSONL file.
Startup followed by quit, interruption before that event, and shell-only use therefore leave no
empty session in the resume list. Existing/opened logs remain immediately durable, and an unsaved
log cannot be forked. Whole-session reload reuses the in-memory log so reloading plugins and
resources does not accidentally create or discard an unsaved session.

Command and input-hook transformations preserve two text views. The effective text remains the
standard user-message content used by Agent, provider projection, replay, and recovery. When it
differs, the submitted product-facing text is stored in the user message's namespaced
`piRs.displayText` extension and projected into product events, transcript history, and queue
snapshots. Pi v4 readers that ignore unknown fields continue to see the effective message, while
pi-rs resume and queue recovery do not expose private expanded prompts. Frontends do not parse or
reverse plugin-specific prompt formats; messages written before this metadata existed display their
persisted content as-is.

`before_agent_start` custom messages remain agent-level messages through lifecycle dispatch and
runtime state. For a normal prompt the submitted user message is emitted first, followed by custom
messages accumulated in hook registration order. The session Adapter persists them as Pi v4
`custom_message` entries, retaining `customType`, string-or-block content, `display`, and `details`;
only the provider-request seam projects them to ordinary user messages. Resuming a session restores
the custom role before the same request-time projection, so extension context and lifecycle events
do not silently turn injected messages into user submissions.

A role-preserving `message_end` replacement is the completed message for both live state and Pi v4
persistence. If the replaced message was provisioned before the run, the session journal records a
same-ID deferred-target correction before appending it. Recovery validation resolves that latest
target, so resume reconstructs the transformed user, assistant, or tool-result message rather than
the pre-hook value while ordinary display-text metadata remains intact.

`AgentSession::create` and `AgentSession::open` adapt the v4 tree to `PiRuntime` and return the
session's stable `Arc<AgentSession>` identity. Configuration changes are v4 entries, completed
messages are persisted on `message_end`, and pi-rs-only prompt snapshots/resource diagnostics use
reserved `customType` values rather than extending the v4 entry
union. Opening restores data state only; executable plugins and resources always come from the
supplied runtime. Checkout, branch summaries, and compaction rebuild runtime context immediately.
`prepare_create` and `prepare_open` expose the same shared identity as a `PreparedAgentSession` whose
`session_start` event is deferred, allowing a host to order replacement lifecycle events correctly.

`pi-session::compaction` ports the Harness `retainedTail` algorithm: provider usage plus trailing
heuristics estimate context size, cut points never begin at a tool result, oversized turns receive a
separate prefix summary, previous summaries update incrementally, and read/modified file metadata is
carried forward. Summary calls use `PiRuntime::complete`, an isolated provider request with no tools
that does not mutate the agent transcript. `AgentSession::compact` performs manual compaction;
`AgentSessionOptions::context_window` enables threshold compaction before/after runs and proactively
between a completed tool result and the next provider request in the same run. The session composes
this policy over any existing `AgentTurnControl`; successful in-run compaction persists its checkpoint
and replaces only the run-local context, then reconciles global Agent state after the run becomes idle.
Failures are best effort and leave the original live context in place. Recoverable context overflow
removes the failed assistant from the prepared context, compacts, and retries once.
Retained pre-compaction usage is ignored for subsequent threshold decisions so it cannot cause an
immediate compaction loop. It is also excluded from product context reporting: context usage is
unknown immediately after compaction and becomes known again only after a later successful
assistant response. Assistant usage is a valid estimate anchor only when its timestamp is not older
than a prefix message inserted before it, such as a compaction summary. Each summary request is also
constrained by the active generation's `ModelSpec`: non-reasoning models force thinking off, and the
configured summary budget is clamped to the model's maximum output tokens.

Normal assistant turns classify provider failures with the same current Pi transient/terminal
rules. An enabled retry persists the failed assistant for audit, removes only that terminal failure
from the next live provider context, publishes retry start/end product events, and waits with an
abortable exponential backoff. Context overflow takes the one compaction-recovery path before
ordinary retries, so it cannot consume both policies or create a compaction loop. Standalone
compaction and branch-summary completions reuse the generation's retry policy without mutating the
agent transcript. Shared HTTP transport retries configured transport failures and retryable status
codes independently at the wire boundary, honors server retry delays up to the configured cap, and
keeps header/body timeouts abortable.

Session extensions use a third, session-owned lifecycle system. `SessionPlugin` mirrors Pi's ten
`session_*` extension hooks: start, info change, before switch/fork/compact/tree, compact success or
failure, shutdown, and tree completion. Every callback receives the plugin ID, session ID, JSONL
path, cwd, parent session ID, and active plugin generation. Observer failures are isolated into
`SessionPluginDiagnostic`; `before_*` hooks run in registration order, the last non-empty result
wins, and the first cancellation short-circuits, matching Pi's extension runner.

`SessionPluginDriver` is the immutable, generation-local hook executor, parallel to `PluginDriver`
and `ProviderPluginDriver`. `AgentSession` is the host: it owns the `SessionPlugins` source blueprint,
the current `Arc<SessionPluginDriver>`, and the atomic reload swap. The driver neither retains plugin
sources nor selects the active generation.

`SessionContextBuildOptions` remains ordinary session projection configuration rather than a
plugin-registration surface. `AgentSessionOptions` independently combines it with a
`SessionPlugins` blueprint. Factory-backed session plugins are rebuilt on reload. The complete next
generation is prepared before the old generation receives `session_shutdown(reload)`; a load
failure therefore leaves the old generation running. A successful reload commits the new
generation and emits `session_start(reload)`.

Plugin-context scalar and point queries use narrow reads owned by `Agent`, `AgentSession`, and
`SessionLog`: current model/thinking/running state does not clone the transcript, activity checks do
not clone the frontend snapshot, and session id/name/label/entry/header reads do not reconstruct a
`SessionDocument`. Coherent multi-entry inspection remains the explicit `SessionSnapshot` path.

`MultiSessionManager` is the multi-session product Module above `AgentSession`. It owns the injected
`AgentSessionRuntimeFactory` and keeps its active-session map private. Ordinary acquisition,
replacement, close, and shutdown operations remain exclusive. UUID-addressed isolated-session
preparations share the read side of that lifecycle gate, so parallel tool calls can build children
concurrently while still excluding replacement, close, and shutdown. Opening an already-active
path reuses its `PiSession`; creating or switching to a path owned by another handle fails before
any session lifecycle transition starts. Manager shutdown drains and closes every managed handle.

## Memory systems

pi-rs has two deliberately separate persistent-memory mechanisms. The default provider,
`pi-plugin-memory-hermes`, now targets **NousResearch/hermes-agent**, pinned to
[`e629c900`](https://github.com/NousResearch/hermes-agent/tree/e629c900a87622ddcc31f67a4b4a756b239fbaf0).
It no longer uses the Pi-Hermes extension's policy-only prompt, correction regex, or
direct-completion/CLI review protocol.

Global `MEMORY.md` and `USER.md` remain canonical under
`<agent-dir>/pi-hermes-memory/`. Their contents are frozen at session start and appended to the
effective prompt; writes update files but not that session's frozen prompt. The unified `memory`
tool supports add/replace/remove and atomic operation batches, targeting memory (default) or user.
It never selects a project target from cwd: repository-owned instructions belong in `HERMES.md` or
`AGENTS.md`, while repository procedures belong in trusted repo-local skills.
Defaults are 2,200/1,375 Unicode characters. Capacity errors return the current entries so the
calling Agent can consolidate and retry without launching another Agent. Following Hermes's
[memory tool](https://github.com/NousResearch/hermes-agent/blob/e629c900a87622ddcc31f67a4b4a756b239fbaf0/tools/memory_tool.py),
capacity, missing-match, and atomic-batch consolidation failures share one invocation-local budget
across targets. The first three failures may return entries/retry guidance; the fourth and later
return only `success: false`, `done: true`, and a stop-retrying message. This is a terminal memory
response, not an Agent-loop termination. A successful write (including duplicate/no-op success)
resets the counter; a new foreground or detached invocation starts at zero. Ordinary standalone
validation, ambiguous standalone matches, and content-safety failures do not consume this budget.
Per-file locks, conflict checks, atomic replacement, content scanning, and the derived SQLite FTS
index remain in the store.

Memory writes and session-start snapshots use Hermes's strict threat-pattern set from the pinned
revision. Scanning is bounded to 65,536 Unicode characters, checks invisible/bidirectional
carriers before NFKC normalization, and then checks normalized text for injection, promptware,
exfiltration, persistence, and hard-coded-secret shapes. The existing provider-specific credential
detectors remain an additive Pi safety check. At session start, a risky on-disk entry is replaced
only in the frozen prompt snapshot with a `[BLOCKED: ...]` marker naming the matched patterns. Its
original text remains in the canonical file and live store so it can be inspected or removed;
already-blocked markers are not wrapped again.

Read-modify-write mutations derive parsing and drift checks from one checked UTF-8 snapshot under
the per-file lock. A leading UTF-8 BOM is ignored and removed by the next successful write. Invalid
UTF-8 or another read failure returns a normal failed memory result and leaves the original bytes
unchanged. `replace`, `remove`, and atomic batches refuse files that cannot round-trip through the
entry format or contain an impossible over-limit entry, save the exact bytes to a
`MEMORY.md.bak.*`/`USER.md.bak.*` recovery file, and return remediation details. `add` reloads the
same checked snapshot while allowing mild external formatting drift, preserving that content when
it appends the new entry. Fingerprint checks around publication still catch writers that race after
the checked read.

Review scheduling is plugin-owned and session-local. User messages increment the memory counter
(default 10; resumed user count is hydrated from the active branch). Model iterations separately
increment the skill counter (default 10), reset by skill_manage use. A successful final response
at `agent_settled` combines due flags into one detached review. There is at most one review per
parent. A new foreground request, compaction, reload, or shutdown cancels it; foreground waits at
most two seconds for acknowledgment. `/refine [focus]` starts the same review explicitly and
bypasses `reviewEnabled`, interval counters, and deferred scheduling. A focus string is appended to
the private request as the user's priority; an empty conversation or already-running review does
not start another fork.

`agent_settled` remains the Pi-compatible notification that session orchestration is idle; it is
only an asynchronous review scheduling point, not a memory commit barrier. The hook returns after
the review has been admitted. No memory-specific completion fields or assistant-message lifecycle
events are added to the Pi contract.

Managed children have `SessionExecutionOrigin::Subagent`, preserved across reload and nested
delegation. They receive the configured frozen memory snapshot as inherited reference context and
may use read-only search surfaces such as `memory_search`, but they cannot mutate durable memory.
The subagent launch policy removes `memory` from inherited and explicit child tool sets; the Hermes
tool-call hook independently blocks it for any managed child that reaches the registry through a
custom or stale generation. Hermes also skips child review counters, autonomous reviews, startup
backfill, and opt-in lifecycle flushes. The parent session is the sole owner of ordinary durable
memory writes. This policy belongs to the two feature plugins; generic session orchestration only
supplies provenance.

Hermes background-review Agents are not ordinary managed children. They are bounded private
ephemeral forks with a freshly constructed `HermesReviewPlugin`, a parent-owned mutation allowlist,
and no session/review-scheduling hooks, so they may commit reviewed memory receipts without gaining
delegation authority or recursively scheduling another review.

Live transcript indexing and startup backfill are derived, cancellable background jobs owned by
the memory provider instance. A new foreground run cancels both before model work starts;
compaction and shutdown cancel and boundedly join them before final indexing/checkpoint work.
Managed children retain live transcript indexing but never launch startup backfill. Blocking index
work receives the originating run/provider cancellation signal and checks it before database work,
between backfill files, within message insertion loops, and before transaction commit, so dropping
an interrupted transaction rolls it back. All provider instances targeting the same canonical
SQLite path share an in-process write coordinator. Schema setup, memory mirroring, session writes,
metadata updates, and checkpoints therefore serialize instead of racing into `database is locked`;
transactions remain scoped to one mirror sync or one session replacement rather than the complete
backfill scan.

Hermes remains one package with two internal responsibilities. `HermesMemoryPlugin` owns tools,
commands, prompt injection, foreground invocation budgets, review scheduling and cancellation.
For each automatic review, explicit consolidation, or lifecycle flush, it constructs a fresh
`HermesReviewPlugin` and attaches it to the private Agent's tool hooks. That plugin has no prompt,
session, or review-scheduling hooks. Each instance owns its consolidation budget and successful
skill-read witnesses. The generation's tools locate this typed state by `RunId` through a
Hermes-private weak directory; RAII leases remove entries on completion, error, timeout,
cancellation, dropped futures, and eventual generation release. Foreground state is bounded by
`agent_start`/`agent_end` with session-specific shutdown cleanup. Shared storage and rule code do
not imply shared execution state. `origin` is only a diagnostic label, never the authority to enable
autonomous skill rules. Missing or mismatched invocation ownership rejects Hermes mutation-tool
execution rather than silently falling back to unrestricted writes. Standalone tool calls have no
Agent invocation and retain their explicit standalone behavior. This is a deliberate Rust plugin
execution design, not an additional installable plugin or a change to Hermes review thresholds.

The sole review transport is a bounded, in-process Agent fork. It pins the current generation,
inherits the effective system prompt, provider authentication, and, by default, the current model
and thinking settings, then replays structured parent history. A configured model that resolves to
a different model starts with thinking off and without the parent's token-budget overrides unless
the review configuration explicitly selects a thinking level. A missing or ambiguous optional
review model falls back to the current model instead of suppressing review. A successfully selected
different model uses an older-text digest plus at least 24 recent messages, keeping tool-call/result
groups together. It shares advertised tool definitions
for cache parity but checks an execution allowlist before argument preparation and execution.
The default allowlist is memory, skill_manage, skill_view, skills_list and the available Pi file
read/search tools. `reviewExtraTools` can opt in other tools only within the parent's active-tool
ceiling. Shell and general file writes are not executable by default. Tools receive no mutable
parent-session, model-control, UI, or child-launch capabilities. This is an invocation boundary,
not a sandbox for trusted native code.

Review uses 16 model/tool iterations, a default aggregate 600,000 input-token budget, and a
120-second timeout. It creates no managed child session or review JSONL file, emits no parent
agent/session hooks, and cannot recursively schedule a review. Its completed assistant, metered
tool, and private compaction calls are aggregated and appended to the parent ledger as one
`background_review` usage adjustment, without adding any parent message or context entry. Session
totals exposed by the TUI, `/session`, and RPC include that adjustment. Cancellation and timeout return the completed,
tool-balanced review-message prefix, so successful writes from earlier complete tool groups remain
available for receipt-based reporting; an interrupted partial group is omitted. Only successful
mutation receipts are reported, not assistant claims. `memoryNotifications` accepts `off`, `on`
(default), or `verbose` (and legacy booleans): `off` suppresses successful action notices, `on`
uses compact generic/tool messages, and `verbose` includes bounded argument previews. Review
failures remain visible independently of this action-notification setting. The reusable
direct-completion API remains available to other plugins; it is
not an alternative review transport, and there is no subprocess fallback.

Long review contexts use detached, in-place LLM compaction after the first provider response and
its complete tool-result group, preserving the initial cache replay. The plugin supplies Hermes's
default model-window profile: 75% of effective input capacity below a 512,000-token window, 50%
otherwise, with the 64,000-token floor capped at 85% of effective capacity when binding. The
runtime retains the protected prefix, a token-bounded recent tail with complete tool groups, and
the newest user request verbatim. Summary calls contain no executable tools, image data, or
reasoning traces. Transcript estimates and output caps use model limits; actual input/cache usage is
charged to the review's existing budget. Invalid, empty, or truncated summaries stop only that
review without replacing its context. Two ineffective summaries disable further compaction.
No private session database, compaction entry, rotation, or lifecycle hook participates. The
summary call contributes only to the parent review's aggregate usage adjustment. If model-window
metadata is unavailable, compaction stays off and the review retains its iteration/input/time bounds.

Automatic review selects memory-only, skill-only, or combined learning instructions. The
plugin-owned prompt assets preserve Hermes e629c900's learning rules; skill-only and combined
reviews use the same complete skill policy. User corrections to style, format, or workflow belong
in the relevant skill body as well as memory when appropriate. Review prefers a relevant skill
loaded in this conversation, then an existing class-level umbrella, then a supporting file, and
creates a new umbrella only when none fits. References, templates, and runnable scripts have
distinct roles and need a pointer from SKILL.md. Fresh reads precede changes to existing files;
a read-guard refusal permits one fresh read and one retry. Verified fixes and successful retry
patterns are reusable; unresolved attempts, transient setup failures, blanket negative tool
claims, and one-off task narratives must not become durable procedures. With no learning signal,
review may save nothing. In combined mode, protected skills do not prevent independent memory
updates. These instructions are appended only to the private review request, preserving the
parent system prompt and tool definitions.

Skills are active procedural memory: review may create class-level skills and improve existing
ones using verified procedures, corrections, and pitfalls. Global creations live below
`<agent-dir>/pi-hermes-memory/skills`; project creations (including background review) live below the active Git
checkout's `.hermes/skills` and require the same project-trust decision used for discovery.
Trusted `.agents/skills` remains the cross-tool repository root. New skill-tool creations receive
versioned `curator.json` provenance. Foreground creations are user-managed; background review
creations are curator-managed. `/curator adopt <name>` explicitly grants management of a global
skill; `project:<project-name>:<skill-name>` or `--scope project` targets the current trusted
checkout. There is no migration, compatibility reader or automatic adoption of old metadata.
Background edits require managed provenance, unchanged whole-package hashes, an unpinned skill,
and a successful read of the exact existing file during that review. User-owned and externally
changed skills fail closed. Supporting files use relative, non-symlink paths. Autonomous deletion
requires an existing `absorbed_into` skill and moves the complete source into its root's hidden
`.archive` directory, excluded from ordinary skill discovery.
Pi-native skill roots and immutable catalog ownership stay with `SkillsPlugin`; reload is required
before newly created skills appear as `/skill:` commands.
The learning prompts use Pi's `/skill:<name>` syntax and skill tools' `content` field. They leave
protected-skill corrections to foreground user action. Memory instructions retain the Pi
policy against storing temporary task progress or secrets.

Hermes owns a separate library-wide Curator module, adapted from the same pinned upstream
`agent/curator.py` and `tools/skill_usage.py`. This is a deliberate Rust product addition, not
Pi conformance. It processes immediate skill packages in the global Hermes root and the current
trusted Git checkout's `.hermes/skills`. These are independent libraries with identical ownership
checks; no historical project registry is scanned. Supporting autonomous project skills is a
deliberate extension of upstream Hermes, which excludes repository-owned skills from curation.
The project `.agents/skills` root, bundled/hub installs, external roots, missing/invalid provenance, pins and externally edited
packages are excluded. In particular, no bundled/hub ownership manifest or bundled pruning is
introduced. Provenance records creation, activity, usage/view/patch counts, active/stale state,
ownership, pinning, and a hash of every package file except the provenance itself.
`SkillsPlugin` exposes an optional semantic invocation observer; SDK wiring connects Hermes
usage persistence without putting storage policy in the catalog, session, or memory-loader.
Invocation and foreground activity observers cover both managed roots and identify packages by
path, so same-named skills in different scopes do not share activity. Successful foreground
`read`/`skill_view` calls also record activity. Maintenance reads do not
refresh inactivity age. Catalogs remain generation-local and immutable: `/reload` is required
after creations, archive, restore or rollback.

The generation-owned Curator worker is enabled by default, checks every 30 seconds, and runs
only in idle user sessions with no queued input: default interval 168 hours, minimum idle time
2 hours, stale after 30 days and archive after 90 days. Its first eligibility check seeds the
clock without doing maintenance. Durable timestamps, pause state and last-report identity are
independent per skill root. Pausing the global library does not pause the project library.
The worker checks both roots independently and skips absent project roots. Process-held
foreground activity leases cover both existing roots and protect long requests across sessions;
OS file locks serialize maintenance and package publication. Foreground work/compaction cancels
an in-flight private run; shutdown/reload cancels and joins the worker.

Optional model consolidation defaults off. It uses the existing bounded ephemeral Agent with
a dedicated prompt, no parent history, no inherited Agent/session hooks, at most eight model
iterations, a default 200,000 input-token budget and 120-second timeout. Only skill tools may
execute; advertised schemas retain the normal ephemeral-session contract. Dry-run denies all
mutations, including usage/provenance/scheduler/report writes inside the skill library. Model
usage is still attributed once to the parent as `task=curator`, without transcript messages or
a new resume entry. Actual changes are reported from successful tool receipts. Consolidation
pins its scope and root in private invocation state. Each library runs separately: listing,
viewing, creation, edits and consolidation targets must stay inside that bound scope, and omitted
create scope defaults to it. Ordinary background-review consolidation also stays within one scope.
Consolidation checks ownership again at publication, requires fresh exact-file reads, and refuses source
retirement until all supporting-file content is preserved in the destination.

`/curator` and the standalone `pi curator` adapter share the same command/policy implementation.
Status, adoption, pinning, local pruning, pause/resume, archive/restore and backup/rollback need
no provider. The standalone adapter resolves the existing project trust policy before passing
cwd and trust into the same scope selector. `status`, `run`, `pause/resume`, backup and listing
commands default to global plus the current trusted project. `--scope global|project|all` narrows
that selection; single-object mutations and rollback require one scope. Bare names/IDs and a
bare rollback retain global semantics; project skill, archive and backup IDs carry the
`project:<project-name>:` prefix. Multi-scope output is keyed by library identity.
Model-backed CLI runs use the normal session/provider construction path. Every
maintenance pass that changes packages first snapshots all eligible packages under that root's `.backups`;
archives retain complete packages, and restore never overwrites an existing destination.
Rollback validates targets before mutation, takes a safeguard snapshot, and archives replaced
versions. It refuses externally edited/pinned targets and never removes unrelated newly created
skills. Partial snapshots without a completion marker are not selectable. Archives, backups and
reports are retained without automatic deletion; exact storage format and retention are Rust
adaptations, not full Hermes CLI/storage compatibility.

Legacy project-memory Markdown remains indexed read-only so existing data is not deleted, but the
project-memory mutation/switch surface is retired. Failure notes, FTS/session search, manual
memory-management commands, and optional standing instructions are retained as Rust product
extensions, not claims of Hermes Agent command/storage parity. No existing user memory or skill
files are deleted by the change.
`/memory-preview-context` shows the actual frozen prompt. `/refine [focus]` is the Hermes-compatible
manual entry to the shared background review path. Explicit
`flushOnCompact`/`flushOnShutdown` enable extra best-effort review at lifecycle boundaries
(minimum six user requests; 30/10-second bounds); both default off. These lifecycle flushes add an
urgency preface to the same complete combined memory-and-skill policy used by automatic review.
The old correction regex and
automatic capacity-consolidation Agent are removed.

Detached review compaction, managed-child review suppression, and per-invocation consolidation
failure limits have deterministic regressions, including the complete review/read/summary/
memory-write handoff, an unchanged parent message tree, and one parent usage adjustment. Private plugin tests cover shared hook/tool run
identity, no parent hooks or registration, fail-closed validation, concurrent review isolation,
fresh read witnesses, and state release across completion/error/cancel/timeout/drop/reload.
Provider-request regressions check the learning rules and action priority in all three automatic
review modes. They verify policy delivery, not a scripted provider's ability to learn. Prompt
wording, JSON configuration, Pi-native skill metadata/catalogs, and filesystem conflict protection
remain Rust adaptations. The compressor
implements the default window/retention profile and detached lifecycle, not every upstream
compressor tuning option or recovery heuristic. This is not byte-for-byte or complete Hermes parity.

Hermes is the only first-party memory provider registered by `pi-sdk`. `memory.json` selects
exactly one provider; an unknown provider id, including the retired `"local"` id, rejects candidate
generation instead of falling back or migrating data. Removing the local provider does not delete
existing files under `<agent-dir>/memory`; the product simply no longer reads them.

Memory providers do not add a fourth plugin Driver. `pi-memory-loader` is a host-side construction crate that
owns `MemoryLoader`. The Loader reads `<agent-dir>/memory.json`, selects exactly one registered
`MemoryProviderFactory`, and awaits provider initialization before the candidate generation is
published. The resulting `Arc<dyn MemoryProviderPlugin>` is projected directly into the existing
`AgentPlugin` and `SessionPlugin` Drivers, so one provider instance and its state are shared by both
systems without a forwarding Adapter, separate lease object, or memory-specific Driver.

`MemoryProviderPlugin` is a marker Interface extending the ordinary `AgentPlugin` and
`SessionPlugin` Interfaces. It adds only the provider identity used by `memory.json`; it does not
copy their hooks into a parallel lifecycle contract. A provider declares its tools, commands,
agent hooks, and session hooks directly through those existing plugin traits. There is no parallel
`capabilities` declaration or generic `MemoryLifecycleTask` event envelope to keep in sync.

There is no provider-neutral record, storage, retrieval, or session-index Interface. Provider
authors receive configuration plus the ordinary plugin lifecycles and own their complete memory
model behind that seam. This avoids freezing the bundled SQLite provider's record and query shapes
into every future provider before a second implementation demonstrates shared semantics.

The Memory Loader owns only the outer shape of `<agent-dir>/memory.json`; generic `settings.json`
parsing neither decodes nor merges memory policy, and project settings cannot redirect durable
memory. The versioned document selects the provider, recall budgets, and provider-specific
configuration under `providers.<id>`. Each provider value remains opaque JSON to the host, and
only the selected subtree is passed to its factory. Missing configuration defaults to Hermes;
an invalid document or unknown provider rejects generation preparation so the previous generation
remains published.

The plugin-facing custom-entry capability permits an atomic extension-state append while the Agent
itself owns the prompt operation. `SessionLog` still assigns the shared mutation sequence; busy
compaction, switch, fork, and tree operations remain rejected. This preserves Pi extension-state
semantics without weakening other session mutation gates.

`SessionContext::launch_isolated_session` is a deliberate Rust product extension seam rather than
Pi core workflow policy. It creates a fresh `PiSession` through the same
`AgentSessionRuntimeFactory`, starts one user-message run, and returns a generation-bound opaque
handle with `wait` and `abort`. Optional initial active tools, model, and thinking level inherit
from the caller when omitted. `MultiSessionManager` resolves those values into one complete
`AgentSessionInitialState`, rejects any tool selection outside the caller's active-tool ceiling,
and passes it to the product factory. The factory applies that state before session preparation, so
the first provider request and the initial Pi v4 model/thinking/tool records agree. The generation
overlay also marks the child as `SessionExecutionOrigin::Subagent` before any hook runs. That
transient origin survives live generation replacement and nested children, without changing Pi v4
wire records or disabling ordinary plugin tools. The caller's
`PiSession` is never replaced. The manager registers
the isolated handle like every other active session so path ownership, plugin-context binding, and
shutdown remain centralized; frontends retain their explicitly returned primary `PiSession` rather
than deriving presentation ownership from the manager inventory. An owner-scoped
`IsolatedSessionObservation` exposes only the child's id, cwd, snapshot, ordered event subscription,
and observation of its own isolated descendants. This lets product frontends display live child
activity without exposing prompt, abort, replacement, or other control operations and without
promoting the child to a frontend-owned `PiSession`. Isolated files live below the
owning session's sibling directory and therefore do not enter the top-level resume listing. Closing
an owner closes its registered isolated descendants. Completed child logs remain outside top-level
resume discovery; a frontend may resolve one through its parent link as a read-only snapshot without
registering or resuming it as a primary session. In-process detached receipts and supervisor
coordination are feature-owned layers over this interface; cross-process reattachment remains
unimplemented. Parallel isolated launches prepare their complete runtime generations concurrently;
the manager's lifecycle gate prevents an owner replacement, close, or shutdown from racing those
preparations, and the UUID-derived paths avoid active-path collisions.

Isolated prompt tasks have manager-owned, abort-on-drop supervisor handles. A prompt panic becomes
an explicit retained terminal failure rather than an unobserved task exit. Launch cancellation and
normal abort keep polling the same prompt to preserve its persistence and settlement cleanup;
shutdown/close signal all relevant runs before joining any. A cancelled registry-drain future
retains the handles and tree membership for another drain. Waiters capture only a result channel,
not the manager or task owner, so waiting cannot prevent owner-drop cancellation. Explicit shutdown
is the awaited cleanup contract; dropping the manager requests task cancellation only. Cooperative
cleanup has no new hard deadline, and a non-cooperative hook may still block it. This does not make
all session lifecycle hooks cancellation-safe or repair an Agent's state after arbitrary native
plugin panics.

`IsolatedSessionOptions.context` defaults to `Fresh`; `Fork` initializes the child from the
caller's active branch after its existing compaction/context projection. During a running tool
batch, the cutoff precedes the requesting assistant message, including the triggering user input
but excluding the entire active tool batch even when a sibling has already returned. MessageEnd
persistence is awaited before tool dispatch; the cutoff matches the active assistant against the
durable/in-memory branch rather than waiting for sibling tools. No pending-tool failures are
invented for that excluded batch. Histories then evolve independently; cwd/files remain shared.

`SessionContext::isolated_fork_point()` captures the last entry in that safe prefix as an
`IsolatedForkPoint { parent_session_id, parent_entry_id }`, returning `None` for an unsaved or empty
parent branch. `IsolatedSessionOptions.fork_point`
pins later forks to that entry in the calling session's immutable tree. Later parent messages,
branch navigation and compaction do not change the selected prefix. Foreign session IDs, missing
entries, and fork points paired with fresh context fail before child preparation. The host owns
cutoff and projection; plugins carry only typed session/entry identities, not JSONL reconstruction.
The fork point is not session-resume authority or a new persisted entry type. This native ABI 19
addition retains existing descriptor checks and process-pinned library lifetime.

The host seeds a prepared child before lifecycle activation using a v4 custom entry
`pi.isolated_context` containing the parent session id and effective `AgentMessage` history.
This is a deliberate Rust divergence from pi-subagents' branch-file copy: the host snapshots the
session log instead of copying its file. Both ordinary and isolated forks reject unsaved parents;
normal tool dispatch follows assistant persistence, so first-turn delegation can still fork.
Seed messages preserve images, retained compaction summaries and unknown
wire extensions. They do not replace child model/thinking/tools, contribute ancestor usage to child
billing, or trigger first-response materialization. Reload/replay projects the seed back into context;
child compaction expands its messages and treats ancestor usage as an obsolete context checkpoint.
An estimated seed at or above the child's known context window fails before launch, with instructions
to compact the parent or choose fresh context; initialization never silently truncates the seed.
The seed remains separate from child-authored message entries and returned run outcomes. Native ABI
17 includes the context-mode field; ABI 18 adds aggregate usage to managed isolated-session
outcomes. Exact-build plugin compatibility and pinned-library lifetime are otherwise unchanged.

`pi-plugin-subagents` is the first policy module over that seam. It registers the parallel-safe
`subagent` tool with focused `scout`, `worker`, `reviewer`, `oracle`, and `delegate` built-ins, plus
`contact_supervisor`, `subagent_supervisor`, and `bg_wait`. Its
generation-local agent catalog overlays those built-ins with recursive Markdown discovery from the
global agent directory's `agents/` subtree and the nearest trusted project `.pi/agents/` subtree;
project definitions win name collisions. Definition frontmatter owns `name`, `description`,
`aliases`, `systemPromptMode`, `inheritProjectContext`, `allowNestedSubagents`,
`maxSubagentDepth`, `tools`, `excludeTools`, `model`, `thinking`, `inheritSkills`, `skills`,
`skillPath`, `defaultContext`, and `timeoutMs`,
while the Markdown body owns the role system prompt. Omitted selections inherit from the immediate
parent; an empty `tools` field selects no tools; an explicit list selects work tools under the
inherited ceiling. Skill-loading and coordination additions described below also remain within
that ceiling, and exclusions can only narrow the resolved set. The mutating `memory` tool is always
removed for ordinary managed children even when a custom profile requests or inherits it;
`memory_search` remains available when selected within the parent ceiling. Canonical names beat
aliases; alias-to-alias ambiguity fails candidate generation. Bare model ids prefer the current
provider and otherwise require a unique available catalogue match; thinking levels are checked
against the resolved model before launch.
Reload rescans the catalog transactionally, and untrusted project definitions never enter the
candidate generation.

Built-in role metadata preserves the vendored profiles' reasoning policy: scout uses `low`,
worker/reviewer/oracle use `high`, and delegate inherits the immediate parent's level. This keeps
the fast reconnaissance role from accidentally inheriting an expensive `xhigh` or `max` parent
configuration while retaining explicit profile validation against the selected model.

The subagent tool accepts optional `async: true` to return a background receipt after the
feature-owned monitor is spawned. Default calls still wait in the foreground; fast terminal results return
directly without a redundant notification. This is detachment from a tool wait, not an independent
process. Background tasks attempt ordinary best-effort completion/request notifications and are cancelled on
owner close. One-shot frontends must wait explicitly if a result is required before they exit.

The subagent tool accepts optional `context: "fresh" | "fork"`. Selection precedence is explicit
tool argument, profile `defaultContext`, then fresh. An implicit fork preference falls back to fresh
only when no persisted parent branch exists; an explicit fork fails in that case. Other capture,
projection or launch errors remain errors. Built-in worker/oracle default to fork;
scout/reviewer/delegate default to fresh. The plugin's provider-context hook removes inherited
subagent call/result pairs, parent-only orchestration notices and old private run markers without
touching other tool pairs, parent storage, or the child's own nested delegation. This policy stays
outside generic session context initialization. Inherited signed/redacted Anthropic thinking is
also excluded from the child provider projection; the child's own reasoning blocks and requested
thinking level remain intact. Parent storage and unknown wire extensions are preserved. Each child
assembles its own role/system prompt. The context policy follows `nicobailon/pi-subagents`
`aa75b3353836f7868898e3bd58234d21eaff1463` (`src/shared/fork-context.ts`).

`subagent_workflow` is a deliberate Rust product addition over the same child-launch Module.
It accepts a bounded static JSON DAG: sequential `stages`, each with exactly one `run`, parallel
`all` nodes, or parallel `lanes` of sequential steps. Keys are `stage`, `stage/node`, or
`stage/lane/step`. Validation rejects duplicate keys, unavailable profiles, forward/sibling inputs,
more than 64 nodes, and submissions over 256 KiB before any launch. It resolves all profiles and
model/thinking selections, then atomically reserves the cumulative spawn budget and a concurrency
pool (`maxParallelism`, default 3, range 1–20). Queued nodes consume planned spawns, not active slots.
All executions reuse ordinary isolated sessions, lineage, monitors, supervisor decisions and usage
attribution. Potential writers execute exclusively within this workflow's shared cwd; there is no
cross-workflow lock or worktree isolation.

Workflow context priority is explicit workflow `context`, node `context`, then profile default.
The shared launch-context Module applies the same implicit-fork fallback as a single subagent and
captures one fork point before the first child launches. Every fork node uses that fork point,
including later stages of a background workflow. Fresh nodes retain their own role/project prompt
but no parent transcript. Explicit `inputs: [{from, as}]` independently pass ancestor final text
as reference data, capped at 32 KiB per source and 64 KiB combined JSON. A truncated source fails
the consumer explicitly instead of silently passing partial evidence. Neither sequential stages nor
lanes implicitly inherit earlier child transcripts. Retained child-session resume, global
`defaultSubagentContext`, `context: "profile"`, and automatic fork pruning are not implemented.

The process-local controller advances ready nodes without a parent model turn. Node failure skips
descendants while independent lanes continue; startup failure cancels and drains already-started
siblings. `async: true` returns an owned aggregate receipt; default foreground calls await it, and
supervisor attention releases the parent wait. `subagent_supervisor status` and `bg_wait` expose the
group by default, with members queryable by their exact or unique-prefix run IDs. `cancel` aborts
an owned run or group and drains it; named group decisions include requests from its members.
Completion notifications are emitted for the group, not each member. Start and terminal snapshots
use existing Pi v4 `subagent_workflow` custom entries, including per-node context/fork point,
dependencies, states, session IDs, bounded output summaries and usage. Snapshots are observability
records, not executable recovery instructions. Parent usage receives each child's aggregate once;
workflow aggregate usage and node snapshots are display-only, with no additional `ToolResult.usage`.
Close/reload cancels live work; retries, detached runner processes, restart recovery and retained
session continuation are outside this contract.

Child prompt specialization follows pi-subagents' prompt layering: a child/fanout boundary and
escaped `<active_agent>` identity precede the role body, `replace` drops the ordinary Pi prompt,
and declared project-context inheritance plus the current working directory remain attached.
pi-rs keeps a private first-line HTML run marker in the task message to bind the independently
constructed child generation; that marker is a native execution-protocol divergence rather than
role guidance.

`SkillsPlugin` exposes a small prompt-projection Interface over its immutable generation catalog.
Normal sessions use the complete model-visible catalog; the subagent Adapter selects inherited and
explicit skills from the feature-owned launch record. Invocation-private `skillPath` roots are
resolved relative to the defining Markdown file, remain outside the parent catalog, and take
precedence only for explicitly selected names. The subagent hook runs before the skills hook so
`systemPromptMode: replace` replaces the base prompt without discarding the projected child skill
catalog. Skill-enabled profiles receive `read` only within the caller's existing capability ceiling;
missing names remain non-fatal result warnings.

A shared feature-owned runtime tracks logical lineage across independently rebuilt child
generations and enforces maximum nesting depth, cumulative spawns per root session, active-run
capacity, and each parent profile's nested-delegation permission. The global depth comes from
`PI_SUBAGENT_MAX_DEPTH`, then `<agent-dir>/extensions/subagent/config.json`, then the built-in
default. A profile's absolute `maxSubagentDepth` can only tighten the inherited lineage ceiling.
This is the
in-process equivalent of pi-subagents' child environment propagation rather than a child-process
environment contract. Invalid feature configuration fails candidate generation transactionally.
The launch record retains the resolved owned profile, so a file change between parent launch and
child binding cannot mix two definition generations. Profile specialization is applied by the plugin's generation hook;
`pi-core` and `pi-session` never interpret profile names or subagent policy. An authorized child
receives the same feature plugin and can launch its own isolated child within inherited limits. The
tool normally waits in the foreground and projects the child's final textual response into the
parent tool result; multiple calls use the existing parallel scheduler. On a blocking supervisor
request, all foreground subagent waits owned by that parent yield retained `detached` receipts so
the parent's tool batch can finish. The original child sessions remain alive. The feature-owned
`ChildRun` monitor, not the original tool invocation, owns completion, cancellation and the profile
`timeoutMs` deadline. That deadline remains a total run limit including supervisor waiting.
Feature-owned monitor handles are drained on session shutdown after cancellation is requested.
Monitors weakly reference their runtime to avoid owning their own task registry. Unwinding
or dropping a monitor publishes a terminal failure and releases capacity; ordinary wait cancellation
still leaves a detached child running. Only terminal completion releases active capacity. Role/skill assignments remain until session
shutdown so a later nested-child notification cannot promote its supervisor into a root agent.
A terminal child outcome includes aggregate billed usage from its independent session document.
The monitor records that usage exactly once as an immediate-parent usage adjustment, including
timeout and abort settlements; repeated terminal-receipt reads never re-attribute it. Nested child
adjustments therefore roll into their owning child's aggregate before that aggregate reaches the
next parent, making each session total subtree-inclusive without charging fork-seed history. The
tool result exposes the same child aggregate under `details.usage` for presentation, but deliberately
does not set `ToolResult.usage`, which repeated `bg_wait` calls could otherwise count more than once.
A failure to persist the parent adjustment remains visible as a result warning rather than silently
claiming complete accounting.
A separate session-plugin adapter clears lineage and drains live children whenever the owning
session shuts down, including reload. Live-run continuity across `/reload` is deliberately outside
the feature contract. The desktop projector recognizes the
feature-owned `isolatedSessionId` in subagent tool updates, observes that child read-only, and emits
the same semantic thread/item stream used for primary sessions. It projects parent-child links as
collaboration tool items so the existing desktop task hierarchy can render nested execution and
select a child for inspection. Parent token updates include attributed child usage, while each
collaboration node and isolated child thread exposes that child's aggregate total independently.
The TUI and other frontends continue to render only their explicitly
held primary session unless they opt into the observation seam.

Supervisor tools follow `nicobailon/pi-subagents` revision
`d3308745d2230d5aa94b9d6e643184adef3fd761` (`native-supervisor-channel.ts`, foreground execution,
and `subagent-wait.ts`). `contact_supervisor` is authorized by the live child assignment, not by
forked history or a caller-supplied destination. `need_decision` and `interview_request` wait for
a correlated reply; `progress_update` delivers immediately without requiring one. Interviews
parse plain or fenced JSON and report parse errors rather than claiming schema validation.
The reply deadline defaults to ten minutes (`PI_INTERCOM_ASK_TIMEOUT_MS`). Parent
`subagent_supervisor` supports `list`, `pending`, `status`, `reply`, and `cancel`. Status is an immediate,
owner-scoped snapshot with optional exact/unique-prefix `id`, execution-state counts, child IDs,
elapsed/deadline timestamps, pending request IDs and bounded result summaries.
`list`/`pending` retain request-list semantics. Feature-owned typed execution state is independent
from attention and detached-wait mode. Starting/running/cancelling are nonterminal; the monitor
publishes completed/failed/cancelled/timed_out exactly once, without parsing error text. A provider
error is failed, not a successful response without text. The reported run deadline uses the same
monitor-start instant as enforcement; launch preparation is not included in that timer. Queries do
not consume outcomes or trigger model work. Replies include exact `replyTo`
or unambiguous request-prefix/agent selection via `to`. Owner checks, expiry and one-shot delivery
reject foreign, stale and repeated replies. Request cancellation withdraws its pending entry.

`bg_wait` supports a run id/prefix, `all`, `timeoutMs` (default thirty minutes), and
`stopOnAttention`; supervisor requests always interrupt a wait even when the latter is false.
It snapshots the owned active set when no id is supplied; a named terminal receipt remains
queryable. Wait timeout or wait cancellation leaves the child running.

`bg_wait({id, nonBlocking:true, timeoutMs})` adds a plugin-local, one-shot observation of a
single detached run. An exact or unique-prefix id is resolved once; an omitted id or `all:true`
is rejected. A ready result or pending decision returns immediately without arming a timer.
Otherwise registration returns `armed`, the full `runId`, `deadlineAt`, and `reused`; another
active registration for the same owner/run reuses the original deadline rather than extending it.
Status exposes the active `nonBlockingWait.deadlineAt`. Completion or a decision request removes
the observation and uses the existing notification, with no additional subscription notification.
On observation expiry, the record is removed and a single best-effort `subagent-wait-expired`
reminder attempts to trigger the parent; the run state and execution deadline do not change.
The caller may rearm after expiry or after answering a decision. This mode observes only the named
run's decisions; ordinary blocking waits still yield for any owned decision to unblock tool batches.
The coordination Module atomically owns registration and each cancellable Tokio timer; timers
weakly reference coordination and use private identities to prevent stale expiry from consuming
a replacement. Completion, attention, run removal and owner cleanup cancel the timer along with
the record. There is no progress stream, polling loop, durable subscription or reload handoff.

Supervisor decision requests and detached completion notifications use the existing custom-message
adapter with `trigger_turn: true`; progress updates use `trigger_turn: false`. Delivery is a
best-effort, single attempt against the currently bound owner session and does not add storage or
acceptance semantics to `pi-session`. A failed notification never changes the run result or removes
a pending decision request: `subagent_supervisor status` and `pending` are the authoritative
fallbacks. Replies are journaled as `subagent_supervisor_reply` custom entries. Closing or reloading
the owner cancels its runs. Detached completion remains first-terminal-wins, and named terminal
receipts remain queryable until owner cleanup. The feature does not promise notification replay,
exactly-once delivery, or live-run handoff across reload.

The deliberate Rust divergence is an in-process mailbox/watch implementation instead of upstream
filesystem IPC. There is no standalone detached runner, process-restart reattachment, external
background-work provider, persistent wait subscription, or live-run continuity across reload.
Execution and state changes remain explicitly process-local. The usage rollup is a deliberate product enhancement over the current upstream
pi-subagents example, which reports child usage in result details but does not attribute it to the
parent session. It requires native ABI 18 but no Pi v4 record-shape change: attribution uses the
existing usage-adjustment entry. Coordination guidance is
run-local and capability-gated. The child launch plan injects coordination tools only within the
parent's active ceiling; exclusions and explicit `tools: []` keep the bridge off. Leaf children
receive `contact_supervisor`; nested coordinators may also receive the reply/wait tools. Parent
coordination calls/results and supervisor notices are removed only from inherited fork history.

Each `PiSession` has one replaceable current `AgentSession`. Its internal `AgentSessionRuntime`
serializes replacement, dispatches `session_before_switch` or `session_before_fork`, settles the
active agent, prepares the complete next session, emits old `session_shutdown`, emits new
`session_start`, and only then publishes the new generation through a Tokio watch channel.
Cancellation performs no preparation. Preparation failure leaves the current session open, while a
successful replacement closes stale `AgentSession` handles so they reject later mutations. New,
resume, reload, fork, and import all use this transaction. Fork creates a Pi v4 branch copy before
preparation and removes it if candidate preparation fails. Import first inspects the source as
native v4 or Pi coding-agent v1/v2/v3. Native files are validated and copied; legacy files are
converted into a newly created v4 destination while preserving source files, tree IDs and parents,
timestamps, parent-session paths, custom messages, compaction context, and unknown agent-message
extensions. v1 compaction indices become entry IDs, v2 hook messages become custom messages, and v3
retained tails are materialized explicitly. Every line is validated before publication; a failed
conversion or candidate generation removes the staged destination. Import deliberately uses Pi's
resume lifecycle reasons rather than adding an import-only session hook.

`pi-session` exposes the narrow storage and migration primitives used for portability:
`SessionLog::export_branch`, session-file inspection, and legacy import. The first-party
`SessionTransferPlugin` owns the complete `/export`, `/import`, and `/share` policy in every product
generation, including safe self-contained HTML serialization, destination selection, path
expansion, and GitHub CLI/viewer-URL behavior. Import validates and stages a unique v4 destination,
requests confirmation through the semantic `UiContext`, then reuses command-session `switch`;
cancellation or replacement failure removes the staged file. The CLI only renders the generic
confirmation request and never switches on those command names.

## Scheduled tasks

`pi-plugin-schedule` is a first-party feature over the same managed isolated-session capability.
`SchedulePlugin` registers the typed `schedule` tool and `/schedule` command;
`ScheduleSessionPlugin` owns a cancellable, generation-local worker activated only by
`session_start` and joined during `session_shutdown`. Both use independent factory registrations
in `pi-sdk`. Factories never start timers or open task storage. Failed preparation keeps the old
worker; successful reload cancels/records active work before retiring its context and starts a
fresh worker in the new `session_start`. Child launch waits for the manager's registration gate.
There is no scheduler binary, fourth plugin lifecycle, OS cron
installation, or scheduling policy in `pi-core`/`pi-session`.

Task definitions and execution records live together in the plugin-owned, versioned
`<agent-dir>/schedule/jobs.json` or trusted `<cwd>/.pi/schedule/jobs.json`. Project trust gates all
project schedule reads and writes. Global jobs still match the currently open session cwd.
One-shots, fixed intervals, and five-field cron with explicit IANA timezones are durable;
an idle primary session claims due work, while isolated origins cannot manage schedules or start
workers. Creation snapshots model/thinking/tools; request-time credentials and the host's current
capability ceiling remain authoritative. Every dispatch creates a fresh managed session, preserving
ordinary resource/plugin construction and lazy Pi v4 persistence without copying parent history.
Semantic notices carry results through the existing frontend stream; the plugin never writes stdout.

An OS metadata lock and atomic file replacement commit a consumed occurrence and its running record
before dispatch. A per-job OS lock spans execution and terminal recording across processes/windows.
Missed windows coalesce; claiming advances the next occurrence rather than replaying a backlog.
An orphaned attempt becomes `unknown` only after acquiring its run lock, and its effects are never
automatically replayed. Shutdown/timeout abort and drain a child before releasing the lock; an
unconfirmed cancellation pauses the job. Terminal history/output are bounded independently of the
normal isolated-session transcripts. All workers stop with their Pi processes. This is a deliberate
Rust product addition inspired by Hermes, using upstream Pi's resource-start/shutdown contract from
`legacy/pi/packages/coding-agent/docs/extensions.md`.

The generic isolated-session launch path owns cancellation until it returns a control handle.
Dropping a launch future while it awaits child readiness aborts the unclaimed prompt, including
when a scheduler's generation is shutting down. This is generic child ownership, not knowledge of
the scheduling plugin, and does not change native ABI or Pi v4 records.

## Event ordering

```text
agent_start
turn_start
message_start/end(user)
message_start/end(custom*)       `before_agent_start` registration order
message_start/update*/end(assistant)
tool_execution_start*      source order
tool_execution_update*     may interleave
tool_execution_end*        completion order
message_start/end(tool)     source order
turn_end
... next turn ...
agent_end
agent_settled                     session-owned; after all automatic continuation
```

Listeners execute and settle in subscription order. Low-level Agent idle state is published only after `agent_end` listeners finish. Product settled state is published only after every `agent_settled` plugin callback finishes; callback failures are diagnostic and do not suppress that event.

## Current validation

The workspace includes an end-to-end test where two delay tools complete in reverse order. It proves:

- provider and tools were plugin-registered;
- tool completion events use completion order;
- tool-result transcript entries use source order;
- the next provider request sees source-ordered tool results;
- Agent returns to idle.

## Next milestones

1. Add opt-in deferred redemption and safe-tool replay adapters without making session open perform
   provider I/O or replay unknown side effects.
2. Add publisher signatures, Git/OCI sources, update/rollback, and CAS garbage collection to the
   native package manager.
3. Continue current-Pi conformance at product seams with deterministic regressions before
   adding broader provider and terminal compatibility coverage.
