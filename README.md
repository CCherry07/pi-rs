# pi_rs

**English** | [简体中文](README.zh-CN.md)

`pi_rs` is a Pi-style terminal coding agent implemented in Rust. It uses the current TypeScript Pi
as its behavioral reference and provides a usable fullscreen TUI, model and provider
configuration, tool execution, skills, project trust, and resumable Pi v4 sessions.

This is more than an agent-loop library port. The interactive TUI, one-shot output, NDJSON event
stream, session restore, context compaction, and plugin reload all run on the same product runtime.

> Status: the repository contains a usable product baseline. Current work continues on Pi
> conformance, crash-recovery orchestration, and native plugin distribution.

## Features

- **Terminal product**: a fullscreen TUI built with Ratatui, Crossterm, and Tokio, with Markdown,
  syntax highlighting, CJK IME input, copy selection, input history, scrolling, and command
  selectors.
- **Three frontend modes**: interactive TUI, one-shot `--print`, and an NDJSON product-event stream
  with `--json`.
- **Plugin-first runtime**: narrow `AgentPlugin`, `ProviderPlugin`, and `SessionPlugin` lifecycles;
  plugins are built as immutable generations and reloaded atomically, with rollback on failure.
- **Native plugins**: version-locked Rust `cdylib` plugins load from global manifests, trusted
  project manifests, or repeated `--plugin` paths without adding another runtime lifecycle.
- **Models and providers**: OpenAI-compatible APIs plus a `models.json` catalog for endpoints,
  request parameters, headers, credentials, and model metadata.
- **Production tools**: `read`, `write`, `edit`, `hashline_edit`, `bash`, `grep`, `find`, and `ls`.
- **Skills and resources**: global and project skill discovery, `/skill:<name>` commands, and
  generation-time system prompt contributions.
- **Pi v4 sessions**: lazy first-response persistence, `/resume`, durable queues, branch/tree
  semantics, compaction, context repair, and recovery reduction.
- **Project trust**: nearest-ancestor decisions persisted in `<agent-dir>/trust.json`, shared by
  project prompts, skills, and native plugins.

## Quick start

Rust 1.85 or newer is required.

```bash
git clone <your-repository-url>
cd pi_rs

# Configure an OpenAI-compatible API
export OPENAI_API_KEY="..."
export OPENAI_MODEL="gpt-4o-mini"
export OPENAI_BASE_URL="https://api.openai.com/v1"

# Start the fullscreen TUI
cargo run -p pi-cli --
```

The terminal alternate screen is enabled by default. To stay on the main terminal screen:

```bash
cargo run -p pi-cli -- --no-fullscreen
```

Other common modes:

```bash
# Run once and print only the final assistant text
cargo run -p pi-cli -- --print "summarize this repository"

# Emit NDJSON product events
cargo run -p pi-cli -- --json "list the Rust crates"

# Read a prompt from stdin
printf 'explain this project' | cargo run -p pi-cli -- --print

# Shell shorthand; provider credentials are not required
cargo run -p pi-cli -- --print '!git status --short'
cargo run -p pi-cli -- --print '!!git status --short'

# Start in a specific project
cargo run -p pi-cli -- --cwd /path/to/project
```

Show all CLI options:

```bash
cargo run -p pi-cli -- --help
```

## Model configuration

The default agent directory is `~/.pi/agent`. Override it with `PI_AGENT_DIR` or `--agent-dir`.
The recommended setup is to register models in `<agent-dir>/models.json`:

```jsonc
{
  // models.json accepts comments
  "providers": {
    "openai-compatible": {
      "api": "openai-completions",
      "baseUrl": "https://api.openai.com/v1",
      "apiKey": "$OPENAI_API_KEY",
      "models": [
        {
          "id": "gpt-4o-mini",
          "name": "GPT-4o mini",
          "reasoning": false,
          "input": ["text", "image"],
          "contextWindow": 128000,
          "maxTokens": 16384
        }
      ]
    }
  }
}
```

Environment variables in `apiKey`, headers, and other string settings are resolved only when a
request is sent. Shell-command values prefixed with `!` are also supported. Credentials are never
copied into the public model catalog.

Initial model selection uses this order:

1. an explicit CLI `--model` / `--provider` request;
2. the model restored from a session, if it is still registered;
3. the first model in the `models.json` catalog;
4. `OPENAI_MODEL` or the CLI fallback.

Use `/model` in the TUI to inspect and switch between models registered in the active generation.
After editing `models.json`, `/reload` atomically rebuilds plugins, models, and resources. Invalid
configuration never replaces the active generation.

## Agent directory and project resources

Default layout:

