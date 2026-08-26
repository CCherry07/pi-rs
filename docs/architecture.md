# Pi Rust core architecture

## Scope

The current product is a plugin-first Rust coding agent with one `MultiSessionManager` / `PiSession`
Interface behind interactive TUI, print, and NDJSON modes. Both the standalone binary and the Node
extension host delegate interactive terminal ownership to the Ratatui frontend in `apps/pi-cli`.
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
crates/pi-core                  contracts, registries, plugin drivers, ModelRuntime
crates/pi-agent                 Agent façade, AgentLoop, StreamAssembler, ToolScheduler
crates/pi-runtime               plugin registration and Agent construction
crates/pi-provider              vendor-neutral HTTP transport and SSE framing
crates/pi-prompt                pure Pi-style system prompt assembly
crates/pi-resources             generic system/append prompts and project context discovery
crates/pi-session               Pi v4 storage plus MultiSessionManager/PiSession product runtime
crates/pi-telemetry             typed Pi AI/harness span schemas and sink adapters
apps/pi-md                     TUI-owned Markdown parsing, streaming repair, highlighting, and Ratatui rendering
crates/pi-plugin-sdk            native plugin author interface and descriptor types
crates/pi-plugin-macros         static plugin preparation, agent hook-interest derivation, and native exports
crates/pi-plugin-loader         manifest discovery, compatibility checks, and factory adapters
crates/pi-plugin-manager        package intent/lock, Registry resolution, CAS, and activation
crates/pi-js-package-manager     Pi-compatible JS discovery and npm/git orchestration
crates/pi-js-plugin             typed JS manifest protocol and three Rust lifecycle adapters
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
pi-plugin-openai     -> pi-core + pi-provider
pi-plugin-anthropic  -> pi-core + pi-provider
pi-plugin-xai        -> pi-core + pi-provider + pi-plugin-openai::responses
pi-plugin-google     -> pi-core + pi-provider
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
and invokes the NAPI `runPi` entry; interactive, print, JSON, piped-input, and plugin-management
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
The print and NDJSON Adapters pin `PiSession::current()` for one invocation; the longer-lived TUI
also watches the handle's replacement stream. This keeps generation changes behind the same
Interface while preventing a single in-flight submission from crossing generations.

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

Native ABI 3 adds the required `AgentPlugin` hook-interest contract to the ABI 2
`AgentContext`/`added_tool_names` surface. The native agent export macro derives that contract from
the callback methods in the annotated impl, so authors do not maintain a second hook list. The
loader reads the stable C descriptor first and rejects older ABIs before resolving any v3 Rust
constructor symbol, preventing a stale in-process plugin from crossing the changed trait boundary.

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

- GitHub Release archives contain the standalone `pi` binary. They support TUI, print, NDJSON, and
  native plugins, but no JavaScript VM or JS/TS extensions.
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
platform `.node` artifact and invokes `runPi` for Ratatui, print, JSON, piped-input, and management
modes. Extension callback generations remain in Node; provider, tool, trust, session, and terminal
authority remain in Rust. `crates/pi-js-plugin` contains only semantic wire values and adapters and
therefore has no NAPI, Node, Jiti, or terminal dependency.

`packages/pi` itself is authored in TypeScript, executed directly with `tsx` for development and
compiled by `tsc` into publishable JavaScript plus declarations under `packages/pi/dist`. Zod owns
runtime validation at the untyped Node seams: Rust host operations, generation manifests, dynamic
extension registrations/results, and native binding exports.
TypeScript protocol types are inferred from those schemas so runtime checks and static interfaces
cannot drift independently.

