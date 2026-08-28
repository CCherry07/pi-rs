# pi CLI

`pi` is the terminal coding-agent product built by `pi-rs`. It can inspect a repository, edit files,
run commands, search code, manage long-running sessions, and work with multiple model providers from
one interactive terminal interface.

The CLI is a Rust implementation of current Pi behavior rather than only an agent-loop library. Its
fullscreen TUI, one-shot output, NDJSON event stream, tools, sessions, model catalog, skills, and
plugins all use the same production runtime.

## Product highlights

- **Interactive coding workspace** — fullscreen Ratatui UI with streamed Markdown, syntax-highlighted
  code, CJK/IME input, command completion, history, scrolling, whole-screen mouse selection, and
  clipboard copy.
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
pi --version
pi
```

The package selects a native optional dependency for the current OS, CPU, and Linux libc. Supported
release targets are macOS arm64/x64, Linux glibc arm64/x64, and Windows MSVC arm64/x64. If npm
skips that optional dependency, the launcher prints exact npx and global-install repair commands
for the installed CLI version and platform, including the public registry override needed when a
mirror is stale.

### Run from source

The repository pins Rust 1.98.0 through `rust-toolchain.toml`.

```bash
git clone https://github.com/CCherry07/pi-rs.git
cd pi-rs
npm install --prefix packages/pi
./scripts/pi-dev
```

`scripts/pi-dev` incrementally builds the current host NAPI library and explicitly selects it for
the Node launcher. The standalone Rust adapter does not embed a JavaScript VM; when JavaScript
extension configuration is active it exits with an actionable launcher error instead of silently
omitting extensions. Use `cargo run -p pi-cli -- --no-extensions` for an intentional native-only
run.

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
`XAI_API_KEY`. Explicit CLI credentials take precedence over stored credentials.

## Product modes

| Mode            | Command                         | Best for                                                    |
| --------------- | ------------------------------- | ----------------------------------------------------------- |
| Interactive TUI | `pi`                            | Daily coding, exploration, edits, and long-running sessions |
| Main-screen TUI | `pi --no-fullscreen`            | Terminal-native selection and main-screen output            |
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

The startup card lists successfully resolved JavaScript/TypeScript packages by their effective
`settings.json` `packages` source (for example `npm:@narumitw/pi-lsp@0.49.5`) instead of guessing a
name from an entry path such as `dist/index.ts`. Explicit files and automatic `.pi/extensions`
entries still use compact path labels. Its Rust plugin row contains only successfully loaded native
plugins configured through global or trusted-project `plugins.json`; built-in Rust plugins and
explicit `--plugin` paths are intentionally omitted. Both rows refresh after `/reload` and session
replacement.

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
| Mouse drag               | Select and copy any visible TUI text in fullscreen mode                |
| `Cmd+C` / `Ctrl+Shift+C` | Re-copy the selection when the terminal forwards the shortcut         |
| `Esc`                    | Close a focused view or interrupt active work                          |
| `Ctrl+C`                 | Close a view, clear the editor, interrupt work, or quit while idle     |
| `Ctrl+D`                 | Quit while idle with an empty editor                                   |

Fullscreen selection is application-owned because mouse capture is needed for transcript scrolling.
It covers the complete final frame, including the transcript, startup card, composer, completion
panel, focused bottom views, project-trust prompt, context panel, and footer. Selection highlighting
is painted after every widget, and active selection pauses animation ticks so copyable text remains
stable. Releasing a non-empty drag writes the selected text immediately because terminal emulators
commonly consume their native copy shortcuts instead of forwarding them to a fullscreen TUI.
`Cmd+C` / `Ctrl+Shift+C` remain best-effort re-copy shortcuts for terminals that do forward them.
`--no-fullscreen` does not capture the mouse and leaves visible-text selection to the terminal.
`/copy` remains a separate semantic operation that copies the last completed assistant response
without requiring a visible selection.

Clipboard writes use the local native clipboard first. Over SSH they use tmux forwarding when
available and then OSC 52 so the text reaches the local terminal; WSL additionally falls back to
PowerShell. OSC 52 payloads are bounded before encoding.

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
request is sent; credentials are not copied into the public model catalog. Custom routes support
`openai-completions`, `openai-responses`, `anthropic-messages`, and `google-generative-ai`.
xAI-compatible Responses gateways use bearer authentication and accept an API base URL (typically
ending in `/v1`) or a complete `/responses` `baseUrl`. Anthropic-compatible gateways accept a
service-root, `/v1`, or complete `/v1/messages` URL and use `x-api-key`; Google routes use
`x-goog-api-key` and the Generative AI streaming endpoint.

Pi-compatible provider/model/upsert/override precedence is preserved. Model overrides support all
catalog fields, including partial cost updates, per-key `samplingParams`, headers, and typed
protocol `compat`. Pricing tiers use the highest matching total-input threshold for the whole
request. Compatibility settings alter request serialization, streaming expectations, caching,
routing, thinking, deferred tools, and session-affinity behavior rather than remaining metadata.
Dynamic `oauth: "radius"` catalogs still require a future OAuth/remote-catalog/`pi-messages`
provider implementation and are rejected explicitly for now.
Initial model selection uses this priority:

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

The npm launcher delegates discovery to the Rust `pi-js-package-manager`, using current Pi
PackageManager precedence:

1. repeated `-e` / `--extension` local, npm, or git sources;
2. trusted project `settings.json` extension entries;
3. trusted project `.pi/extensions` auto-discovery;
4. user `settings.json` extension entries;
5. user `extensions` auto-discovery;
6. extension resources from configured local, npm, or git packages.

Package manifests, package filters, ignore files, canonical-path deduplication, missing-package
installation, and `PI_OFFLINE` behavior follow the same discovery layer. `--no-extensions` keeps
explicit `-e` sources but disables settings, package, and automatic discovery.

The Node host receives only the resolved ordered paths; Jiti import and JavaScript callbacks remain
in Node while discovery and install policy remain in Rust.

Package configuration and managed npm/git installs use the same Rust PackageManager:

```bash
pi install npm:example-extension
pi install --local git:github.com/example/project-extension@v1 --approve
pi list
pi update --extensions
pi update npm:example-extension
pi remove npm:example-extension
```

`install` and `remove` use user scope by default; `--local` selects trusted project scope. Exact npm
versions stay pinned during update, while npm ranges and unversioned packages update in their
managed scope. Git updates reconcile the configured ref or the checkout's upstream branch. Bare
`pi update` retains current Pi's self-update meaning; pi-rs does not yet implement self-update, so
use `--extensions` for all JavaScript packages.

```bash
# Installed npm launcher
pi --cwd /path/to/project
pi --no-extensions -e /path/to/extension.ts

