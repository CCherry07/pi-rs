# pi-rs

**English** | [简体中文](README.zh-CN.md)

`pi-rs` is a production-oriented Rust implementation of the Pi terminal coding agent. It uses the
current TypeScript Pi as its behavioral reference and provides a usable fullscreen TUI, model and
provider configuration, tool execution, skills, project trust, and resumable Pi v4 sessions.

This is more than an agent-loop library port. The interactive TUI, one-shot output, Pi-compatible
NDJSON and stdin/stdout RPC, session restore, context compaction, and plugin reload all run on the
same product runtime.

> Status: the repository contains a usable product baseline. Current work continues on current-Pi
> conformance, opt-in safe replay after interrupted operations, and authenticated native plugin
> distribution.

## Tribute

No [Pi](https://pi.dev), no `pi-rs`. This project is first and foremost a Rust tribute to Pi: its
clear product philosophy, deliberately small core, and extension-first design are what make this
implementation worth building. Our deepest thanks go to Mario Zechner, Earendil Works, and every
contributor to the [original Pi project](https://github.com/earendil-works/pi).

`pi-rs` is an independent implementation, not an official Pi distribution. We aim to show our
respect through careful compatibility work: reading the current source, preserving intentional
behavior, and being explicit whenever the Rust product diverges.

## Features

- **Terminal product**: a fullscreen TUI built with Ratatui, Crossterm, and Tokio, with Markdown,
  syntax highlighting, CJK IME input, copy selection, input history, scrolling, and command
  selectors.
- **Five frontend modes**: interactive TUI, one-shot `--print`, Pi-compatible NDJSON with `--json`,
  bidirectional stdin/stdout RPC with `--mode rpc`, and ACP stable v1 with `--acp`.
- **Plugin-first runtime**: narrow `AgentPlugin`, `ProviderPlugin`, and `SessionPlugin` lifecycles;
  plugins are built as immutable generations and reloaded atomically, with rollback on failure.
- **Native plugins**: version-locked Rust `cdylib` plugins load from global manifests, trusted
  project manifests, or repeated `--plugin` paths; local/HTTP/GitHub packages and static registries
  install through exact locks and a content-addressed store.
- **Pi JS/TS extensions**: an optional Node 20 + NAPI-RS launcher discovers and reloads Pi-style
  extensions, including managed local/npm/git packages, while the Rust runtime and Ratatui product
  remain authoritative.
- **Models and providers**: OpenAI-compatible APIs plus built-in OpenAI Codex, Anthropic/Claude
  Code, Google Gemini and Vertex, xAI Grok, Mistral, Azure OpenAI Responses, Amazon Bedrock,
  OpenRouter, and GitHub Copilot providers. A `models.json` catalog configures custom endpoints,
  request parameters, headers, credentials, and model metadata.
- **Authentication**: `/login`, `/logout`, and `pi auth` manage Pi-compatible API-key and OAuth
  credentials without exposing secrets in the TUI.
- **Production tools**: `read`, `write`, `edit`, `hashline_edit`, `bash`, `grep`, `find`, `ls`, and
  bounded `subagent` delegation through isolated child sessions with Markdown-defined roles.
- **Image input**: attach images with startup `@file` in TUI, print, or JSON mode; paste local
  clipboard images with `Ctrl+V`. Shared image handling validates, resizes, and converts formats.
- **Skills and prompt templates**: global and project discovery, `/skill:<name>` commands, Markdown
  prompt-template slash commands, and generation-time system prompt contributions.
- **Hermes memory and Curator**: persistent user facts, background learning, and scoped maintenance
  of global and trusted-project skills, with ownership checks, archives, backups, and rollback.
- **Scheduled prompts**: persistent one-shot, interval, and cron jobs through `schedule` and
  `/schedule`, dispatched into isolated sessions while a matching primary session remains open.
- **Pi v4 sessions**: lazy first-response persistence, `/resume`, durable queues, branch/tree
  semantics, compaction, context repair, recovery reduction, and non-destructive import of Pi
  coding-agent v1/v2/v3 sessions.
- **Project trust**: nearest-ancestor decisions persisted in `<agent-dir>/trust.json`, shared by
  project settings, prompts, skills, extensions, and native plugins.

## Quick start

### Install from npm

The published package is the recommended complete product entry point. It requires Node.js 20 or
newer, installs both `pi` and `pi-rs`, and selects the native package for the current platform.

```bash
npm install --global @pi-rs/cli
npm list --global @pi-rs/cli --depth=0
pi --version
pi
```

On first use, configure credentials and choose a model from inside the TUI:

```text
/login
/model
```

### Run from source

Repository development requires Rust 1.98 or newer and Node.js 20 or newer. The repository pins
Rust 1.98.0 through `rust-toolchain.toml`.

```bash
git clone https://github.com/CCherry07/pi-rs.git
cd pi-rs
npm install --prefix packages/pi
./scripts/pi-dev
```

The examples below use the installed `pi` command. From a source checkout, replace `pi` with
`./scripts/pi-dev`.

The terminal alternate screen is enabled by default. To stay on the main terminal screen:

```bash
pi --no-fullscreen
```

Other common modes:

```bash
# Run once and print only the final assistant text
pi --print "summarize this repository"

# Emit NDJSON product events
pi --json "list the Rust crates"

# Attach images alongside the prompt
pi --print @before.png @after.png "Compare these screenshots"

# Start the bidirectional Pi stdin/stdout RPC adapter
pi --mode rpc

# Serve ACP stable v1 for Zed or another ACP client
pi --acp --no-extensions

# Read a prompt from stdin
printf 'explain this project' | pi --print

# Shell shorthand; provider credentials are not required
pi --print '!git status --short'
pi --print '!!git status --short'

# Start with Anthropic API credentials
export ANTHROPIC_API_KEY="..."
pi --provider anthropic --model claude-sonnet-4-6

# Or use an existing Claude Pro/Max OAuth access token with Claude Code request shaping
export ANTHROPIC_OAUTH_TOKEN="sk-ant-oat-..."
pi --provider anthropic --model claude-sonnet-4-6

# Start with xAI Grok (uses XAI_API_KEY)
export XAI_API_KEY="..."
pi --provider xai --model grok-4.6
```

Image attachments support PNG, JPEG, GIF, WebP, and BMP; BMP is converted to PNG. In the TUI,
`Ctrl+V` saves a local clipboard image as a temporary PNG and inserts its path for the agent to
read. Native clipboard access is unavailable over SSH; use startup `@file` or paste a file path
instead. See [the CLI guide](apps/pi-cli/README.md#interactive-workflow) for image settings and
input behavior. Audio and video attachments are not supported yet.

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
pi auth login github-copilot --oauth
pi auth login openrouter --oauth
pi auth login xai --oauth

# Provider-specific credential-chain setup
pi auth login amazon-bedrock
pi auth login google-vertex

# Prompt for an API key without echo
pi auth login anthropic --api-key
pi auth login xai --api-key

# Store an existing OAuth access token when importing credentials
pi auth login anthropic --oauth-token
pi auth login openai-codex --oauth-token
pi auth login github-copilot --oauth-token
pi auth login openrouter --oauth-token
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

Stored OAuth credentials for Anthropic, OpenAI Codex, GitHub Copilot, and xAI are refreshed
automatically shortly before expiry. OpenRouter's browser flow returns a persistent API key rather
than a refresh token. After changing credentials from another process while the TUI is running,
use `/reload` to rebuild the provider generation.

The built-in xAI provider uses `https://api.x.ai/v1/responses` and exposes the current Grok 4.5
and Grok 4.6 catalog. Without `XAI_API_KEY`, these models remain registered for diagnostics but are
hidden from the available-model selector.

```bash
# Start the native-only adapter in a specific project
cargo run -p pi-cli -- --no-extensions --cwd /path/to/project
```

Show all CLI options:

```bash
pi --help
```

To run Pi-compatible JavaScript or TypeScript extensions from a source checkout, use the
workspace launcher. It incrementally builds the current host NAPI library and passes its exact path
to Node, so a stale copied `.node` file cannot win resolution:

```bash
npm install --prefix packages/pi
./scripts/pi-dev --cwd /path/to/project

# Or load an exact extension path
./scripts/pi-dev --no-extensions -e /path/to/extension.ts
```

The standalone `target/debug/pi` adapter fails with an actionable message when extension
configuration is active instead of silently omitting it. Pass `--no-extensions` only when a
native-only run is intentional.

It discovers trusted project `.pi/extensions`, global `<agent-dir>/extensions`, and explicit `-e`
paths in Pi order. `/reload` rebuilds JavaScript callbacks in the same atomic product-generation
transaction as Rust plugins, models, resources, and sessions. See
[packages/pi/README.md](packages/pi/README.md) for the supported extension API and explicit gaps.

## JavaScript extension packages

The Node launcher delegates Pi-compatible extension discovery and package state to the Rust package
manager. It supports local, npm, and git sources in user or trusted-project scope:

```bash
pi install npm:example-extension
pi install --local git:github.com/example/project-extension@v1 --approve
pi list
pi update --extensions
pi update npm:example-extension
pi remove npm:example-extension
```

User scope is the default. `--local` writes the trusted project's `.pi/settings.json`. Exact npm
versions remain pinned; version ranges, unversioned npm packages, and git packages can be updated.
Bare `pi update` is reserved for product self-update and is not implemented yet. Package discovery,
filters, precedence, offline behavior, and settings format are documented in
[packages/pi/README.md](packages/pi/README.md#pi-javascripttypescript-extensions).

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

Custom routes support eight wire APIs: `openai-completions`, `openai-responses`,
`azure-openai-responses`, `mistral-conversations`, `anthropic-messages`,
`google-generative-ai`, `google-vertex`, and `bedrock-converse-stream`. xAI and other
Responses-compatible gateways use `openai-responses`; their `baseUrl` may be the API base URL or
the complete `/responses` endpoint. Anthropic routes use Messages, Google routes use Gemini SSE,
Mistral uses its native chat stream, Azure applies deployment and `api-version` routing, Vertex
supports API-key express mode or Google application credentials, and Bedrock supports bearer auth
or SigV4 credentials.

Provider, model, and `modelOverrides` layers follow Pi's merge order. Overrides cover `name`,
`reasoning`, `thinkingLevelMap`, `input`, partial `cost`, `contextWindow`, `maxTokens`, merged
`samplingParams`, `headers`, and protocol-specific `compat`. Cost rates are dollars per million
tokens; the highest matching `inputTokensAbove` tier prices the whole request, including prompt
cache reads/writes. `compat` controls the actual wire request (roles, thinking formats, token-budget
fields, routing, caching, strict/deferred tools, session affinity, and provider-specific behavior),
and is validated against the selected API during generation construction.

The dynamic `oauth: "radius"` provider remains a separate boundary: its OAuth flow, remote catalog,
and `pi-messages` protocol are not implemented by the static `models.json` router yet, so that
configuration is rejected explicitly instead of being accepted without working end to end.

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
├── auth.json            # Pi-compatible API-key and OAuth credentials
├── models.json          # Provider and model catalog
├── settings.json        # Global product settings
├── memory.json          # Memory provider selection and options (optional)
├── trust.json           # Project trust decisions
├── SYSTEM.md            # Optional global system prompt
├── APPEND_SYSTEM.md     # Optional global appended prompt
├── skills/              # Global skills
├── pi-hermes-memory/    # Default Hermes memory directory
│   ├── MEMORY.md        # Durable facts and notes
│   ├── USER.md          # User preferences and profile
│   └── skills/          # Global skills created by the memory plugin
├── schedule/            # Persisted scheduled prompts and run history
├── prompts/             # Global Markdown prompt templates
├── extensions/          # Global JS/TS extensions
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
├── CLAUDE.md            # Project context; not gated by trust
├── .agents/skills/      # Discovered from cwd toward the Git root
├── .hermes/skills/      # Memory-plugin skills at the trusted Git root
└── .pi/
    ├── settings.json    # Project extensions and packages
    ├── schedule/        # Trusted cwd-local scheduled prompts
    ├── SYSTEM.md
    ├── APPEND_SYSTEM.md
    ├── prompts/         # Project Markdown prompt templates
    ├── extensions/      # Project JS/TS extensions
    ├── plugins.json      # Shareable project plugin intent
    ├── plugins.lock      # Exact project resolution
    ├── plugins/
    │   ├── store/sha256/ # Local immutable CAS blobs; ignore in version control
    │   └── installed/    # Current ordered project plugin activation view
    └── skills/
```

Project `.pi` settings, prompts, extensions, skills, and native plugins are loaded only after the
project is trusted. `AGENTS.md` and `CLAUDE.md` context discovery remains independent of trust.
Interactive runs show a trust selector. Non-interactive runs default to untrusted unless
`--approve` / `-a` or `--no-approve` supplies an explicit decision.

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

## Memory and skill curation

Hermes is the default memory provider. `MEMORY.md` and `USER.md` under
`<agent-dir>/pi-hermes-memory/` store durable facts and preferences; each session receives a frozen
prompt snapshot. Successful writes persist immediately but do not replace that session's snapshot.
The `memory` tool targets global memory or user notes, not project-specific memory files.
Background reviews can save verified learning, and `/refine [focus]` starts a review explicitly.
These are pi-rs product features inspired by Hermes, not built-in Pi compatibility guarantees.

Reusable procedures remain scoped skills. `skill_manage` can create portable skills in the global
Hermes skill root or repository-specific skills in `<git-root>/.hermes/skills` with `scope=project`.
Project access requires the existing trust decision. Curator maintains those two libraries
independently; it does not scan other projects, `.agents/skills`, or bundled/external skill roots.

Only packages with valid `curator.json` metadata marked as managed, unpinned, and unchanged since
the last recorded write are eligible. Background-created skills are managed; foreground-created
skills remain user-managed. Older metadata is not migrated or automatically adopted. Use
`/curator adopt <skill-id>` only when you intentionally grant Curator control of that skill.

```text
/curator status
/curator run --scope project --dry-run
/curator run --scope project --consolidate
/curator pin project:my-repo:release
/curator backup --scope project
/curator rollback --scope project --list
/curator rollback --scope project
```

Status and runs default to both available scopes; `--scope global|project|all` selects libraries.
Bare skill names and rollback default to global, while `project:<repo-name>:<skill-name>` identifies
a current-project skill. Activity, pause state, archives, backups, and rollback are independent per
root, and consolidation never merges across scopes. Reload with `/reload` after creating,
archiving, restoring, or rolling back skills to refresh the skill catalog.

Automatic Curator checks run only in open, idle user sessions: by default, maintenance is due
every seven days after at least two idle hours; skills become stale after 30 inactive days and
eligible for archive after 90. Model-backed consolidation defaults off. Configure it through
`<agent-dir>/memory.json`, for example:

```json
{
  "version": 1,
  "provider": "hermes",
  "providers": {
    "hermes": {
      "curator": {
        "enabled": true,
        "intervalHours": 168,
        "minIdleHours": 2,
        "consolidate": false
      }
    }
  }
}
```

If present, `hermes-memory-config.json` in the agent directory takes precedence over the Hermes
provider options above; `memoryDir` can change the global memory directory. Set `enabled: false`
at the top level of `memory.json` to disable the memory provider. The former `local` provider and
its evaluation crate have been removed; existing configurations selecting `local` must be changed
to `hermes` or disabled.

The standalone `pi curator ...` accepts the same maintenance arguments. Inspection, local pruning,
and backup/restore need no model; `--consolidate` uses the normal provider setup. To preview a
project explicitly from the CLI:

```bash
pi --cwd /path/to/project --approve curator run --scope project --dry-run
```

See [the memory architecture](docs/architecture.md#memory-systems) for review policy and safeguards.

## Scheduled prompts

Use the `schedule` tool or `/schedule` to create self-contained prompts with a relative delay,
recurring interval, RFC3339 timestamp, or five-field cron expression:

```text
/schedule create {"name":"Repository summary","schedule":"every 30m","scope":"project","prompt":"Summarize commits from the last 30 minutes in this repository.","max_runs":3}
/schedule list project
/schedule history <job-id> project
/schedule pause <job-id> project
```

Keep a primary Pi session open in the job's working directory. Jobs dispatch when that session is
idle with no queued input and run in fresh isolated sessions without the parent conversation.
Global storage is the default; project storage requires trust. Both scopes run only jobs matching
the open cwd. Closing all matching sessions stops execution; this is not an OS background service.
Missed intervals coalesce rather than replaying a backlog, and uncertain interrupted attempts are
not automatically retried. See [the scheduling guide](plugins/features/pi-plugin-schedule/README.md)
for time zones, limits, storage, and recovery behavior.

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
/login [provider]           Configure OAuth or API-key authentication
/logout                     Remove a stored provider credential
/model [provider/model|id]  List or switch models
/thinking <level>           Change the thinking level
/compact [instructions]     Compact the context, optionally with guidance
/fork                       Branch from a previous user message
/clone                      Clone the session at its current position
/tree                       Navigate the current session tree
/name [name]                Show or set the session name
/session                    Show session path, usage, and cost
/copy                       Copy the last completed assistant response
/clear                      Clear the visible transcript
/help                       Show commands
/quit                       Exit
/skill:<name> [task]        Invoke a discovered skill explicitly
/refine [focus]            Review the conversation for reusable learning (Hermes)
/curator [arguments]       Inspect and maintain scoped Hermes skill libraries
/schedule [action]         Manage persistent scheduled prompts
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
- Opening an interrupted session reconciles accepted deferred writes and undelivered input, then
  closes the interrupted operation as aborted. It never performs provider I/O or blindly replays a
  tool with unknown external side effects.

## Architecture

| Directory                                        | Responsibility                                                                           |
| ------------------------------------------------ | ---------------------------------------------------------------------------------------- |
| `apps/pi-cli`                                    | CLI, TUI, and terminal lifecycle Adapter                                                  |
| `crates/pi-sdk`                                  | Shared headless product assembly for CLI, desktop, and embedded adapters                 |
| `crates/pi-core`                                 | Strongly typed contracts, registries, and plugin drivers                                 |
| `crates/pi-media`                                | Shared image validation, resizing, and format conversion                               |
| `crates/pi-agent`                                | Agent façade, agent loop, stream assembly, and tool scheduling                           |
| `crates/pi-runtime`                              | Generation construction, prompt assembly, and atomic reload                              |
| `crates/pi-session`                              | Pi v4 JSONL, tree/branch state, compaction, recovery reducer, and session runtime        |
| `crates/pi-rpc`                                  | Pi JSON projection and stdin/stdout RPC                                                   |
| `crates/pi-acp` / `pi-mcp`                      | ACP stable-v1 sessions and protocol-neutral MCP client/tool integration                  |
| `crates/pi-telemetry`                            | Typed provider/harness span schemas and sink adapters                                    |
| `crates/pi-provider`                             | Provider-neutral HTTP transport and SSE                                                  |
| `crates/pi-prompt` / `pi-resources`              | System prompt and project context discovery                                              |
| `apps/pi-cli/src/markdown`                       | TUI-owned Markdown parsing, streaming repair, syntax highlighting, and Ratatui rendering |
| `crates/pi-plugin-sdk` / `pi-plugin-loader`      | Native author interface, compatibility checks, discovery, and factory adapters           |
| `crates/pi-plugin-manager`                       | Package intent/lock, static Registry resolution, target selection, and CAS installation  |
| `crates/pi-js-package-manager`                   | Pi-compatible JS/TS discovery and local/npm/git package management                       |
| `crates/pi-js-plugin` / `bindings/pi-napi`       | Typed JS lifecycle adapters and the Node/NAPI boundary                                   |
| `packages/pi`                                    | Node launcher, Pi extension discovery, Jiti loader, and callback generations             |
| `plugins/`                                       | Prompt/skill features, provider catalog, and independent production tool plugins         |
| `plugins/features/pi-plugin-memory-hermes`       | Persistent memory, background learning, and scoped skill curation                       |
| `plugins/features/pi-plugin-schedule`            | Persistent scheduled prompts and isolated-session dispatch                              |
| `legacy/pi`                                      | Current TypeScript Pi behavioral oracle                                                  |
| `e2e`                                            | Runtime acceptance, black-box product E2E, and example projects                          |
| `scripts/perf`                                   | Rust/TypeScript performance measurements and an offline dashboard                       |

Dependencies point inward: core contracts do not own terminal behavior, filesystem discovery,
session storage, or vendor routing policy. See [docs/architecture.md](docs/architecture.md) for hook
ordering, persistence invariants, and the detailed design.

## Development and validation

Run the Rust / TypeScript performance suite and open its offline dashboard:

```bash
./scripts/bench-perf --open
```

The performance suite requires Node.js 22.19 or newer on macOS/Linux and a prepared local Pi
checkout for TypeScript comparisons. Use `--quick` for a smoke run or `--backend rust` without the
TypeScript oracle. Setup, measurement caveats, options, and raw JSON/CSV
outputs are documented in [scripts/perf/README.md](scripts/perf/README.md).

The Pi core conformance subset and its oracle mapping are documented in
[docs/pi-core-test-matrix.md](docs/pi-core-test-matrix.md). Run that focused set with:

```bash
./scripts/test-core.sh
```

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

Run the deterministic black-box product E2E stack, including the standalone CLI and Node/NAPI
adapters, with:

```bash
npm ci --prefix packages/pi
npm --prefix packages/pi run e2e
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

- Add explicit safe-tool/deferred replay adapters without replaying side effects during open.
- Add publisher signatures, Git/OCI sources, update/rollback, and CAS garbage collection to native
  plugin distribution.
- Continue using `legacy/pi` as the oracle for user-visible behavior and cross-platform terminal
  conformance.
