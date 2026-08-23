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
  project manifests, or repeated `--plugin` paths; local/HTTP/GitHub packages and static registries
  install through exact locks and a content-addressed store.
- **Pi JS/TS extensions**: an optional Node 20 + NAPI-RS launcher discovers and reloads Pi-style
  extensions while the Rust runtime and Ratatui product remain authoritative.
- **Models and providers**: OpenAI-compatible APIs, built-in OpenAI Codex, Anthropic/Claude Code,
  and xAI Grok providers, plus a `models.json` catalog for endpoints, request parameters, headers,
  credentials, and model metadata.
- **Production tools**: `read`, `write`, `edit`, `hashline_edit`, `bash`, `grep`, `find`, and `ls`.
- **Skills and resources**: global and project skill discovery, `/skill:<name>` commands, and
  generation-time system prompt contributions.
- **Pi v4 sessions**: lazy first-response persistence, `/resume`, durable queues, branch/tree
  semantics, compaction, context repair, and recovery reduction.
- **Project trust**: nearest-ancestor decisions persisted in `<agent-dir>/trust.json`, shared by
  project prompts, skills, and native plugins.

## Quick start

Rust 1.98 or newer is required. The repository pins Rust 1.98.0 through `rust-toolchain.toml`.

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

# Start with Anthropic API credentials
export ANTHROPIC_API_KEY="..."
cargo run -p pi-cli -- --provider anthropic --model claude-sonnet-4-6

# Or use an existing Claude Pro/Max OAuth access token with Claude Code request shaping
export ANTHROPIC_OAUTH_TOKEN="sk-ant-oat-..."
cargo run -p pi-cli -- --provider anthropic --model claude-sonnet-4-6

# Start with xAI Grok (uses XAI_API_KEY)
export XAI_API_KEY="..."
cargo run -p pi-cli -- --provider xai --model grok-4.6
```

The built-in Anthropic provider also accepts `ANTHROPIC_AUTH_TOKEN` (bearer auth), with precedence
over `ANTHROPIC_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, and `<agent-dir>/auth.json`. Explicit `--api-key`
has highest priority. OAuth credentials stored in Pi-compatible `auth.json` use Claude Code identity
headers, system identity, tool-name mapping, and reasoning-signature replay. Interactive `/login`
and `/logout` use the same credential store and rebuild the active session generation after a
change, while supported OAuth credentials refresh automatically before expiry.

Credential management commands write Pi-compatible `<agent-dir>/auth.json` using a hidden prompt,
file locking, atomic replacement, and mode `0600` on Unix:

```bash
# Configure any provider interactively; the selector includes models.json providers
pi auth login

# Browser/subscription OAuth for built-in providers
pi auth login anthropic --oauth
pi auth login openai-codex --oauth
pi auth login xai --oauth

# Prompt for an API key without echo
pi auth login anthropic --api-key
pi auth login xai --api-key

# Store an existing OAuth access token when importing credentials
pi auth login anthropic --oauth-token
pi auth login openai-codex --oauth-token
pi auth login xai --oauth-token

# Authorize with an xAI/Grok subscription in the browser
pi auth login xai --oauth

# Inspect metadata without printing secrets, or remove a credential
pi auth status
pi auth logout anthropic
```

The hidden `--token` option is available for automation but should be avoided interactively because
command-line arguments may be visible in shell history or process listings.

Credentials may also be persisted in Pi's `<agent-dir>/auth.json` format:

```json
{
  "anthropic": {
    "type": "oauth",
    "access": "sk-ant-oat-...",
    "refresh": "...",
    "expires": 0
  },
  "xai": {
    "type": "api_key",
    "key": "xai-..."
  }
}
```

Stored OAuth credentials for Anthropic, OpenAI Codex, and xAI are refreshed automatically shortly
before expiry. After changing credentials from another process while the TUI is running, use `/reload` to rebuild the provider
generation.

The built-in xAI provider uses `https://api.x.ai/v1/responses` and exposes the current Grok 4.5
and Grok 4.6 catalog. Without `XAI_API_KEY`, these models remain registered for diagnostics but are
hidden from the available-model selector.

