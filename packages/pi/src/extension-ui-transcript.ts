import { z } from "zod";
import { uiNodeSchema, type UiNode, type UiSnapshot } from "./extension-ui-frame.js";
import { PiComponentAdapter, piTheme, withPiTheme, type PiComponent, type PiTheme } from "./pi-component-adapter.js";

export interface PresentationContext {
  expanded: boolean;
  outputPad: number;
}
export type MessageRenderer = (entry: Record<string, unknown>, context: PresentationContext, theme: PiTheme) => UiNode | PiComponent | undefined;
export type EntryRenderer = MessageRenderer;
export type MarkdownTransformer = (
  text: string,
  context: { messageType: "user" | "assistant" | "assistant-thinking"; isStreaming: boolean },
) => string;
export interface ToolRenderContext extends PresentationContext {
  args: Record<string, unknown>;
  toolCallId: string;
  invalidate(): void;
  lastComponent: PiComponent | undefined;
  state: unknown;
  cwd: string;
  executionStarted: boolean;
  argsComplete: boolean;
  isPartial: boolean;
  showImages: boolean;
  isError: boolean;
  durationMs: number | undefined;
}
export interface ToolRenderers {
  renderCall?(args: Record<string, unknown>, theme: PiTheme & ToolRenderContext, context: ToolRenderContext): UiNode | PiComponent | undefined;
  renderResult?(result: Record<string, unknown>, options: ToolRenderContext, theme: PiTheme, context: ToolRenderContext): UiNode | PiComponent | undefined;
}
export type ToolRendererResolver = (name: string, next: () => ToolRenderers | undefined) => ToolRenderers | undefined;
const callback = z.custom<(...args: never[]) => unknown>((value) => typeof value === "function", "expected a presentation function");
const toolsSchema = z.object({ renderCall: callback.optional(), renderResult: callback.optional() });
const record = (value: unknown): Record<string, unknown> => value !== null && typeof value === "object" ? value as Record<string, unknown> : {};
const text = (value: unknown): string => typeof value === "string" ? value : "";
interface Row { state: unknown; revision: number; signature?: string; node?: UiNode; components: Map<string, PiComponentAdapter> }
function disposeRow(row: Row) {
  for (const component of row.components.values()) {
    try { component.dispose(); } catch { /* A faulty cleanup must not keep other components alive. */ }
  }
  row.components.clear();
}

/** Rows retain callback state and Pi components; Rust owns transcript composition and rendering. */
export class ExtensionTranscript {
  readonly #messages = new Map<string, MessageRenderer>();
  readonly #entries = new Map<string, EntryRenderer>();
  readonly #tools = new Map<string, ToolRenderers>();
  readonly #resolvers: ToolRendererResolver[] = [];
  readonly #markdown: MarkdownTransformer[] = [];
  readonly #rows = new Map<string, Row>();
  #revision = 0;
  #thinkingLabel: string | undefined;
  #retired = false;

