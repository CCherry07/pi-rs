import { existsSync, readFileSync, readdirSync, realpathSync, statSync } from "node:fs";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { createJiti } from "jiti";
import { z } from "zod";

import {
  type AgentPluginManifest,
  type CommandManifest,
  type GenerationManifest,
  type GenerationRequest,
  type HookManifest,
  type HostMode,
  type Invocation,
  type ProviderPluginManifest,
  type SessionPluginManifest,
  type ToolExecutionMode,
  type ToolManifest,
  isRecord,
  parseHostOperation,
  parseJson,
  toolExecutionModeSchema,
} from "./extension-protocol.js";

const hostDirectory = dirname(fileURLToPath(import.meta.url));
const compatibilityModulePath = [
  join(hostDirectory, "compat-api.js"),
  join(hostDirectory, "compat-api.ts"),
].find(existsSync);
if (!compatibilityModulePath) {
  throw new Error(`Cannot locate the Pi extension compatibility module in ${hostDirectory}`);
}
const compatibilityModule: string = compatibilityModulePath;

const jsonObjectSchema = z.looseObject({});
const packageManifestSchema = z.looseObject({
  pi: z
    .looseObject({
      extensions: z.array(z.string()).optional(),
    })
    .optional(),
});
const toolResultSchema = z.looseObject({
  content: z.array(z.unknown()),
  details: z.unknown().optional(),
  usage: z.unknown().optional(),
  isError: z.boolean().default(false),
  terminate: z.boolean().default(false),
});
const callableSchema = z.custom<(...arguments_: never[]) => unknown>(
  (value) => typeof value === "function",
  "expected a function",
);
const toolDefinitionSchema = z.looseObject({
  name: z.string(),
  label: z.string().optional(),
  description: z.string().optional(),
  parameters: z.unknown(),
  promptSnippet: z.string().optional(),
  promptGuidelines: z.array(z.string()).optional(),
  executionMode: toolExecutionModeSchema.optional(),
  prepareArguments: z.unknown().optional(),
  execute: callableSchema,
});
const commandOptionsSchema = z.looseObject({
  description: z.string().optional(),
  argumentHint: z.string().optional(),
  handler: callableSchema,
});
const extensionContextInputSchema = z.looseObject({
  cwd: z.string().optional(),
  session: z.looseObject({ cwd: z.string().optional() }).optional(),
});
const toolInvocationPayloadSchema = z.looseObject({
  context: extensionContextInputSchema.extend({ toolCallId: z.string() }),
  input: z.unknown(),
});
const commandInvocationPayloadSchema = z.looseObject({
  context: extensionContextInputSchema,
  arguments: z.string(),
});
const hookInvocationPayloadSchema = z.looseObject({
  context: extensionContextInputSchema,
  event: jsonObjectSchema,
});

function parseExternal<T>(schema: z.ZodType<T>, value: unknown, description: string): T {
  const result = schema.safeParse(value);
  if (result.success) return result.data;
  throw new Error(`${description} is invalid: ${z.prettifyError(result.error)}`, {
    cause: result.error,
  });
}

const PI_PACKAGE_NAMES = [
  "@earendil-works/pi-coding-agent",
  "@mariozechner/pi-coding-agent",
] as const;
const AGENT_HOOKS = new Set([
  "input",
  "before_agent_start",
  "agent_start",
  "agent_end",
  "turn_start",
  "turn_end",
  "message_start",
  "message_update",
  "message_end",
  "tool_execution_start",
  "tool_execution_update",
  "tool_execution_end",
  "context",
  "tool_call",
  "tool_result",
]);
const PROVIDER_HOOKS = new Set(["before_provider_request"]);
const SESSION_HOOKS = new Set([
  "session_start",
  "session_info_changed",
  "session_before_switch",
  "session_before_fork",
  "session_before_compact",
  "session_compact",
  "session_compact_failed",
  "session_shutdown",
  "session_before_tree",
  "session_tree",
]);

interface ExtensionContext extends Record<string, unknown> {
  cwd: string;
  hasUI: false;
  mode: HostMode;
  signal: AbortSignal;
  isProjectTrusted(): boolean;
  ui: Record<"notify" | "select" | "confirm" | "input" | "editor", () => never>;
}