```bash
# Start in a specific project
cargo run -p pi-cli -- --cwd /path/to/project
```

Show all CLI options:

```bash
cargo run -p pi-cli -- --help
```

To run Pi-compatible JavaScript or TypeScript extensions, build the optional Node host:

```bash
cd packages/pi
npm install
npm run build:native
npm start -- --cwd /path/to/project

# Or load an exact extension path
npm start -- --no-extensions -e /path/to/extension.ts
```

It discovers trusted project `.pi/extensions`, global `<agent-dir>/extensions`, and explicit `-e`
paths in Pi order. `/reload` rebuilds JavaScript callbacks in the same atomic product-generation
transaction as Rust plugins, models, resources, and sessions. See
[packages/pi/README.md](packages/pi/README.md) for the supported extension API and explicit gaps.

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
├── plugins.json         # Ordered native plugin intent
├── plugins.lock         # Exact target-specific resolution and install record
├── plugins/
│   ├── store/sha256/    # Immutable CAS blobs named by digest
│   └── installed/       # Current ordered native plugin activation view
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
    ├── plugins.json      # Shareable project plugin intent
    ├── plugins.lock      # Exact project resolution
    ├── plugins/
    │   ├── store/sha256/ # Local immutable CAS blobs; ignore in version control
    │   └── installed/    # Current ordered project plugin activation view
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

## Native plugin packages

```bash
pi plugin install ./path/to/package
pi plugin install https://example.com/pi-plugin-release.json
pi plugin install registry:frontend-check@^1 \
  --registry https://plugins.example/index.json
pi plugin list
pi plugin sync --registry https://plugins.example/index.json
pi plugin remove frontend-check
```

Pass `-l` to manage the trusted current project's `.pi/plugins.json` and `.pi/plugins.lock` instead
of global agent state. The manager selects the exact host target, preserves declared plugin order,
verifies artifact SHA-256, writes the lock, and activates immutable CAS entries for the existing
native loader. See [crates/pi-plugin-manager/README.md](crates/pi-plugin-manager/README.md) for
release and static Registry formats. SHA-256 currently provides integrity, not publisher
authentication; signatures and OCI sources remain a later milestone.

Normal startup automatically reconciles global intent and trusted project intent; `/reload` does
the same for a running session. Locked versions remain pinned, while edited options and rebuilt
local artifacts are applied transactionally and rolled back if the next generation fails to load.

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
| `crates/pi-plugin-manager` | Package intent/lock, static Registry resolution, target selection, and CAS installation |
| `crates/pi-js-plugin` / `bindings/pi-napi` | Typed JS lifecycle adapters and the Node/NAPI boundary |
| `packages/pi` | Node launcher, Pi extension discovery, Jiti loader, and callback generations |
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
the deterministic scripted provider in `pi-test-support`.

## Multi-platform packaging

Build the standalone archive and NAPI artifact for the target matching the current host:

```bash
cd packages/pi && npm install && cd ../..
./scripts/package-target.sh aarch64-apple-darwin
```

macOS and Linux can install the newest matching archive from `dist/release/`:

```bash
./scripts/install-package.sh
```

The release matrix covers macOS arm64/x64, Linux glibc arm64/x64, and Windows MSVC arm64/x64.
GitHub archives are standalone Rust builds; npm uses a JavaScript root plus one OS/CPU/libc-specific
NAPI optional package. Release Please maintains the version/changelog PR, while npm Trusted
Publishing supplies short-lived OIDC authentication and automatic provenance. The current artifacts
are checksummed and smoke-tested but unsigned and not notarized. See
[apps/pi-cli/README.md](apps/pi-cli/README.md#multi-platform-packaging) for the Release Module
Interface and artifact layout.

## Open boundaries

- Connect the recovery reducer to complete post-crash operation replay orchestration.
- Add signed, content-addressed native plugin installation and remote distribution.
- Continue using `legacy/pi` as the oracle for user-visible behavior and cross-platform terminal
  conformance.
