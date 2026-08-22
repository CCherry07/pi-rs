import type { HostMode, ToolExecutionMode } from "./extension-protocol.js";

export interface PiExtensionContext extends Record<string, unknown> {
  cwd: string;
  hasUI: boolean;
  mode: HostMode;
  signal: AbortSignal;
  isProjectTrusted(): boolean;
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
    context: PiExtensionContext,
  ): unknown | Promise<unknown>;
}

export interface PiExtensionApi {
  registerTool<TInput>(definition: PiToolDefinition<TInput>): void;
  registerCommand(name: string, options: PiCommandOptions): void;
  on<TEvent extends Record<string, unknown>, TResult = unknown>(
    event: string,
    handler: (
      event: TEvent,
      context: PiExtensionContext,
    ) => TResult | Promise<TResult>,
  ): void;
}
