import { CURSOR_MARKER, getKeybindings, visibleWidth, type Component, type Focusable } from "@earendil-works/pi-tui";
import { uiNodeSchema, type UiNode, type UiRegion, type UiSnapshot } from "./extension-ui-frame.js";

export type PiComponent = Component & Partial<Focusable> & { dispose?(): void };
type InputListener = (data: string) => { consume?: boolean; data?: string } | undefined;
export interface CustomTui {
  readonly terminal: { readonly columns: number; readonly rows: number };
  requestRender(): void;
  setFocus(component: PiComponent | null): void;
  addInputListener(listener: InputListener): () => void;
}
export type CustomFactory<T = unknown> = (tui: CustomTui, theme: PiTheme, keys: ReturnType<typeof getKeybindings>, done: (value: T) => void) => PiComponent | Promise<PiComponent>;
export interface CustomOptions {
  signal?: AbortSignal;
  timeout?: number;
  overlay?: boolean;
  overlayOptions?: { width?: number | `${number}%`; maxHeight?: number | `${number}%`; nonModal?: boolean };
}
export { getKeybindings };

function isComponent(value: unknown): value is PiComponent {
  return value !== null && typeof value === "object" && typeof Reflect.get(value, "render") === "function";
}

/** Owns one Pi component. Mounts and awaited results remain owned by ExtensionUI. */
export class PiComponentAdapter {
  #component?: PiComponent;
  #focused?: PiComponent | null;
  #width = -1;
  #disposed = false;
  readonly #listeners = new Set<InputListener>();

