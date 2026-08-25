# pi CLI

`pi` is the terminal coding-agent product built by `pi-rs`. It can inspect a repository, edit files,
run commands, search code, manage long-running sessions, and work with multiple model providers from
one interactive terminal interface.

The CLI is a production Rust implementation that aims to match intentional, observable behavior in
current Pi rather than only providing an agent-loop library. Its fullscreen TUI, one-shot output,
NDJSON event stream, tools, sessions, model catalog, skills, and plugins all use the same runtime.

## Product highlights

- **Interactive coding workspace** — fullscreen Ratatui UI with streamed Markdown, syntax-highlighted
  code, CJK/IME input, command completion, history, scrolling, mouse selection, and clipboard copy.
- **Repository-aware agent** — built-in `read`, `write`, `edit`, `hashline_edit`, `bash`, `grep`,
  `find`, and `ls` tools for understanding and changing real projects.
- **Multiple providers and models** — built-in OpenAI-compatible, OpenAI Codex, Anthropic, Google,
  and xAI integrations, plus custom providers and models declared in `models.json`.
- **Authentication in the product** — `/login` and `/logout` manage Pi-compatible credentials from
  the TUI; browser/device OAuth and hidden API-key prompts are supported.
- **Persistent Pi v4 sessions** — resume previous work, queue steering or follow-up messages, branch
  from earlier messages, navigate the session tree, and compact long contexts.
- **Plugin-first customization** — Rust native plugins, Pi-compatible JavaScript/TypeScript
  extensions, skills, commands, provider hooks, and session lifecycle hooks.
- **Project safety** — nearest-ancestor project-trust decisions gate project `.pi` resources,
  extensions, skills, and native plugins before they load.
- **Automation-friendly frontends** — use the same agent through the interactive TUI, final-text
  `--print` output, or structured NDJSON `--json` events.

## Install and start

### npm package

The npm package is the recommended installation when JavaScript/TypeScript extension support is
needed. It requires Node.js 20 or newer and installs both `pi` and `pi-rs` commands.

```bash
npm install --global @pi-rs/cli
npm list --global @pi-rs/cli --depth=0
pi --version
pi
```

The package selects a native optional dependency for the current OS, CPU, and Linux libc. Supported
release targets are macOS arm64/x64, Linux glibc arm64/x64, and Windows MSVC arm64/x64. Both `pi`
and `pi-rs` invoke the same installed launcher; `npm list --global` confirms which npm version is
installed. If npm skips the native optional dependency, the launcher prints exact npx and
global-install repair commands for the installed CLI version and platform. This can happen when
optional dependencies are disabled or a configured npm registry mirror has not synchronized the
matching platform package.

### Run from source

For repository contributors with access, the project pins Rust 1.98.0 through
`rust-toolchain.toml`:

```bash
git clone https://github.com/CCherry07/pi-rs.git
cd pi-rs
cargo run -p pi-cli --
```

The standalone Rust binary does not embed a JavaScript VM. Use the npm/Node launcher when loading
Pi-compatible JavaScript or TypeScript extensions.

## First use

Start the TUI in a project, configure a provider, and select an available model:

```bash
pi --cwd /path/to/project
```

Then run these commands inside the TUI:

```text
/login
/model
```

`/login` opens a provider selector. API keys are read without echo; OAuth providers open their
browser or device flow. Authentication temporarily switches out of the fullscreen UI and returns to
the current session when complete. The session generation is rebuilt automatically so `/model`
immediately reflects the new credentials.

Authentication can also be managed before entering the TUI:

```bash
# Select a provider and authentication method interactively
pi auth login

# Start a browser/device OAuth flow directly
pi auth login anthropic --oauth
pi auth login openai-codex --oauth
pi auth login xai --oauth

# Prompt for an API key without echo
pi auth login anthropic --api-key
pi auth login google --api-key

# Inspect credential metadata without printing secrets
pi auth status

# Remove only the stored credential for a provider
pi auth logout anthropic
```

Credentials are stored in Pi-compatible `<agent-dir>/auth.json` with file locking, atomic
replacement, and mode `0600` on Unix. `/logout` removes stored credentials only; environment
variables and credentials declared in `models.json` remain unchanged.

Environment variables are supported as an alternative, including `OPENAI_API_KEY`,
`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_OAUTH_TOKEN`, `GEMINI_API_KEY`, and
`XAI_API_KEY`. Explicit `--api-key` takes precedence over stored credentials and provider
environment variables for the selected run.

## Product modes

