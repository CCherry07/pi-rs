# pi_rs agent guide

## Mission

Build `pi_rs` into a production-quality Rust implementation of the current Pi coding-agent
product. Match observable Pi behavior where it is intentional, while keeping the Rust design
plugin-first, generation-based, and strongly typed. The target is a usable CLI/TUI product, not
only a library port.

Treat `legacy/pi` as the local behavior oracle. Verify the relevant current TypeScript source before
claiming Pi compatibility; do not implement from memory or from an older Pi shape.

## Current baseline

- A generation-based runtime registers agent/tool/command, provider/catalog, and session plugin
  systems and rebuilds factory-backed plugins on reload.
- Skills, `models.json`, project resources, OpenAI-compatible routing, production filesystem/shell
  tools, and deterministic faux-provider fixtures are integrated.
- Pi-compatible project trust persists nearest-ancestor decisions, prompts interactively, and
  gates project `.pi` resources and project skill roots before generation construction.
- Pi v4 sessions support resume, queues, branching, compaction, context repair, recovery reduction,
  and lazy first-response persistence through `AgentSessionRuntime`.
- The Ratatui CLI provides interactive, print, and NDJSON modes with fullscreen operation, product
  events, slash-command selectors, Markdown rendering, history, scrolling, selection, and IME input.

## Context pointers

- **Architecture changes:** read [`docs/architecture.md`](docs/architecture.md) before changing
  cross-crate boundaries, plugin lifecycles, runtime generations, prompt assembly, event ordering,
  model selection, or session persistence.
- **Pi conformance:** inspect the corresponding implementation under `legacy/pi` before changing a
  user-visible behavior, wire format, hook contract, command, session rule, or resource precedence.
- **CLI/TUI changes:** read [`apps/pi-cli/README.md`](apps/pi-cli/README.md) and the focused tests in
  `apps/pi-cli/src/tui.rs` before changing terminal modes, input behavior, commands, transcript
  rendering, scrolling, selection, or status presentation.
- **Historical status:** treat `docs/incomplete-handoff.md` as a historical snapshot. Derive current
  status from code and tests instead of copying its milestone or test counts.

## Architectural invariants

### Boundaries

- Dependencies point inward. `pi-core` owns contracts, not product policy, filesystem discovery,
  vendor routing, session storage, or terminal rendering.
- Terminal ownership stays in `apps/pi-cli`. Reusable crates may expose semantic data and product
  events, but terminal setup, alternate-screen control, Ratatui widgets, input decoding, and visual
  styling remain in the app layer.
- Put policy in the module that owns the concept. Keep app wiring thin; avoid command-name,
  provider-name, tool-name, or skill-name switches in generic runtimes.

### Plugins and reload

- Keep agent/tool/command plugins (`AgentPlugin`), provider/catalog plugins (`ProviderPlugin`), and
  session lifecycle extensions (`SessionPlugin`) as distinct systems with narrow drivers.
- Registration happens while building a generation. Registries are immutable after publication,
  duplicate identities fail construction, and hooks run in registration order.
- Every product plugin must be reloadable through a factory-backed next generation. Prepare and
  validate the complete generation before swapping it; a failed reload keeps the previous
  generation intact. Never mutate live registries in place.
- Native dynamic-library loading, when added, belongs behind the existing fallible generation
  factory seams rather than introducing a second runtime model.
- Provider plugins own provider implementations, routing overlays, and their model catalog entries.
  Keep the frozen `ModelRuntime` as the generation-local query surface; do not reintroduce a
  separate model-plugin lifecycle for catalog registration alone.
- `SkillsPlugin` owns skill roots, discovery, parsing, collisions, catalog prompt contribution, and
  `/skill:<name>` commands. Generic resources and prompt assembly remain skill-agnostic.

### Models and prompts

- `models.json` owns registered model/provider catalog and request routing. Keep credentials and
  environment expansion request-time only.
- Initial model priority is explicit request, restorable session model, catalog default, then
  runtime fallback. Keep this policy in `ModelRuntimeServices` / `InitialModelResolver`, outside the
  catalog loader.
- Generation-time prompt changes flow through plugin hooks. Prompt contributions are run-local and
  must not mutate the reusable base prompt.

### Sessions

- Preserve the Pi v4 JSONL schema, shared mutation sequence, tree/lane semantics, and provider
  projection rules. Unknown agent-message wire extensions must survive replay.
- A new `AgentSession` exists in memory immediately, but its JSONL file materializes only after the
  first assistant `message_end`. Quit-before-response and shell-only use leave no resume entry.
  Reloading an unsaved session reuses its in-memory log; an unsaved session cannot be forked.
- Persist every assistant tool call with its matching tool result before the next provider request.
  Recovery and context projection must never emit a dangling tool call.
- Session plugins receive lifecycle events; executable plugin code and resources are rebuilt, not
  deserialized from session data.

### Product UI

- Drive every frontend mode from the same `AgentSessionRuntime` and semantic product-event stream.
  Provider errors, tool failures, queues, reloads, and resumed history must remain visible without
  writing application output outside the TUI renderer.
- Fullscreen alternate-screen mode is the default. Preserve main-screen opt-out, Unicode/IME input,
  copy selection, transcript scrolling, registered `/model`, `/resume`, and `/skill:` selectors,
  and shell shorthand.
- Keep user, assistant, tool, error, and working entries semantically distinct while using shared
  spacing and alignment rules. Prefer tests of layout relationships over terminal-specific pixel
  assumptions.

## Open boundaries

- Future project settings, packages, and native plugins must use the existing project trust service
  before loading. Match Pi by leaving `AGENTS.md`/`CLAUDE.md` context discovery independent of
  trust; trust gates project `.pi` resources and project skills, not tool execution.
- Filesystem tools follow Pi path semantics: resolve relative paths from cwd, expand `~`, and allow
  absolute or parent-relative paths outside cwd. Operating-system permissions are the boundary;
  do not introduce a `readable_roots` sandbox as part of project trust.
- The reducer reconstructs interrupted session state, but full operation replay/recovery execution
  orchestration is still separate from the live runtime.
- Factory-backed reload is implemented; version-locked native plugin discovery/loading is not.
- Native plugin work: inspect Farm's loader and lifetime model under
  `legacy/farm/crates/node/src/plugin_adapters/rust_plugin_adapter` and its build/distribution
  conventions under `legacy/farm/packages/plugin-tools` before choosing the ABI, manifest/version
  checks, or unload boundary, then adapt those patterns behind the existing generation factories.

## Change workflow

1. Locate the owning Rust module and the matching Pi source. State whether the change is conformance
   or a deliberate Rust/product divergence.
2. Write or identify a focused regression at the owning seam. For cross-layer behavior, add the
   smallest integration test that proves the handoff between layers.
3. Implement in the deepest owning module, then adapt outward. Preserve unrelated work in the
   worktree and keep protocol/storage compatibility explicit.
4. Update `docs/architecture.md` when an architectural invariant, lifecycle, persistence rule, or
   deliberate Pi divergence changes.
5. Finish only when all affected tests pass and the full quality gates are green:

   ```text
   cargo fmt --all -- --check
   cargo test --workspace
   cargo clippy --workspace --all-targets -- -D warnings
   git diff --check
   ```

For real-provider checks, keep credentials out of source, logs, fixtures, and distribution
artifacts. Deterministic faux-provider tests remain the default validation path.
