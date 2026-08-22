# Pi Rust core architecture

## Scope

The current product is a plugin-first Rust coding agent with a shared runtime behind interactive
TUI, print, and NDJSON modes. Its core implements the deterministic:

```text
prompt -> provider stream -> assistant message -> tool calls -> tool results -> next turn
```

HTTP providers, production tools, model/resource discovery, resumable sessions, compaction,
project trust, native dynamic-library loading, and terminal presentation are outer modules around
that core. A Node/NAPI host adds Pi-compatible JavaScript and TypeScript extensions without moving
terminal or session ownership out of Rust. Live operation replay and signed/OCI native-plugin
distribution remain open product seams.

## Workspace

```text
crates/pi-core                  contracts, registries, plugin drivers, ModelRuntime
crates/pi-agent                 Agent façade, AgentLoop, StreamAssembler, ToolScheduler
crates/pi-runtime               plugin registration and Agent construction
crates/pi-provider              vendor-neutral HTTP transport and SSE decoding
crates/pi-prompt                pure Pi-style system prompt assembly
crates/pi-resources             generic system/append prompts and project context discovery
crates/pi-session               Pi v4 session tree, storage backends, and runtime adapter
apps/pi-md                     TUI-owned Markdown parsing, streaming repair, highlighting, and Ratatui rendering
crates/pi-plugin-sdk            native plugin author interface and descriptor types
crates/pi-plugin-macros         agent/provider/session native export macros
crates/pi-plugin-loader         manifest discovery, compatibility checks, and factory adapters
crates/pi-plugin-manager        package intent/lock, Registry resolution, CAS, and activation
crates/pi-js-plugin             typed JS manifest protocol and three Rust lifecycle adapters
bindings/pi-napi                NAPI-RS boundary between Node callbacks and the Rust product
packages/pi                     Node launcher, Pi extension discovery, jiti loader, callback host
crates/pi-test-support          deterministic scripted providers and tools for tests
plugins/providers/pi-plugin-openai        OpenAI protocol, provider, registration, and examples
plugins/providers/pi-plugin-models        models.json catalog, routing, and request-time config
crates/pi-tool-support           shared path validation, argument, and truncation helpers
plugins/tools/pi-plugin-{read,write,edit,hashline-edit,bash,grep,find,ls}
                                one production tool per plugin crate
e2e/                            deterministic full-agent and ignored real-network E2E
apps/pi-cli/src/project_trust.rs product trust policy, persistence, and TUI request broker
```

Dependencies point inward:

```text
pi-agent             -> pi-core
pi-provider          -> pi-core
pi-prompt            -> standard library only
pi-resources         -> pi-prompt
pi-session           -> pi-core + pi-prompt + pi-resources + pi-runtime
pi-plugin-openai     -> pi-core + pi-provider
pi-tool-support      -> pi-core
production tools     -> pi-core + pi-tool-support
plugins/features/pi-plugin-skills
                     -> pi-core (skill discovery, prompt contribution, explicit invocation)
plugins/providers/pi-plugin-models
                     -> pi-core + pi-plugin-openai (credential-blind catalog and routing)
other plugins/*      -> pi-core
pi-runtime           -> pi-core + pi-agent + pi-prompt
apps/pi-cli          -> pi-md + product runtimes and plugins
apps/pi-md           -> Ratatui presentation dependencies only
pi-plugin-manager    -> HTTP + filesystem package source adapters
pi-js-plugin         -> pi-core + pi-session (no Node or terminal dependency)
bindings/pi-napi     -> pi-js-plugin + apps/pi-cli + NAPI-RS
packages/pi          -> Node + jiti + platform pi-napi artifact
```

`apps/pi-md` is a private frontend library module rather than a reusable core crate. It owns the
Ratatui-specific Markdown adapter used by `pi-cli`; no crate under `crates/` or plugin may depend on
it. Keeping parsing, streaming repair, highlighting, and rendering behind its small `render`
interface gives the TUI locality without moving terminal presentation into the core layers.

