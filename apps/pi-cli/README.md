# pi CLI

The product entry point for `pi_rs`. Interactive, print, and NDJSON adapters all enter through
`PiApplication` and a managed `PiSession`.

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
`apps/pi-cli`; every frontend mode uses the same `PiApplication` / `PiSession` Interface. The
application keeps active-session bookkeeping private, while `/reload` atomically rebuilds JS
callbacks together with Rust/native plugins, models, resources, and session plugins.
Print and NDJSON pin the handle's current generation for their one-shot invocation; the TUI also
observes session replacements for `/new`, `/resume`, `/fork`, and `/reload`.
CLI startup stays in `lib.rs`; `session_factory.rs` is the production Adapter that assembles each
complete runtime/session generation behind `PiApplication`.
See [`packages/pi/README.md`](../../packages/pi/README.md) for the supported Pi API surface and
current explicit limitations. Passing `--extension` to the standalone `cargo run -p pi-cli` entry
does not create a JavaScript VM; use the Node launcher for those paths.

The TUI implementation follows a tui-realm-style model/message/module split without handing terminal
ownership to the framework. `tui.rs` owns `App`, terminal setup, and the async event loop;
`tui/message.rs` defines semantic state-update messages; `tui/controller.rs` routes input and runs
commands/effects; `tui/view.rs` owns layout and Ratatui rendering; and `tui/components` contains the
stateful composer and selection-list modules. Runtime notifications are reduced through
`App::update`, while terminal keys remain controller inputs. Stateful selectors use
`tui-realm-stdlib::List` through the local selector seam. The multiline composer continues to use
`ratatui-textarea`: tui-realm's standard `Textarea` is a read-only scrolling text component and
cannot preserve Pi's cursor editing, undo, or newline behavior. Pi commands and runtime semantics
remain authoritative; Codex-only account and service surfaces are not synthesized.

Finalized transcript Markdown is cached by terminal width and appearance. Redraw requests are
coalesced to the same 120 FPS ceiling used by Codex, and raw wheel-event density is normalized for
common terminal emulators so trackpad and mouse scrolling do not enqueue redundant full renders.
While a turn is waiting for output, the `Working` label uses a moving shimmer and shows compact
elapsed time beside the interrupt hint. Inline code uses a clean accent foreground, while fenced
code keeps a subtle full-width surface, language label, padding, and syntax highlighting.

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
- Mouse drag: select transcript text
- `Cmd+C` / `Ctrl+Shift+C`: copy the selected transcript text
- `Ctrl+End`: return to the latest transcript content
- `Esc`: dismiss the active completion/view first; while work is active, interrupt and restore
  undelivered queue items to the editor
- `Ctrl+C`: dismiss a focused view, clear a non-empty editor, interrupt active work, or quit while
  idle with an empty editor
- `Ctrl+D`: quit while idle with an empty editor

The TUI uses the terminal alternate screen by default. Pass `--no-fullscreen`
to keep it on the main terminal screen; the existing `--fullscreen` flag is
still accepted for compatibility.

Interactive commands include `/new`, `/resume`, `/reload`, `/trust`, `/model`,
`/thinking`, `/compact [instructions]`, `/fork`, `/clone`, `/tree`, `/name [name]`, `/session`,
`/copy`, `/clear`, `/help`,
and `/quit`. Plugin commands are read from the active runtime generation; discovered skills appear
as `/skill:<name>`.

## Multi-platform packaging

Install the Node release tooling once, then package the target matching the current host:

```bash
cd packages/pi && npm install && cd ../..
./scripts/package-target.sh aarch64-apple-darwin
```

The supported matrix is macOS arm64/x64, Linux glibc arm64/x64, and Windows MSVC arm64/x64. Each
target is built on a matching CI runner rather than being advertised from an unexecuted
cross-compile. A target build performs locked release builds for both `pi-cli` and `pi-napi`, runs a
standalone shell smoke, strips supported native artifacts, and creates:

```text
dist/release/pi-<version>-<rust-target>.tar.gz     # macOS/Linux
dist/release/pi-<version>-<rust-target>.zip        # Windows
dist/release/pi-napi.<platform-suffix>.node
dist/release/*.sha256
```

`package-macos-arm64.sh` remains a compatibility Adapter for the Apple Silicon target. On macOS or
Linux, install the newest archive matching the current host into `~/.local/bin`:

```bash
./scripts/install-package.sh
```

You can also provide an archive explicitly or choose another destination:

```bash
INSTALL_DIR=/usr/local/bin ./scripts/install-package.sh \
  dist/release/pi-<version>-<rust-target>.tar.gz
```

The installer checks the host architecture, verifies the adjacent `.sha256`
file when present, validates the packaged binary, and installs it atomically.

The authoritative product version is `[workspace.package].version` in the root `Cargo.toml`;
`pi-cli` and `pi-napi` inherit it, while the release check requires the npm version and Git tag to
match. Arbitrary artifact-version overrides are intentionally rejected.

After CI collects every target, `npm run release:assemble` creates one native npm package per target
and a JavaScript-only `@pi-rs/cli` root package. Platform packages publish first and the root package
last. Release Please owns the version/changelog PR and dispatches the tag workflow after creating a
draft release. The protected `npm-publish` environment uses Trusted Publishing OIDC without an npm
token; the draft release is published only after the workflow verifies all registry tarballs. See
[`packages/pi/README.md`](../../packages/pi/README.md#distribution--发布).

The current artifacts use checksums and native smoke tests but are not Developer-ID/Authenticode
signed or notarized. Never include `.env` files or API keys in a distribution archive.
