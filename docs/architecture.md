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
crates/pi-prompt                pure Pi-style system prompt assembly
crates/pi-resources             generic system/append prompts and project context discovery
crates/pi-session               Pi v4 storage/runtime plus plugin contracts under plugin/ and types/
crates/pi-settings              current-format settings documents, snapshots, and safe writes
evals/pi-memory                 deterministic provider-only semantic-memory retrieval evaluation
crates/pi-telemetry             typed Pi AI/harness span schemas and sink adapters
crates/pi-rpc                   Pi JSON projector and stdin/stdout RPC adapter
crates/pi-mcp                   protocol-neutral MCP client, tool projection, and process ownership
crates/pi-acp                   official stable-v1 ACP adapter and ACP session policy
apps/pi-md                     TUI-owned Markdown parsing, streaming repair, highlighting, and Ratatui rendering
crates/pi-plugin-sdk            native plugin author interface and descriptor types
crates/pi-plugin-macros         static plugin preparation, agent hook-interest derivation, and native exports
crates/pi-plugin-loader         manifest discovery, compatibility checks, and factory adapters
crates/pi-memory-loader         memory.json loading and provider construction Interface
crates/pi-plugin-manager        package intent/lock, Registry resolution, CAS, and activation
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
plugins/features/pi-plugin-memory-hermes
                                default curated USER.md/MEMORY.md provider with frozen prompt snapshots
plugins/features/pi-plugin-memory-local
                                declared local Agent/Session plugin, SQLite/FTS/sqlite-vec storage, ranking,
                                FastEmbed lifecycle, tools, recall injection, maintenance, and rebuild policy
crates/pi-tool-support           shared path validation, argument, and truncation helpers
plugins/tools/pi-plugin-{read,write,edit,hashline-edit,bash,grep,find,ls}
                                one production tool per plugin crate
e2e/                            runtime acceptance plus deterministic black-box product E2E
apps/pi-cli/src/project_trust.rs product trust policy, persistence, and TUI request broker
```

Dependencies point inward:

```text
pi-agent             -> pi-core
pi-provider          -> pi-core
pi-prompt            -> standard library only
pi-resources         -> pi-prompt
pi-session           -> pi-core + pi-prompt + pi-resources + pi-runtime
pi-settings          -> serde JSON + filesystem persistence only
pi-memory-eval       -> pi-plugin-memory-local
                        (development-only corpus, adapters, runner, metrics, and reports)
pi-rpc               -> pi-agent + pi-core + pi-session
pi-mcp               -> pi-core + rmcp
pi-acp               -> pi-agent + pi-core + pi-mcp + pi-session + official ACP SDK
pi-plugin-openai     -> pi-core + pi-provider
pi-plugin-anthropic  -> pi-core + pi-provider
pi-plugin-xai        -> pi-core + pi-provider + pi-plugin-openai::responses
pi-plugin-google     -> pi-core + pi-provider
pi-tool-support      -> pi-core
production tools     -> pi-core + pi-tool-support
plugins/features/pi-plugin-skills
                     -> pi-core (skill discovery, prompt contribution, explicit invocation)
pi-memory-loader     -> pi-core + pi-session
                        (provider selection, opaque configuration, and construction)
plugins/features/pi-plugin-memory-hermes
                     -> pi-memory-loader + filesystem locking
                        (default bounded, file-backed curated memory provider)
plugins/features/pi-plugin-memory-local
                     -> pi-memory-loader + SQLite + version-pinned sqlite-vec
                        + FastEmbed + Hugging Face Hub
                        (bundled local provider implementation, provider-owned initialization,
                        factory, and maintenance policy)
plugins/providers/pi-plugin-models
                     -> pi-core + pi-plugin-openai (credential-blind catalog and routing)
