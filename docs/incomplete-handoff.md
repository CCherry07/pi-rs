# Implementation handoff

Status: `pi_rs` has a usable product baseline. It is no longer only an Agent Core MVP.
Interactive TUI, print, and NDJSON modes share the same session runtime, and the main plugin,
provider, tool, resource, trust, and Pi v4 persistence paths are integrated.

This document is a point-in-time handoff, not the architectural source of truth. Read
[`architecture.md`](architecture.md), the current code, and focused tests before changing behavior.
Use `legacy/pi` as the oracle for intentional Pi compatibility.

## Implemented baseline

### Product frontends

- Fullscreen Ratatui TUI by default, with a main-screen opt-out.
- One-shot `--print` mode and newline-delimited `--json` product events.
- Shared `AgentSessionRuntime` and semantic product-event stream across all frontend modes.
- Markdown rendering, syntax highlighting, CJK/IME input, history, scrolling, copy selection, and
  slash-command/skill selectors.
- Steering and follow-up queues, abort handling, shell shorthand, model switching, compaction,
  session resume, reload, and project-trust interaction.

### Agent and plugin runtime

- Strongly typed provider, tool, command, cancellation, message, stream, and lifecycle contracts.
- Separate `AgentPlugin`, `ProviderPlugin`, and `SessionPlugin` systems with ordered hooks.
- Immutable runtime generations and atomic reload; failed generation construction retains the
  current generation.
- Generation-local model catalog and provider routing through `ModelRuntime`.
- Native Rust `cdylib` plugins with manifests, compatibility checks, content-hash load snapshots,
  explicit paths, global discovery, trusted project discovery, and process-pinned library lifetime.
- Native agent, provider, and session plugin export macros and author SDK.

### Providers and models

- Provider-neutral HTTP transport and SSE decoding.
- OpenAI completions-compatible provider implementation.
- `models.json` parsing, structural validation, model inheritance/overrides, catalog registration,
  provider routing overlays, headers, request parameters, and request-time credential expansion.
- Initial model policy covering explicit selection, restorable session models, catalog defaults,
  and runtime fallback.
- Deterministic faux provider and ignored opt-in real-network coverage.

### Tools and resources

- Production `read`, `write`, `edit`, `hashline_edit`, `bash`, `grep`, `find`, and `ls` plugins.
- Pi-compatible path handling for cwd-relative, parent-relative, absolute, `~`, and `file://` paths;
  operating-system permissions remain the boundary.
- Global and project prompt resources plus `AGENTS.override.md`, `AGENTS.md`, and `CLAUDE.md`
  discovery.
- Global and trusted-project skill discovery, frontmatter parsing, collision diagnostics, prompt
  catalog contribution, and `/skill:<name>` commands.
- Pi-compatible project trust with nearest-ancestor persisted decisions and interactive requests.

### Sessions

- Pi v4 JSONL header and mutation schema with a shared sequence, entry tree, lanes, records,
  pointers, global facts, and unknown message-extension preservation.
- In-memory and JSONL repositories, resume listing, branch/tree semantics, checkout, and storage
  fork primitives.
- Lazy first-response persistence: a new session file is not materialized until the first assistant
  `message_end`.
- Persistence of assistant tool calls with matching tool results before the next provider request.
- Provider projection, context repair, queue persistence, configuration restoration, and current
  generation rebuild on resume.
- Manual and threshold compaction, retained-tail behavior, overflow compaction with one retry, and
  isolated summary requests.
- Record-log validation and pure recovery reduction for interrupted operations, attempts, queues,
  deferred writes, and tool batches.
- Session lifecycle plugins and transactional new/open/reload replacement through
  `AgentSessionRuntime`.

### Packaging

- Apple Silicon macOS release archive and local installer scripts with architecture and SHA-256
  validation.

## Open product boundaries

### 1. Crash recovery execution orchestration

The recovery reducer reconstructs interrupted state but is not connected to a complete live replay
or repair workflow. The remaining work includes deciding and executing recovery for unfinished
steps/tool batches, applying deferred writes safely, restoring pending queues, and surfacing recovery
diagnostics without producing dangling tool calls.

### 2. Broader provider and authentication support

The shipping provider path is OpenAI completions-compatible. Current Pi additionally supports
provider-specific Anthropic, Google, Bedrock, Azure, and other protocols, subscription/OAuth flows,
credential management through `/login` and `/logout`, catalog refresh, and additional transport
choices. `models.json` currently rejects unsupported APIs and OAuth configuration during generation
construction.

### 3. Session management product surface

The storage layer has tree, checkout, branch, and fork primitives, but the CLI/TUI does not yet expose
Pi's complete `/tree`, `/fork`, `/clone`, `/name`, `/session`, `/export`, `/import`, and `/share`
workflows or the equivalent startup flags. These should be added through the existing session runtime
replacement transaction rather than by mutating stale session handles.

### 4. Settings and customization resources

Project trust recognizes project settings, prompts, and themes as trust-requiring resources, but the
product does not yet implement the full Pi settings surface. Missing areas include project settings
overrides, `/settings`, configurable keybindings, prompt templates, user/project themes, scoped model
cycling, delivery modes, transport preferences, and hot reload for those resources.

### 5. TUI conformance and multimodal input

The TUI baseline is usable, but current Pi also provides file-reference search, path completion,
external-editor integration, image paste/drag input, model/thinking shortcuts, queue retrieval, and
collapsible thinking/tool output. Cross-terminal coverage should continue for Windows Terminal,
tmux, Unicode/IME behavior, image protocols, and main-screen rendering.

### 6. Native plugin packaging and distribution

Native plugins are local and exact-build/version locked. Signed content-addressed installation,
remote registry/distribution, artifact download and verification, upgrades, rollback, and package
management commands are not implemented. Any ABI or unload changes must preserve generation
atomicity and process-pinned library safety.

### 7. Programmatic and remote integration

The Rust crates provide reusable internal seams, but there is no stabilized embedding SDK, bidirectional
Pi-compatible RPC mode, or implementation of the experimental protocol/session-server stack found in
current Pi. NDJSON product events are output, not a control protocol.

### 8. Release engineering

Packaging is currently limited to unsigned, unnotarized Apple Silicon macOS archives. Production
release work still includes CI quality gates, Linux and Windows artifacts, Intel macOS where desired,
code signing/notarization, release smoke tests, installation/update policy, and cross-platform
terminal validation.

## Intentional non-goals unless delivered by plugins

Following Pi's product philosophy, these should not be added as generic runtime policy without an
explicit architectural decision:

- built-in MCP;
- built-in sub-agents;
- built-in plan mode;
- built-in permission popups or a project-trust filesystem sandbox;
- built-in TODO management;
- built-in background shell job management.

The plugin seams should support product-specific implementations without introducing provider, tool,
or command-name switches into generic runtimes.

## Recommended next milestones

1. Connect recovery reduction to deterministic crash/deferred-operation repair and replay.
2. Expose existing tree, checkout, and fork capabilities through focused session-management UI.
3. Complete global/project settings consumption and add prompt-template, theme, and keybinding
   resources through generation rebuilds.
4. Add provider-specific protocols and authentication flows behind provider plugins.
5. Add stable RPC/embedding surfaces only after their lifecycle, event, and session semantics are
   covered by conformance tests.
6. Build signed cross-platform distribution and a verified native-plugin package pipeline.

## Validation

Before handing off a change, run the repository quality gates rather than relying on historical test
counts in this file:

```text
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

Use deterministic faux-provider tests by default. Keep real credentials out of source, fixtures,
logs, session samples, and distribution artifacts.