interface ToolResult {
  content: unknown[];
  details?: unknown;
  usage?: unknown;
  isError: boolean;
  terminate: boolean;
}

interface ToolDefinition {
  name: string;
  label?: string;
  description?: string;
  parameters: unknown;
  promptSnippet?: string;
  promptGuidelines?: string[];
  executionMode?: ToolExecutionMode;
  prepareArguments?: unknown;
  execute(
    toolCallId: string,
    input: unknown,
    signal: AbortSignal,
    update: undefined,
    context: ExtensionContext,
  ): unknown | Promise<unknown>;
}

interface CommandOptions {
  description?: string;
  argumentHint?: string;
  handler(arguments_: string, context: ExtensionContext): unknown | Promise<unknown>;
}

type HookHandler = (event: Record<string, unknown>, context: ExtensionContext) => unknown | Promise<unknown>;
type ExtensionCallback = (invocation: Invocation, signal: AbortSignal) => Promise<unknown>;

interface Contribution {
  tools: ToolManifest[];
  commands: CommandManifest[];
  agentHooks: HookManifest[];
  providerHooks: HookManifest[];
  sessionHooks: HookManifest[];
}

interface GenerationState {
  callbacks: Map<string, ExtensionCallback>;
  active: Map<string, AbortController>;
  mode: HostMode;
  projectTrusted: boolean;
}

interface ExtensionApi {
  registerTool(definition: unknown): void;
  on(event: string, handler: unknown): void;
  registerCommand(name: string, options: unknown): void;
  registerShortcut(): never;
  registerFlag(): never;
  registerProvider(): never;
  registerMessageRenderer(): never;
  registerMarkdownTransformer(): never;
  registerEntryRenderer(): never;
  sendMessage(): never;
  sendUserMessage(): never;
  appendEntry(): never;
  setSessionName(): never;
  setLabel(): never;
  getFlag(): undefined;
}

function readPiManifest(directory: string): string[] | undefined {
  const path = join(directory, "package.json");
  if (!existsSync(path)) return undefined;
  try {
    const value = parseExternal(
      packageManifestSchema,
      parseJson(readFileSync(path, "utf8"), `${path} manifest`),
      `${path} manifest`,
    );
    return value.pi?.extensions;
  } catch {
    return undefined;
  }
}

function resolveExtensionEntries(directory: string): string[] | undefined {
  const declared = readPiManifest(directory);
  if (declared?.length) {
    const entries = declared
      .map((entry) => resolve(directory, entry))
      .filter((entry) => existsSync(entry));
    if (entries.length) return entries;
  }
  for (const name of ["index.ts", "index.js", "index.mts", "index.mjs", "index.cts", "index.cjs"]) {
    const entry = join(directory, name);
    if (existsSync(entry)) return [entry];
  }
  return undefined;
}

export function discoverExtensions(directory: string): string[] {
  if (!existsSync(directory)) return [];
  const rootEntries = resolveExtensionEntries(directory);
  if (rootEntries) return rootEntries;

  const discovered: string[] = [];
  for (const entry of readdirSync(directory, { withFileTypes: true }).sort((left, right) =>
    left.name.localeCompare(right.name),
  )) {
    if (entry.name.startsWith(".") || entry.name === "node_modules") continue;
    const path = join(directory, entry.name);
    let stats;
    try {
      stats = entry.isSymbolicLink() ? statSync(path) : entry;
    } catch {
      continue;
    }
    if (stats.isFile() && /\.(?:[cm]?[jt]s)$/.test(entry.name)) {
      discovered.push(path);
    } else if (stats.isDirectory()) {
      discovered.push(...(resolveExtensionEntries(path) ?? []));
    }
  }
  return discovered;
}

function uniqueCanonicalPaths(paths: string[], cwd: string): string[] {
  const unique = new Set<string>();
  const result: string[] = [];
  for (const path of paths) {
    const absolute = isAbsolute(path) ? path : resolve(cwd, path);
    if (!existsSync(absolute)) {
      throw new Error(`JavaScript extension does not exist: ${absolute}`);
    }
    const canonical = realpathSync(absolute);
    const entries = statSync(canonical).isDirectory()
      ? (resolveExtensionEntries(canonical) ?? discoverExtensions(canonical))
      : [canonical];
    for (const entry of entries) {
      const resolved = realpathSync(entry);
      if (!unique.has(resolved)) {
        unique.add(resolved);
        result.push(resolved);
      }
    }
  }
  return result;
}