## Plugin-first rules

1. Tools and commands are registered through `AgentPlugin`. `ProviderPlugin` contributes provider implementations, routing overlays, model catalog entries, and provider request hooks. Agent code has no provider/tool name switches.
2. Agent plugin hooks, provider plugin registration, and provider request hooks each execute in builder order. There is no numeric priority.
3. Duplicate IDs are rejected within each plugin system; duplicate tool, command, provider, or model IDs fail runtime construction.
4. Registries are mutable only during registration and frozen before Agent construction.
5. `tool_call` runs in order, chains argument patches, and the first block decision wins. Hook errors fail closed for that tool call.
6. `tool_result` patches results in order. A hook failure converts that executed call to an error result.
7. `before_agent_start` runs once per prompt/continue invocation in registration order; prompt replacements chain and injected messages are accumulated for that run only.
8. `context` runs before every provider request and chains message replacements without mutating the persisted transcript.
9. `tool_call` chains argument patches and may block; patched arguments are revalidated. `tool_result` chains result patches. Legacy before/after tool hooks remain compatible.
10. Lifecycle events are delivered through independent plugin methods (`agent_start/end`, `turn_start/end`, `message_start/update/end`, and `tool_execution_start/update/end`) in registration order; observer errors do not fail the run.
11. A native plugin is trusted in-process code; the loader and trait interfaces are not a sandbox.
12. Registered slash commands own both their `CommandSpec` and execution. A `TransformInput` result then passes through `input` hooks in registration order before the agent run; `Handled` stops the submission. Text preprocessing retains both the product-facing submitted text and the effective model text rather than requiring a frontend to reverse an expansion.
13. `before_provider_request` runs after a concrete provider has serialized its final wire payload and before transport. Replacements chain in provider-plugin order; hook errors fail the provider request instead of sending a payload that skipped a requested mutation.

## Runtime generations and reload

`PiRuntime` keeps a reusable blueprint and publishes immutable runtime generations. A generation contains the agent and provider plugin drivers, the frozen `ModelRuntime`/registries, and the assembled base prompt that must move together. Agent plugins may contribute tools, commands, input processing, and lifecycle hooks; provider plugins have a narrow surface for provider/catalog registration and provider request lifecycle hooks.

`reload()` prepares the complete next generation off to the side, validates it against the current provider and active-tool selection, waits for the active run to settle, and then swaps one `Arc<AgentRuntime>`. A failed factory, duplicate registration, or incompatible provider/tool selection leaves the prior generation untouched. Each agent run captures one generation before invoking hooks or resolving providers and tools, so a run cannot observe a mixture of old and new plugin state.

Use `agent_plugin_factory` / `try_agent_plugin_factory` for reloadable agent plugins and `provider_plugin_factory` / `try_provider_plugin_factory` for providers, catalogs, routing overlays, and request hooks. Their pinned `agent_plugin` / `provider_plugin` and `*_arc` forms intentionally reuse an instance, primarily for stateless plugins and externally observed fixtures. `pi-plugin-loader` adapts version-locked dynamic libraries through the existing type-erased fallible factory seams and never mutates live registries in place.

Product wiring installs agent, provider, and session plugins through their three independent factory
seams. `AgentSessionRuntime` owns cross-system atomicity: its factory prepares the complete runtime
and session plugin generations before shutting down or replacing the current session.
`pi-plugin-loader` discovers global manifests and trusted project manifests, resolves explicit
`--plugin` paths, verifies a C-layout descriptor before resolving an exact-build Rust constructor,
and partitions packages into separately ordered agent, provider, and session factories. It snapshots
each dynamic library by content hash before loading, so rebuilt artifacts receive a distinct path
while unchanged content reuses one process-pinned handle. Libraries remain pinned for the process
lifetime because plugin code may retain worker threads. Package metadata and artifact lifetime are
loader concerns rather than a fourth lifecycle or cross-lifecycle bundle.

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

