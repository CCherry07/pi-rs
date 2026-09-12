# Pi Desktop extension SDK

Trusted desktop extensions own their React components and CSS. The host provides exact
tool/custom-message registration, common presentation components, session previews and invocation
of existing registered commands. Business state and commands remain with the plugin that owns them.

This is an intentional desktop product extension to Pi's `renderCall` / `renderResult` and widget
model. It does not change native plugin ABI, session message format or the runtime's three plugin
lifecycles. Desktop view generations load separately from native runtime generations.

## Write a view

```tsx
import { useState } from "react";
import {
  defineDesktopExtension,
  Markdown,
  StatusBadge,
  ToolCard,
  type DesktopItemViewProps,
} from "@pi-rs/desktop-sdk";
import "./style.css";

function CheckResult({ item, expanded, onToggle }: DesktopItemViewProps) {
  const [showRaw, setShowRaw] = useState(false);
  return (
    <ToolCard
      title={item.title}
      expanded={expanded}
      onToggle={onToggle}
      status={<StatusBadge state="completed" label="Finished" />}
      actions={<button onClick={() => setShowRaw(!showRaw)}>Details</button>}
    >
      <Markdown>{item.output ?? item.detail}</Markdown>
      {showRaw && <pre>{JSON.stringify(item.data, null, 2)}</pre>}
    </ToolCard>
  );
}

export default defineDesktopExtension({
  id: "example.checks",
  views: [{
    id: "result",
    slot: "tool.result",
    target: { tool: "check" },
    component: CheckResult,
  }],
});
```

Use `{ customType: "example.report" }` for a custom-message renderer. Match identities exactly;
there are no title or tool-type heuristics. `workspace.panel` views instead have a `title` and a
component without item props. Installed packages replace an entire bundled definition with the
same extension id. Duplicate installed ids or conflicting render targets reject the next generation.

`ToolCard` is an optional controlled disclosure with separate action controls. `StatusBadge` accepts
a semantic state and a label supplied by the plugin. `Markdown` delegates file-link behavior and
formatting to the host. `InlineCommandForm` owns draft retention around asynchronous commands. Any
of these can be replaced with the plugin's own JSX/CSS.

## Use existing host capabilities

```tsx
import {
  defineWidgetChannel,
  InlineCommandForm,
  SessionView,
  usePluginCommand,
  useWidget,
} from "@pi-rs/desktop-sdk";

const progressChannel = defineWidgetChannel({
  key: "example.tasks",
  decode(value: unknown) {
    if (!value || typeof value !== "object" || !("completed" in value)) {
      throw new Error("Invalid example.tasks value");
    }
    return value as { completed: number; sessionId: string };
  },
});

function TaskPreview() {
  const progress = useWidget(progressChannel);
  const retry = usePluginCommand<{ task: string }>("example:retry");
  if (progress.status !== "ready") return null;
  return <>
    <SessionView reference={{ sessionId: progress.value.sessionId }} />
    <InlineCommandForm ariaLabel="Retry task" submitLabel="Retry"
      pending={retry.pending} error={retry.error}
      onSubmit={task => retry.run({ task })} />
  </>;
}
```

The context includes `workspaceId`, `threadId`, `pluginId`, `locale`, `readOnly`, `signal`, `widgets`,
`sessionStatus(reference)`, `renderSession(reference)` and `runCommand(name, args)`. The widget map
contains namespaced keys emitted by backend plugins. It updates independently of a completed tool
call. `defineWidgetChannel` and `useWidget` keep the decoder with the owning plugin and return
`missing`, `ready`, or `invalid` without exposing malformed values to the view. Widgets remain
presentation state, not a replacement for the plugin's task model.

`SessionView` accepts a direct `sessionId`, or an `isolatedSessionId` with its `ownerSessionId`.
The host owns loading, subscription, history and recursive-preview protection. Its status exposes
only whether a turn is processing and optional token usage. Domain completion/failure state belongs
to plugin data. Closing a card only unmounts its preview; it never stops a running task.
Live session previews set `readOnly` while continuing to render nested extension views. Render
their live status normally and omit action controls; command invocation is disabled in previews.

For actions, adapt a command the backend plugin already registers:

```tsx
const interrupt = usePluginCommand<{ taskId: string }>("example:interrupt");
await interrupt.run({ taskId });
```

The hook owns JSON serialization plus pending/error state. The host binds the call to the current session and
rejects read-only or retired view callbacks. Aborting the view's `signal` cancels view-owned work;
it does not undo a command already accepted by the backend. Command authorization and business
validation remain in the backend plugin.

## Package and load

A source package contains `src/index.tsx`, any imported local CSS/assets, and `pi-desktop.json`.
Use the desktop app's `build:extension` helper to produce a bundled ESM/CSS package. React,
ReactDOM and JSX runtimes resolve to the exact host instances through the versioned desktop runtime;
never ship another React instance. The build helper checks the host version when loading a bundle.

Local installed packages are discovered under the agent directory's `desktop-extensions/` and
trusted project `.pi/desktop-extensions/` directories. See the desktop app README for the manifest
and build/install commands. A complete candidate is imported and validated before replacing the
previous view generation. Failed reloads retain the previous working views; revoked project trust
removes project views even if another package is invalid. Session/workspace changes and successful
generation swaps unmount old views and abort their context signals.

Missing or crashing tool renderers fall back to the normal transcript. Inherited read-only
snapshots use the stored transcript rather than opening live plugin views. React error boundaries
handle render errors; `usePluginCommand` handles its command failures while other plugin-owned
event handlers and asynchronous work remain the plugin's responsibility.

Extensions run as trusted application code in the desktop renderer, with shared global CSS and
JavaScript. This is not an isolation boundary for untrusted packages. A generation swap manages
registered React views; it cannot roll back arbitrary module-global side effects. Initialize
subscriptions in effects and clean them up on unmount. Module initialization has a bounded wait,
but synchronous JavaScript that blocks the renderer cannot be interrupted by React.