  constructor(private readonly id: string) {}
  get component() { return this.#component; }
  setComponent(value: unknown): void {
    if (!isComponent(value)) throw new Error("Pi TUI custom factory must return a Component with render(width)");
    if (this.#disposed) { value.dispose?.(); return; }
    if (value === this.#component) return;
    const previous = this.#component;
    this.#component = value;
    this.invalidate();
    if (previous) {
      this.#focus(null);
      this.#focused = undefined;
      previous.dispose?.();
    }
  }
  invalidate() { this.#width = -1; }
  render(width: number, interactive = false): UiNode {
    const component = this.#component;
    if (!component || this.#disposed) throw new Error("Pi component is disposed");
    width = Math.max(1, Math.floor(width));
    if (interactive && this.#focused === undefined) this.#focus(component);
    if (width !== this.#width) { component.invalidate?.(); this.#width = width; }
    const rendered = component.render(width);
    if (!Array.isArray(rendered) || rendered.some(line => typeof line !== "string")) throw new Error("Pi TUI Component.render(width) must return string[]");
    let cursor: [number, number] | undefined;
    const lines = rendered.map((line, row) => {
      const marker = line.indexOf(CURSOR_MARKER);
      if (marker >= 0 && row <= 65535) cursor = [row, Math.min(65535, visibleWidth(line.slice(0, marker)))];
      // pi-tui's private fake-cursor markers are local styling, not terminal commands.
      return line.replaceAll(CURSOR_MARKER, "").replaceAll("\x1b_pi:fc\x07", "\x1b[7m").replaceAll("\x1b_pi:/fc\x07", "\x1b[27m");
    });
    return { id: this.id, type: "custom", lines, interactive, keyRelease: Boolean((this.#focused ?? component).wantsKeyRelease), ...(cursor ? { cursor } : {}) };
  }
  present(value: unknown, width: number): UiNode | undefined {
    if (isComponent(value)) { this.setComponent(value); return this.render(width); }
    this.#release();
    return value === undefined ? undefined : uiNodeSchema.parse(value);
  }
  #focus(component: PiComponent | null) {
    if (this.#focused && "focused" in this.#focused) this.#focused.focused = false;
    this.#focused = component;
    if (component && "focused" in component) component.focused = true;
  }
  tui(snapshot: () => UiSnapshot, wake: () => void): CustomTui {
    return {
      terminal: { get columns() { return snapshot().columns; }, get rows() { return snapshot().rows; } },
      requestRender: () => { if (!this.#disposed) wake(); },
      setFocus: component => { if (!this.#disposed) { this.#focus(component); wake(); } },
      addInputListener: listener => {
        if (!this.#disposed) this.#listeners.add(listener);
        return () => { this.#listeners.delete(listener); };
      },
    };
  }
  handleInput(data: string) {
    if (this.#disposed) return;
    for (const listener of this.#listeners) {
      const result = listener(data);
      if (this.#disposed || result?.consume) return;
      data = result?.data ?? data;
    }
    (this.#focused === undefined ? this.#component : this.#focused)?.handleInput?.(data);
  }
  #release() {
    const component = this.#component;
    this.#component = undefined;
    this.#focus(null);
    this.#focused = undefined;
    component?.dispose?.();
  }
  dispose() {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#listeners.clear();
    this.#release();
  }
}

function dimension(value: number | `${number}%` | undefined, available: number): number | undefined {
  if (value === undefined) return undefined;
  const number = typeof value === "number" ? value : available * Number.parseFloat(value) / 100;
  if (!Number.isFinite(number) || number < 0) throw new Error("Pi TUI overlay dimensions must be nonnegative numbers or percentages");
  return Math.min(65535, Math.floor(number));
}

export function customLayout(snapshot: UiSnapshot, options: CustomOptions): { width: number; placement: UiRegion["placement"]; overlay?: UiRegion["overlay"] } {
  if (!options.overlay) return { width: snapshot.areas.find(area => area.placement === "editor")?.width || snapshot.columns, placement: "editor" };
  const width = Math.min(snapshot.columns, dimension(options.overlayOptions?.width, snapshot.columns) ?? Math.max(1, snapshot.columns - 4));
  return { width, placement: "overlay", overlay: { width, maxHeight: dimension(options.overlayOptions?.maxHeight, snapshot.rows) ?? snapshot.rows, modal: !options.overlayOptions?.nonModal } };
}

const foregrounds: Record<string, number> = {
  accent: 36, border: 36, borderAccent: 36, borderMuted: 90, success: 32, error: 31, warning: 33,
  muted: 90, dim: 90, text: 39, toolTitle: 36, toolOutput: 39, userMessageText: 39,
  customMessageText: 39, customMessageLabel: 36, mdHeading: 36, mdLink: 34, mdLinkUrl: 90,
  mdCode: 33, mdCodeBlock: 39, mdCodeBlockBorder: 90, mdQuote: 90, mdQuoteBorder: 90, mdHr: 90,
  mdListBullet: 36, toolDiffAdded: 32, toolDiffRemoved: 31, toolDiffContext: 90,
  syntaxComment: 90, syntaxKeyword: 35, syntaxFunction: 34, syntaxVariable: 39,
  syntaxString: 32, syntaxNumber: 33, syntaxType: 36, syntaxOperator: 39, syntaxPunctuation: 90,
};

type ColorValue = string | number;
function color(value: ColorValue | undefined, background: boolean, fallback: number): string {
  if (typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= 255) return `\x1b[${background ? 48 : 38};5;${value}m`;
  if (typeof value === "string" && /^#[\da-f]{6}$/i.test(value)) {
    const rgb = [1, 3, 5].map(offset => Number.parseInt(value.slice(offset, offset + 2), 16));
    return `\x1b[${background ? 48 : 38};2;${rgb.join(";")}m`;
  }
  return `\x1b[${fallback}m`;
}

/** Pi theme formatting, using terminal palette colors by default. */
export class PiTheme {
  readonly name: string;
  constructor(private readonly foreground: Record<string, ColorValue> = {}, private readonly background: Record<string, ColorValue> = {}, _mode?: unknown, options: { name?: string } = {}) {
    this.name = options.name ?? "system";
  }
  fg(token: string, text: string): string { return `${this.getFgAnsi(token)}${text}\x1b[39m`; }
  bg(token: string, text: string): string { return `${this.getBgAnsi(token)}${text}\x1b[49m`; }
  getFgAnsi(token: string): string { return color(this.foreground[token], false, foregrounds[token] ?? 39); }
  getBgAnsi(token: string): string { return color(this.background[token], true, 49); }
  bold(text: string): string { return `\x1b[1m${text}\x1b[22m`; }
  dim(text: string): string { return `\x1b[2m${text}\x1b[22m`; }
  italic(text: string): string { return `\x1b[3m${text}\x1b[23m`; }
  underline(text: string): string { return `\x1b[4m${text}\x1b[24m`; }
  inverse(text: string): string { return `\x1b[7m${text}\x1b[27m`; }
  strikethrough(text: string): string { return `\x1b[9m${text}\x1b[29m`; }
}

export const piTheme = new PiTheme();

/** The second renderCall argument serves both the new context and the old Theme contract. */
export function withPiTheme<T extends object>(context: T): T & PiTheme {
  return Object.assign(Object.create(piTheme), context) as T & PiTheme;
}

export function getSelectListTheme() {
  return { selectedPrefix: (s: string) => piTheme.fg("accent", s), selectedText: (s: string) => piTheme.fg("accent", s), description: (s: string) => piTheme.fg("muted", s), scrollInfo: (s: string) => piTheme.fg("muted", s), noMatch: (s: string) => piTheme.fg("muted", s) };
}
export function getEditorTheme() { return { borderColor: (s: string) => piTheme.fg("borderMuted", s), selectList: getSelectListTheme() }; }
export function getMarkdownTheme() {
  return {
    heading: (s: string) => piTheme.fg("mdHeading", s), link: (s: string) => piTheme.fg("mdLink", s), linkUrl: (s: string) => piTheme.fg("mdLinkUrl", s),
    code: (s: string) => piTheme.fg("mdCode", s), codeBlock: (s: string) => piTheme.fg("mdCodeBlock", s), codeBlockBorder: (s: string) => piTheme.fg("mdCodeBlockBorder", s),
    quote: (s: string) => piTheme.fg("mdQuote", s), quoteBorder: (s: string) => piTheme.fg("mdQuoteBorder", s), hr: (s: string) => piTheme.fg("mdHr", s), listBullet: (s: string) => piTheme.fg("mdListBullet", s),
    bold: (s: string) => piTheme.bold(s), italic: (s: string) => piTheme.italic(s), underline: (s: string) => piTheme.underline(s), strikethrough: (s: string) => piTheme.strikethrough(s),
    highlightCode: (code: string) => code.split("\n"),
  };
}