# Development launcher
npm install --prefix packages/pi
./scripts/pi-dev --cwd /path/to/project
```

See [`packages/pi/README.md`](../../packages/pi/README.md) for the supported Pi extension API and
explicit compatibility gaps.

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
instead of the global agent directory. See
[`crates/pi-plugin-sdk/README.md`](../../crates/pi-plugin-sdk/README.md) for the native author API and
[`crates/pi-plugin-manager/README.md`](../../crates/pi-plugin-manager/README.md) for package and
registry formats.

## Development

CLI startup and frontend selection live in `src/lib.rs`; `src/session_factory.rs` assembles the
production runtime generation. Terminal ownership remains in this crate, while reusable agent,
provider, plugin, resource, and session behavior stays in the workspace libraries.

Run the CLI directly during development:

```bash
# Complete Node-hosted product with JavaScript extensions
./scripts/pi-dev

# Intentional native-only modes
cargo run -p pi-cli -- --no-extensions
cargo run -p pi-cli -- --no-extensions --print "summarize this repository"
cargo run -p pi-cli -- --no-extensions --json "list the Rust crates"

# Quality gates
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

## Packaging and release

Build a standalone archive and NAPI binding for the current target:

```bash
cd packages/pi && npm install && cd ../..
./scripts/package-target.sh aarch64-apple-darwin
```

Target builds produce a standalone archive, a platform NAPI binding, and SHA-256 checksums under
`dist/release/`. On macOS or Linux, install a standalone archive with:

```bash
./scripts/install-package.sh

# Or choose an exact archive and destination
INSTALL_DIR=/usr/local/bin ./scripts/install-package.sh \
  dist/release/pi-<version>-<rust-target>.tar.gz
```

The npm release uses a small `@pi-rs/cli` root package plus exact-version native platform packages.
Platform packages publish first and the root package publishes last. Release Please owns the
version/changelog PR; the protected workflow publishes through npm Trusted Publishing OIDC and
verifies every registry tarball before publishing the draft GitHub release. See
[`packages/pi/README.md`](../../packages/pi/README.md#distribution--发布) for the release workflow.

Release artifacts currently use checksums and native smoke tests but are not
Developer-ID/Authenticode signed or notarized. Never include `.env` files, API keys, or OAuth tokens
in a distribution artifact.
