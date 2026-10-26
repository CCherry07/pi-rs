import { randomUUID } from "node:crypto";
import { getAgentDir, withAgentDir } from "./extension-ui-paths.js";
import { ExtensionTranscript } from "./extension-ui-transcript.js";
import { PiComponentAdapter, customLayout, getKeybindings, piTheme, type CustomOptions, type CustomFactory, type CustomTui, type PiComponent, type PiTheme } from "./pi-component-adapter.js";
import { ExtensionFooter, type FooterDataProvider } from "./extension-footer.js";
import {
  uiCommandSchema, uiEventSchema, uiMultiSelectResponseSchema, uiNodeSchema, uiSnapshotSchema,
  type UiCommand, type UiHostOperation, type UiNode, type UiSnapshot, type UiComponentEvent, type UiMouseResult,
  type UiMultiSelectRequest, type UiMultiSelectResponse, type Placement, type UiRegion,
} from "./extension-ui-frame.js";
import type { NativeExtensionContext } from "./native-binding.js";

export interface DialogOptions { signal?: AbortSignal; timeout?: number }
export type UiTheme = "light" | "dark" | "system";
export type SlotFactory = (tui: CustomTui, theme: PiTheme) => PiComponent;
export type FooterFactory = (tui: CustomTui, theme: PiTheme, data: FooterDataProvider) => PiComponent;
export interface MountOptions extends DialogOptions {
  id?: string;
  placement?: Placement;
  overlay?: UiRegion["overlay"];
  onEvent?(event: UiComponentEvent): void;
}
export interface UiMount {
  readonly id: string;
  update(node: UiNode): void;
  focus(nodeId: string): void;
  unmount(): void;
}
type NodeIdentities = Map<string, { type: UiNode["type"] }>;
interface Mounted {
  region: UiRegion;
  nodes: NodeIdentities;
  render?(snapshot: UiSnapshot): Pick<UiRegion, "root" | "overlay"> | undefined;
  onEvent?: (event: UiComponentEvent) => void;
  cleanup(): void;
  onError?(error: unknown): void;
}
interface SurfaceFrame {
  owner: string; revision: number; regions: UiRegion[]; hideWorking: boolean; renderTranscript: boolean;
  transcript: ReturnType<ExtensionTranscript["render"]>; shortcuts: { key: string }[];
}
function nodeIdentities(root: UiNode, previous?: NodeIdentities): NodeIdentities {
  const nodes: NodeIdentities = new Map();
  const visit = (node: UiNode) => {
    const identity = previous?.get(node.id);
    nodes.set(node.id, identity?.type === node.type ? identity : { type: node.type });
    if (node.type === "column" || node.type === "row") node.children.forEach(visit);
    else if (node.type === "panel" || node.type === "scroll") visit(node.child);
  };
  visit(root);
  return nodes;
}
interface ExtensionUIOptions { agentDir?: string; cwd?: string; outputPad?: 0 | 1; showImages?: boolean }
/** Generation-owned mounts and transport. Rust owns regions; Pi components own their local state. */
export class ExtensionUI {
  readonly owner = randomUUID();
  readonly transcript: ExtensionTranscript;
  readonly #regions = new Map<string, Mounted>();
  readonly #components = new Map<string, PiComponentAdapter>();
  readonly #shortcuts = new Map<string, () => Promise<void>>();
  readonly #statuses = new Map<string, string>();
  readonly #pending = new Set<() => void>();
  readonly #hostRequests = new Map<number, { resolve(value: string | undefined): void; reject(error: Error): void }>();
  readonly #agentDir: string;
  readonly #cwd: string;
  #native?: NativeExtensionContext;
  #running = false;
  #retired = false;
  #revision = 0;
  #lastDocument = "";
  #hostId = 0;
  #dirty = true;
  #lastFrame?: SurfaceFrame;
  #workingConfigured = false;
  #workingVisible = true;
  #workingMessage = "Working...";
  #snapshot: UiSnapshot = uiSnapshotSchema.parse({ columns: 80, rows: 24, editorText: "", toolsExpanded: false });