```text
~/.pi/agent/
├── models.json          # Provider and model catalog
├── settings.json        # Global product settings
├── trust.json           # Project trust decisions
├── SYSTEM.md            # Optional global system prompt
├── APPEND_SYSTEM.md     # Optional global appended prompt
├── skills/              # Global skills
├── plugins/             # Installed native plugin manifests and platform artifacts
├── plugin-data/         # Persistent per-plugin data
└── sessions/            # Pi v4 JSONL sessions

~/.agents/skills/        # Always-trusted user skill root
```

A project may provide:

```text
project/
├── AGENTS.md            # Project context; not gated by trust
├── .agents/skills/      # Discovered from cwd toward the Git root
└── .pi/
    ├── SYSTEM.md
    ├── APPEND_SYSTEM.md
    ├── plugins/          # Trusted project-native plugins
    └── skills/
```

Project `.pi` prompts and project skills are loaded only after the project is trusted. Interactive
runs show a trust selector. Non-interactive runs default to untrusted unless `--approve` / `-a` or
`--no-approve` / `-na` supplies an explicit decision.

Global `settings.json` can define the default policy:

```json
{
  "defaultProjectTrust": "ask"
}
```

Supported values are `ask`, `always`, and `never`.

> Project trust is not a filesystem sandbox. Like Pi, filesystem tools accept cwd-relative paths,
> absolute paths, `~`, `file://`, and parent-relative paths outside cwd. The process and operating
> system permissions are the actual boundary.

## TUI commands

Built-in commands include:

```text
/new [path]                 Create a session
/resume [query|path]        List, filter, or open sessions
/reload                     Rebuild the plugin/model/resource generation
/trust                      Change trust for the current project
/model [provider/model|id]  List or switch models
/thinking <level>           Change the thinking level
/compact                    Compact the current context
/clear                      Clear the visible transcript
/help                       Show commands
/quit                       Exit
/skill:<name> [task]        Invoke a discovered skill explicitly
```

Type `/` and use the arrow keys to select a command; press `Tab` to complete it. See
[apps/pi-cli/README.md](apps/pi-cli/README.md) for the full keyboard reference.

## Session behavior

- A new session starts in memory. Its JSONL file is created only after the first assistant
  `message_end`.
- Quitting immediately, interrupting before the first response, or using only shell shorthand does
  not add an empty item to `/resume`.
- Every assistant tool call is persisted with its matching tool result before the next provider
  request, preventing dangling tool calls after restore.
- `/resume` restores cwd, model, message tree, queues, and compaction state. Executable plugins and
  resources are rebuilt from the current generation rather than deserialized from session data.

## Architecture

| Directory | Responsibility |
| --- | --- |
| `apps/pi-cli` | CLI, TUI, terminal lifecycle, project trust, and product assembly |
| `crates/pi-core` | Strongly typed contracts, registries, and plugin drivers |
| `crates/pi-agent` | Agent façade, agent loop, stream assembly, and tool scheduling |
| `crates/pi-runtime` | Generation construction, prompt assembly, and atomic reload |
| `crates/pi-session` | Pi v4 JSONL, tree/branch state, compaction, recovery reducer, and session runtime |
| `crates/pi-provider` | Provider-neutral HTTP transport and SSE |
| `crates/pi-prompt` / `pi-resources` | System prompt and project context discovery |
| `apps/pi-md` | TUI-owned Markdown parsing, streaming repair, syntax highlighting, and Ratatui rendering |
| `crates/pi-plugin-sdk` / `pi-plugin-loader` | Native author interface, compatibility checks, discovery, and factory adapters |
| `plugins/` | Skills, provider catalog, and independent production tool plugins |
| `legacy/pi` | Current TypeScript Pi behavioral oracle |
| `e2e` | Deterministic full-agent tests and an example project |

Dependencies point inward: core contracts do not own terminal behavior, filesystem discovery,
session storage, or vendor routing policy. See [docs/architecture.md](docs/architecture.md) for hook
ordering, persistence invariants, and the detailed design.

## Development and validation

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

Never put real provider credentials in source, logs, or fixtures. The default validation path uses
the deterministic faux provider.

## Apple Silicon packaging

Build a release archive on an Apple Silicon Mac:

```bash
./scripts/package-macos-arm64.sh
```

Install the newest archive from `dist/`:

```bash
./scripts/install-package.sh
```

The current artifact is unsigned and not notarized. See
[apps/pi-cli/README.md](apps/pi-cli/README.md#package-for-apple-silicon-macos) for packaging options.

## Open boundaries

- Connect the recovery reducer to complete post-crash operation replay orchestration.
- Add signed, content-addressed native plugin installation and remote distribution.
- Continue using `legacy/pi` as the oracle for user-visible behavior and cross-platform terminal
  conformance.
