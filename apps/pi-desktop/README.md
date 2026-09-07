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
- text and image submission, steering, interruption, and live streaming;
- reasoning, generic tool, shell output, error, and usage events;
- the Pi runtime model catalog, provider-qualified model selection, and per-session thinking level;
- Git, file browser, prompt, terminal, and layout UI.

The desktop-only infrastructure is limited to the app updater, release notes,
and Sentry. Provider credentials come from the Pi runtime environment. Pi uses
operating-system permissions directly, so the desktop does not expose a separate
access or approval mode.

## Configuration

```bash
export OPENAI_API_KEY=...
export OPENAI_BASE_URL=https://api.openai.com/v1
export OPENAI_MODEL=gpt-4o-mini
```

`OPENAI_BASE_URL` and `OPENAI_MODEL` are optional. A local OpenAI-compatible
endpoint can omit `OPENAI_API_KEY`. Set `PI_AGENT_DIR` to override the default
`~/.pi/agent` session/resources directory.

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
