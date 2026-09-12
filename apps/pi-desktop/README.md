# Pi

Pi is the desktop client for pi-rs. It runs agent sessions in-process
through native Pi runtime state.

The imported UI is based on CodexMonitor commit
`dd61b9abd37de5ded86e82b9fe8a83fd49d46fa5` and remains under its MIT license.

## Runtime

```text
React UI
  -> Tauri commands + thread event projection
  -> PiRuntimeState / SessionStore
  -> MultiSessionManager / PiSession
  -> pi-rs providers, tools, resources, prompts, and skills
```

The Tauri boundary projects Pi session events into the UI's `thread/*`,
`turn/*`, and `item/*` vocabulary.

Currently wired:

- workspace persistence;
- list, create, resume, fork, rename, archive, and compact Pi sessions;
- text and image submission, steering, interruption (including observed subagent runs), and live
  streaming;
- collapsible child-agent chat panels embedded in the parent conversation, with lazy history
  loading, live messages/reasoning/tools, independent scrolling and retryable read errors;
- independent child timelines: `fork` parent history stays in a collapsed, read-only creation
  snapshot rather than appearing as child messages; `fresh` explicitly reports no inherited
  history. The source session and available snapshot boundary are shown above the child chat,
  and later parent messages are never pulled into the snapshot automatically;
- reasoning, generic tool, shell output, error, usage, and native plugin notice events;
- live-session native command and skill slash completion, with argument hints;
- single-pass runtime command submission, including handled commands, transformed prompts,
  queued input, and command errors;
- event-subscription and command-catalog refresh when a native command replaces its `PiSession`;
- the Pi runtime model catalog, provider-qualified model selection, and per-session thinking level;
- Git, file browser, prompt, terminal, and layout UI.

The desktop-only infrastructure is limited to the app updater, release notes,
and Sentry. Provider credentials come from the Pi runtime environment. Pi uses
operating-system permissions directly, so the desktop does not expose a separate
access or approval mode.

## Commands and runtime boundaries

Completion reads `command_specs()` from the selected thread's current generation. When a
workspace has no selected thread, Desktop prepares one reusable in-memory draft and exposes its
plugin commands and skills in both the workspace home input and the composer. The model selector
shares that prepared runtime. Preparing a draft does not add a sidebar thread or a resume entry;
the first submission uses that same session, and persistence still waits for the first assistant
response. Background generation uses separate sessions. Failed reloads leave the previous
catalog and event subscription intact. Successful native session navigation changes the selected
thread identity; the old saved thread is not silently rebound to the new session.

Desktop actions `/compact`, `/fast`, `/fork`, `/new`, `/resume`, and `/status` retain priority
(case-insensitive exact command names) in both completion and dispatch. `/prompts:*` remains
the Desktop custom-prompt namespace. Conflicting runtime names are hidden, not advertised with
an execution path that runs another command. In particular `/resume` refreshes the Desktop
thread, and `/fast` retains the imported UI setting; the native adapter does not forward a
service-tier setting to providers. These are Desktop presentation choices, not Pi TUI parity.

Other slash commands go through `AgentSession.submit` exactly once; a handled command need not
start an agent turn or create a session file. Tauri waits for the real submit receipt while Pi
product events stream independently. The local follow-up queue retains Desktop queue/steer
intent; steering uses native preprocessing and handled commands do not stall queue draining.
Plugin notices are transient transcript entries with severity, not provider messages or saved
session entries. Notices render separately from assistant messages. Desktop follows the latest
session through `PiSession::subscribe()` and refreshes history, status, and commands from one
subscription snapshot, including same-ID reloads. Intermediate replacements may be coalesced;
transient notices emitted before the new subscription is installed (including `session_start`
notices) are not replayed. The composer visibly warns when Desktop action names hide runtime commands.

Only commands registered by the Rust runtime (including built-in feature plugins and loaded
native plugins) are exposed. The Desktop does not host legacy Pi JavaScript runtime extensions,
TUI widgets, argument-completion callbacks, or interactive extension dialogs. The separate React
presentation extension interface is described below. CLI-only
frontend commands such as `/reload` are not implicitly registered here; a native command may
use the runtime reload/navigation capabilities. Project native plugins still use shared Pi
project trust; desktop presentation packages consume that same decision.

## Configuration

```bash
export OPENAI_API_KEY=...
export OPENAI_BASE_URL=https://api.openai.com/v1
export OPENAI_MODEL=gpt-4o-mini
```

`OPENAI_BASE_URL` and `OPENAI_MODEL` are optional. A local OpenAI-compatible
endpoint can omit `OPENAI_API_KEY`. Set `PI_AGENT_DIR` to override the default
`~/.pi/agent` session/resources directory.

## MCP settings

Settings → MCP manages independent global and trusted-project `mcp.json` files without creating
a session. Add/select a server for basic fields, toggle enabled state, or use the `mcp.json` tab
to edit headers, env, cwd and unknown/advanced fields directly. New servers start disabled.
Save writes atomically with external-change detection; Test connects only to saved, enabled
configuration and reports discovered tools. Saving does not alter an existing runtime:
run `/mcp reload` in that conversation (or restart the app). `/mcp` and `/mcp paths` also work
through the normal registered-command path. Project entries replace global entries by name.