The callback boundary uses four generation-scoped operations encoded as JSON: `prepareGeneration`,
`invoke`, `cancel`, and `retireGeneration`. Every `invoke` also receives a NAPI class instance named
`NativeExtensionContext`; it is a direct native capability rather than another serialized host
operation or a process-global callback broker. Its deliberately small Interface has three methods:
`query` for synchronous reads, `notify` for non-blocking commands, and `request` for awaited
commands. The operation payloads and results are JSON, but the capability object itself is passed
as the second threadsafe-function argument. Before `prepareGeneration`, `ProductSessionFactory`
calls the deep Rust Module `pi-js-package-manager` through its
`resolve(request) -> resolution` Interface. Its side-effect-free
`requires_javascript_host() -> bool` query is also used by the native-only startup Adapter after
construction from the same request.
The Module merges explicit `-e` local/npm/git sources first,
then trusted project settings entries, trusted project auto-discovery, user settings entries, user
auto-discovery, and configured package resources in current Pi precedence. Package manifests and
filters, ignore files, canonical-path deduplication, managed npm/git installation, custom
`npmCommand`, and `PI_OFFLINE` are hidden behind that Interface. The Node Adapter receives only the
ordered `extensionPaths` load list and loads TS/JS with Jiti `moduleCache: false`; it has no settings,
source, installation, filtering, or precedence policy. JavaScript functions stay in a Node-owned
callback table; Rust stores only opaque generation and callback IDs. `invoke` crosses a weak NAPI
threadsafe function and awaits the JavaScript Promise without blocking either the Node event loop or Tokio.
Rust aborts send `cancel`, which aborts the callback's `AbortController`; retirement aborts all
remaining work and drops every callback for that generation. The native context is guarded by the
same generation epoch, so a context retained by extension code fails with a retired-context error
after its generation is gone. `ExtensionSessionBinding` connects each prepared generation to its
concrete `AgentSession` before `session_start`, then to the stable outer `PiSession` after initial
startup. Reads therefore remain generation-correct during activation and follow successful
new/resume/fork/reload replacements afterward. Both links are non-owning (`Weak<AgentSession>` and
`WeakPiSession`), because the concrete session owns the plugin generation; a strong context-to-
session edge would form a cycle and prevent generation retirement. The weak TSFN lets Node exit
once the exported `runPi` Promise settles.

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
hooks, `before_provider_request`, the ten session hooks, read-only session/model context, and the
command-safe session operations described above. UI is an intentional product divergence: every
JavaScript context reports `hasUI = false` and exposes one explicit inert UI object with
Pi-compatible default return values. Its `notify` method crosses the native context as a transient
`AgentSessionEvent::ExtensionNotice`; each Rust frontend owns presentation, and nothing is persisted
to the v4 session log. UI registrations, renderers, flags, dynamic providers, resource discovery,
and other recognized-but-inactive facilities do not fail generation construction; they produce an
`inactive` generation diagnostic and contribute no runtime callback. A hook name known to current
Pi but not implemented follows the same inactive policy, while an unknown hook name remains a hard
extension error so typos are not hidden. Unsupported result fields on an otherwise supported hook
are ignored rather than failing the callback. The maintained capability matrix is
[`docs/js-extension-compatibility.md`](js-extension-compatibility.md). JavaScript extensions are
trusted in-process Node code and share the process and OS authority of the product.

`ModelsPlugin` is a provider plugin loaded after the base protocol provider. It loads one immutable,
credential-blind `models.json` snapshot per generation and composes layers in Pi order: built-in
catalog, provider `baseUrl`/`compat`, custom-model upsert, then one explicit model override. The
registry exposes narrow, construction-only provider/model override seams; duplicate overrides and
missing override targets fail candidate construction instead of mutating a published registry.
Full model overrides preserve partial cost rates, tier replacement, input modalities, context and
output limits, thinking maps, per-key sampling parameters, request headers, and Pi's four special
nested `compat` merges.

Custom routes dispatch by their declared wire API. `openai-completions` and `openai-responses` reuse
their OpenAI protocol projections, `anthropic-messages` reuses the standard Anthropic projection,
and `google-generative-ai` owns its Generative AI request/SSE adapter. The effective `ModelSpec` and
session id travel with each semantic `ProviderRequest`, so protocol modules apply merged compat,
model limits, thinking controls, prompt caching, routing, deferred tools, and session affinity at
serialization time. The provider overlay resolves credentials and headers only when sending the
request and decorates terminal usage with Pi-compatible per-million and tiered cost calculation.
Protocol serialization remains outside `ModelsPlugin`. `models_json_schema()` exposes the same
strict compat and catalog definitions for editors and tooling. A failed parse, validation, or
active-provider compatibility check prevents publication of the new generation, so `/reload`
retains the complete prior provider/catalog pair.

Radius is intentionally not projected through this static routing path. `oauth: "radius"` requires
one provider-owned OAuth, persisted remote-catalog, and `pi-messages` lifecycle; until that deep
module exists, generation construction rejects Radius configuration explicitly.