function cloneSchema(schema: unknown, toolName: string): Record<string, unknown> {
  try {
    const cloned = parseJson(JSON.stringify(schema), `JavaScript tool ${toolName} schema`);
    return parseExternal(jsonObjectSchema, cloned, `JavaScript tool ${toolName} schema`);
  } catch {
    throw new Error(`JavaScript tool ${toolName} parameters must be a JSON schema object`);
  }
}

function normalizeToolResult(result: unknown, toolName: string): ToolResult {
  return parseExternal(toolResultSchema, result, `JavaScript tool ${toolName} result`);
}

function callDynamic(
  callback: unknown,
  thisArgument: unknown,
  arguments_: readonly unknown[],
): unknown {
  if (typeof callback !== "function") throw new Error("dynamic callback is not callable");
  return Reflect.apply(callback, thisArgument, arguments_);
}

function parseToolDefinition(value: unknown, path: string): ToolDefinition {
  const result = toolDefinitionSchema.safeParse(value);
  if (!result.success) {
    throw new Error(
      `registerTool received an invalid definition (${path}): ${z.prettifyError(result.error)}`,
      { cause: result.error },
    );
  }
  const definition = result.data;
  return {
    ...definition,
    execute: (toolCallId, input, signal, update, context) =>
      callDynamic(definition.execute, definition, [toolCallId, input, signal, update, context]),
  };
}

function parseCommandOptions(value: unknown, path: string): CommandOptions {
  const result = commandOptionsSchema.safeParse(value);
  if (!result.success) {
    throw new Error(
      `registerCommand received invalid options (${path}): ${z.prettifyError(result.error)}`,
      { cause: result.error },
    );
  }
  const options = result.data;
  return {
    ...options,
    handler: (arguments_, context) =>
      callDynamic(options.handler, options, [arguments_, context]),
  };
}

function extensionId(index: number, path: string): string {
  return `js:${index}:${basename(path).replace(/[^a-zA-Z0-9._-]/g, "-")}`;
}

export class ExtensionHost {
  #generation = 0;
  readonly #generations = new Map<string, GenerationState>();