  constructor(private readonly wake: () => void, private readonly outputPad = 1, private readonly showImages = true) {}
  get active() { return Boolean(this.#messages.size || this.#entries.size || this.#tools.size || this.#resolvers.length || this.#markdown.length || this.#thinkingLabel !== undefined); }
  invalidate() { if (!this.#retired) { this.#revision++; this.wake(); } }
  registerMessage(type: string, renderer: unknown) { this.#messages.set(type, callback.parse(renderer) as MessageRenderer); this.invalidate(); }
  registerEntry(type: string, renderer: unknown) { this.#entries.set(type, callback.parse(renderer) as EntryRenderer); this.invalidate(); }
  registerMarkdown(transformer: unknown) { this.#markdown.push(callback.parse(transformer) as MarkdownTransformer); this.invalidate(); }
  registerTool(name: string, value: unknown) {
    const parsed = toolsSchema.parse(value) as ToolRenderers;
    if (parsed.renderCall || parsed.renderResult) { this.#tools.set(name, parsed); this.invalidate(); }
  }
  registerResolver(resolver: unknown) { this.#resolvers.push(callback.parse(resolver) as ToolRendererResolver); this.invalidate(); }
  setHiddenThinkingLabel(label = "Thinking...") { this.#thinkingLabel = label; this.invalidate(); }
  #resolve(name: string, index = 0): ToolRenderers | undefined {
    const resolver = this.#resolvers[index];
    return resolver ? resolver(name, () => this.#resolve(name, index + 1)) : this.#tools.get(name);
  }
  #markdownNode(value: string, messageType: "user" | "assistant" | "assistant-thinking", isStreaming: boolean): UiNode {
    for (const transformer of this.#markdown) value = transformer(value, { messageType, isStreaming });
    return { id: messageType, type: "markdown", text: value };
  }
  #message(payload: Record<string, unknown>, kind: "user" | "assistant"): UiNode | undefined {
    if (!this.#markdown.length && this.#thinkingLabel === undefined) return undefined;
    const children: UiNode[] = [];
    if (payload.thinking) children.push({ id: "thinking", type: "text", text: this.#thinkingLabel ?? "Thinking...", tone: "muted" });
    if (payload.text) children.push(this.#markdownNode(text(payload.text), kind, Boolean(payload.streaming)));
    if (payload.error) children.push({ id: "error", type: "text", text: text(payload.error), tone: "error" });
    return { id: "content", type: "column", children };
  }
  #presentation(row: Row, slot: string, value: unknown, width: number): UiNode | undefined {
    let component = row.components.get(slot);
    if (!component) { component = new PiComponentAdapter(slot); row.components.set(slot, component); }
    return component.present(value, width);
  }
  #tool(payload: Record<string, unknown>, row: Row, expanded: boolean, width: number): UiNode | undefined {
    const renderers = this.#resolve(text(payload.name));
    if (!renderers) return undefined;
    const args = record(payload.args);
    const result = record(payload.result);
    const context = withPiTheme<ToolRenderContext>({
      args, toolCallId: text(payload.toolCallId), state: row.state, cwd: text(payload.cwd),
      lastComponent: row.components.get("call")?.component,
      invalidate: () => {
        if (this.#retired) return;
        for (const component of row.components.values()) component.invalidate();
        row.revision++; this.wake();
      },
      executionStarted: Boolean(payload.executionStarted), argsComplete: Boolean(payload.argsComplete),
      isPartial: Boolean(payload.isPartial), expanded, showImages: this.showImages,
      isError: Boolean(payload.isError), outputPad: this.outputPad,
      durationMs: !payload.isPartial && typeof result.durationMs === "number" ? result.durationMs : undefined,
    });
    const call = renderers.renderCall?.(args, context, context);
    const children: UiNode[] = [this.#presentation(row, "call", call, width) ?? { id: "call", type: "text", text: `${text(payload.name)} ${JSON.stringify(args)}`, tone: "accent" }];
    row.state = context.state;
    if (payload.result) {
      const fallback = Array.isArray(result.content) ? result.content.map((block) => text(record(block).text)).filter(Boolean).join("\n") : "";
      const resultContext: ToolRenderContext = { ...context, lastComponent: row.components.get("result")?.component };
      const rendered = renderers.renderResult?.(result, resultContext, piTheme, resultContext);
      children.push(this.#presentation(row, "result", rendered, width) ?? { id: "result", type: "text", text: expanded ? fallback : fallback.split("\n").slice(0, 10).join("\n") });
      row.state = resultContext.state;
    }
    return { id: "content", type: "column", children };
  }
  #present(payload: Record<string, unknown>, row: Row, expanded: boolean, width: number): UiNode | undefined {
    const kind = text(payload.kind);
    if (kind === "user" || kind === "assistant") return this.#message(payload, kind);
    if (kind === "customMessage" || kind === "customEntry") {
      const entry = record(payload.entry);
      const renderer = (kind === "customMessage" ? this.#messages : this.#entries).get(text(entry.customType));
      const rendered = renderer?.(entry, { expanded, outputPad: this.outputPad }, piTheme);
      return this.#presentation(row, "content", rendered, width);
    }
    return kind === "tool" ? this.#tool(payload, row, expanded, width) : undefined;
  }
  render(snapshot: UiSnapshot) {
    if (this.#retired) return [];
    const alive = new Set<string>();
    const output: { index: number; fingerprint: string; root: UiNode }[] = [];
    for (const item of snapshot.transcript) {
      const kind = text(item.payload.kind);
      const entry = record(item.payload.entry);
      const identity = kind === "tool" ? `tool:${text(item.payload.toolCallId)}` : `${kind}:${text(entry.id) || item.index}`;
      alive.add(identity);
      let row = this.#rows.get(identity);
      if (!row) { row = { state: {}, revision: 0, components: new Map() }; this.#rows.set(identity, row); }
      const width = snapshot.transcriptWidth || snapshot.columns;
      const signature = [item.fingerprint, snapshot.toolsExpanded, width, this.#revision, row.revision].join(":");
      if (row.signature !== signature) {
        const node = this.#present(item.payload, row, snapshot.toolsExpanded, width);
        row.node = node === undefined ? undefined : uiNodeSchema.parse(node);
        row.signature = signature;
      }
      if (row.node) output.push({ index: item.index, fingerprint: item.fingerprint, root: row.node });
    }
    for (const [identity, row] of this.#rows) if (!alive.has(identity)) { this.#rows.delete(identity); disposeRow(row); }
    return output;
  }
  dispose() {
    if (this.#retired) return;
    this.#retired = true;
    for (const row of this.#rows.values()) disposeRow(row);
    this.#rows.clear();
  }
}
