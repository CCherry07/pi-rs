# Shared Rust plugin UI

The terminal and component runtime live in `apps/pi-cli`. Rust and JavaScript plugins submit the
same semantic tree defined by `pi-plugin`; JS supplies business logic and descriptions only.
This deliberately replaces the upstream JavaScript TUI API. No compatibility shim, vendored
upstream renderer, `pi-tui` runtime or Coding Agent npm runtime is required.

## JavaScript

```ts
import type { PiExtensionApi } from "@pi-rs/cli";

export default function (pi: PiExtensionApi) {
  pi.registerCommand("environment", {
    description: "Choose an environment",
    async handler(_args, ctx) {
      const value = await ctx.ui.select("Environment", ["staging", "production"]);
      if (value) ctx.ui.setStatus("environment", value);
    },
  });
}
```

Custom UI supplies a tree and a callback, with no `render(width)` or key decoding:

```ts
const panel = ctx.ui.mount({
  id: "form",
  type: "panel",
  title: "Search",
  child: { id: "query", type: "input", placeholder: "Search text" },
}, {
  placement: "overlay",
  overlay: { width: 60, maxHeight: 12 },
  onEvent(event) {
    if (event.event.type === "submit") {
      ctx.ui.notify(`Submitted: ${String(event.event.value)}`);
      panel.unmount();
    }
  },
});
panel.focus("query");
```

Use `custom(node, options)` to await one submit/activate result; Escape, timeout and AbortSignal
resolve `undefined`. `mount` returns `update(node)`, `focus(nodeId)` and `unmount()`. A mount survives its creating
command; an explicit signal or timeout can shorten its generation-owned lifetime. Updating a node
with the same ID/kind preserves Rust-owned edits, selection, cursor and undo. `initialValue` and
`initialSelected` seed a new node only. Change the node ID when intentionally resetting it.

## Native Rust

Implement `pi_plugin::UiComponent`: `render()` returns a `UiSurfaceFrame` containing nodes and
`handle_event()` handles semantic interactions. Call `ctx.ui.custom(component, signal).await`.
The driver supplies owner/revision, catches callback panics, rejects stale generations and closes
on success, cancellation, errors or future drop. `UiComponentWaker::request_render()` lets a
background result publish a new description without owning terminal state. See
[`native-ui.rs`](../crates/pi-plugin/examples/native-ui.rs) for a complete example.

## Component semantics

| Nodes | Rust behavior |
| --- | --- |
| Text / Link / Markdown | Shared palette, hyperlink placement and Rust Markdown rendering. |
| Column / Row / Panel / Spacer | Native measurement, spacing and clipping. |
| Scroll | Fixed-height viewport with local wheel/PageUp/PageDown scrolling. |
| Select | Native list, selected index, keyboard/mouse selection and numeric submit value. |
| Input / Editor | Shared composer, Unicode/paste/cursor/undo; Input Enter submits, Editor Ctrl+Enter submits. |
| Completions | Input/Editor `completions: [{value,label,description?}]`; Rust filters by prefix, selects with arrows and accepts with Tab. Accepted value replaces the full text. |
| Button / Progress | Activation or shared progress presentation. |
| Image | Base64 content, MIME type, alt text and cell dimensions; Rust encodes final Kitty/iTerm output. |

Node IDs must be unique within a tree; region IDs must be unique within an owner. Owners may
contribute header, aboveEditor, editor, belowEditor, footer and overlay regions. Header/editor/footer
slots use the most recent owner; overlay stacking and modal input capture belong to Rust.
Transcript callbacks return nodes using the same renderer; layout is never computed in JS.

## Lifecycle and protocol

One owner has one pending exchange. Revisions change only when document content changes. Equal
revisions may repeat an identical document, while stale or conflicting updates never replace live
state. Rust applies local input first, coalesces unread changes, and preserves ordered submit/cancel
events. On a document change, pending interactions survive only for retained node IDs/kinds.
The JS adapter checks live mount identity before invoking callbacks.

Frontend close, generation retirement and failed callbacks release UI resources. Failed generation
preparation keeps the previous generation. Frames, local editing state and callbacks are transient;
Pi v4 session storage remains unchanged. Native ABI 29 requires rebuilding native plugins.

The shared palettes are `light`, `dark` and `system`. Upstream JSON theme discovery/watchers,
custom editor subclasses, arbitrary terminal escape output and raw key listeners are removed.
Plugins use semantic nodes/actions for these operations. Print/JSON/RPC keep inactive defaults;
Desktop requires its own renderer for this contract.

## Verification

Run the Rust workspace gates plus `npm run check`, `npm test`, `npm run build:native`,
`npm run test:bridge` under `packages/pi`. The real Node/NAPI/Rust terminal scenario is
`python test/ui-runtime.pty.py` after installing `test/requirements-pty.txt` and building JS/native.
