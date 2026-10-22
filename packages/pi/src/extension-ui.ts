import {
  Container,
  Editor,
  Input,
  SelectList,
  matchesKey,
  type Component,
  type EditorComponent,
  type OverlayHandle,
  type OverlayOptions,
  type Terminal,
  type TUI,
  type TuiInputListener,
} from "@pi-rs/pi-tui-runtime";
import { randomUUID } from "node:crypto";
import type { NativeExtensionContext } from "./native-binding.js";
import {
  componentRegion,
  uiEventSchema,
  uiSnapshotSchema,
  type Placement,
  type UiSnapshot,
} from "./extension-ui-frame.js";
import {
  CustomEditor,
  extensionTheme,
  extensionKeybindings,
  getEditorTheme,
  getSelectListTheme,
} from "./extension-ui-theme.js";

type Disposable = Component & { dispose?(): void; focused?: boolean };
type Factory<T> = (
  tui: TUI,
  theme: typeof extensionTheme,
  keybindings: typeof extensionKeybindings,
  done: (value: T) => void,
) => Disposable | Promise<Disposable>;
type EditorFactory = (
  tui: TUI,
  theme: ReturnType<typeof getEditorTheme>,
  keybindings: typeof extensionKeybindings,
) => EditorComponent & { dispose?(): void };
interface Mounted {
  component: Disposable;
  placement: Placement;
  options?: OverlayOptions | (() => OverlayOptions);
  hidden?: boolean;
  close?: () => void;
}
interface DialogOptions {
  signal?: AbortSignal;
  timeout?: number;
}
interface CustomOptions {
  overlay?: boolean;
  overlayOptions?: OverlayOptions | (() => OverlayOptions);
  onHandle?: (handle: OverlayHandle) => void;
}

