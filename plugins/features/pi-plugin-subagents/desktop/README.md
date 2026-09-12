# Subagent desktop extension

This package owns the subagent React view, styles, translations, state interpretation and UI
commands. It imports only React and `@pi-rs/desktop-sdk`. The desktop host provides reusable
`ToolCard`, `StatusBadge`, `Markdown`, `SessionView` and `InlineCommandForm` components. The
`defineWidgetChannel` / `useWidget` pair runs this package's decoder, while `usePluginCommand`
owns command serialization, pending state and errors. The host does not interpret subagent results
or agent identifiers.

From the repository root, with desktop dependencies installed:

```sh
apps/pi-desktop/node_modules/.bin/tsc -p plugins/features/pi-plugin-subagents/desktop/tsconfig.json
node --test plugins/features/pi-plugin-subagents/desktop/model.test.mjs
npm --prefix apps/pi-desktop run build:extension -- ../../plugins/features/pi-plugin-subagents/desktop
```

The build emits a self-contained `dist/pi-desktop.json`, `dist/index.js` and `dist/style.css`.
Install the **contents of dist** as a desktop extension directory. The TypeScript configuration
resolves source-checkout author types from the host; the generated JavaScript uses the host React
runtime and SDK without importing host application internals.

The Rust plugin publishes `pi.ui.widget` custom entries through
`pi_plugin_sdk::desktop::WidgetPublisher`, using key `subagents.tasks`. Its version 1
value contains `runtimeId`, `ownerSessionId`, a map of minimal task snapshots keyed by exact
`agentId`, and `liveAgentIds`. Each task contains its profile, an original assignment preview, actual runtime
state, opaque session reference and token total. Full child transcripts stay in their sessions.
Assignment previews are bounded to 1 KiB with an ellipsis. The widget retains all live children
and evicts the oldest historical previews to stay within 128 tasks / 240 KiB. Eviction does not
delete the original tool result or child session; old cards can still show their conversation.
Restoration reads only the active journal branch. Equal snapshots are suppressed, and the plugin publishes after state transitions including after
the asynchronous spawn tool has returned. Custom entries do not enter provider context.

The view parses ordinary tool `arguments` and `details`. Compatibility with old desktop
`collabToolCall` data is isolated in `src/model.ts`. A completed spawn receipt does not mean that
its child task completed; the widget runtime state distinguishes completion, failure,
interruption and timeout. `SessionView` supplies transcript loading, live updates, retry and cycle
protection. Collapsing the card or reloading its React code only unmounts the view.

Stop and follow-up buttons call the plugin's existing command surface:

```text
/subagents:interrupt {"target":"exact-agent-id"}
/subagents:followup {"target":"exact-agent-id","task":"Continue with…"}
```

Both handlers validate direct-child ownership in `SubagentRuntime`; they do not execute arbitrary
tools. The UI disables controls for historical/unowned tasks, while server-side validation remains
authoritative. Native session reload retains its existing behavior of stopping children and
resetting the live graph. Session startup refreshes saved widget data with current ownership;
history never reconstructs executable child handles. During shutdown, widget publication stops
before draining children, preserving the existing final usage accounting.
