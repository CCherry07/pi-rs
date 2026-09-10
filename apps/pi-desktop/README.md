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
native plugins) are exposed. The Desktop does **not** host JavaScript/TypeScript extensions or
extension widgets, argument-completion callbacks, or interactive extension dialogs. CLI-only
frontend commands such as `/reload` are not implicitly registered here; a native command may
use the runtime reload/navigation capabilities. Project native plugins still use shared Pi
project trust. No new plugin loader or execution sandbox is introduced.

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
