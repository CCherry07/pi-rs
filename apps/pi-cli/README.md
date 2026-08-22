# pi CLI

The product entry point for `pi_rs`. It uses the same `AgentSessionRuntime`
for interactive, print, and NDJSON modes.

```bash
# Interactive fullscreen TUI; ~/.pi/agent/models.json owns the default model
cargo run -p pi-cli --

# An explicit CLI model still has highest priority
OPENAI_API_KEY=... cargo run -p pi-cli -- --model gpt-4o-mini

# One-shot output
cargo run -p pi-cli -- --print "summarize this repository"

# Product event stream
cargo run -p pi-cli -- --json "list the Rust crates"

# Shell shorthand (works without provider credentials)
cargo run -p pi-cli -- --print '!git status --short'
cargo run -p pi-cli -- --print '!!git status --short'

# Load a development native plugin; repeat --plugin to preserve plugin order
cargo run -p pi-cli -- --plugin /path/to/plugin.dylib

# Install a local or remote native plugin package
cargo run -p pi-cli -- plugin install /path/to/package
cargo run -p pi-cli -- plugin install https://example.com/pi-plugin-release.json

# Resolve a plugin from a static Registry
cargo run -p pi-cli -- plugin install registry:frontend-check@^1 \
  --registry https://plugins.example/index.json
```

Pi-compatible JavaScript and TypeScript extensions use the Node/NAPI launcher rather than the
standalone Rust binary:

```bash
cd packages/pi
npm install
npm run build:native

# Discovers trusted <cwd>/.pi/extensions and <agent-dir>/extensions
npm start -- --cwd /path/to/project

# Explicit extension paths preserve argument order; -ne disables discovery
npm start -- -ne -e /path/to/extension.ts
```

The Node launcher still calls this app library, so TUI/fullscreen ownership remains in
`apps/pi-cli` and every frontend mode uses the same `AgentSessionRuntime`. `/reload` atomically
rebuilds JS callbacks together with Rust/native plugins, models, resources, and session plugins.
See [`packages/pi/README.md`](../../packages/pi/README.md) for the supported Pi API surface and
current explicit limitations. Passing `--extension` to the standalone `cargo run -p pi-cli` entry
does not create a JavaScript VM; use the Node launcher for those paths.

Project-local `.pi` prompts and skills, plus ancestor `.agents/skills`, use the
same trust policy as Pi. Decisions are inherited from the nearest saved
ancestor in `<agent-dir>/trust.json`. Interactive runs prompt when needed;
print/JSON runs default to untrusted unless settings or flags decide otherwise.
Use `--approve`/`-a` or `--no-approve`/`-na` for a run-local override, and set
`defaultProjectTrust` to `ask`, `always`, or `never` in global `settings.json`.
Global resources remain available from `--agent-dir`, `PI_AGENT_DIR`,
`~/.pi/agent`, and `~/.agents/skills`. Like Pi, `AGENTS.md` and `CLAUDE.md`
context discovery is independent of project trust.

Native plugin manifests below `<agent-dir>/plugins` are always considered. Project manifests below
`<cwd>/.pi/plugins` are opened only after the same project-trust decision succeeds. `--plugin`
accepts a dynamic library, `pi-plugin.toml`, or a package directory; see
[`crates/pi-plugin-sdk/README.md`](../../crates/pi-plugin-sdk/README.md) for the author interface and
manifest format.

`pi plugin install/list/remove/sync` manages global `<agent-dir>/plugins.json` and `plugins.lock`.
Pass `-l` to use the trusted project's `.pi/plugins.json` and `.pi/plugins.lock`. Installed artifacts
live as immutable blobs under `plugins/store/sha256`, and `plugins/installed` exposes the current
ordered activation view to the loader. Static Registry, release, and lock schemas are documented in
[`crates/pi-plugin-manager/README.md`](../../crates/pi-plugin-manager/README.md).

Startup automatically reconciles global `plugins.json` and trusted project intent before native
plugins load; `/reload` performs the same transaction for an active session. Existing lock versions
remain pinned, so this repairs activation and applies edited options or rebuilt local artifacts
without turning startup into an implicit package update. `pi plugin sync` remains available for an
explicit forced reconciliation.

Interactive keys:

- `Enter`: complete the selected command, or submit while idle / steer while running
- `Alt+Enter`: queue follow-up
- `Ctrl+J`: insert a newline
- `Up` / `Down`: select a matching slash command or skill; otherwise recall older/newer input
- `Tab`: complete the selected slash command or skill
- `PageUp` / `PageDown`: scroll the transcript
- `Ctrl+End`: return to the latest transcript content
- `Esc`: abort and restore undelivered queue items to the editor
- `Ctrl+C`: clear the editor, or quit when it is empty

The TUI uses the terminal alternate screen by default. Pass `--no-fullscreen`
to keep it on the main terminal screen; the existing `--fullscreen` flag is
still accepted for compatibility.

Interactive commands include `/new`, `/resume`, `/reload`, `/trust`, `/model`,
`/thinking`, `/compact`, `/clear`, `/help`, and `/quit`. Plugin commands are
read from the active runtime generation; discovered skills appear as
`/skill:<name>`.

## Package for Apple Silicon macOS

Run the release packaging script on an Apple Silicon Mac:

```bash
./scripts/package-macos-arm64.sh
```

It performs a locked release build for `aarch64-apple-darwin`, verifies the
binary architecture, strips debug symbols, and creates these ignored artifacts:

```text
dist/pi-<version>-aarch64-apple-darwin.tar.gz
dist/pi-<version>-aarch64-apple-darwin.tar.gz.sha256
```

Install the newest package from `dist/` into `~/.local/bin`:

```bash
./scripts/install-package.sh
```

You can also provide an archive explicitly or choose another destination:

```bash
INSTALL_DIR=/usr/local/bin ./scripts/install-package.sh dist/pi-<version>-aarch64-apple-darwin.tar.gz
```

The installer checks the host architecture, verifies the adjacent `.sha256`
file when present, validates the packaged binary, and installs it atomically.

The version comes from `apps/pi-cli/Cargo.toml`. For an alpha or otherwise
custom artifact name, override it without changing the crate version:

```bash
PI_VERSION=0.1.0-alpha.1 ./scripts/package-macos-arm64.sh
```

The package is currently unsigned and not notarized. Its included `README.txt`
contains installation, provider configuration, and macOS quarantine guidance.
Never include `.env` files or API keys in a distribution archive.