| Mode            | Command                         | Best for                                                    |
| --------------- | ------------------------------- | ----------------------------------------------------------- |
| Interactive TUI | `pi`                            | Daily coding, exploration, edits, and long-running sessions |
| Main-screen TUI | `pi --no-fullscreen`            | Keeping output in the terminal scrollback                   |
| Final text      | `pi --print "prompt"`           | Shell scripts and one-shot answers                          |
| NDJSON events   | `pi --json "prompt"`            | Integrations that consume structured product events         |
| Piped input     | `printf 'prompt' \| pi --print` | Unix pipelines and generated prompts                        |

Shell shorthand works in every frontend and does not require provider credentials:

```bash
# Run a command and include its output in agent context
pi --print '!git status --short'

# Run a command without adding its output to agent context
pi --print '!!cargo test -p pi-core'
```

## Interactive workflow

While the agent is idle, `Enter` submits a new prompt. While it is working, `Enter` sends steering
input into the active turn and `Alt+Enter` queues a follow-up for the next turn. Tool calls, provider
errors, queued input, compaction, and session changes remain visible in the same transcript.

Common commands:

| Command                       | Purpose                                                               |
| ----------------------------- | --------------------------------------------------------------------- |
| `/login [provider]`           | Configure OAuth or API-key authentication                             |
| `/logout`                     | Select and remove a stored provider credential                        |
| `/model [provider/model\|id]` | Inspect or change the active model                                    |
| `/thinking <level>`           | Change reasoning depth for the active model                           |
| `/new [path]`                 | Start a new session                                                   |
| `/resume [query\|path]`       | Find and continue a previous session                                  |
| `/compact [instructions]`     | Compact the current context, optionally with guidance                 |
| `/fork`                       | Branch before a selected previous user message                        |
| `/clone`                      | Clone the session at its current position                             |
| `/tree`                       | Navigate the current session tree                                     |
| `/name [name]`                | Show or set the session name                                          |
| `/session`                    | Show session path, ID, messages, tokens, and cost                     |
| `/reload`                     | Atomically rebuild plugins, models, resources, and session extensions |
| `/trust`                      | Review or change trust for the current project                        |
| `/copy`                       | Copy the last completed assistant response                            |
| `/clear`                      | Clear the visible transcript without deleting the session             |
| `/help`                       | Show the command list                                                 |
| `/quit`                       | Exit the application                                                  |
| `/skill:<name>`               | Invoke a discovered skill                                             |

Plugin-provided commands are added to the same command palette as built-in commands and skills.

Key bindings:

| Key                      | Action                                                                 |
| ------------------------ | ---------------------------------------------------------------------- |
| `Enter`                  | Complete a selected command, submit while idle, or steer while running |
| `Alt+Enter`              | Queue a follow-up message                                              |
| `Ctrl+J`                 | Insert a newline                                                       |
| `Up` / `Down`            | Select a command or browse input history                               |
| `Tab`                    | Complete the selected command or skill                                 |
| `PageUp` / `PageDown`    | Scroll the transcript                                                  |
| `Ctrl+End`               | Jump back to the latest transcript content                             |
| Mouse drag               | Select transcript text                                                 |
| `Cmd+C` / `Ctrl+Shift+C` | Copy selected transcript text                                          |
| `Esc`                    | Close a focused view or interrupt active work                          |
| `Ctrl+C`                 | Close a view, clear the editor, interrupt work, or quit while idle     |
| `Ctrl+D`                 | Quit while idle with an empty editor                                   |

## Models and providers

The default agent directory is `~/.pi/agent`. Override it with `PI_AGENT_DIR` or `--agent-dir`.
Register custom providers and models in `<agent-dir>/models.json`:

```jsonc
{
  // Comments are supported.
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

Environment references and shell-command values in model configuration are resolved only when a
request is sent; credentials are not copied into the public model catalog. Initial model selection
uses this priority:

1. explicit `--model` / `--provider` arguments;
2. the model restored from a session, if still available;
3. the catalog default from `models.json`;
4. the runtime fallback.

After editing `models.json`, run `/reload`. A candidate generation is prepared and validated before
it replaces the active generation, so invalid configuration leaves the current session working.

## Sessions and context

Sessions use the Pi v4 JSONL format. A new session exists in memory immediately, but its file is
created only after the first assistant response completes. Quitting before that response, or using
only shell shorthand, does not leave an empty resume entry.

The product supports:

- session discovery and `/resume`;
- durable steering and follow-up queues;
- branching with `/fork`, `/clone`, and `/tree`;
- manual and automatic context compaction;
- model and thinking-level restoration;
- interrupted-state reduction and provider-context repair;
- preservation of matching tool calls and tool results across persistence and replay.

## Skills, resources, and project trust

Global resources load from the agent directory. Project resources are discovered from the current
project and its ancestors using Pi-compatible precedence.

```text
~/.pi/agent/
├── auth.json
├── models.json
├── settings.json
├── trust.json
├── SYSTEM.md
├── APPEND_SYSTEM.md
├── skills/
├── extensions/
├── plugins.json
├── plugins.lock
└── plugins/