Supported transports are stdio and Streamable HTTP (JSON/SSE responses). Legacy HTTP+SSE and
interactive OAuth are not implemented. Use environment references such as `Bearer ${MCP_TOKEN}`
in headers. See the [CLI MCP configuration guide](../pi-cli/README.md#mcp-servers).

## Native plugin settings

Settings → Plugins manages global `<agent-dir>/plugins.json` and trusted-project
`<project>/.pi/plugins.json` package state. Browse or search installed/configured Native plugins,
inspect source/version/type/target/checksum, install from a local package directory, HTTP release
manifest, GitHub Release reference or static registry, explicitly sync, and remove packages.
A registry source needs a registry URL (or `PI_PLUGIN_REGISTRY`). Sync preserves satisfying locked
versions; it is not an update command. Relative global sources resolve from the agent directory;
use the folder picker or an absolute path when installing local packages.

Browsing is read-only: it does not download, reconcile, load native code, create a session, or
approve a project. Project access uses the existing trust policy and is checked again on the
backend for every operation. Installation does not prove native ABI compatibility. Native plugins
are trusted in-process code, not sandboxed extensions; SHA-256 checks content integrity, not
publisher identity.

The page shows package records separately from the configured Native plugin IDs observed in an
explicit current session. That runtime inventory does not include built-ins or explicit-path
plugins and cannot identify a loaded package's version, hash or installation scope. Package
operations change disk state only. Explicitly reload the current conversation to apply changes;
other conversations are not reloaded. A failed reload retains the previous runtime generation.
This page does not provide enable switches, plugin-specific settings forms, JavaScript extension
hosting, or a marketplace.

## Development

```bash
npm install
npm run typecheck
npm run test
npm run tauri:dev
```

For the Rust side:

```bash
cd src-tauri
cargo check
```

## Attribution

- UI foundation: CodexMonitor, MIT license.
- Agent runtime: pi-rs.

See `LICENSE` and `THIRD_PARTY_NOTICES.md`.

## Skill management

Settings → Skills supports global/project catalog browsing, Markdown/source preview,
new skills, local folder or Markdown import, editing, copying absolute document paths,
and moving skills to the OS Trash. The list includes parser/collision diagnostics and
uses actual file paths independently of slash-command availability. Existing sessions
are not reloaded when files change; restart the application and start a new conversation
to refresh their generation. Project writes require the existing project trust decision.
Symlink paths remain read-only; imports reject links/special files. Failed Trash operations
never fall back to permanent deletion. Viewing skills does not count as Agent usage.


## React plugin views

Plugins can write React, Hooks and CSS and optionally compose the shared `ToolCard`, `StatusBadge`,
`Markdown`, `SessionView` and `InlineCommandForm` modules from `@pi-rs/desktop-sdk`. Typed
`defineWidgetChannel` / `useWidget` and `usePluginCommand` adapters keep decoding and command names
inside the plugin. The host passes raw tool arguments,
result `details`, partial results and custom-message data. Domain-specific interpretation belongs to
the plugin. The subagent UI is the bundled example, including task state and stop/follow-up controls:
[`plugins/features/pi-plugin-subagents/desktop`](../../plugins/features/pi-plugin-subagents/desktop).

Build a local extension from the desktop directory:

```sh
npm run build:extension -- ../../plugins/features/pi-plugin-subagents/desktop
```

A source package contains `src/index.tsx`, CSS and `pi-desktop.json`:

```json
{"schemaVersion":1,"id":"example.checks","name":"Checks","entry":"dist/index.js","styles":["dist/style.css"]}
```

Install the **contents** of its built `dist` directory under
`~/.pi/agent/desktop-extensions/<package>/` or `<workspace>/.pi/desktop-extensions/<package>/`.
Project packages require the existing project trust decision. Click **Reload desktop extensions**
in the conversation after rebuilding/installing. A matching installed ID replaces the bundled
extension without rebuilding the desktop. Native package installation remains separate.

For a provider-free preview of the dynamically loaded subagent bundle, build it as above, run
`npm run dev`, and open `/examples/preview.html`. The preview supplies simulated tasks and commands;
it verifies actual bundle loading and React interactions without launching model work.

The SDK guide in [`packages/pi-desktop-sdk`](../../packages/pi-desktop-sdk/README.md) documents exact
interfaces and lifetime rules. Extensions execute as trusted webview code using the host React
instance. Missing/crashing renderers have normal transcript fallbacks. UI-only reload does not stop
background agents; native session reload retains its existing child-shutdown behavior.

Backend plugins publish later status changes with the native author helper:

```rust
use pi_plugin_sdk::desktop::WidgetPublisher;

let mut progress = WidgetPublisher::new("example.progress")?;
progress.publish(&context.session, &serde_json::json!({
    "completed": 3, "total": 5
}))?;
```

The host retains the latest value on the current branch and forwards updates using durable record
sequence numbers. Use null to remove a key, keep values below 256 KiB, and coalesce unchanged
snapshots. `WidgetPublisher` hides the entry envelope, size validation, tombstones and duplicate
suppression. These records are hidden from the model. Plugins own their payload validation,
historical state and command implementations. No general plugin RPC server is required.
