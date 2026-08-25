import type { HostMode, ToolExecutionMode } from "./extension-protocol.js";

export interface PiContextUsage {
  tokens: number | null;
  contextWindow: number;
  percent: number | null;
}

export interface PiReadonlySessionManager {
  getCwd(): string;
  getSessionDir(): string;
  getSessionId(): string;
  getSessionFile(): string | undefined;
  getLeafId(): string | null;
  getLeafEntry(): unknown | undefined;
  getEntry(id: string): unknown | undefined;
  getLabel(id: string): string | undefined;
  getBranch(fromId?: string): unknown[];
  buildContextEntries(): unknown[];
  getHeader(): unknown | null;
  getEntries(): unknown[];
  getTree(): unknown[];
  getSessionName(): string | undefined;
}

export interface PiModelRegistry {
  getAll(): Record<string, unknown>[];
  getAvailable(): Record<string, unknown>[];
  find(provider: string, modelId: string): Record<string, unknown> | undefined;
  hasConfiguredAuth(model: Record<string, unknown>): boolean;
  getProviderDisplayName(provider: string): string;
}

export interface PiExtensionContext extends Record<string, unknown> {
  cwd: string;
  hasUI: false;
  mode: HostMode;
  signal: AbortSignal;
  ui: Record<string, unknown>;
  sessionManager: PiReadonlySessionManager;
  modelRegistry: PiModelRegistry;
  model: Record<string, unknown> | undefined;
  scopedModels: readonly Record<string, unknown>[];
  thinkingLevel?: string;
  isIdle(): boolean;
  isProjectTrusted(): boolean;
  abort(): void;
  hasPendingMessages(): boolean;
  shutdown(): void;
  getContextUsage(): PiContextUsage | undefined;
  compact(options?: { customInstructions?: string }): void;
  getSystemPrompt(): string;
}

export interface PiExtensionCommandContext extends PiExtensionContext {
  getSystemPromptOptions(): Record<string, unknown>;
  waitForIdle(): Promise<void>;
  newSession(options?: {
    parentSession?: string;
    withSession?: (context: PiExtensionCommandContext) => Promise<void> | void;
  }): Promise<{ cancelled: boolean }>;
  fork(
    entryId: string,
    options?: {
      position?: "before" | "at";
      withSession?: (context: PiExtensionCommandContext) => Promise<void> | void;
    },
  ): Promise<{ cancelled: boolean }>;
  navigateTree(
    targetId: string,
    options?: {
      summarize?: boolean;
      customInstructions?: string;
      replaceInstructions?: boolean;
      label?: string;
    },
  ): Promise<{ cancelled: boolean }>;
  switchSession(
    sessionPath: string,
    options?: {
      withSession?: (context: PiExtensionCommandContext) => Promise<void> | void;
    },
  ): Promise<{ cancelled: boolean }>;
  reload(): Promise<void>;
}

export interface PiToolResult {
  content: unknown[];
  details?: unknown;
  usage?: unknown;
  isError?: boolean;
  terminate?: boolean;
}

export interface PiToolDefinition<TInput = Record<string, unknown>> {
  name: string;
  label?: string;
  description?: string;
  parameters: Record<string, unknown>;
  promptSnippet?: string;
  promptGuidelines?: string[];
  executionMode?: ToolExecutionMode;
  execute(
    toolCallId: string,
    input: TInput,
    signal: AbortSignal,
    update: undefined,
    context: PiExtensionContext,
  ): PiToolResult | Promise<PiToolResult>;
}

export interface PiCommandOptions {
  description?: string;
  argumentHint?: string;
  handler(
    arguments_: string,
    context: PiExtensionCommandContext,
  ): unknown | Promise<unknown>;
}

export interface PiExtensionApi {
  registerTool<TInput>(definition: PiToolDefinition<TInput>): void;
  registerCommand(name: string, options: PiCommandOptions): void;
  registerShortcut(shortcut: string, options: unknown): void;
  registerFlag(name: string, options: unknown): void;
  getFlag(name: string): boolean | string | undefined;
  registerMessageRenderer(customType: string, renderer: unknown): void;
  registerMarkdownTransformer(transformer: unknown): void;
  registerEntryRenderer(customType: string, renderer: unknown): void;
  sendMessage(message: unknown, options?: unknown): void;
  sendUserMessage(content: unknown, options?: unknown): void;
  appendEntry(customType: string, data?: unknown): void;
  setSessionName(name: string): void;
  getSessionName(): string | undefined;
  setLabel(entryId: string, label: string | undefined): void;
  exec(
    command: string,
    args: string[],
    options?: unknown,
  ): Promise<{ stdout: string; stderr: string; code: number; killed: boolean }>;
  getActiveTools(): string[];
  getAllTools(): unknown[];
  setActiveTools(toolNames: string[]): void;
  getCommands(): unknown[];
  setModel(model: Record<string, unknown>): Promise<boolean>;
  getThinkingLevel(): string;
  setThinkingLevel(level: string): void;
  registerProvider(nameOrProvider: string | Record<string, unknown>, config?: unknown): void;
  unregisterProvider(name: string): void;
  readonly events: {
    emit(channel: string, data: unknown): void;
    on(channel: string, handler: (data: unknown) => unknown): () => void;
  };
  on<TEvent extends Record<string, unknown>, TResult = unknown>(
    event: string,
    handler: (
      event: TEvent,
      context: PiExtensionContext,
    ) => TResult | Promise<TResult>,
  ): void;
}