<project>/.pi/
├── settings.json
├── skills/
├── extensions/
├── plugins.json
├── plugins.lock
└── plugins/
```

Project `.pi` resources are loaded only after the shared trust service approves the project.
Interactive runs prompt when needed; `--print` and `--json` default to untrusted unless a stored
decision or CLI flag decides otherwise. Use `--approve` / `-a` or `--no-approve` / `-na` for a
run-local override.

Like current Pi, `AGENTS.md` and `CLAUDE.md` context discovery is independent of project trust.
Skills under `~/.agents/skills` are also supported.

## Plugins and extensions

### Pi JavaScript/TypeScript extensions

The npm launcher calls the Rust `pi-js-package-manager`, which uses Pi's PackageManager discovery
precedence:

1. repeated `-e` / `--extension` sources;
2. local extension entries from trusted `<cwd>/.pi/settings.json`;
3. trusted `<cwd>/.pi/extensions` auto-discovery;
4. local extension entries from `<agent-dir>/settings.json`;
5. `<agent-dir>/extensions` auto-discovery;
6. extension resources from configured local, npm, or git packages.

Settings may declare direct extension files/directories and distributable packages:

```json
{
  "extensions": ["/path/to/extension.ts", "/path/to/extension-directory"],
  "packages": [
    "npm:@scope/pi-package@1.0.0",
    "git:github.com/example/pi-package@v1",
    {
      "source": "npm:filtered-package",
      "extensions": ["extensions/*.ts", "!extensions/legacy.ts"]
    }
  ]
}
```

Package `package.json` files may declare entry points under `pi.extensions`; otherwise an
`extensions/` convention directory is discovered. Missing npm/git packages are installed into the
Pi-managed user, project, or temporary package root. `PI_OFFLINE=1` disables missing-package
installation and temporary git refresh. `--no-extensions` disables settings, package, and automatic
discovery while preserving explicit `-e` sources, including `npm:` and `git:` sources.

Rust owns settings parsing, trust gating, source precedence, package filters, installation, and
canonical-path deduplication. Node receives only the ordered extension paths and owns Jiti import
plus JavaScript callback lifetimes.

The Rust PackageManager also owns the Pi-compatible management commands:

```bash
pi install npm:example-extension
pi install --local git:github.com/example/project-extension@v1 --approve
pi list
pi update --extensions
pi update npm:example-extension
pi remove npm:example-extension
```

User scope is the default; `--local` writes trusted `<cwd>/.pi/settings.json`. Exact npm versions
remain pinned during updates. Ranges and unversioned npm packages update in batches per scope, and
git packages reconcile their configured ref or upstream branch. Bare `pi update` is reserved for
Pi self-update and is not implemented by pi-rs yet; use `--extensions` to update every configured
JavaScript package.

```bash
# Installed npm launcher
pi --cwd /path/to/project
pi --no-extensions -e /path/to/extension.ts

# Development launcher
cd packages/pi
npm ci
npm run build:native
npm start -- --cwd /path/to/project
```

Extensions can currently register tools, commands, agent hooks, `before_provider_request`, and
session lifecycle hooks. Base contexts expose native read-only model/session state; command contexts
also expose wait, new/fork/switch/navigation, and reload operations. Managed installs intentionally
leave Pi SDK and TypeBox peer dependencies to the host. Runtime-neutral helpers such as
`defineTool`, `StringEnum`, `Type`, `CONFIG_DIR_NAME`, `VERSION`, `getAgentDir`, and the built-in
tool-result guards are available through current and legacy `pi-ai` / `pi-coding-agent` package
names; `typebox` and `@sinclair/typebox`, including subpaths such as `typebox/value`, resolve to the
host's bundled runtime.

JavaScript extensions do not own the Ratatui frontend: `ctx.hasUI` is false and `ctx.ui` is an
explicit no-op implementation. Hooks and registrations known to current Pi but not yet implemented
are reported as inactive and do not fail generation construction; genuinely unknown hook names
still fail so typos remain visible. See the repository's
[JavaScript compatibility matrix](../../docs/js-extension-compatibility.md) for the precise hook,
context, UI, and module status. Extension code runs in the Node process as trusted code; it is not
sandboxed.

### Native Rust plugins

Native plugins are version-locked dynamic libraries loaded from global manifests, trusted project
manifests, or explicit `--plugin` paths.

```bash
pi --plugin /path/to/plugin.dylib
pi plugin install /path/to/package
pi plugin install https://example.com/pi-plugin-release.json
pi plugin install registry:frontend-check@^1 \
  --registry https://plugins.example/index.json