  constructor(options: ExtensionUIOptions = {}) {
    this.#agentDir = options.agentDir ?? getAgentDir();
    this.#cwd = options.cwd ?? process.cwd();
    this.transcript = new ExtensionTranscript(() => this.#wake(), options.outputPad ?? 1, options.showImages ?? true);
  }
  run<T>(callback: () => T): T { return withAgentDir(this.#agentDir, callback); }
  get nativeContext() { return this.#native; }
  get theme() { return piTheme; }
  bind(native: NativeExtensionContext) { this.#assertActive(); this.#native = native; }
  activate() { if (this.#shortcuts.size || this.transcript.active || this.#regions.size) this.#wake(); }
  #assertActive() { if (this.#retired) throw new Error("Extension UI generation retired"); }
  forContext(signal: AbortSignal): object {
    return new Proxy(this, {
      get: (target, property) => {
        target.#assertActive();
        const value = Reflect.get(target, property, target);
        if (typeof value !== "function") return value;
        return (...args: unknown[]) => {
          target.#assertActive();
          const index = property === "custom" ? 1 : ["select", "confirm", "input", "editor"].includes(String(property)) ? 2 : -1;
          if (index >= 0) {
            const opts = args[index] as DialogOptions | undefined;
            args[index] = { ...opts, signal: opts?.signal ? AbortSignal.any([signal, opts.signal]) : signal };
          }
          return target.run(() => Reflect.apply(value, target, args));
        };
      },
    });
  }
  #query() {
    if (this.#native) this.#snapshot = uiSnapshotSchema.parse(JSON.parse(this.#native.query('{"type":"uiSnapshot"}')));
    return this.#snapshot;
  }
  #command(command: UiCommand) {
    if (!this.#retired) this.#native?.notify(JSON.stringify({ type: "uiCommand", owner: this.owner, command: uiCommandSchema.parse(command) }));
  }
  #wake() {
    if (this.#retired || !this.#native) return;
    this.#dirty = true;
    if (this.#running) this.#command({ channel: "surface", command: { type: "wake" } });
    else { this.#running = true; void this.#pump(); }
  }
  #changed() { this.#wake(); }
  #frame(): SurfaceFrame {
    if (!this.#dirty && this.#lastFrame) return this.#lastFrame;
    this.#dirty = false;
    const snapshot = this.#query();
    for (const [id, state] of this.#regions) {
      if (!state.render) continue;
      const rendered = this.run(() => state.render!(snapshot));
      if (!rendered || this.#regions.get(id) !== state) continue;
      const root = uiNodeSchema.parse(rendered.root);
      state.region = { ...state.region, root, overlay: rendered.overlay };
      state.nodes = nodeIdentities(root, state.nodes);
    }
    const transcript = this.run(() => this.transcript.render(snapshot));
    const regions = [...this.#regions.values()].map(({ region }) => region);
    if (this.#statuses.size && !this.#regions.has("footer")) regions.push({ id: "_statuses", placement: "footer", root: { id: "statuses", type: "text", text: [...this.#statuses.values()].join(" · "), tone: "muted" } });
    if (this.#workingConfigured && this.#workingVisible && snapshot.isRunning) regions.push({ id: "_working", placement: "aboveEditor", root: { id: "working", type: "progress", text: this.#workingMessage } });
    const document = {
      regions,
      hideWorking: this.#workingConfigured, renderTranscript: this.transcript.active,
      transcript,
      shortcuts: [...this.#shortcuts.keys()].map((key) => ({ key })),
    };
    const signature = JSON.stringify(document);
    if (signature !== this.#lastDocument) { this.#lastDocument = signature; this.#revision++; }
    return this.#lastFrame = { owner: this.owner, revision: this.#revision, ...document };
  }
  async #pump() {
    try {
      while (!this.#retired && this.#native) {
        const frame = this.#frame();
        const sentRegions = new Map([...this.#regions].map(([id, mounted]) => [id, { mounted, nodes: mounted.nodes }]));
        const sentShortcuts = new Map(this.#shortcuts);
        const event = uiEventSchema.parse(JSON.parse(await this.#native.request(JSON.stringify({ type: "uiExchange", frame }))));
        if (event.type === "closed") { this.retire(); break; }
        this.#snapshot = event.snapshot;
        if (event.type !== "interaction" || event.event.type !== "customMouse") this.#dirty = true;
        if (event.type === "hostResult") {
          const request = this.#hostRequests.get(event.id);
          this.#hostRequests.delete(event.id);
          if (event.error) request?.reject(new Error(event.error)); else request?.resolve(event.result ?? undefined);
        } else if (event.type === "interaction") {
          if (event.event.type === "customMouse") {
            const request = event.event;
            let result: UiMouseResult = { handled: false, capture: false, focus: false, cancelled: true };
            const mounted = this.#regions.get(event.regionId);
            const sent = sentRegions.get(event.regionId);
            const adapter = this.#components.get(event.regionId);
            const identity = mounted?.nodes.get(event.nodeId);
            try {
              if (event.revision === frame.revision && mounted && mounted === sent?.mounted && identity && identity === sent.nodes.get(event.nodeId)
                && !mounted.region.overlay?.hidden && adapter && adapter.instanceId === request.instanceId) {
                const { wheelDelta, clickCount, ...mouse } = request.event;
                result = this.run(() => adapter.handleMouse({ ...mouse,
                  ...(wheelDelta == null ? {} : { wheelDelta }), ...(clickCount == null ? {} : { clickCount }),
                }, request.target));
                if (result.render) this.#dirty = true;
              }
            } catch (error) {
              if (mounted?.onError) mounted.onError(error); else this.#report(error);
            } finally {
              this.#command({ channel: "surface", command: { type: "mouseResult", id: request.id, result } });
            }
            continue;
          }
          if (event.revision !== frame.revision) continue;
          if (event.event.type === "shortcut") {
            const handler = this.#shortcuts.get(event.event.key);
            if (handler && handler === sentShortcuts.get(event.event.key)) void this.run(handler).catch((error) => this.notify(String(error), "error"));
          } else {
            const mounted = this.#regions.get(event.regionId);
            const sent = sentRegions.get(event.regionId);
            const identity = mounted?.nodes.get(event.nodeId);
            if (mounted && mounted === sent?.mounted && identity && identity === sent.nodes.get(event.nodeId)) this.run(() => mounted.onEvent?.(event));
          }
        }
      }
    } catch (error) {
      if (!this.#retired) { try { this.notify(`Extension UI: ${String(error)}`, "error"); } catch {} this.retire(); }
    } finally { this.#running = false; }
  }

  mount(node: UiNode, options: MountOptions = {}): UiMount {
    return this.#mount(node, options);
  }
  #mount(node: UiNode, options: MountOptions, render?: Mounted["render"], onUnmount?: () => void, onError?: Mounted["onError"]): UiMount {
    this.#assertActive();
    const id = options.id ?? randomUUID();
    const parsed = uiNodeSchema.parse(node);
    if (this.#regions.has(id)) throw new Error(`UI region ${id} is already mounted`);
    let active = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const unmount = () => {
      if (!active) return;
      active = false;
      const mounted = this.#regions.get(id);
      if (mounted === state) this.#regions.delete(id);
      this.#components.delete(id);
      state.cleanup();
      this.#changed();
    };
    const state: Mounted = {
      region: { id, placement: options.placement ?? "aboveEditor", root: parsed, ...(options.overlay ? { overlay: options.overlay } : {}) },
      nodes: nodeIdentities(parsed),
      render,
      onError,
      onEvent: options.onEvent,
      cleanup: () => { clearTimeout(timer); options.signal?.removeEventListener("abort", unmount); onUnmount?.(); },
    };
    if (!options.signal?.aborted) {
      this.#regions.set(id, state);
      options.signal?.addEventListener("abort", unmount, { once: true });
      if (options.timeout !== undefined) timer = setTimeout(unmount, options.timeout);
      this.#changed();
    } else active = false;
    return Object.freeze({
      id,
      update: (value: UiNode) => { this.#assertActive(); if (!active || this.#regions.get(id) !== state) throw new Error("UI region unmounted"); const root = uiNodeSchema.parse(value); state.region = { ...state.region, root }; state.nodes = nodeIdentities(root, state.nodes); this.#changed(); },
      focus: (nodeId: string) => { this.#assertActive(); if (active && this.#regions.get(id) === state) this.#command({ channel: "surface", command: { type: "focus", regionId: id, nodeId } }); },
      unmount,
    });
  }
  custom<T>(factory: CustomFactory<T>, options?: CustomOptions): Promise<T | undefined>;
  custom(node: UiNode, options?: MountOptions): Promise<unknown | undefined>;
  custom(node: UiNode | CustomFactory, options: MountOptions | CustomOptions = {}): Promise<unknown | undefined> {
    this.#assertActive();
    if (options.signal?.aborted) return Promise.resolve(undefined);
    return new Promise((resolve, reject) => {
      let mount: UiMount | undefined;
      let timer: ReturnType<typeof setTimeout> | undefined;
      let settled = false;
      let hidden = false;
      let removed = false;
      const adapter = typeof node === "function" ? new PiComponentAdapter("custom-content") : undefined;
      const finish = (value?: unknown, error?: unknown) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        options.signal?.removeEventListener("abort", cancel);
        this.#pending.delete(cancel);
        if (mount) this.#components.delete(mount.id);
        try { mount?.unmount(); } catch (cleanupError) { error ??= cleanupError; }
        try { this.run(() => adapter?.dispose()); } catch (disposeError) { error ??= disposeError; }
        if (error !== undefined) reject(error); else resolve(value);
      };
      const cancel = () => finish();
      this.#pending.add(cancel);
      options.signal?.addEventListener("abort", cancel, { once: true });
      if (options.timeout !== undefined) timer = setTimeout(cancel, options.timeout);
      const show = (root: UiNode, settings: MountOptions, render?: Mounted["render"]) => {
        if (settled) return;
        mount = this.#mount(root, {
          ...settings, signal: undefined, timeout: undefined,
          placement: settings.placement ?? "overlay",
          onEvent: event => {
            if (settled || removed || hidden || mount && this.#regions.get(mount.id)?.region.overlay?.hidden) return;
            try {
              settings.onEvent?.(event);
              if (event.event.type === "cancel") finish();
              else if (event.event.type === "submit") finish(event.event.value);
              else if (event.event.type === "activate") finish(event.nodeId);
              else if (event.event.type === "customInput" && adapter) {
                adapter.handleInput(event.event.data);
                if (!settled) this.#changed();
              }
            } catch (error) { finish(undefined, error); }
          },
        }, render, () => { if (!removed) cancel(); }, error => finish(undefined, error));
        if (settled) { mount.unmount(); return; }
        if (adapter) this.#components.set(mount.id, adapter);
      };
      try {
        if (typeof node !== "function") { show(node, options as MountOptions); return; }
        const component = adapter!;
        const created = this.run(() => node(component.tui(() => this.#query(), () => this.#changed()), this.theme, getKeybindings(), value => finish(value)));
        void Promise.resolve(created).then(value => {
          this.run(() => component.setComponent(value));
          if (settled) return;
          let layout: ReturnType<typeof customLayout>;
          let root: UiNode = { id: "custom-content", type: "custom", lines: [], interactive: true };
          const render: NonNullable<Mounted["render"]> = snapshot => {
            if (settled) return;
            try {
              const width = component.component && Reflect.get(component.component, "width");
              layout = customLayout(snapshot, options as CustomOptions, typeof width === "number" ? width : undefined);
              if (layout.overlay) layout.overlay.hidden ||= hidden || removed;
              const regionId = mount?.id;
              const state = snapshot.regions.find(region => region.owner === this.owner && region.regionId === regionId);
              const focused = !layout.overlay?.hidden && (state?.focused ?? !layout.overlay?.nonCapturing);
              component.setVisible(!layout.overlay?.hidden);
              component.setFocused(focused);
              if (!layout.overlay?.hidden) root = component.render(layout.width, true, focused);
              return { root, overlay: layout.overlay };
            } catch (error) { finish(undefined, error); return; }
          };
          const snapshot = this.#query();
          const initial = this.run(() => render(snapshot));
          if (!initial) return;
          show(initial.root, layout!, render);
          if ((options as CustomOptions).overlay && mount && !settled) {
            const active = () => !settled && !removed;
            const visible = () => active() && !hidden && !this.#regions.get(mount!.id)?.region.overlay?.hidden;
            const state = () => this.#query().regions.find(region => region.owner === this.owner && region.regionId === mount!.id);
            (options as CustomOptions).onHandle?.({
              hide: () => {
                if (!active()) return;
                removed = true;
                component.setVisible(false);
                component.setFocused(false);
                this.#components.delete(mount!.id);
                mount!.unmount();
              },
              setHidden: value => { if (active() && value !== hidden) { hidden = value; component.setVisible(!hidden); this.#changed(); } },
              isHidden: () => hidden || removed,
              focus: () => { if (visible()) mount!.focus("custom-content"); },
              unfocus: settings => {
                if (!visible()) return;
                if (settings?.target) {
                  const target = [...this.#components].find(([, adapter]) => adapter.component === settings.target);
                  if (!target) throw new Error("Overlay focus target must be a mounted Pi component");
                  this.#command({ channel: "surface", command: { type: "unfocus", regionId: mount!.id, target: { type: "region", regionId: target[0] } } });
                } else {
                  this.#command({ channel: "surface", command: { type: "unfocus", regionId: mount!.id, target: settings?.target === null ? { type: "app" } : undefined } });
                }
              },
              isFocused: () => visible() && (state()?.focused ?? false),
              getBounds: () => visible() ? state()?.bounds ?? undefined : undefined,
            });
          }
        }).catch(error => finish(undefined, error));
      } catch (error) { finish(undefined, error); }
    });
  }
  #dialog(title: string, control: UiNode, hint: string, opts?: DialogOptions) {
    const [heading = "", ...description] = title.split("\n");
    const children: UiNode[] = [];
    if (description.length) children.push({ id: "description", type: "text", text: description.join("\n") });
    children.push(control, { id: "hint", type: "text", text: hint, tone: "muted" });
    return this.custom({ id: "dialog", type: "panel", title: heading, child: { id: "body", type: "column", gap: 1, children } }, { ...opts, overlay: { width: 60 } });
  }
  async select(title: string, options: string[], opts?: DialogOptions): Promise<string | undefined> {
    const result = await this.#dialog(title, { type: "select", id: "value", options }, "↑↓ move · enter select · esc cancel", opts);
    return typeof result === "number" && Number.isInteger(result) ? options[result] : undefined;
  }
  async confirm(title: string, message: string, opts?: DialogOptions): Promise<boolean> {
    return await this.select(`${title}\n${message}`, ["Yes", "No"], opts) === "Yes";
  }
  async input(title: string, placeholder = "", opts?: DialogOptions): Promise<string | undefined> {
    const result = await this.#dialog(title, { type: "input", id: "value", initialValue: "", placeholder }, "enter submit · esc cancel", opts);
    return typeof result === "string" ? result : undefined;
  }
  async editor(title: string, prefill = "", opts?: DialogOptions): Promise<string | undefined> {
    const result = await this.#dialog(title, { type: "editor", id: "value", initialValue: prefill }, "ctrl+enter submit · esc cancel", opts);
    return typeof result === "string" ? result : undefined;
  }
  async multiSelect(request: UiMultiSelectRequest, opts?: DialogOptions): Promise<UiMultiSelectResponse | undefined> {
    const result = await this.custom({ id: "multi-select", type: "multiSelect", request }, opts);
    const parsed = uiMultiSelectResponseSchema.safeParse(result);
    return parsed.success ? parsed.data : undefined;
  }
  #report(error: unknown) {
    if (!this.#retired) { try { this.notify(`Extension UI: ${String(error)}`, "error"); } catch {} }
  }
  #remove(id: string) {
    const state = this.#regions.get(id);
    if (!state) return;
    this.#regions.delete(id);
    this.#components.delete(id);
    try { this.run(() => state.cleanup()); } catch (error) { this.#report(error); }
    this.#changed();
  }
  #set(id: string, placement: Placement, content?: UiNode | string[] | SlotFactory | FooterFactory) {
    this.#assertActive();
    if (typeof content !== "function") {
      const node = Array.isArray(content) ? { id: "slot-content", type: "custom", lines: content } : content;
      const parsed = node === undefined ? undefined : uiNodeSchema.parse(node);
      this.#remove(id);
      if (parsed) this.mount(parsed, { id, placement }); else this.#changed();
      return;
    }
    const component = new PiComponentAdapter("slot-content");
    const footer = placement === "footer" ? new ExtensionFooter({
      cwd: () => {
        const cwd: unknown = this.#native ? JSON.parse(this.#native.query('{"type":"cwd"}')) : this.#cwd;
        return typeof cwd === "string" ? cwd : this.#cwd;
      },
      statuses: () => this.#statuses,
      providerCount: () => {
        const models: unknown = this.#native ? JSON.parse(this.#native.query('{"type":"availableModels"}')) : [];
        if (!Array.isArray(models)) return 0;
        return new Set(models.flatMap((model: unknown) => model && typeof model === "object" && "provider" in model && typeof model.provider === "string" ? [model.provider] : [])).size;
      },
      run: callback => this.run(callback), report: error => this.#report(error),
    }) : undefined;
    let disposed = false;
    const cleanup = () => {
      if (disposed) return;
      disposed = true;
      footer?.dispose();
      this.run(() => component.dispose());
    };
    const render = (snapshot: UiSnapshot) => {
      const width = snapshot.areas.find(area => area.placement === placement)?.width || snapshot.columns;
      const focused = snapshot.regions.find(region => region.owner === this.owner && region.regionId === id)?.focused ?? false;
      return { root: component.render(width, false, focused) };
    };
    try {
      const tui = component.tui(() => this.#query(), () => this.#changed());
      component.setComponent(this.run(() => footer ? (content as FooterFactory)(tui, this.theme, footer) : (content as SlotFactory)(tui, this.theme)));
      const initial = this.run(() => render(this.#query()));
      this.#remove(id);
      this.#mount(initial.root, { id, placement, onEvent: event => {
        if (event.event.type === "customInput") {
          try { component.handleInput(event.event.data); this.#changed(); }
          catch (error) { this.#remove(id); this.#report(error); }
        }
      } }, snapshot => {
        try { return render(snapshot); }
        catch (error) { this.#remove(id); this.#report(error); return undefined; }
      }, cleanup, error => { this.#remove(id); this.#report(error); });
      this.#components.set(id, component);
    } catch (error) {
      try { cleanup(); } catch (disposeError) { this.#report(disposeError); }
      throw error;
    }
  }
  setWidget(key: string, content?: UiNode | string[] | SlotFactory, options?: { placement?: "aboveEditor" | "belowEditor" }) { this.#set(`widget:${key}`, options?.placement ?? "aboveEditor", content); }
  setHeader(content?: UiNode | SlotFactory) { this.#set("header", "header", content); }
  setFooter(content?: UiNode | FooterFactory) { this.#set("footer", "footer", content); }
  setStatus(key: string, text?: string) { this.#assertActive(); if (text === undefined) this.#statuses.delete(key); else this.#statuses.set(key, text); this.#changed(); }
  registerShortcut(key: string, handler: () => Promise<void>) { this.#assertActive(); this.#shortcuts.set(key, handler); this.#changed(); }
  notify(message: unknown, level = "info") { this.#assertActive(); this.#native?.notify(JSON.stringify({ type: "uiNotify", message: String(message), level })); }
  setTheme(name: UiTheme) {
    this.#assertActive();
    if (name !== "light" && name !== "dark" && name !== "system") throw new Error(`Unknown UI theme: ${name}`);
    this.#command({ channel: "product", command: { type: "theme", name } });
  }
  setProgress(active: boolean) { this.#assertActive(); this.#command({ channel: "product", command: { type: "progress", active } }); }
  setProgramStatus(status: { state: "working" | "error" | "success" | "clear"; text?: string; progress?: number }) {
    this.#assertActive(); this.#command({ channel: "product", command: { type: "programStatus", state: status.state, text: status.text ?? null, progress: status.progress ?? null } });
  }
  setTitle(title: string) { this.#assertActive(); this.#command({ channel: "product", command: { type: "title", title } }); }
  setEditorText(text: string) { this.#assertActive(); this.#command({ channel: "product", command: { type: "editorText", text, append: false } }); }
  pasteToEditor(text: string) { this.#assertActive(); this.#command({ channel: "product", command: { type: "editorText", text, append: true } }); }
  getEditorText() { this.#assertActive(); return this.#query().editorText; }
  getToolsExpanded() { this.#assertActive(); return this.#query().toolsExpanded; }
  setToolsExpanded(expanded: boolean) { this.#assertActive(); this.#command({ channel: "product", command: { type: "toolsExpanded", expanded } }); }
  setWorkingMessage(message = "Working...") { this.#workingConfigured = true; this.#workingMessage = message; this.#changed(); }
  setWorkingVisible(visible: boolean) { this.#workingConfigured = true; this.#workingVisible = visible; this.#changed(); }
  setHiddenThinkingLabel(label?: string) { this.transcript.setHiddenThinkingLabel(label); }
  hostOperation(operation: UiHostOperation): Promise<string | undefined> {
    this.#assertActive();
    if (!this.#native) return Promise.reject(new Error("Extension UI is unavailable"));
    return new Promise((resolve, reject) => {
      const id = ++this.#hostId;
      this.#hostRequests.set(id, { resolve, reject });
      this.#wake();
      try { this.#command({ channel: "host", command: { id, operation } }); } catch (error) { this.#hostRequests.delete(id); reject(error); }
    });
  }
  retire() {
    if (this.#retired) return;
    try { this.#command({ channel: "surface", command: { type: "close" } }); } catch {}
    this.#retired = true;
    for (const cancel of this.#pending) cancel();
    this.#pending.clear();
    for (const request of this.#hostRequests.values()) request.reject(new Error("Extension UI generation retired"));
    this.#hostRequests.clear();
    for (const id of [...this.#regions.keys()]) this.#remove(id);
    this.#components.clear();
    this.#statuses.clear();
    this.#shortcuts.clear();
    this.run(() => this.transcript.dispose());
  }
}