`AppSessionFactory` prepares global package state and, after trust resolution, trusted project
package state before native discovery. The manager holds a package-state guard and retains the
previous lock and activation view until the complete runtime and session generation prepares
successfully. Failed native loading or plugin initialization therefore rolls package activation
back together with the generation; success commits the prepared package state. The same factory is
used for initial sessions and `/reload`. The loader consumes only the local activation view and does
not know about networks, semver, registries, install commands, or package transactions. This
distribution layer does not introduce a fourth plugin lifecycle or mutable runtime registries.

The current static Registry is signed-data-ready transport only: SHA-256 proves downloaded content
integrity but not publisher identity. Publisher signatures, Git repository and OCI adapters,
package update/rollback commands, and store garbage collection remain explicit package-manager
milestones.

## NAPI-hosted Pi extensions

The JavaScript extension path deliberately has one product runtime, not a Node sidecar protocol and
not a fourth plugin lifecycle. Node is the executable launcher and JavaScript VM. It loads one
platform `.node` artifact; the NAPI export calls `pi_cli::run_with_js_host`, so terminal setup,
Ratatui rendering, trust, providers, tools, sessions, and product events remain owned by Rust in
`apps/pi-cli`. `crates/pi-js-plugin` contains only semantic wire values and adapters and therefore
has no NAPI, Node, Jiti, or terminal dependency.

`packages/pi` itself is authored in TypeScript, executed directly with `tsx` for development and
compiled by `tsc` into publishable JavaScript plus declarations under `packages/pi/dist`. Zod owns
runtime validation at the untyped Node seams: Rust host operations, generation manifests, dynamic
extension registrations/results, package extension declarations, and native binding exports.
TypeScript protocol types are inferred from those schemas so runtime checks and static interfaces
cannot drift independently.

The boundary uses four generation-scoped operations encoded as JSON: `prepareGeneration`,
`invoke`, `cancel`, and `retireGeneration`. `prepareGeneration` discovers trusted project
extensions, global extensions, and explicit `-e` paths in current Pi order, then loads TS/JS with
Jiti `moduleCache: false`. JavaScript functions stay in a Node-owned callback table; Rust stores
only opaque generation and callback IDs. `invoke` crosses a weak NAPI threadsafe function and
awaits the JavaScript Promise without blocking either the Node event loop or Tokio. Rust aborts send
`cancel`, which aborts the callback's `AbortController`; retirement aborts all remaining work and
drops every callback for that generation. The weak TSFN lets Node exit once the exported `runPi`
Promise settles.

One JavaScript source may contribute to all three systems, but the manifest partitions it into
separate agent, provider, and session plugin adapters. No `PluginBundle` is reintroduced. Manifest
validation rejects unknown hooks, duplicate lifecycle IDs, duplicate callback IDs, invalid tool
schemas, and later ordinary registry collisions before a candidate is published. Tool prompt
metadata and execution mode become ordinary `ToolSpec` fields; hook replacement and cancellation
semantics continue through the existing typed drivers.

`AppSessionFactory` asks the Node host for a fresh callback generation on initial construction,
new/resumed sessions, and `/reload`, alongside native plugins, resources, models, and session
plugins. The existing whole-session transaction prepares all of them before swap. A failed import,
factory, manifest, or registry build drops the candidate (retiring its Node callbacks) and keeps the
old Rust and JavaScript generations active. Executable JavaScript is never serialized into Pi v4
sessions.

This is compatibility by explicit capability, not an unsafe claim that every Pi TUI API is already
portable. The current bridge supports registered tools and commands, the Rust agent lifecycle
hooks, `before_provider_request`, and the ten session hooks. APIs that require a bidirectional UI,
renderer, dynamic-provider stream, resource contribution, or tool-progress channel throw during
registration/use rather than silently doing nothing. JavaScript extensions are trusted in-process
Node code and share the process and OS authority of the product.

