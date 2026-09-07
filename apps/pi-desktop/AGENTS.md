# Pi Desktop Agent Guide

Treat `src-tauri/src/pi_runtime/*` and `pi-sdk::Pi` as the Pi integration seam.

## Scope

- Frontend: React + Vite in `src/`.
- Backend: Tauri Rust process in `src-tauri/src/`.
- Runtime: in-process `MultiSessionManager` / `PiSession`.

The `thread/*`, `turn/*`, and `item/*` event vocabulary is a frontend projection
of native Pi session events.

## Working Rules

- Keep Pi runtime policy in pi-rs modules; keep this app as presentation and
  desktop IPC wiring.
- Keep chat, model, tool, prompt, skill, and session behavior on native Pi
  runtime APIs.
- Keep frontend Tauri calls in `src/services/tauri.ts`.
- Keep app state orchestration in `src/features/app/*` and thread state in
  `src/features/threads/*`.
- Preserve the app-owned updater/release-notes path and Sentry integration.
- Keep generated build outputs, `node_modules`, Tauri `target`, and Tauri `gen`
  out of source control.

## Validation

Run checks based on the touched area:

```bash
npm run typecheck
npm test
npm run lint
cd src-tauri && cargo test
```

Run the repository-wide quality gates from the root after changes that cross
the desktop/Pi runtime boundary.