pi plugin list
pi plugin sync
pi plugin remove <plugin-id>
```

Pass `-l` to plugin-management commands to operate on the trusted project's `.pi` configuration
instead of the global agent directory. Repository contributors can find the native author API in
`crates/pi-plugin-sdk/README.md` and package and registry formats in
`crates/pi-plugin-manager/README.md`.

## Development

Development spans two layers: the Rust workspace owns the agent runtime and terminal product, while
this package provides the TypeScript launcher and Pi-compatible JavaScript extension host. Node.js
20 or newer is required; the repository pins Rust 1.98.0 through `rust-toolchain.toml`.

Build the native bridge for the current machine, then start the complete Node-hosted product:

```bash
git clone https://github.com/CCherry07/pi-rs.git
cd pi-rs/packages/pi
npm ci
npm run check
npm test
npm run build:native
npm start -- --cwd ../..
```

`npm run build:native` compiles `pi-napi` for the current host and places the resulting `.node`
binding in this package. Set `PI_RS_NATIVE_BINDING` to an exact `.node`, `.dylib`, `.so`, or `.dll`
when testing a binding from another build location. Rebuild the binding after pulling Rust changes
or changing the workspace version; an existing local `.node` file is not refreshed automatically.

Useful package commands:

| Command                        | Purpose                                                   |
| ------------------------------ | --------------------------------------------------------- |
| `npm start -- [pi arguments]`  | Run the TypeScript launcher against the local binding     |
| `npm run check`                | Type-check the Node host without emitting files           |
| `npm test`                     | Test discovery, loading, release logic, and host behavior |
| `npm run test:native`          | Smoke-test the real Node → NAPI → Rust callback path      |
| `npm run build`                | Emit publishable JavaScript and declarations to `dist/`   |
| `npm run build:native`         | Build a debug native binding for the current host         |
| `npm run build:native:release` | Build a release native binding for the current host       |

To work on the standalone Rust CLI without the JavaScript host, run from the repository root:

```bash
cd ../..

# Fullscreen TUI
cargo run -p pi-cli --

# One-shot and NDJSON modes
cargo run -p pi-cli -- --print "summarize this repository"
cargo run -p pi-cli -- --json "list the Rust crates"
```

Before submitting a change, run both Node checks and the workspace quality gates:

```bash
cd packages/pi
npm run check
npm test

cd ../..

cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

## Packaging and release

Build a standalone archive and NAPI binding for the current target:

```bash
cd packages/pi && npm ci && cd ../..
./scripts/package-target.sh <rust-target>
```

Target builds produce a standalone archive, a platform NAPI binding, and SHA-256 checksums under
`dist/release/`. The target must match the current native host, for example
`aarch64-apple-darwin` on Apple Silicon or `x86_64-unknown-linux-gnu` on x64 glibc Linux. On macOS
or Linux, install a standalone archive with:

```bash
./scripts/install-package.sh

# Or choose an exact archive and destination
INSTALL_DIR=/usr/local/bin ./scripts/install-package.sh \
  dist/release/pi-<version>-<rust-target>.tar.gz
```

The npm release uses a small `@pi-rs/cli` root package plus exact-version native platform packages.
Platform packages publish first and the root package publishes last. Release Please owns the
version/changelog PR; the protected workflow publishes through npm Trusted Publishing OIDC and
verifies every registry tarball before publishing the draft GitHub release. Trusted Publishing must
be configured separately for the root package and all six platform packages, using repository
`CCherry07/pi-rs`, workflow `release.yml`, and environment `npm-publish`. The complete pipeline is in
`.github/workflows/release.yml`.

Release artifacts currently use checksums and native smoke tests but are not
Developer-ID/Authenticode signed or notarized. Never include `.env` files, API keys, or OAuth tokens
in a distribution artifact.