`ModelsPlugin` is a provider plugin loaded after the base protocol provider. It loads one immutable, credential-blind `models.json` snapshot per generation, uses derived structural validation before compiling inheritance and overrides into runtime metadata, contributes `ModelSpec` values, and installs provider overlays. `models_json_schema()` exposes the same Rust definitions as JSON Schema for editors and standalone tooling. Environment variables, shell-command values, credentials, and configured headers resolve only when a request is sent. A failed parse, validation, or active-provider compatibility check prevents publication of the new generation, so `/reload` retains the complete prior provider/catalog pair.

Initial selection is a separate product policy in `pi-session`. `ModelRuntimeServices` adapts the
model portion of an assembled `PiRuntime` generation, while `InitialModelResolver` resolves an
explicit request, a restorable session model, the catalog default, or the runtime fallback in that
order. The resolver never reads `models.json`, resolves credentials, or registers providers. This
keeps file/routing mechanics inside `ModelsPlugin`, immutable catalog lookup inside `ModelRuntime`,
and new/resumed session policy above both. A removed session model falls back to the current catalog
with a diagnostic instead of silently restoring an unregistered route.

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
uses the decision to gate project `.pi` prompt files, project skill roots, and native plugin
manifests; future project settings and packages must consume this same service rather than add
local trust flags. As in Pi, `AGENTS.override.md`, `AGENTS.md`, and `CLAUDE.md` context discovery is
not gated by project trust, and trust is not a tool sandbox.

Filesystem tool paths follow Pi's `resolveToCwd` behavior. Relative paths resolve from the active
cwd, while absolute paths, `~` paths, `file://` URLs, and parent-relative paths may address files
outside it. Tool access is bounded by the process and operating-system permissions, not by a
`readable_roots` registry. The read tool additionally preserves Pi's macOS filename fallbacks for
Unicode normalization, screenshot AM/PM spacing, and curly apostrophes.

Startup resolves trust before constructing the first runtime generation. A session switch to a
different cwd sends a semantic trust request to the TUI and waits before constructing that
generation, so no project resource can be loaded before the decision. `/trust` updates persisted
policy for the current cwd; generation rebuild/restart applies the changed resource set.

`SkillsPlugin` is an example of the intended deep-plugin seam: it owns skill root configuration, discovery, frontmatter parsing, collision policy, catalog formatting, `/skill:name` command registration/expansion, and its generation-local diagnostics. Each registered `SkillCommand` owns its metadata and execution, so command discovery, duplicate validation, and dispatch share one source of truth. Generic resource loading and prompt assembly contain no skill-specific policy. The plugin contributes its catalog through `before_agent_start`, which makes a separate `PromptContributor` trait unnecessary.

## Core contracts

- `Provider`: accepts semantic `ProviderRequest` data plus a generation-local `ProviderCallContext`, invokes wire hooks when its final payload exists, and returns `Stream<Item = Result<StreamEvent, ProviderError>>`.
- `Tool`: publishes `ToolSpec` and executes validated JSON arguments with an `AbortSignal` and `ToolUpdateSink`.
- `AgentPlugin`: registers tools/commands and participates in input, lifecycle, context, and tool hooks.
- `ProviderPlugin`: registers providers, routing overlays, and model metadata and may implement `before_provider_request` without implementing a provider.
- `SessionPlugin`: participates only in session lifecycle hooks and is rebuilt by `SessionPlugins`.
- `PluginDriver`: is the only component that invokes plugin hooks.
- `ProviderPluginDriver`: validates, registers, and invokes the ordered provider plugin set for one runtime generation.
- `SessionPluginDriver`: validates and invokes the ordered session plugin set for one session generation.
- `ModelRuntime`: is the immutable, generation-local model catalog and provider resolver.
- `AbortHandle` / `AbortSignal`: provide cooperative cancellation without exposing Tokio types in public signatures.

