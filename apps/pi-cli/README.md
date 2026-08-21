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
```

Project-local `.pi` prompts and skills, plus ancestor `.agents/skills`, use the
same trust policy as Pi. Decisions are inherited from the nearest saved
ancestor in `<agent-dir>/trust.json`. Interactive runs prompt when needed;
print/JSON runs default to untrusted unless settings or flags decide otherwise.
Use `--approve`/`-a` or `--no-approve`/`-na` for a run-local override, and set
`defaultProjectTrust` to `ask`, `always`, or `never` in global `settings.json`.
Global resources remain available from `--agent-dir`, `PI_AGENT_DIR`,
`~/.pi/agent`, and `~/.agents/skills`. Like Pi, `AGENTS.md` and `CLAUDE.md`
context discovery is independent of project trust.

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