  async dispatch(rawOperation: string): Promise<string> {
    const operation = parseHostOperation(rawOperation);
    switch (operation.type) {
      case "prepareGeneration":
        return JSON.stringify(await this.#prepareGeneration(operation.request));
      case "invoke":
        return JSON.stringify(await this.#invoke(operation.invocation));
      case "cancel":
        this.#cancel(operation.invocationId);
        return "null";
      case "retireGeneration":
        this.#retireGeneration(operation.generationId);
        return "null";
    }
  }

  async #prepareGeneration(request: GenerationRequest): Promise<GenerationManifest> {
    const generationId = `js-${++this.#generation}`;
    const state: GenerationState = {
      callbacks: new Map(),
      active: new Map(),
      mode: request.mode,
      projectTrusted: request.projectTrusted,
    };
    this.#generations.set(generationId, state);
    try {
      const discovered = request.discoverExtensions
        ? [
            ...(request.projectTrusted
              ? discoverExtensions(join(request.cwd, ".pi/extensions"))
              : []),
            ...discoverExtensions(join(request.agentDir, "extensions")),
          ]
        : [];
      const paths = uniqueCanonicalPaths([...discovered, ...request.explicitPaths], request.cwd);
      const agentPlugins: AgentPluginManifest[] = [];
      const providerPlugins: ProviderPluginManifest[] = [];
      const sessionPlugins: SessionPluginManifest[] = [];

      for (const [index, path] of paths.entries()) {
        const id = extensionId(index, path);
        const contribution: Contribution = {
          tools: [],
          commands: [],
          agentHooks: [],
          providerHooks: [],
          sessionHooks: [],
        };
        const api = this.#createExtensionApi(generationId, id, path, contribution);
        const alias = Object.fromEntries(PI_PACKAGE_NAMES.map((name) => [name, compatibilityModule]));
        const jiti = createJiti(import.meta.url, {
          alias,
          moduleCache: false,
          interopDefault: true,
        });
        const imported = await jiti.import<unknown>(path, { default: true });
        const factory = parseExternal(
          callableSchema,
          imported,
          `Extension default export (${path})`,
        );
        await callDynamic(factory, undefined, [api]);
        if (
          contribution.tools.length ||
          contribution.commands.length ||
          contribution.agentHooks.length
        ) {
          agentPlugins.push({
            id,
            tools: contribution.tools,
            commands: contribution.commands,
            hooks: contribution.agentHooks,
          });
        }
        if (contribution.providerHooks.length) {
          providerPlugins.push({ id, hooks: contribution.providerHooks });
        }
        if (contribution.sessionHooks.length) {
          sessionPlugins.push({ id, hooks: contribution.sessionHooks });
        }
      }

      return { generationId, agentPlugins, providerPlugins, sessionPlugins };
    } catch (error) {
      this.#retireGeneration(generationId);
      throw error;
    }
  }

  #createExtensionApi(
    generationId: string,
    pluginId: string,
    path: string,
    contribution: Contribution,
  ): ExtensionApi {
    const state = this.#requireGeneration(generationId);
    const unsupported = (name: string): never => {
      throw new Error(`${name} is not supported by the pi_rs NAPI host yet (${path})`);
    };
    return {
      registerTool: (value) => {
        const definition = parseToolDefinition(value, path);
        if (definition.prepareArguments !== undefined) {
          unsupported(`prepareArguments for JavaScript tool ${definition.name}`);
        }
        const callbackId = `${pluginId}:tool:${definition.name}`;
        this.#registerCallback(state, callbackId, async (invocation, signal) => {
          const payload = parseExternal(
            toolInvocationPayloadSchema,
            invocation.payload,
            `JavaScript tool ${definition.name} invocation payload`,
          );
          const result = await definition.execute(
            payload.context.toolCallId,
            payload.input,
            signal,
            undefined,
            this.#extensionContext(payload.context, generationId, signal),
          );
          return normalizeToolResult(result, definition.name);
        });
        contribution.tools.push({
          callbackId,
          name: definition.name,
          label: definition.label ?? definition.name,
          description: definition.description ?? "",
          parameters: cloneSchema(definition.parameters, definition.name),
          promptSnippet: definition.promptSnippet,
          promptGuidelines: definition.promptGuidelines ?? [],
          executionMode: definition.executionMode ?? "parallel",
        });
      },
      on: (event, value) => {
        const registeredHandler = parseExternal(
          callableSchema,
          value,
          `pi.on(\"${event}\") handler (${path})`,
        );
        const handler: HookHandler = (eventValue, context) =>
          callDynamic(registeredHandler, undefined, [eventValue, context]);
        const target = AGENT_HOOKS.has(event)
          ? contribution.agentHooks
          : PROVIDER_HOOKS.has(event)
            ? contribution.providerHooks
            : SESSION_HOOKS.has(event)
              ? contribution.sessionHooks
              : undefined;
        if (!target) return unsupported(`pi.on(\"${event}\")`);
        const callbackId = `${pluginId}:hook:${event}:${target.length}`;
        this.#registerCallback(state, callbackId, async (invocation, signal) => {
          const payload = parseExternal(
            hookInvocationPayloadSchema,
            invocation.payload,
            `JavaScript hook ${event} invocation payload`,
          );
          const eventValue = payload.event;
          const result = await handler(
            eventValue,
            this.#extensionContext(payload.context, generationId, signal),
          );
          const resultRecord = isRecord(result) ? result : undefined;
          if (event === "input" && resultRecord?.images !== undefined) {
            unsupported("image replacement from an input hook");
          }
          if (event === "before_agent_start" && resultRecord?.message !== undefined) {
            unsupported("message injection from before_agent_start");
          }
          if (event === "message_end" && resultRecord?.message !== undefined) {
            unsupported("message replacement from message_end");
          }
          if (event === "tool_call") {
            return { ...(resultRecord ?? {}), input: eventValue.input };
          }
          return result ?? null;
        });
        target.push({ name: event, callbackId });
      },
      registerCommand: (name, value) => {
        if (!name) throw new Error(`registerCommand requires a name and handler (${path})`);
        const options = parseCommandOptions(value, path);
        const callbackId = `${pluginId}:command:${name}`;
        this.#registerCallback(state, callbackId, async (invocation, signal) => {
          const payload = parseExternal(
            commandInvocationPayloadSchema,
            invocation.payload,
            `JavaScript command ${name} invocation payload`,
          );
          const result = await options.handler(
            payload.arguments,
            this.#extensionContext(payload.context, generationId, signal),
          );
          if (typeof result === "string") return { action: "transform", text: result };
          return isRecord(result) && result.action ? result : { action: "handled" };
        });
        contribution.commands.push({
          callbackId,
          name,
          description: options.description ?? "",
          argumentHint: options.argumentHint,
        });
      },
      registerShortcut: () => unsupported("pi.registerShortcut"),
      registerFlag: () => unsupported("pi.registerFlag"),
      registerProvider: () => unsupported("pi.registerProvider"),
      registerMessageRenderer: () => unsupported("pi.registerMessageRenderer"),
      registerMarkdownTransformer: () => unsupported("pi.registerMarkdownTransformer"),
      registerEntryRenderer: () => unsupported("pi.registerEntryRenderer"),
      sendMessage: () => unsupported("pi.sendMessage"),
      sendUserMessage: () => unsupported("pi.sendUserMessage"),
      appendEntry: () => unsupported("pi.appendEntry"),
      setSessionName: () => unsupported("pi.setSessionName"),
      setLabel: () => unsupported("pi.setLabel"),
      getFlag: () => undefined,
    };
  }

  #registerCallback(
    state: GenerationState,
    callbackId: string,
    callback: ExtensionCallback,
  ): void {
    if (state.callbacks.has(callbackId)) {
      throw new Error(`duplicate JavaScript callback id: ${callbackId}`);
    }
    state.callbacks.set(callbackId, callback);
  }

  #requireGeneration(generationId: string): GenerationState {
    const state = this.#generations.get(generationId);
    if (!state) throw new Error(`JavaScript generation is retired: ${generationId}`);
    return state;
  }

  #extensionContext(
    context: Record<string, unknown>,
    generationId: string,
    signal: AbortSignal,
  ): ExtensionContext {
    const parsedContext = extensionContextInputSchema.parse(context);
    const cwd = parsedContext.cwd ?? parsedContext.session?.cwd ?? process.cwd();
    const state = this.#requireGeneration(generationId);
    const unavailable = (name: string) => (): never => {
      throw new Error(`${name} is unavailable in the pi_rs NAPI host`);
    };
    return {
      ...context,
      cwd,
      hasUI: false,
      mode: state.mode,
      signal,
      isProjectTrusted: () => state.projectTrusted,
      ui: {
        notify: unavailable("ctx.ui.notify"),
        select: unavailable("ctx.ui.select"),
        confirm: unavailable("ctx.ui.confirm"),
        input: unavailable("ctx.ui.input"),
        editor: unavailable("ctx.ui.editor"),
      },
    };
  }

  async #invoke(invocation: Invocation): Promise<unknown> {
    const state = this.#requireGeneration(invocation.generationId);
    const callback = state.callbacks.get(invocation.callbackId);
    if (!callback) throw new Error(`Unknown JavaScript callback: ${invocation.callbackId}`);
    const controller = new AbortController();
    state.active.set(invocation.invocationId, controller);
    try {
      return await callback(invocation, controller.signal);
    } finally {
      state.active.delete(invocation.invocationId);
    }
  }

  #cancel(invocationId: string): void {
    for (const state of this.#generations.values()) {
      const controller = state.active.get(invocationId);
      if (controller) {
        controller.abort();
        return;
      }
    }
  }

  #retireGeneration(generationId: string): void {
    const state = this.#generations.get(generationId);
    if (!state) return;
    for (const controller of state.active.values()) controller.abort();
    state.active.clear();
    state.callbacks.clear();
    this.#generations.delete(generationId);
  }
}