## Agent layering

`Agent` is the stateful façade. It owns transcript state, active-run cancellation, steering/follow-up queues, subscriptions, and idle settlement. It is an `Arc`-backed cloneable handle so another task can call `abort`, `steer`, or `follow_up` while a prompt is running.

`AgentLoop` is a stateless single-run engine over an `AgentContext` snapshot. It emits lifecycle events, invokes a provider, delegates stream assembly and tool execution, polls steering after each turn, polls follow-up before settlement, and returns the final context plus messages added by that invocation.

`StreamAssembler` is the sole owner of provider-stream assembly:

```text
StreamEvent -> partial snapshots -> final AssistantMessage
```

It validates Start/Delta/End/Done transitions, preserves content-index order, and parses tool argument JSON only after the tool-call block ends.

`ToolScheduler` has three phases:

1. **Prepare, source ordered:** emit start, resolve tool, prepare/validate arguments, run `tool_call`, then revalidate patched arguments.
2. **Execute:** sequential globally or when any ready tool declares sequential; otherwise bounded parallel execution.
3. **Finalize:** run `tool_result`, emit end in completion order, then emit/persist tool-result messages in assistant source order.

Unknown, invalid, blocked, and truncated tool calls produce error tool-result messages rather than disappearing from provider history.

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

`validate_record_log()` and `reduce_lane_state()` mirror the newest Harness recovery reducer. They
reject contradictory operation, attempt, queue, tool, provisioned-entry, and deferred-handle logs,
then reconstruct pending input, deferred writes, unfinished steps, tool batches, effective
configuration, structural targets, overflow state, and terminal-failure provenance without mutating
storage.

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
pi_rs resume and queue recovery do not expose private expanded prompts. Frontends do not parse or
reverse plugin-specific prompt formats; messages written before this metadata existed display their
persisted content as-is.

`AgentSession::create` and `AgentSession::open` adapt the v4 tree to `PiRuntime`. Configuration
changes are v4 entries, completed messages are persisted on `message_end`, and pi_rs-only prompt
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
immediate compaction loop. Harness-level crash/deferred operation replay remains separate from this
live execution path.

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

`AgentSessionRuntime` is the multi-session product-runtime module above `AgentSession`. It owns the
current session and an injected `AgentSessionRuntimeFactory`, serializes replacement, dispatches
`session_before_switch`, settles the active agent, prepares the complete next session, emits old
`session_shutdown`, emits new `session_start`, and only then publishes the new handle through a
Tokio watch channel. Cancellation performs no preparation. Preparation failure leaves the current
session open, while successful replacement closes stale `AgentSession` handles so they reject
later mutations. `new_session`, `switch_session`, and whole-session `reload` all share this
transaction. Whole-session reload rebuilds runtime, provider, resource, feature, and session plugin
state through one factory seam; fork and import can reuse the same replacement transaction when
their storage operations are added.

## Event ordering

```text
agent_start
turn_start
message_start/end(user)
message_start/update*/end(assistant)
tool_execution_start*      source order
tool_execution_update*     may interleave
tool_execution_end*        completion order
message_start/end(tool)     source order
turn_end
... next turn ...
agent_end
```

Listeners execute and settle in subscription order. Agent idle state is published only after `agent_end` listeners finish.

## Current validation

The workspace includes an end-to-end test where two delay tools complete in reverse order. It proves:

- provider and tools were plugin-registered;
- tool completion events use completion order;
- tool-result transcript entries use source order;
- the next provider request sees source-ordered tool results;
- Agent returns to idle.

## Next milestones

1. Connect the recovery reducer to crash/deferred operation replay above the v4 session backend.
2. Add publisher signatures, Git/OCI sources, update/rollback, and CAS garbage collection to the
   native package manager.
3. Continue current-Pi conformance at product seams with deterministic regressions before
   adding broader provider and terminal compatibility coverage.