The independent `pi-plugin-anthropic` provider owns Anthropic Messages projection/SSE parsing, credential precedence, standard configurable routing, Claude Code request mode, browser PKCE authorization-code exchange/refresh, and its Claude catalog as one deep vendor Module. `ModelsPlugin` reuses its standard route for Anthropic-compatible custom providers without importing built-in Anthropic credential or OAuth policy. Claude Code mode preserves thinking signatures and applies OAuth identity headers, the required system identity, and bidirectional canonical tool-name mapping. `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, and Pi-compatible `<agent-dir>/auth.json` credentials are supported; explicit CLI credentials win, followed by environment credentials and stored credentials. The CLI owns secure credential persistence through `pi auth login/logout/status`: writes use a sibling lock, atomic replacement, and Unix mode `0600`, while status never emits secrets.

The built-in `openai-codex` plugin is installed in every product generation rather than only when Codex is initially selected. It owns the explicit Pi-compatible model catalog, Codex Device OAuth/refresh, and a Codex Responses Adapter. Reusable OpenAI Responses message/tool projection and SSE adaptation live directly in `pi-plugin-openai::responses`; the separate `pi-plugin-xai` provider depends on that plugin crate while retaining its own lifecycle, credentials, headers, payload policy, OAuth device flow, refresh, and Grok catalog. xAI exposes the current Grok 4.5/4.6 Responses models, resolves `XAI_API_KEY` or Pi-compatible stored credentials when rebuilding a generation, supports explicit xAI Device OAuth through `pi auth login xai --oauth`, and proactively refreshes stored xAI OAuth credentials before application startup. The built-in `pi-plugin-google` provider owns the Google Generative AI projection and current Gemini catalog, resolves `GEMINI_API_KEY` before Pi-compatible stored credentials, and exposes API-key login as `pi auth login google`; Google OAuth identities are not part of this provider. Anthropic and OpenAI Codex stored OAuth credentials use the same startup refresh transaction. `pi auth login` without a provider builds its selector from built-ins, validated JSONC `models.json` provider IDs, and existing stored credentials; unknown third-party providers receive API-key auth unless a future provider-owned OAuth capability declares otherwise. Device authorization validates xAI HTTPS verification URLs before invoking the platform browser Adapter; token polling and refresh remain provider-owned while locked atomic `auth.json` persistence remains CLI-owned. At generation construction the Codex plugin credential-blindly probes Codex CLI credentials from `~/.codex/auth.json` and `~/.config/codex/auth.json`; a valid access-token JWT with `chatgpt_account_id` makes the provider selectable. Requests use the ChatGPT Codex Responses endpoint and its required bearer, account, beta, originator, and user-agent headers. This reuse does not write or refresh Codex CLI credentials and is not Pi's `/login` flow. The catalog supplies context windows, output limits, input modalities, reasoning support, and costs; it is not remote discovery. `ModelRuntime` keeps the complete registered catalog distinct from its credential-blind available view and exposes provider availability diagnostics. Providers report whether the current immutable generation has enough configuration to be selectable without resolving secret values. Initial selection and `/model` consume the available view, while restore and diagnostics can still inspect registered models. `AgentSession` derives compaction limits from the active generation's current `ModelSpec`, so model switches immediately change threshold and overflow decisions. An explicit session context-window option remains an embedding override.

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
StreamEvent -> partial snapshots -> final AssistantMessage
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
immediate compaction loop. Each summary request is also constrained by the active generation's
`ModelSpec`: non-reasoning models force thinking off, and the configured summary budget is clamped
to the model's maximum output tokens.

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

`MultiSessionManager` is the multi-session product Module above `AgentSession`. It owns the injected
`AgentSessionRuntimeFactory`, serializes manager-level acquisition and shutdown, and keeps its
active-session map private. Opening an already-active path reuses its `PiSession`; creating or
switching to a path owned by another handle fails before any session lifecycle transition starts.
Manager shutdown drains and closes every managed handle.

Each `PiSession` has one replaceable current `AgentSession`. Its internal `AgentSessionRuntime`
serializes replacement, dispatches `session_before_switch` or `session_before_fork`, settles the
active agent, prepares the complete next session, emits old `session_shutdown`, emits new
`session_start`, and only then publishes the new generation through a Tokio watch channel.
Cancellation performs no preparation. Preparation failure leaves the current session open, while a
successful replacement closes stale `AgentSession` handles so they reject later mutations. New,
resume, reload, and fork all use this transaction. Fork creates a Pi v4 branch copy before
preparation and removes it if candidate preparation fails; import remains outside the live
replacement path.

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
