# Sikemux integration

Sikemux can use its existing Pi RPC driver to launch the Node-hosted pi-rs CLI.
The desktop and core remain Sikemux processes; pi-rs owns provider execution,
extensions and native session history. The integration uses `--mode rpc`.

Pi's built-in GUI chat requires Sikemux
[0.6.1 or later](https://github.com/nodelike/sikemux/releases/tag/v0.6.1).
Sikemux 0.6.0 exposes Pi only in TUI mode; its agent catalog disables the GUI
selector. Upgrade the Sikemux application as well as selecting the pi-rs launcher.

The reference source is
[`nodelike/sikemux@118ec0f`](https://github.com/nodelike/sikemux/tree/118ec0f2d42b1b9471ee9a399a7dd62dc87cca67).
Its Pi driver injects `sikemux_pi_tools.ts`, probes models with `--no-session`,
and probes credentials with `auth check --json`.

## Executable

Use the Node-hosted executable from `packages/pi`. Sikemux's injected TypeScript
extension needs that host. A standalone Rust binary supports RPC but cannot load
the injected JavaScript extension by itself.

For a local development build:

```sh
npm --prefix packages/pi run build:native
npm --prefix packages/pi run build
chmod +x packages/pi/dist/bin/pi.js
"$PWD/packages/pi/dist/bin/pi.js" auth check --json
```

The executable path is the absolute path to `packages/pi/dist/bin/pi.js`.
The process launching it needs a PATH with Node 22.19 or newer. The package loader
finds the native artifact created by `build:native`. For an existing macOS
`cargo build -p pi-napi` build, `PI_RS_NATIVE_BINDING` can instead point to the
absolute `target/debug/libpi_napi.dylib` path.

Sikemux can discover this launcher as the normal `pi` command on its launch PATH.
If choosing a separate executable path, its underlying agent profile accepts an
`executablePath`, but the reference Profiles UI filters out Pi because its catalog
has `accounts = false`. A custom-path integration therefore needs to expose that
setting or provide the profile through the existing core configuration API. An
executable path is one program path, not a shell command with arguments.

## Runtime behavior

- Model discovery accepts `get_state` and `get_available_models` in one input batch
  followed by stdin EOF, and flushes both responses before exit. Pending extension
  commands remain cancellable when the client disconnects.
- `--no-session` creates an in-memory session. Replies, new-session replacements,
  reloads and forks retain that storage mode. Initial saved-session lookups are
  skipped; `--session-id` can still name the fresh session. An explicit later
  `switch_session` can open stored history.
- `--skill <path>` loads Sikemux's skill library. It accepts repeated paths to
  directories or Markdown files; relative paths resolve from the startup cwd.
  Explicit skill paths remain active when a session is replaced or reloaded.
- `auth check --json` reports configured readiness with exit codes 0 (ready),
  1 (not ready) and 2 (invalid selection or configuration). Bare invocation checks
  the configured/default model; this extends the reference Pi command, which
  requires `--provider` or `--model`. It is not a network health check or an
  inventory of every connected account. OAuth credentials may refresh when
  needed; `--no-refresh` inspects existing configuration without refreshing.
- A persistent draft's `get_state.sessionFile` is its intended path even before
  its first assistant response. The file still materializes lazily. In-memory
  sessions omit that field. Clients can retain the path and later resume it with
  `--session` after a response has been saved.
- Prompt responses report `started`, `queued` or `handled` after input preparation
  succeeds. Preparation errors return a failed response; handled commands need
  no subsequent `agent_settled` event.
- RPC dialogs use `extension_ui_request` with `method: "confirm"`, `"select"`,
  `"input"` or `"editor"` and correlated `extension_ui_response` replies, including
  during session startup. Text answers preserve empty strings and multiline content.
  Cancellation, timeout, generation retirement and disconnect release pending questions;
  a refused or cancelled confirmation does not authorize the tool. Select/input/confirm
  deadlines are sent to the desktop. Notifications use `method: "notify"` from startup
  through shutdown and share an ordered output channel with dialogs and responses.
- The reference Sikemux driver already handles these dialogs and notifications.
  Arbitrary terminal components, component widgets, headers and footers retain legacy
  Pi's inactive RPC behavior. Status, text widgets, title and editor-text updates remain
  outside pi-rs's RPC subset; the reference Sikemux driver also ignores those methods.

## Verification

The deterministic bridge suite starts the actual Node launcher and NAPI runtime,
uses an isolated agent directory and a local mock provider, and covers stable paths,
resume, temporary sessions, command receipts, permission decisions, semantic dialogs,
startup notifications, cancellation, timeouts and generation replacement:

```sh
PI_RS_NATIVE_BINDING="$PWD/target/debug/libpi_napi.dylib" \
  npm --prefix packages/pi run test:bridge
```

To repeat the tool-confirmation scenarios with Sikemux's actual extension, set
`PI_RS_SIKEMUX_EXTENSION_PATH` to the absolute path of
`crates/sikemux-agents/resources/sikemux_pi_tools.ts` in a checkout at the reference
revision, then run the same suite. Without that variable the suite uses a minimal
extension exercising the same permission contract. These checks exercise the
process protocol; desktop/core UI acceptance is a separate check.

The native suite can include the reference Sikemux extension's actual MCP tool
registration/execution and permission hook. Set `PI_RS_STANDALONE_BINARY` to an absolute native `pi` executable to include
the standalone readiness/exit-code checks alongside the Node launcher checks.

## Native history

RPC `get_entries` projects pi-rs v4 history into Pi-compatible entries and supports
resume through the driver. Sikemux also has a separate disk scanner that expects
a v3 `type: "session"` header and renames by appending `session_info`. pi-rs stores
a v4 `kind: "header"` journal. Desktop-wide discovery and rename need a
Sikemux-side implementation that understands this format or uses pi-rs session
operations. Preserve the v4 schema and shared mutation sequence when adding that
support.
