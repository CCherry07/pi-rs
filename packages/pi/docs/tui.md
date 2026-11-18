# Shared Rust plugin UI

The terminal and component runtime live in `apps/pi-cli`. Rust and JavaScript plugins submit the
same `UiNode` tree defined by `pi-plugin`. Semantic controls run in Rust; upstream Pi components
can draw their own contents through a `Custom` leaf. The Node host supplies a pinned `pi-tui`
package under both `@earendil-works/pi-tui` and `@mariozechner/pi-tui`. It reuses components and
helpers without starting another terminal loop or installing the Coding Agent npm runtime.

Within the CLI, all implementation files live under `tui/extensions/`. The surface consumes the
wire protocol and returns explicit product effects; startup and the active session frontend share
that same reduction path, while physical terminal and OS work stays in the host adapter.

Stdio RPC has a semantic dialog bridge in `pi-rpc`. Rust plugins and Node-hosted extensions can use
`confirm`, `select`, `input`, `editor` and `notify`, including during startup and reload. Questions use
Pi's correlated `extension_ui_request` / `extension_ui_response` protocol. Text answers preserve empty
strings and multiline content; cancellation returns `undefined` in JavaScript and `None` in Rust.
Native selection keeps returning the matching option index. An AbortSignal, timeout, generation
retirement or connection close releases the pending question; only a live, matching affirmative
response grants confirmation. Standard select/input/confirm deadlines also travel to the frontend;
editor retains local cancellation without adding a timeout field to Pi's editor wire request.
Notifications share an ordered output queue with dialogs and responses, and are drained at shutdown.
RPC leaves arbitrary custom terminal components, component widgets, headers and footers inactive,
matching legacy Pi. See [Sikemux integration](https://github.com/CCherry07/pi-rs/blob/HEAD/apps/pi-cli/SIKEMUX.md) for desktop usage.

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

Semantic UI supplies a tree and a callback:

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

Existing Pi extensions can also pass a component factory:

```ts
import { Input } from "@earendil-works/pi-tui";

const value = await ctx.ui.custom<string | undefined>((tui, theme, keybindings, done) => {
  const input = new Input();
  input.onSubmit = done;
  input.onEscape = () => done(undefined);
  return input;
});
```

`custom(factory)` occupies the editor region by default. `{ overlay: true, overlayOptions: {
width: "80%", maxHeight: 12, anchor: "right-center" } }` requests an overlay. The supplied TUI facade
supports terminal dimensions, `requestRender()`, `setFocus()` and component-local `addInputListener()`.
Rust routes focused keys and paste to the JS component, then composes its rendered lines. Both
forms of `custom` share completion, cancellation, timeout and generation retirement. A component
is disposed once even if its asynchronous factory finishes after cancellation; a component error
closes that operation while retaining other mounts.

Old Pi slot factories use the same component adapter:

```ts
ctx.ui.setWidget("tasks", ["Two tasks remaining"], { placement: "aboveEditor" });
ctx.ui.setHeader((_tui, theme) => ({
  render: () => [theme.fg("accent", "Project workspace")],
  invalidate() {},
}));
ctx.ui.setFooter((tui, theme, data) => {
  const unsubscribe = data.onBranchChange(() => tui.requestRender());
  return {
    render: () => [theme.fg("muted", `${data.getGitBranch() ?? "no repository"} · ${[...data.getExtensionStatuses().values()].join(" · ")}`)],
    invalidate() {},
    dispose: unsubscribe,
  };
});
```

Widgets accept `string[]`, component factories or semantic nodes. Header/footer accept factories
or semantic nodes. A factory runs once per replacement; its component rerenders on refresh and
resize, and is disposed on replacement, clearing with `undefined`, or generation retirement.
Slot render failures remove that slot while preserving other UI. The footer data provider exposes
Git branch (including worktrees and detached HEAD), keyed extension statuses, the available
provider count and branch-change subscriptions. Branch notifications use bounded asynchronous
polling while the footer is alive. A custom footer owns status presentation; clearing it restores
the automatic status footer.

Overlay options support width/minWidth/maxHeight, nine anchors, absolute or percentage row/col,
offsetX/offsetY, margins, `visible(columns, rows)` and `nonCapturing`. Rust owns final clipped
geometry, stacking and focus. Noncapturing overlays initially leave focus with the current
component or composer; explicit focus raises them. Component `focused` state follows Rust focus.
`nonModal` remains available for the earlier pi-rs overlay call shape.

`overlayOptions` may be an object or function. Functions are reevaluated on every UI refresh,
including resize and `tui.requestRender()`. This is a deliberate extension of the local upstream
implementation, which evaluates the function only when opening the overlay.

`onHandle(handle)` exposes `focus()`, `unfocus({ target })`, `isFocused()`, `getBounds()`,
`setHidden(boolean)`, `isHidden()` and `hide()`. Bounds are the latest painted Rust rectangle
(`row`, `col`, `width`, `height`); they are unavailable before the first paint or while invisible.
Focus and bounds queries reflect the latest frontend snapshot after queued commands are applied.
`unfocus()` restores previous eligible focus; `target: null` returns to the composer, and another
mounted Pi custom component can be an explicit target. Hidden overlays retain local state and
receive no input. `setHidden(false)` allows them to reappear; `hide()` removes them permanently.
As in Pi, hiding does not finish the `custom` promise: call `done(value)`, cancel, or retire the
generation to settle it and dispose the component.

## Native Rust

Implement `pi_plugin::UiComponent`: `render()` returns a `UiSurfaceFrame` containing nodes and
`handle_event()` handles semantic interactions. Call `ctx.ui.custom(component, signal).await`.
The driver supplies owner/revision, catches callback panics, rejects stale generations and closes
on success, cancellation, errors or future drop. `UiComponentWaker::request_render()` lets a
background result publish a new description without owning terminal state. See
[`native-ui.rs`](https://github.com/CCherry07/pi-rs/blob/HEAD/crates/pi-plugin/examples/native-ui.rs) for a complete example.

`UiComponent` and `UiSurfaceFrame` are the shared author interface. JavaScript's `mount`, slot,
status and dialog methods are convenience adapters over that interface, not a second UI runtime.
Native authors express persistent or multi-region behavior in component state and return actions
from `handle_event`; JavaScript authors normally use the higher-level lifetime helpers.

## Component semantics

| Nodes | Rust behavior |
| --- | --- |
| Text / Link / Markdown | Shared palette, hyperlink placement and Rust Markdown rendering. |
| Custom | Plugin-drawn lines and optional cursor; SGR styling is parsed into cells, rows are clipped without wrapping, and focused keys/paste emit `CustomInput`. |
| Column / Row / Panel / Spacer | Native measurement, spacing and clipping. |
| Scroll | Fixed-height viewport with local wheel/PageUp/PageDown scrolling. |
| Select | Native list, selected index, keyboard/mouse selection and numeric submit value. |
| MultiSelect | Search, category filters, sorting, multi-selection, confirmation and semantic action result. Available as `ctx.ui.multiSelect` and `UiContext::multi_select`. |
| Input / Editor | Shared composer, Unicode/paste/cursor/undo; Input Enter submits, Editor Ctrl+Enter submits. |
| Completions | Input/Editor `completions: [{value,label,description?}]`; Rust filters by prefix, selects with arrows and accepts with Tab. Accepted value replaces the full text. |
| Button / Progress | Activation or shared progress presentation. |
| Image | Base64 content, MIME type, alt text and cell dimensions; Rust encodes final Kitty/iTerm output. |

Node IDs must be unique within a tree; region IDs must be unique within an owner. Owners may
contribute header, aboveEditor, editor, belowEditor, footer and overlay regions. Header/editor/footer
slots use the most recent owner; overlay stacking and modal input capture belong to Rust.
Transcript callbacks return nodes or Pi components using the same adapter. Tool call/result slots
retain their own `lastComponent` across updates, resize and partial results. JS components lay out
their local contents; Rust owns the surrounding regions and transcript layout.

## Lifecycle and protocol

One owner has one pending exchange. Revisions change only when document content changes. Equal
revisions may repeat an identical document, while stale or conflicting updates never replace live
state. Rust applies semantic control input locally, coalesces unread changes, and preserves ordered
submit/cancel and custom input events. Custom component input requires a JS round trip. On a document
change, pending interactions survive only for retained node IDs/kinds.
The JS adapter checks live mount identity before invoking callbacks.

Frontend close, generation retirement and failed callbacks release UI resources. Failed generation
preparation keeps the previous generation. Frames, local editing state and callbacks are transient;
Pi v4 session storage remains unchanged. Native ABI 33 requires rebuilding native plugins. UI
commands use one transport envelope with separate surface, product and host-operation channels.

Native updates return `Vec<UiCommand>`: use `UiCommand::Surface(UiSurfaceCommand::Focus { ... })`
for local interaction, `UiCommand::Product(UiProductCommand::Title { ... })` for product
presentation, and `UiCommand::Host(UiHostRequest { id, operation })` for correlated system work.
The Rust bridge method is `ui_command(owner, command)`. The JavaScript adapter sends
`{ type: "uiCommand", owner, command: { channel: "product", command: { type: "title", title } } }`;
high-level methods such as `ctx.ui.setTitle` and `ctx.ui.hostOperation` retain their call shape.
The old flat `uiAction` transport is removed, so the Node host and native binding must be rebuilt
together. Command channels describe execution ownership; they do not restrict callback permissions.

`hasUI` / `UiContext::is_available` is true only when the presentation mode is interactive and a
frontend bridge is installed. A custom embedder cannot advertise UI by selecting TUI mode alone.

The shared palettes are `light`, `dark` and `system`. Pi callback theme helpers format supported
tokens with terminal palette colors. Complete upstream themes and keybinding configuration,
JSON theme discovery/watchers, custom editor subclasses and arbitrary
terminal control remain unsupported.
Mounted Pi components and widget/header/footer factories support `handleMouse`, including nested
targets, press/release/click, drag capture, vertical wheel, focus and render results. Rust owns
global hit testing and focus; the adapter keeps nested component objects in JS. Mouse-only slots
do not take keyboard focus when mounted. Correlated replies, retained instance IDs and gesture
cancellation prevent stale callbacks from affecting replacement components. Transcript-only
renderers remain noninteractive; see `js-extension-compatibility.md` for bridge cancellation rules.
Use semantic nodes/actions for those supported host operations. Print/JSON/RPC keep inactive defaults.
The currently shipped renderer is the CLI TUI; Desktop intentionally reports UI unavailable until
it supplies its own adapter for this semantic interface.

## Verification

Run the Rust workspace gates plus `npm run check`, `npm test`, `npm run build:native`,
`npm run test:bridge` under `packages/pi`. The real Node/NAPI/Rust terminal scenario is
`python test/ui-runtime.pty.py` after installing `test/requirements-pty.txt` and building JS/native.
It checks slot factory lifetime, footer data, overlay geometry, focus, visibility and resize in
addition to semantic controls and host operations. Set `UI_ACCEPTANCE_TASKS_EXTENSION` to an
installed `@tintinweb/pi-tasks/src/index.ts` to also exercise its original TaskCreate tool and
widget factory in an isolated temporary workspace. `PI_UI_EVIDENCE_DIR` saves terminal evidence.