/** One component host per generation. Never starts a terminal or writes to stdout. */
export class ExtensionUI {
  readonly owner = randomUUID();
  readonly theme = extensionTheme;
  readonly #components = new Map<string, Mounted>();
  readonly #listeners = new Set<TuiInputListener>();
  readonly #statuses = new Map<string, string>();
  readonly #dialogs: Mounted[] = [];
  readonly #pendingClosers = new Set<() => void>();
  readonly #shortcuts = new Map<string, () => Promise<void>>();
  readonly #tui: TUI;
  #native?: NativeExtensionContext;
  #snapshot: UiSnapshot = {
    columns: 80,
    rows: 24,
    editorText: "",
    toolsExpanded: false,
  };
  #running = false;
  #retired = false;
  #editorFactory?: EditorFactory;
  #editor?: EditorComponent & { dispose?(): void };
  #sequence = 0;
  #editorTextOverride?: string;

  constructor() {
    const terminal: Terminal = {
      start: () => {
        throw new Error("Ratatui owns the terminal");
      },
      stop() {},
      drainInput: async () => {},
      write: () => {
        throw new Error(
          "Direct terminal output is unsupported; return component lines",
        );
      },
      get columns() {
        return host.#snapshot.columns;
      },
      get rows() {
        return host.#snapshot.rows;
      },
      kittyProtocolActive: false,
      moveBy() {},
      hideCursor() {},
      showCursor() {},
      clearLine() {},
      clearFromCursor() {},
      clearScreen() {},
      setTitle: (title) => this.setTitle(title),
      setProgress() {},
      setProgramStatus() {},
    };
    const host = this;
    const root = new Container();
    this.#tui = Object.assign(root, {
      mode: "fullscreen" as const,
      terminal,
      fullRedraws: 0,
      getShowHardwareCursor: () => true,
      setShowHardwareCursor() {},
      getClearOnShrink: () => false,
      setClearOnShrink() {},
      setFocus: (c: Component | null) => {
        for (const m of this.#all()) m.component.focused = m.component === c;
      },
      showOverlay: (component: Component, options?: OverlayOptions) =>
        this.#overlay(component, options),
      hideOverlay: () => {
        this.#dialogs.at(-1)?.close?.();
        this.#wake();
      },
      hasOverlay: () => this.#dialogs.some((m) => !m.hidden),
      start: () => {
        throw new Error("Ratatui owns the terminal");
      },
      stop() {},
      renderNow: () => this.#wake(),
      requestRender: () => this.#wake(),
      addInputListener: (listener: TuiInputListener) =>
        this.onTerminalInput(listener),
      removeInputListener: (listener: TuiInputListener) => {
        this.#listeners.delete(listener);
      },
      onTerminalColorSchemeChange: () => () => {},
      setTerminalColorSchemeNotifications() {},
      queryTerminalColors: async () => ({}),
    }) as TUI;
  }

  get nativeContext() {
    return this.#native;
  }
  forContext(signal: AbortSignal): object {
    const dialogs = new Set(["select", "confirm", "input"]);
    return new Proxy(this, {
      get: (target, property) => {
        if (target.#retired) throw new Error("Extension UI generation retired");
        const value = Reflect.get(target, property, target);
        if (typeof value !== "function") return value;
        return (...args: unknown[]) => {
          if (target.#retired) throw new Error("Extension UI generation retired");
          if (dialogs.has(String(property))) {
            const opts = args[2] as DialogOptions | undefined;
            args[2] = {
              ...opts,
              signal: opts?.signal
                ? AbortSignal.any([signal, opts.signal])
                : signal,
            };
          } else if (property === "custom" || property === "editor")
            args[2] = signal;
          return Reflect.apply(value, target, args);
        };
      },
    });
  }
  bind(native: NativeExtensionContext) {
    this.#native = native;
  }
  activate() {
    if (this.#shortcuts.size) this.#wake();
  }
  registerShortcut(key: string, handler: () => Promise<void>) {
    this.#shortcuts.set(key, handler);
  }

  #query() {
    if (this.#native)
      this.#snapshot = uiSnapshotSchema.parse(
        JSON.parse(this.#native.query('{"type":"uiSnapshot"}')),
      );
    if (this.#editorTextOverride !== undefined) {
      if (this.#snapshot.editorText === this.#editorTextOverride) this.#editorTextOverride = undefined;
      else this.#snapshot = { ...this.#snapshot, editorText: this.#editorTextOverride };
    }
    return this.#snapshot;
  }
  #action(action: Record<string, unknown>) {
    if (!this.#retired)
      this.#native?.notify(
        JSON.stringify({ type: "uiAction", owner: this.owner, action }),
      );
  }
  #wake() {
    if (this.#retired || !this.#native) return;
    if (!this.#running) {
      this.#running = true;
      void this.#pump();
    } else this.#action({ type: "wake" });
  }
  #all(): Mounted[] {
    return [...this.#components.values(), ...this.#dialogs];
  }
  async #pump() {
    try {
      while (!this.#retired && this.#native) {
        const snapshot = this.#query();
        const focused =
          [...this.#dialogs].reverse().find((m) => !m.hidden) ??
          this.#components.get("editor");
        const regions = this.#all()
          .filter(
            (m) =>
              !m.hidden &&
              (m.placement !== "editor" ||
                m === focused ||
                focused?.placement === "overlay"),
          )
          .map((m) => {
            m.component.focused = m === focused;
            const options =
              typeof m.options === "function" ? m.options() : m.options;
            const size = (
              n: number | `${number}%` | undefined,
              total: number,
              fallback: number,
            ) =>
              n === undefined
                ? fallback
                : typeof n === "number"
                  ? n
                  : Math.floor((total * parseFloat(n)) / 100);
            const width = Math.max(
              1,
              Math.min(
                snapshot.columns,
                size(
                  options?.width,
                  snapshot.columns,
                  m.placement === "overlay"
                    ? Math.min(80, snapshot.columns)
                    : snapshot.columns,
                ),
              ),
            );
            const region = componentRegion(
              m.component.render(width),
              m.placement,
              width,
            );
            if (m.placement === "overlay") {
              const height = Math.min(
                region.lines.length,
                snapshot.rows,
                size(options?.maxHeight, snapshot.rows, snapshot.rows),
              );
              region.lines = region.lines.slice(0, height);
              const anchor = options?.anchor ?? "center";
              region.row = Math.max(
                0,
                Math.min(
                  snapshot.rows - height,
                  size(
                    options?.row,
                    Math.max(0, snapshot.rows - height),
                    anchor.startsWith("top")
                      ? 0
                      : anchor.startsWith("bottom")
                        ? snapshot.rows - height
                        : Math.floor((snapshot.rows - height) / 2),
                  ) + (options?.offsetY ?? 0),
                ),
              );
              region.column = Math.max(
                0,
                Math.min(
                  snapshot.columns - width,
                  size(
                    options?.col,
                    Math.max(0, snapshot.columns - width),
                    anchor.endsWith("left")
                      ? 0
                      : anchor.endsWith("right")
                        ? snapshot.columns - width
                        : Math.floor((snapshot.columns - width) / 2),
                  ) + (options?.offsetX ?? 0),
                ),
              );
            }
            return region;
          });
        if (this.#statuses.size && !this.#components.has("footer"))
          regions.push(
            componentRegion(
              [[...this.#statuses.values()].join(" · ")],
              "footer",
              snapshot.columns,
            ),
          );
        const event = uiEventSchema.parse(
          JSON.parse(
            await this.#native.request(
              JSON.stringify({
                type: "uiExchange",
                frame: {
                  owner: this.owner,
                  regions,
                  captureInput:
                    !!focused ||
                    this.#listeners.size > 0 ||
                    this.#shortcuts.size > 0,
                },
              }),
            ),
          ),
        );
        if (event.type === "closed") {
          this.retire();
          break;
        }
        this.#snapshot = event.snapshot;
        if (event.type === "input") {
          this.#sequence = event.sequence;
          let data = event.data;
          let consumed = false;
          for (const listener of this.#listeners) {
            const result = listener(data);
            if (result?.consume) {
              consumed = true;
              break;
            }
            if (result?.data !== undefined) data = result.data;
          }
          if (!consumed && this.#dialogs.length === 0) {
            for (const [key, handler] of this.#shortcuts) {
              if (matchesKey(data, key as Parameters<typeof matchesKey>[1])) {
                consumed = true;
                void handler().catch((error) => {
                  if (!this.#retired) this.notify(String(error), "error");
                });
                break;
              }
            }
          }
          if (!consumed) {
            if (focused?.component.handleInput)
              focused.component.handleInput(data);
            else
              this.#action({ type: "forwardInput", sequence: event.sequence });
          }
        } else {
          if (
            this.#editor &&
            this.#editor.getText() !== event.snapshot.editorText
          )
            this.#editor.setText(event.snapshot.editorText);
          for (const m of this.#all()) m.component.invalidate();
        }
      }
    } catch (error) {
      if (!this.#retired) {
        try {
          this.notify(
            `Extension UI: ${error instanceof Error ? error.message : String(error)}`,
            "error",
          );
        } catch {
          /* retired context */
        }
        this.retire();
      }
    } finally {
      this.#running = false;
    }
  }

  #dispose(component: Disposable) {
    try {
      component.dispose?.();
    } catch (error) {
      try {
        this.notify(`Extension dispose: ${String(error)}`, "error");
      } catch {}
    }
  }
  #remove(m: Mounted) {
    const index = this.#dialogs.indexOf(m);
    if (index < 0) return;
    this.#dialogs.splice(index, 1);
    this.#dispose(m.component);
    this.#wake();
  }
  #overlay(component: Disposable, options?: OverlayOptions): OverlayHandle {
    const m: Mounted = { component, placement: "overlay", options };
    m.close = () => this.#remove(m);
    this.#dialogs.push(m);
    this.#wake();
    return this.#handle(m);
  }
  #handle(m: Mounted): OverlayHandle {
    return {
      hide: () => m.close?.(),
      setHidden: (hidden) => {
        m.hidden = hidden;
        this.#wake();
      },
      isHidden: () => !!m.hidden,
      focus: () => {
        m.hidden = false;
        const i = this.#dialogs.indexOf(m);
        if (i >= 0) this.#dialogs.splice(i, 1);
        this.#dialogs.push(m);
        this.#wake();
      },
      unfocus: () => {},
      isFocused: () =>
        [...this.#dialogs].reverse().find((d) => !d.hidden) === m,
      getBounds: () => undefined,
    } satisfies OverlayHandle;
  }
  async custom<T>(
    factory: Factory<T>,
    options?: CustomOptions,
    signal?: AbortSignal,
  ): Promise<T | undefined> {
    if (this.#retired || signal?.aborted) return undefined;
    return new Promise<T | undefined>((resolve, reject) => {
      let m: Mounted | undefined;
      let finished = false;
      const cancel = () => done(undefined);
      const done = (value: unknown) => {
        if (finished) return;
        finished = true;
        this.#pendingClosers.delete(cancel);
        signal?.removeEventListener("abort", cancel);
        if (m) this.#remove(m);
        resolve(value as T | undefined);
      };
      this.#pendingClosers.add(cancel);
      signal?.addEventListener("abort", cancel, { once: true });
      Promise.resolve()
        .then(() => factory(this.#tui, this.theme, extensionKeybindings, done))
        .then((component) => {
          if (finished || this.#retired) {
            component.dispose?.();
            done(undefined);
            return;
          }
          m = {
            component,
            placement: options?.overlay ? "overlay" : "editor",
            options: options?.overlayOptions,
            close: () => done(undefined),
          };
          this.#dialogs.push(m);
          options?.onHandle?.(this.#handle(m));
          this.#wake();
        })
        .catch((error) => {
          this.#pendingClosers.delete(cancel);
          signal?.removeEventListener("abort", cancel);
          reject(error);
        });
    });
  }
  #dialog<T>(
    title: string,
    build: (done: (value: T | undefined) => void) => Disposable,
    options?: DialogOptions,
  ): Promise<T | undefined> {
    if (options?.signal?.aborted) return Promise.resolve(undefined);
    return this.custom<T | undefined>((_tui, _theme, _kb, done) => {
      const started = Date.now();
      const child = build(done);
      const abort = () => done(undefined);
      options?.signal?.addEventListener("abort", abort, { once: true });
      if (options?.signal?.aborted) queueMicrotask(abort);
      const timeout =
        options?.timeout === undefined
          ? undefined
          : setTimeout(abort, Math.max(0, options.timeout));
      const countdown =
        options?.timeout === undefined
          ? undefined
          : setInterval(() => this.#wake(), 250);
      return {
        render: (width) => {
          child.focused = true;
          return [
            ...title
              .split("\n")
              .map((line) =>
                this.theme.fg(
                  "accent",
                  line +
                    (options?.timeout === undefined
                      ? ""
                      : ` (${Math.max(0, Math.ceil((options.timeout - Date.now() + started) / 1000))}s)`),
                ),
              ),
            "",
            ...child.render(width),
          ];
        },
        handleInput: (data) => child.handleInput?.(data),
        invalidate: () => child.invalidate(),
        dispose: () => {
          clearTimeout(timeout);
          clearInterval(countdown);
          options?.signal?.removeEventListener("abort", abort);
          child.dispose?.();
        },
      };
    });
  }
  select(title: string, options: string[], opts?: DialogOptions) {
    return this.#dialog<string>(
      title,
      (done) => {
        const list = new SelectList(
          options.map((value) => ({ value, label: value })),
          10,
          getSelectListTheme(),
        );
        list.onSelect = (item) => done(item.value);
        list.onCancel = () => done(undefined);
        return list;
      },
      opts,
    );
  }
  async confirm(title: string, message: string, opts?: DialogOptions) {
    return (
      (await this.select(`${title}\n${message}`, ["Yes", "No"], opts)) === "Yes"
    );
  }
  input(title: string, placeholder?: string, opts?: DialogOptions) {
    return this.#dialog<string>(
      title,
      (done) => {
        const input = new Input({ placeholder });
        input.onSubmit = done;
        input.onEscape = () => done(undefined);
        return input;
      },
      opts,
    );
  }
  editor(title: string, prefill = "", signal?: AbortSignal) {
    return this.#dialog<string>(
      `${title} · Enter submits · Shift+Enter newline · Esc cancels`,
      (done) => {
        const editor = new Editor(this.#tui, getEditorTheme());
        editor.setText(prefill);
        editor.onSubmit = done;
        return {
          render: (width) => {
            editor.focused = true;
            return editor.render(width);
          },
          invalidate: () => editor.invalidate(),
          handleInput: (data) => {
            if (matchesKey(data, "escape")) done(undefined);
            else editor.handleInput(data);
          },
        };
      },
      { signal },
    );
  }
  #set(
    key: string,
    placement: Placement,
    content?:
      | string[]
      | ((tui: TUI, theme: typeof extensionTheme) => Disposable),
  ) {
    const previous = this.#components.get(key);
    if (previous) this.#dispose(previous.component);
    this.#components.delete(key);
    if (content)
      this.#components.set(key, {
        placement,
        component:
          typeof content === "function"
            ? content(this.#tui, this.theme)
            : { render: () => content, invalidate() {} },
      });
    this.#wake();
  }
  setWidget(
    key: string,
    content?:
      | string[]
      | ((tui: TUI, theme: typeof extensionTheme) => Disposable),
    options?: { placement?: "aboveEditor" | "belowEditor" },
  ) {
    this.#set(`widget:${key}`, options?.placement ?? "aboveEditor", content);
  }
  setHeader(factory?: (tui: TUI, theme: typeof extensionTheme) => Disposable) {
    this.#set("header", "header", factory);
  }
  setFooter(
    factory?: (
      tui: TUI,
      theme: typeof extensionTheme,
      data: {
        getGitBranch(): undefined;
        getExtensionStatuses(): ReadonlyMap<string, string>;
        onBranchChange(cb: () => void): () => void;
      },
    ) => Disposable,
  ) {
    this.#set(
      "footer",
      "footer",
      factory &&
        ((tui, theme) =>
          factory(tui, theme, {
            getGitBranch: () => undefined,
            getExtensionStatuses: () => this.#statuses,
            onBranchChange: () => () => {},
          })),
    );
  }
  setStatus(key: string, text?: string) {
    if (text === undefined) this.#statuses.delete(key);
    else this.#statuses.set(key, text);
    this.#wake();
  }
  notify(message: unknown, level = "info") {
    this.#native?.notify(
      JSON.stringify({ type: "uiNotify", message: String(message), level }),
    );
  }
  onTerminalInput(listener: TuiInputListener) {
    this.#listeners.add(listener);
    this.#wake();
    return () => {
      this.#listeners.delete(listener);
      this.#wake();
    };
  }
  setTitle(title: string) {
    this.#action({ type: "title", title });
  }
  getEditorText() {
    return this.#editor?.getText() ?? this.#query().editorText;
  }
  setEditorText(text: string) {
    this.#editorTextOverride = text;
    this.#editor?.setText(text);
    this.#action({ type: "editorText", text, append: false });
    this.#wake();
  }
  pasteToEditor(text: string) {
    if (this.#editor) this.#editor.handleInput(`\x1b[200~${text}\x1b[201~`);
    else this.#action({ type: "editorText", text, append: true });
    this.#wake();
  }
  getToolsExpanded() {
    return this.#query().toolsExpanded;
  }
  setToolsExpanded(expanded: boolean) {
    this.#action({ type: "toolsExpanded", expanded });
  }
  getEditorComponent() {
    return this.#editorFactory;
  }
  setEditorComponent(factory?: EditorFactory) {
    const text = this.getEditorText();
    this.#editorFactory = factory;
    this.#editor?.dispose?.();
    this.#editor = undefined;
    this.#components.delete("editor");
    if (factory) {
      const editor = factory(this.#tui, getEditorTheme(), extensionKeybindings);
      this.#editor = editor;
      editor.setText(text);
      editor.onChange = (value) =>
        this.#action({ type: "editorText", text: value, append: false });
      editor.onSubmit = (value) => {
        editor.setText("");
        this.#action({ type: "submit", text: value });
      };
      if (editor instanceof CustomEditor || "onEscape" in editor) {
        Object.assign(editor, {
          onEscape: () =>
            this.#action({ type: "forwardInput", sequence: this.#sequence }),
          onCtrlD: () =>
            this.#action({ type: "forwardInput", sequence: this.#sequence }),
        });
      }
      this.#components.set("editor", {
        component: editor,
        placement: "editor",
      });
    }
    this.#wake();
  }
  setWorkingMessage(_message?: string) {
    this.notify("setWorkingMessage is not yet supported by Ratatui", "warning");
  }
  setWorkingVisible(_visible: boolean) {
    this.notify("setWorkingVisible is not yet supported by Ratatui", "warning");
  }
  setWorkingIndicator(_options?: { frames: string[]; interval?: number }) {
    this.notify(
      "setWorkingIndicator is not yet supported by Ratatui",
      "warning",
    );
  }
  setHiddenThinkingLabel(_label?: string) {
    this.notify(
      "setHiddenThinkingLabel is not yet supported by Ratatui",
      "warning",
    );
  }
  addAutocompleteProvider(_factory: unknown) {
    this.notify(
      "addAutocompleteProvider is not yet supported by Ratatui",
      "warning",
    );
  }
  getAllThemes() {
    return [{ name: "dark", path: undefined }];
  }
  getTheme(name: string) {
    return name === "dark" ? this.theme : undefined;
  }
  setTheme(name: string | typeof extensionTheme) {
    if (name === "dark" || name === this.theme) return { success: true };
    return {
      success: false,
      error: "Ratatui extension surfaces currently use the dark theme",
    };
  }
  retire() {
    if (this.#retired) return;
    try {
      this.#action({ type: "close" });
    } catch {
      /* native epoch may already be retired */
    }
    this.#retired = true;
    for (const cancel of [...this.#pendingClosers]) cancel();
    for (const m of [...this.#dialogs]) m.close?.();
    for (const m of this.#components.values()) {
      try {
        m.component.dispose?.();
      } catch {
        /* continue disposing */
      }
    }
    this.#components.clear();
    this.#dialogs.length = 0;
    this.#listeners.clear();
    this.#shortcuts.clear();
  }
}
