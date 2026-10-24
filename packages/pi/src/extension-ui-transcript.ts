import { z } from "zod";
import { uiNodeSchema, type UiNode, type UiSnapshot } from "./extension-ui-frame.js";

export interface PresentationContext {
  expanded: boolean;
  outputPad: number;
}
export type MessageRenderer = (entry: Record<string, unknown>, context: PresentationContext) => UiNode | undefined;
export type EntryRenderer = MessageRenderer;
export type MarkdownTransformer = (
  text: string,
  context: { messageType: "user" | "assistant" | "assistant-thinking"; isStreaming: boolean },
) => string;
export interface ToolRenderContext extends PresentationContext {
  args: Record<string, unknown>;
  toolCallId: string;
  invalidate(): void;
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
  renderCall?(args: Record<string, unknown>, context: ToolRenderContext): UiNode;
  renderResult?(result: Record<string, unknown>, context: ToolRenderContext): UiNode;
}
export type ToolRendererResolver = (name: string, next: () => ToolRenderers | undefined) => ToolRenderers | undefined;
const callback = z.custom<(...args: never[]) => unknown>((value) => typeof value === "function", "expected a presentation function");
const toolsSchema = z.object({ renderCall: callback.optional(), renderResult: callback.optional() });
const record = (value: unknown): Record<string, unknown> => value !== null && typeof value === "object" ? value as Record<string, unknown> : {};
const text = (value: unknown): string => typeof value === "string" ? value : "";
interface Row { state: unknown; revision: number; signature?: string; node?: UiNode }

/** Plugin callbacks produce semantic content. Rust alone lays out and renders these nodes. */
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
  #tool(payload: Record<string, unknown>, row: Row, expanded: boolean): UiNode | undefined {
    const renderers = this.#resolve(text(payload.name));
    if (!renderers) return undefined;
    const args = record(payload.args);
    const result = record(payload.result);
    const context: ToolRenderContext = {
      args, toolCallId: text(payload.toolCallId), state: row.state, cwd: text(payload.cwd),
      invalidate: () => { if (!this.#retired) { row.revision++; this.wake(); } },
      executionStarted: Boolean(payload.executionStarted), argsComplete: Boolean(payload.argsComplete),
      isPartial: Boolean(payload.isPartial), expanded, showImages: this.showImages,
      isError: Boolean(payload.isError), outputPad: this.outputPad,
      durationMs: !payload.isPartial && typeof result.durationMs === "number" ? result.durationMs : undefined,
    };
    const children: UiNode[] = [renderers.renderCall?.(args, context) ?? { id: "call", type: "text", text: `${text(payload.name)} ${JSON.stringify(args)}`, tone: "accent" }];
    if (payload.result) {
      const fallback = Array.isArray(result.content) ? result.content.map((block) => text(record(block).text)).filter(Boolean).join("\n") : "";
      children.push(renderers.renderResult?.(result, context) ?? { id: "result", type: "text", text: expanded ? fallback : fallback.split("\n").slice(0, 10).join("\n") });
    }
    row.state = context.state;
    return { id: "content", type: "column", children };
  }
  #present(payload: Record<string, unknown>, row: Row, expanded: boolean): UiNode | undefined {
    const kind = text(payload.kind);
    if (kind === "user" || kind === "assistant") return this.#message(payload, kind);
    if (kind === "customMessage" || kind === "customEntry") {
      const entry = record(payload.entry);
      return (kind === "customMessage" ? this.#messages : this.#entries).get(text(entry.customType))?.(entry, { expanded, outputPad: this.outputPad });
    }
    return kind === "tool" ? this.#tool(payload, row, expanded) : undefined;
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
      if (!row) { row = { state: {}, revision: 0 }; this.#rows.set(identity, row); }
      const signature = [item.fingerprint, snapshot.toolsExpanded, this.#revision, row.revision].join(":");
      if (row.signature !== signature) {
        const node = this.#present(item.payload, row, snapshot.toolsExpanded);
        row.node = node === undefined ? undefined : uiNodeSchema.parse(node);
        row.signature = signature;
      }
      if (row.node) output.push({ index: item.index, fingerprint: item.fingerprint, root: row.node });
    }
    for (const identity of this.#rows.keys()) if (!alive.has(identity)) this.#rows.delete(identity);
    return output;
  }
  dispose() { this.#retired = true; this.#rows.clear(); }
}