other plugins/*      -> pi-core
pi-runtime           -> pi-core + pi-agent + pi-prompt
apps/pi-cli          -> pi-md + pi-rpc + pi-acp + product runtimes and plugins
apps/pi-md           -> Ratatui presentation dependencies only
pi-plugin-manager    -> HTTP + filesystem package source adapters
pi-js-package-manager -> filesystem + npm/git process adapters (no Node dependency)
pi-js-plugin         -> pi-core + pi-session (no Node or terminal dependency)
bindings/pi-napi     -> pi-js-plugin + apps/pi-cli + NAPI-RS
packages/pi          -> Node + jiti + platform pi-napi artifact
```

`apps/pi-md` is a private frontend library module rather than a reusable core crate. It owns the
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

The clipboard Module retains its small `ClipboardWriter::set_text` Interface. Its system Adapter
uses the native clipboard first for local sessions, PowerShell as the WSL fallback, and tmux then
bounded OSC 52 for terminal-mediated copying. SSH deliberately skips the remote native clipboard so
copied text reaches the user's local terminal.

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

All frontend adapters enter the product through two public session Modules. `MultiSessionManager`
owns the runtime factory, manager shutdown, and a private table of active handles. `PiSession` is the
cloneable per-frontend handle for current-session events and new/resume/fork/reload transitions.
There is deliberately no public `SessionRegistry`: duplicate-path checks and handle bookkeeping are
implementation details of `MultiSessionManager`. `AgentSessionRuntime` remains the lower-level replacement
transaction used inside each `PiSession`, rather than a type frontend adapters coordinate directly.
The print and NDJSON Adapters pin `PiSession::current()` for one invocation; the longer-lived TUI and
RPC adapters watch the handle's replacement stream. This keeps generation changes behind the same
Interface while preventing a single in-flight submission from crossing generations.

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
5. `tool_call` runs in order, chains argument patches, revalidates patched arguments, and lets the first block decision win. It is the intentional fail-closed exception: a hook error fails that tool call. Every typed `AgentPlugin` callback receives the same `Arc<AgentContext>` snapshot for the batch, including the current system prompt, transcript with the requesting assistant message, and active-tool names. The JavaScript extension Adapter projects Pi's narrower extension event and does not serialize this native context into Node.
6. `input` receives text, images, source, and optional streaming behavior. Text/image replacements chain in registration order, `Handled` stops the submission, and a hook error is recorded as a generation-local plugin diagnostic before later hooks continue.
7. `before_agent_start` runs once per prompt/continue invocation in registration order; prompt replacements chain and injected messages are accumulated for that run only. Hook errors are diagnosed and skipped without discarding earlier replacements.
8. `context` runs before every provider request and chains message replacements without mutating the persisted transcript. Hook errors are diagnosed and later hooks still run.
9. `tool_result` receives that same batch context and chains content, details, usage, `added_tool_names`, and error patches. Hook errors are diagnosed and skipped; they do not rewrite a successfully executed tool result into a failure. `added_tool_names` then survives the tool-result message and session/provider projection. Legacy before/after tool hooks remain compatible.
10. Lifecycle events are delivered through independent plugin methods (`agent_start/end/settled`, `turn_start/end`, `message_start/update/end`, and `tool_execution_start/update/end`) in registration order. `agent_start`/`agent_end` belong to each low-level run, while session orchestration emits `agent_settled` once no automatic retry, compaction, or queued continuation remains and before publishing the product settled event. Turns use a zero-based per-run index and `turn_start` also carries its millisecond timestamp. `message_end` may replace a message while preserving its role; each valid replacement becomes the next hook's input and the final message is used by Agent state, listeners, provider context, tool scheduling, and persistence. Observer errors and invalid cross-role replacements are diagnosed and skipped without failing the run.
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

Native ABI 15 replaces the core tool-run state container with `ToolContext::run_id()` and explicit
invocation-private Agent-plugin attachments. ABI 14 added optional detached compaction to
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
context patches affect only the private Agent. Tool blocking, argument revalidation, and result
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
`shutdown`, direct-completion, and ephemeral tool-loop controls. A direct completion carries an explicit system
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
ABIs before resolving any v15 Rust constructor symbol, preventing a stale in-process plugin from
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

`ProductSessionFactory` in `apps/pi-cli/src/session_factory.rs` is the production Adapter at the
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

## End-to-end validation

End-to-end validation is an outer Test Module and does not add a product runtime or a testing-only
construction path. Its small Interface is `runProductScenario(scenario) -> ProductRun` under
`e2e/product`. Scenarios declare product intent—a frontend adapter, input, deterministic provider
turns, and optional fixture/extension paths—and assert on returned product events, captured provider
requests, process output, and Pi v4 session data. They never reach through this
Interface to inspect registries, `ProductSessionFactory`, or plugin implementation state.

Two Adapters make the process seam concrete. `native-cli` starts the standalone Rust binary;
`node-napi` starts the compiled Node launcher and its selected NAPI binding. Both enter the same
production `pi-cli` assembly and NDJSON frontend. A private local OpenAI-compatible SSE Adapter is
the local-substitutable provider dependency. The harness also owns temporary HOME/agent/session
state, credential scrubbing, offline mode, process deadlines, exhaustive provider scripts, NDJSON
decoding, and cleanup. CI YAML supplies toolchains and invokes this Interface; scenario and
transport policy do not live in workflow steps.

The in-process Rust test in `e2e/tests/runtime_agent.rs` remains a runtime acceptance test. It gives
fast, precise coverage of prompt assembly, plugin hooks, production filesystem tools, agent loops,
settlement, and session persistence, but it is not labeled as black-box product coverage because it
bypasses argument parsing and `ProductSessionFactory`. Focused Node/NAPI bridge tests likewise stay
below the product-E2E seam. The deterministic product suite starts both real process Adapters on
every pull request and uses no external network or credentials. Real-provider checks are a separate
future opt-in layer, and fullscreen interaction requires a future PTY Adapter rather than terminal
logic in the generic harness.

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
the repository root. The user-level `~/.agents/skills` root is always trusted. The current runtime
uses the decision to gate project `.pi` prompt files, project skill roots, project JavaScript
settings/packages/extensions, and native plugin manifests. The Rust JavaScript PackageManager is
called only after this decision and does not introduce a second trust store or prompt. As in Pi,
`AGENTS.override.md`, `AGENTS.md`, and `CLAUDE.md` context discovery is not gated by project trust,
and trust is not a tool sandbox.

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
`prepare_next_turn` may replace only the run-local context/provider/model/thinking values, then
`should_stop_after_turn` runs, and only then does queue polling occur. These replacements never
mutate the reusable Agent configuration. `FnTurnControl` is the closure Adapter for this single
Interface; it hides async future boxing rather than introducing a second callback lifecycle. Turn
callbacks receive `Arc`-backed immutable snapshots, while the live loop mutates its context and
per-run transcript with copy-on-write. The normal sequential callback path therefore shares the
same snapshot without cloning the transcript; retaining a snapshot beyond its callback is safe but
may make a later mutation clone the retained data.

Each assistant stream owns one mutable assembler state behind a read-only `AssistantStream` view.
`message_update` carries that constant-size shared handle and exactly one `StreamEvent`; the Agent
reducer, ordered native hooks, and listeners therefore do not clone cumulative content. Consumers
that require a full message call `snapshot()` explicitly, while `message_end` and `turn_end` share
the completed immutable assistant message. These hook fields were introduced in native ABI 7;
ABI 8 adds UI confirmation, ABI 9 adds isolated-session control, ABI 10 adds isolated-session
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

`AgentSession::create` and `AgentSession::open` adapt the v4 tree to `PiRuntime`. Configuration
changes are v4 entries, completed messages are persisted on `message_end`, and pi-rs-only prompt
snapshots/resource diagnostics use reserved `customType` values rather than extending the v4 entry
union. Opening restores data state only; executable plugins and resources always come from the
supplied runtime. Checkout, branch summaries, and compaction rebuild runtime context immediately.
`prepare_create` and `prepare_open` expose the same construction as a `PreparedAgentSession` whose
`session_start` event is deferred, allowing a host to order replacement lifecycle events correctly.

`pi-session::compaction` ports the Harness `retainedTail` algorithm: provider usage plus trailing
heuristics estimate context size, cut points never begin at a tool result, oversized turns receive a
separate prefix summary, previous summaries update incrementally, and read/modified file metadata is
carried forward. Summary calls use `PiRuntime::complete`, an isolated provider request with no tools
that does not mutate the agent transcript. `AgentSession::compact` performs manual compaction;
`AgentSessionOptions::context_window` enables threshold compaction before/after runs. Recoverable
context overflow removes the failed assistant from the prepared context, compacts, and retries once.
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
`AgentSessionRuntimeFactory`, serializes manager-level acquisition and shutdown, and keeps its
active-session map private. Opening an already-active path reuses its `PiSession`; creating or
switching to a path owned by another handle fails before any session lifecycle transition starts.
Manager shutdown drains and closes every managed handle.

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

Review scheduling is plugin-owned and session-local. User messages increment the memory counter
(default 10; resumed user count is hydrated from the active branch). Model iterations separately
increment the skill counter (default 10), reset by skill_manage use. A successful final response
at `agent_settled` combines due flags into one detached review. There is at most one review per
parent. A new foreground request, compaction, reload, or shutdown cancels it; foreground waits at
most two seconds for acknowledgment.
Managed children have `SessionExecutionOrigin::Subagent`, preserved across reload and nested
delegation. Hermes skips their review counters, autonomous reviews, and opt-in lifecycle flushes,
equivalent to the upstream `skip_background_review` behavior. They still receive the memory
snapshot and can explicitly use the normal memory/skill tools. This gate belongs to the memory
plugin; generic session orchestration only supplies provenance.

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
inherits effective system prompt/model/thinking/provider authentication, and replays structured
parent history. A different configured review model uses an older-text digest plus at least 24
recent messages, keeping tool-call/result groups together. It shares advertised tool definitions
for cache parity but checks an execution allowlist before argument preparation and execution.
The default allowlist is memory, skill_manage, skill_view, skills_list and the available Pi file
read/search tools. `reviewExtraTools` can opt in other tools only within the parent's active-tool
ceiling. Shell and general file writes are not executable by default. Tools receive no mutable
parent-session, model-control, UI, or child-launch capabilities. This is an invocation boundary,
not a sandbox for trusted native code.

Review uses 16 model/tool iterations, a default aggregate 600,000 input-token budget, and a
120-second timeout. It creates no managed session or JSONL file, emits no parent agent/session
hooks, and cannot recursively schedule a review. Only successful mutation receipts are reported,
not assistant claims. The reusable direct-completion API remains available to other plugins; it is
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
No session database, journal, rotation, or lifecycle hook participates. If model-window metadata
is unavailable, compaction stays off and the review retains its iteration/input/time bounds.

Skills are active procedural memory: review may create class-level skills and improve existing
ones using verified procedures, corrections, and pitfalls. Skill-tool creations receive curator
metadata. Background edits require agent provenance, an unchanged content hash, an unpinned skill,
and a successful read of the exact existing file during that review. User-owned and externally
changed skills fail closed. Supporting files use relative, non-symlink paths. Autonomous deletion
requires an existing `absorbed_into` skill and archives the source outside all skill roots.
Pi-native skill roots and immutable catalog ownership stay with `SkillsPlugin`; reload is required
before newly created skills appear as `/skill:` commands.

Existing project/failure notes, FTS/session search, manual memory-management commands, and
optional standing instructions are retained as Rust product extensions, not claims of Hermes
Agent command/storage parity. No existing user memory or skill files are deleted by the change.
`/memory-preview-context` shows the actual frozen prompt. Explicit
`flushOnCompact`/`flushOnShutdown` enable extra best-effort review at lifecycle boundaries
(minimum six user requests; 30/10-second bounds); both default off. The old correction regex and
automatic capacity-consolidation Agent are removed.

Detached review compaction, managed-child review suppression, and per-invocation consolidation
failure limits have deterministic regressions, including the complete review/read/summary/
memory-write handoff and unchanged parent JSONL. Private plugin tests cover shared hook/tool run
identity, no parent hooks or registration, fail-closed validation, concurrent review isolation,
fresh read witnesses, and state release across completion/error/cancel/timeout/drop/reload.
Prompts, JSON configuration, Pi-native skill
metadata/catalogs, and filesystem conflict protection remain Rust adaptations. The compressor
implements the default window/retention profile and detached lifecycle, not every upstream
compressor tuning option or recovery heuristic. This is not byte-for-byte or complete Hermes parity.

The older local semantic provider is retained as an explicitly selected `"local"` provider for
vector/FTS recall use cases. It is not the default and does not claim Hermes compatibility.
`memory.json` selects exactly one provider; the loader never combines their tool contracts or
storage semantics.

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

`pi-plugin-memory-local` is the deep bundled Provider Module. Its external construction Interface is
`LocalMemoryProviderFactory`, while its public `LocalMemoryPlugin` directly implements both
`AgentPlugin` and `SessionPlugin`. The local implementation owns the `memory` and `session_search`
tools, tool schemas and write policy, local `MemoryRecord`/mutation/query types, transient context
recall, per-run recall caching, start/compact/tree/shutdown reconciliation, maintenance commands,
and on-device storage. Its `LocalMemoryProvider` implementation hides SQLite/FTS persistence,
sqlite-vec dense retrieval, hybrid ranking, session indexing, embedding validation, and the pinned
FastEmbed asset lifecycle. It stores a local database at
`<agent-dir>/memory/memory.sqlite3`, uses WAL plus FTS5, sets the directory to `0700` and the
database, WAL, and SHM files to `0600` on Unix, and opens a fresh connection in a blocking
worker for each operation. SQLite, ranking, or embedding internals can change without changing the
factory or lifecycle Interface. A different Provider remains free to register different tools and
implement different lifecycle policy.

The crate layout follows those responsibilities: `factory.rs` is the sole construction Adapter,
`plugin.rs` owns agent/session hooks, `commands/` and `tools/` hide their concrete registrations,
`embedding/` owns model acquisition and initialization, and `storage/` owns SQLite plus sqlite-vec.
The pure ranking Module remains separate from storage mechanics. Provider initialization types are
crate-private; the root exports only the product factory/plugin and concrete types required by the
evaluation Adapter.

Non-empty SQLite recall keeps four concrete ranking modes inside the local Provider Module. The
product-default lightweight Hybrid mode asks FTS5/BM25 for 32–100 active,
scope-filtered candidates; an internal ranking Module combines candidate-local term rarity, term
coverage, phrase order, contiguous spans, code atoms, and reciprocal sparse rank. It does not infer
query intent from language-specific keyword vocabularies or use `MemoryKind` as a lexical ranking
prior. Greedy diversity promotes complementary evidence over near-duplicate records, and a relative
confidence cutoff may return fewer than the requested maximum. `SqliteRecallRanking::Bm25` remains
the deterministic sparse control. `SparseDenseRawRrf` is a historical evaluation control that
deliberately omits confidence, cutoff, and diversity; it is selectable only through a concrete
SQLite Adapter constructor and is never product construction policy.

The optional `SparseDenseRrf` mode accepts one injected `MemoryEmbedder` at an internal seam and
stores compact float32 vectors in a sqlite-vec `vec0` table. Its immutable
model/revision/dimension descriptor owns the embedding space; a descriptor change atomically
recreates the derived vector table. Cosine KNN applies `scope_key` metadata filters before its
bounded candidate limit, while materialization repeats the active/scope predicate as defense in
depth. The ranking Module first runs the ordinary lexical diversity/cutoff path to form a protected
core. It then scores the BM25/dense union with lexical structure, normalized cosine similarity,
and a bounded reciprocal-rank contribution; raw BM25 and distance values are never directly
mixed. A dense rescue must add substantive query evidence not covered by the core, or strongly
repeat at least three substantive query terms, before it enters the same seeded diversity and
relative-cutoff policy. Within the rescue pool, a candidate that adds previously uncovered
substantive evidence and expresses it as an adjacent query phrase or exact query code atom is
selected before score-only repeated evidence; that coherent complement survives the relative
cutoff that still applies to ordinary rescues. This prevents redundant subsystem summaries from
consuming every multi-hop slot without globally lowering the cutoff. The substantive-token rule is
shape based (CJK, digit-bearing, or at least four characters), not an intent vocabulary or a
`MemoryKind` prior. Tombstones and supersession remove vectors regardless of replay order.
Query/model failures return the ordinary lexical result, write-time model failures leave an active
record pending rather than weakening the canonical write, and bounded backfill performs model work
outside SQLite transactions. Empty-query recency and LIKE fallback behavior are unchanged.

The local-provider Module privately owns sqlite-vec's required C entry-point conversion.
Registration is process-wide, checked, and memoized. Unsafe code is denied crate-wide and allowed
only in that single audited registration function; the module is not exported. Embeddings, model
state, ranks, and backfill state remain derived provider data rather than canonical `MemoryRecord`
fields.

The same local Provider Module owns the concrete FastEmbed Adapter, its asset lifecycle, and its
`LocalMemoryProviderInitializer`. `LocalMemoryProviderFactory` parses only `providers.local`,
derives the database and model paths, awaits storage/model initialization, and returns the declared
`LocalMemoryPlugin`; that plugin attaches the provider-specific `/memory-local-*` maintenance
commands and owns its lifecycle implementation. An initialization error rejects the candidate generation and
leaves the previous generation intact. The initial embedding space is
`intfloat/multilingual-e5-small` at one immutable Hugging Face commit, 384
dimensions, mean pooling, normalized FastEmbed output, and versioned E5 `query:` / `passage:`
prefix policy. Model identity includes both the upstream commit and Pi Adapter-policy revision, so a
change that would invalidate stored document vectors recreates the derived vector index. FastEmbed
is opened from provider-verified local bytes rather than its automatic Hub constructor; `HF_HOME`
cannot redirect ordinary inference, recall, or writes.

The Memory Loader owns only the outer shape of `<agent-dir>/memory.json`; generic `settings.json`
parsing neither decodes nor merges Memory policy, and project settings cannot redirect durable
memory. The versioned document selects the provider, recall budgets, and provider-specific
configuration under `providers.<id>`. Each value in `providers` remains opaque JSON to the host.
Only the selected subtree is passed to its factory; inactive provider configurations are neither
decoded nor exposed to another provider. The provider developer owns that subtree's complete
schema, defaults, paths, credentials, model acquisition, and validation policy.
Missing configuration safely defaults to the Hermes curated provider. Selecting the local Provider
with no `providers.local` configuration uses its `offline` initialization; an
invalid existing document fails generation preparation so the previous generation remains
published. For the local Provider, `providers.local.initialization` selects `offline` or
`automatic`. Offline initialization creates and validates local storage and activates dense recall
only when a complete verified model is already present; it never accesses the network. Automatic
initialization lets the Provider download missing pinned assets, load the model, create the vector
index, and backfill existing active records before the generation is published. Both paths verify
exact sizes and SHA-256 digests and atomically publish a versioned completion marker under a
cross-process file lock. `HF_ENDPOINT` may select a Hub mirror, but not a different model revision.
The explicit `/memory-local-model-install` operation remains available: it prepares and backfills a dense
Adapter, then performs the normal whole-generation reload. Installed model sessions are shared
between overlapping generations in-process, avoiding a second 470 MB ONNX load during the atomic
swap. `/memory-local-model-backfill` repairs pending rows, and `/memory-local-rebuild` backfills after replacing
the canonical derived index.

The Pi v4 JSONL session remains the source of truth. A curated write validates a compact
`MemoryRecord`, appends a `pi.memory.v1` custom entry, and only then applies that mutation to the
derived database. If indexing fails after the append, the tool reports the durable write as
pending rather than inventing a second commit point; lifecycle reconciliation replays JSONL by
mutation id. `remember` records and `forget` tombstones are immutable. A correction is a new record
whose `supersedes` points at the old id, so replay from multiple sessions is commutative and does
not depend on a global sequence number. Active queries exclude tombstoned and superseded ids.
Reusing one mutation id with a different payload is rejected as journal corruption rather than
silently accepting whichever session happened to reconcile first.

SQLite maintenance remains a concrete local capability behind `LocalMemoryPlugin` rather than a
requirement imposed on every provider. `/memory-local-status`
reports schema, SQLite quick-check state, row counts, and index size; `/memory-local-list` and
`/memory-local-search` expose active records with provenance; `/memory-local-rebuild` replaces all derived rows
from the configured v4 session directories. A rebuild acquires an immediate SQLite write
transaction before reading its sources, uses the non-repairing `SessionLog::read` path so an
actively appended torn tail is ignored rather than truncated, and commits the complete replacement
atomically. Concurrent curated writes append JSONL first and then wait behind the transaction, so
they cannot be cleared by the replacement. Legacy v1-v3 files are skipped until explicitly imported
through the existing session-import seam; invalid v4 files and duplicate session ids fail the whole
rebuild and preserve the old index. If SQLite identifies the database as corrupt or not a database
during generation construction, the file and any sidecars are preserved under a `.corrupt-*` name
before a clean derived database is initialized; status directs the user to rebuild it from JSONL.

The local Provider's `MemoryRecord` V1 contains id, exactly one user/project/session scope, kind,
text, origin, required
evidence, record time, and optional supersession. UUIDv7 identities make retries and reconciliation
explicit. Embeddings, access counters, ranks, and future confidence/valid-time fields are derived
provider data rather than canonical fields. Automatic capture is intentionally absent: the model
can write only through the visible `memory` tool, whose contract forbids credentials and transient
task details.

Recall runs from the generation `context` hook with a bounded record count, approximate token
budget, and timeout. Results are cached per agent run, rendered as one hidden custom message before
the latest real user message, and never appended to JSONL. The wrapper says that current user
instructions win and quoted memory cannot become a system/tool command. `session_search` indexes
only user and assistant text from each current active branch; tool results are excluded because
they commonly contain source files, credentials, or very large payloads. Replacing a session index
therefore removes abandoned branch text. Project search is rooted at the nearest Git ancestor (or
the canonical cwd when no Git root exists), while the database stays outside the project.

`pi-memory-eval` is a development-only Module with its own narrow seam. Its `EvalBackend`
Interface has one operation and receives only query text, scopes, and a result limit; question ids,
ability labels, gold evidence, forbidden records, and expected answers remain runner-owned. The
local production Adapter delegates to `LocalMemoryProvider::recall`, while no-recall and
deliberately gold-privileged Oracle Adapters provide lower and upper bounds. The traced SQLite
evaluation Adapter uses
the concrete `recall_with_candidates` capability to return rank-ordered sparse/dense identities
alongside the unchanged final result. For the product sparse/dense policy it additionally records
protected-core, gate-eligible, and pre-cutoff identities; ordinary product recall does not collect
any of those ids. The bundled corpus replays ordinary `MemoryMutation` values and scores
evidence-hop Recall@1/5/8, all-hop success, MRR, scope/stale/distractor risk, evidence density,
timeout rate, and latency percentiles. Report schema v5 also groups retrieval metrics by language
relation and hop shape, compares gold coverage in the sparse, dense, and deduplicated candidate
union, and aggregates coverage at each final-ranking boundary. Gold comparison happens only after
`EvalBackend::gather` returns. Language relation, split membership, and every other gold field
remain runner-owned and never cross the `EvalBackend` seam.

Corpus schema v2 separates haystacks from evaluation suites: a suite references one haystack and an
explicit ordered question-id set. The fixed-seed `small` haystack expands seven curated sessions to
100 sessions and 306 records. Corpus version `1.3.0-holdout-v2` contains 45 unique questions:
`small-dev`, the first frozen holdout, and a second pre-run frozen holdout select 15 questions each
over those exact records. The fixed-seed `medium` haystack retains the curated needles and expands
to 500 sessions, 1,506 records, and 1,507 replay mutations; its dev and two holdout suites reuse the
corresponding small-tier question ids. Paired small/medium reports therefore isolate scale pressure,
while only a never-before-run question split can measure unseen-query generalization. Fixture tests
enforce shared mutations, ordered v2 question identity, ability/language/hop composition, Oracle
upper bounds, and query-input uniqueness. This prevents split names from changing generated filler
and makes lexical tuning failures visible on separately worded cross-language queries. Checked BM25
and lightweight-hybrid baselines omit machine-specific latency while runtime reports retain it.
This is a `pi-rs` provider benchmark inspired by LongMemEval-V2, not an official
trajectory-benchmark score and not a reason to widen the production memory Interface.

The explicit `sqlite-dense` evaluation Adapter loads the same pinned FastEmbed model and exercises
the production protected-lexical/dense-rescue path through `LocalMemoryProvider::recall`; it never
installs or downloads model assets. Its cache path is mandatory, haystack embedding occurs before timed
queries, and the real-model regression is ignored by ordinary workspace tests. This keeps
deterministic lexical controls and offline CI independent from a 470 MB asset while making the
model-backed path reproducible on machines where that exact revision was installed.
`sqlite-dense-raw-rrf` uses the same concrete Adapter and model but selects the historical
equal-weight RRF control. The choice stays outside `LocalMemoryPlugin`, keeping evaluation
ablations from becoming product plugin policy.

The plugin-facing custom-entry capability permits an atomic extension-state append while the Agent
itself owns the prompt operation. `SessionLog` still assigns the shared mutation sequence; busy
compaction, switch, fork, and tree operations remain rejected. This lets a tool establish the JSONL
commit before updating a derived provider without weakening other session mutation gates.

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
than deriving presentation ownership from the manager inventory. Isolated files live below the
owning session's sibling directory and therefore do not enter the top-level resume listing. Closing
an owner closes its registered isolated descendants. Fork seeding, detached-run reattachment, and
background status remain later layers rather than widening this interface.

`pi-plugin-subagents` is the first policy module over that seam. It registers one parallel-safe
`subagent` tool with focused `scout`, `worker`, `reviewer`, `oracle`, and `delegate` built-ins. Its
generation-local agent catalog overlays those built-ins with recursive Markdown discovery from the
global agent directory's `agents/` subtree and the nearest trusted project `.pi/agents/` subtree;
project definitions win name collisions. Definition frontmatter owns `name`, `description`,
`aliases`, `systemPromptMode`, `allowNestedSubagents`, `maxSubagentDepth`, `tools`, `excludeTools`,
`model`, `thinking`, `inheritSkills`, `skills`, `skillPath`, and `timeoutMs`,
while the Markdown body owns the role system prompt. Omitted selections inherit from the immediate
parent; an empty `tools` field selects no tools; an explicit list is a strict allowlist under the
inherited ceiling, and exclusions can only narrow the resolved set. Canonical names beat aliases;
alias-to-alias ambiguity fails candidate generation. Bare model ids prefer the current provider and otherwise require a unique
available catalogue match; thinking levels are checked against the resolved model before launch.
Reload rescans the catalog transactionally, and untrusted project definitions never enter the
candidate generation.

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
default. A profile's absolute `maxSubagentDepth` can only tighten the inherited lineage ceiling;
already-launched children retain that ceiling when a root generation reloads. This is the
in-process equivalent of pi-subagents' child environment propagation rather than a child-process
environment contract. Invalid feature configuration fails candidate generation transactionally.
The launch record retains the resolved owned profile, so a file change between parent launch and
child binding cannot mix two definition generations. Profile specialization is applied by the plugin's generation hook;
`pi-core` and `pi-session` never interpret profile names or subagent policy. An authorized child
receives the same feature plugin and can launch its own isolated child within inherited limits. The
current tool waits in the foreground and projects the child's final textual response into the parent
tool result; multiple tool calls emitted in one assistant turn use the existing parallel tool
scheduler. A positive profile `timeoutMs` bounds that wait, aborts the isolated handle on expiry,
and returns a terminal timed-out result without widening the generic isolated-session Interface.
A separate session-plugin adapter clears lineage on quit or logical-session replacement
while retaining cumulative state across a generation reload. Frontends continue to render their
explicitly held primary session.

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
