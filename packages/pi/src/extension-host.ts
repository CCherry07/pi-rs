import { existsSync } from 'node:fs'
import { createRequire } from 'node:module'
import { basename, dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { createJiti } from 'jiti'
import { z } from 'zod'

import { CompatibilityResolver } from './compatibility-resolver.js'
import { createExtensionContext } from './extension-context.js'
import { ExtensionRuntime, type ExecOptions } from './extension-runtime.js'
import type { PiExtensionCommandContext, PiExtensionContext } from './extension-api.js'
import type { NativeExtensionContext } from './native-binding.js'
import {
  type AgentPluginManifest,
  type CommandManifest,
  type GenerationManifest,
  type GenerationRequest,
  type HookBatchInvocation,
  type HookManifest,
  type HostMode,
  type Invocation,
  type ProviderPluginManifest,
  type ProviderRegistrationManifest,
  type SessionPluginManifest,
  type StreamHookBatchInvocation,
  type StreamUpdate,
  type ToolExecutionMode,
  type ToolManifest,
  isRecord,
  parseHostOperation,
  parseJson,
  toolExecutionModeSchema,
} from './extension-protocol.js'

const hostDirectory = dirname(fileURLToPath(import.meta.url))
const compatibilityModulePath = [join(hostDirectory, 'compat-api.js'), join(hostDirectory, 'compat-api.ts')].find(existsSync)
if (!compatibilityModulePath) {
  throw new Error(`Cannot locate the Pi extension compatibility module in ${hostDirectory}`)
}
const compatibilityTuiModulePath = [join(hostDirectory, 'compat-tui.js'), join(hostDirectory, 'compat-tui.ts')].find(existsSync)
if (!compatibilityTuiModulePath) {
  throw new Error(`Cannot locate the Pi TUI compatibility module in ${hostDirectory}`)
}
const require = createRequire(import.meta.url)
const compatibilityResolver = new CompatibilityResolver(require, {
  pi: compatibilityModulePath,
  tui: compatibilityTuiModulePath,
})

const jsonObjectSchema = z.looseObject({})
const toolResultSchema = z.looseObject({
  content: z.array(z.unknown()),
  details: z.unknown().optional(),
  usage: z.unknown().optional(),
  isError: z.boolean().default(false),
  terminate: z.boolean().default(false),
})
const toolUpdateSchema = z.looseObject({
  content: z.array(z.unknown()),
  details: z.unknown().optional(),
})
const optionalStringSchema = z.string().nullable().transform(value => value ?? undefined)
const callableSchema = z.custom<(...arguments_: never[]) => unknown>(value => typeof value === 'function', 'expected a function')
const toolDefinitionSchema = z.looseObject({
  name: z.string(),
  label: z.string().optional(),
  description: z.string().optional(),
  parameters: z.unknown(),
  promptSnippet: z.string().optional(),
  promptGuidelines: z.array(z.string()).optional(),
  executionMode: toolExecutionModeSchema.optional(),
  prepareArguments: callableSchema.optional(),
  execute: callableSchema,
})
const commandOptionsSchema = z.looseObject({
  description: z.string().optional(),
  argumentHint: z.string().optional(),
  handler: callableSchema,
})
const extensionContextInputSchema = z.looseObject({
  cwd: z.string().optional(),
  session: z.looseObject({ cwd: z.string().optional() }).optional(),
})
const toolInvocationPayloadSchema = z.looseObject({
  context: extensionContextInputSchema.extend({ toolCallId: z.string() }),
  input: z.unknown(),
})
const toolPreparationPayloadSchema = z.looseObject({ input: z.unknown() })
const commandInvocationPayloadSchema = z.looseObject({
  context: extensionContextInputSchema,
  arguments: z.string(),
})
const hookInvocationPayloadSchema = z.looseObject({
  context: extensionContextInputSchema,
  event: jsonObjectSchema,
})
const beforeAgentStartResultSchema = z.looseObject({
  message: z.looseObject({
    customType: z.string(),
    content: z.union([z.string(), z.array(z.unknown())]).nullish(),
    display: z.boolean().nullish(),
    details: z.unknown().optional(),
  }).optional(),
  systemPrompt: z.string().optional(),
})
const providerHeadersSchema = z.record(z.string(), z.union([z.string(), z.null()]))
const providerCostSchema = z.looseObject({
  input: z.number().optional(),
  output: z.number().optional(),
  cacheRead: z.number().optional(),
  cacheWrite: z.number().optional(),
  tiers: z.array(z.looseObject({
    inputTokensAbove: z.number(),
    input: z.number(),
    output: z.number(),
    cacheRead: z.number(),
    cacheWrite: z.number(),
  })).optional(),
})
const providerModelSchema = z.looseObject({
  id: z.string().min(1),
  name: z.string().optional(),
  api: z.string().optional(),
  baseUrl: z.string().optional(),
  reasoning: z.boolean().optional(),
  thinkingLevelMap: z.record(z.string(), z.string().nullable()).optional(),
  input: z.array(z.enum(['text', 'image'])).optional(),
  cost: providerCostSchema.optional(),
  contextWindow: z.number().int().positive().optional(),
  maxTokens: z.number().int().positive().optional(),
  samplingParams: z.record(z.string(), z.unknown()).optional(),
  headers: z.record(z.string(), z.string()).optional(),
  compat: jsonObjectSchema.optional(),
})
const providerConfigSchema = z.looseObject({
  name: z.string().optional(),
  baseUrl: z.string().optional(),
  apiKey: z.string().optional(),
  api: z.string().optional(),
  headers: z.record(z.string(), z.string()).optional(),
  authHeader: z.boolean().optional(),
  models: z.array(providerModelSchema).optional(),
  streamSimple: callableSchema.optional(),
  refreshModels: callableSchema.optional(),
  oauth: z.unknown().optional(),
})

const SUPPORTED_PROVIDER_CONFIG_KEYS = new Set([
  'name', 'baseUrl', 'apiKey', 'api', 'headers', 'authHeader', 'models',
])
const KNOWN_PROVIDER_CONFIG_KEYS = new Set([
  ...SUPPORTED_PROVIDER_CONFIG_KEYS,
  'streamSimple', 'refreshModels', 'oauth',
])

function normalizeProviderConfig(
  value: unknown,
  path: string,
): { config: Record<string, unknown>; inactive: string[] } {
  const parsed = parseExternal(providerConfigSchema, value, `pi.registerProvider config (${path})`)
  const unknown = Object.keys(parsed).filter(key => !KNOWN_PROVIDER_CONFIG_KEYS.has(key))
  if (unknown.length > 0) {
    throw new Error(`pi.registerProvider config has unknown field${unknown.length === 1 ? '' : 's'} ${unknown.map(key => JSON.stringify(key)).join(', ')} (${path})`)
  }
  const config = Object.fromEntries(
    Object.entries(parsed).filter(([key, entry]) => SUPPORTED_PROVIDER_CONFIG_KEYS.has(key) && entry !== undefined),
  )
  const inactive = ['streamSimple', 'refreshModels', 'oauth'].filter(key => parsed[key] !== undefined)
  return { config, inactive }
}

function parseExternal<T>(schema: z.ZodType<T>, value: unknown, description: string): T {
  const result = schema.safeParse(value)
  if (result.success) return result.data
  throw new Error(`${description} is invalid: ${z.prettifyError(result.error)}`, {
    cause: result.error,
  })
}

const AGENT_HOOKS = new Set([
  'input',
  'before_agent_start',
  'agent_start',
  'agent_end',
  'agent_settled',
  'turn_start',
  'turn_end',
  'message_start',
  'message_update',
  'message_end',
  'tool_execution_start',
  'tool_execution_update',
  'tool_execution_end',
  'context',
  'tool_call',
  'tool_result',
])
const PROVIDER_HOOKS = new Set([
  'before_provider_request',
  'before_provider_headers',
  'after_provider_response',
])
const SESSION_HOOKS = new Set([
  'session_start',
  'session_info_changed',
  'session_before_switch',
  'session_before_fork',
  'session_before_compact',
  'session_compact',
  'session_compact_failed',
  'session_shutdown',
  'session_before_tree',
  'session_tree',
])
const KNOWN_HOOKS = new Set([
  'project_trust',
  'resources_discover',
  ...AGENT_HOOKS,
  ...PROVIDER_HOOKS,
  ...SESSION_HOOKS,
  'model_select',
  'thinking_level_select',
  'user_bash',
])

type ExtensionContext = PiExtensionContext
type ExtensionCommandContext = PiExtensionCommandContext

interface ToolResult {
  content: unknown[]
  details?: unknown
  usage?: unknown
  isError: boolean
  terminate: boolean
}

interface ToolDefinition {
  name: string
  label?: string
  description?: string
  parameters: unknown
  promptSnippet?: string
  promptGuidelines?: string[]
  executionMode?: ToolExecutionMode
  prepareArguments?(input: unknown): unknown
  execute(toolCallId: string, input: unknown, signal: AbortSignal, update: ((result: unknown) => void) | undefined, context: ExtensionContext): unknown | Promise<unknown>
}

interface CommandOptions {
  description?: string
  argumentHint?: string
  handler(arguments_: string, context: ExtensionCommandContext): unknown | Promise<unknown>
}

type HookHandler = (event: Record<string, unknown>, context: ExtensionContext) => unknown | Promise<unknown>
type ExtensionCallback = (
  invocation: Invocation,
  signal: AbortSignal,
  nativeContext: NativeExtensionContext | undefined,
) => Promise<unknown>

interface Contribution {
  tools: ToolManifest[]
  commands: CommandManifest[]
  agentHooks: HookManifest[]
  providerHooks: HookManifest[]
  sessionHooks: HookManifest[]
}

interface ExtensionDiagnostic {
  pluginId: string
  path: string
  feature: string
  status: 'inactive'
  message: string
}

interface GenerationState {
  callbacks: Map<string, ExtensionCallback>
  active: Map<string, AbortController>
  events: ExtensionEventBus
  mode: HostMode
  projectTrusted: boolean
  runtime: ExtensionRuntime
  flagValues: Map<string, boolean | string | undefined>
  registeredFlags: Map<string, 'boolean' | 'string'>
  initializing: boolean
  providerRegistrations: ProviderRegistrationManifest[]
  streams: Map<string, AssistantStreamState>
}

type AssistantStreamBlock =
  | { kind: 'text'; base: Record<string, unknown>; chunks: string[] }
  | { kind: 'thinking'; base: Record<string, unknown>; chunks: string[] }
  | { kind: 'toolCall'; base: Record<string, unknown>; argumentChunks: string[] }
  | { kind: 'static'; base: Record<string, unknown> }

interface AssistantStreamState {
  header: Record<string, unknown>
  content: (AssistantStreamBlock | undefined)[]
}

interface ExtensionEventBus {
  emit(channel: string, data: unknown): void
  on(channel: string, handler: unknown): () => void
  clear(): void
}

interface ExtensionApi {
  registerTool(definition: unknown): void
  on(event: string, handler: unknown): void
  registerCommand(name: string, options: unknown): void
  registerShortcut(): void
  registerFlag(name: string, options: unknown): void
  registerProvider(nameOrProvider: string | Record<string, unknown>, config?: unknown): void
  registerMessageRenderer(): void
  registerMarkdownTransformer(): void
  registerEntryRenderer(): void
  sendMessage(message: unknown, options?: unknown): void
  sendUserMessage(content: unknown, options?: unknown): void
  appendEntry(customType: string, data?: unknown): void
  setSessionName(name: string): void
  setLabel(entryId: string, label: string | undefined): void
  getSessionName(): string | undefined
  exec(command: string, args: string[], options?: ExecOptions): Promise<{ stdout: string; stderr: string; code: number; killed: boolean }>
  getActiveTools(): string[]
  getAllTools(): Record<string, unknown>[]
  setActiveTools(toolNames: string[]): void
  getCommands(): Record<string, unknown>[]
  setModel(model: Record<string, unknown>): Promise<boolean>
  getThinkingLevel(): string
  setThinkingLevel(level: string): void
  unregisterProvider(name: string): void
  getFlag(name: string): boolean | string | undefined
  events: ExtensionEventBus
}

function createExtensionEventBus(): ExtensionEventBus {
  const listeners = new Map<string, Set<(data: unknown) => unknown>>()
  return {
    emit: (channel, data) => {
      for (const listener of listeners.get(channel) ?? []) {
        try {
          Promise.resolve(listener(data)).catch(() => undefined)
        } catch {
          // Cross-extension event failures are isolated from the emitter.
        }
      }
    },
    on: (channel, value) => {
      const registeredHandler = parseExternal(callableSchema, value, `pi.events.on("${channel}") handler`)
      const handler = (data: unknown): unknown => callDynamic(registeredHandler, undefined, [data])
      const listenersForChannel = listeners.get(channel) ?? new Set()
      listenersForChannel.add(handler)
      listeners.set(channel, listenersForChannel)
      return () => {
        listenersForChannel.delete(handler)
        if (listenersForChannel.size === 0) listeners.delete(channel)
      }
    },
    clear: () => listeners.clear(),
  }
}

function cloneSchema(schema: unknown, toolName: string): Record<string, unknown> {
  try {
    const cloned = parseJson(JSON.stringify(schema), `JavaScript tool ${toolName} schema`)
    return parseExternal(jsonObjectSchema, cloned, `JavaScript tool ${toolName} schema`)
  } catch {
    throw new Error(`JavaScript tool ${toolName} parameters must be a JSON schema object`)
  }
}

function normalizeToolResult(result: unknown, toolName: string): ToolResult {
  return parseExternal(toolResultSchema, result, `JavaScript tool ${toolName} result`)
}

function callDynamic(callback: unknown, thisArgument: unknown, arguments_: readonly unknown[]): unknown {
  if (typeof callback !== 'function') throw new Error('dynamic callback is not callable')
  return Reflect.apply(callback, thisArgument, arguments_)
}

function streamBlock(value: unknown): AssistantStreamBlock {
  if (!isRecord(value) || typeof value.type !== 'string') {
    throw new Error('assistant stream initial content contains an invalid block')
  }
  const base = { ...value }
  switch (value.type) {
    case 'text':
      return { kind: 'text', base, chunks: [typeof value.text === 'string' ? value.text : ''] }
    case 'thinking':
      return { kind: 'thinking', base, chunks: [typeof value.thinking === 'string' ? value.thinking : ''] }
    case 'toolCall':
      return { kind: 'toolCall', base, argumentChunks: [] }
    default:
      return { kind: 'static', base }
  }
}

function createAssistantStream(initialMessage: Record<string, unknown>): AssistantStreamState {
  if (!Array.isArray(initialMessage.content)) {
    throw new Error('assistant stream initial message is missing content')
  }
  const { content, ...header } = initialMessage
  return { header, content: content.map(streamBlock) }
}

function requireStreamBlock<K extends AssistantStreamBlock['kind']>(
  state: AssistantStreamState,
  index: number,
  kind: K,
): Extract<AssistantStreamBlock, { kind: K }> {
  const block = state.content[index]
  if (!block || block.kind !== kind) {
    throw new Error(`assistant stream content ${index} is not ${kind}`)
  }
  return block as Extract<AssistantStreamBlock, { kind: K }>
}

function setOptional(target: Record<string, unknown>, key: string, value: unknown): void {
  if (value === null || value === undefined) delete target[key]
  else target[key] = value
}

function applyStreamUpdate(state: AssistantStreamState, update: StreamUpdate): void {
  switch (update.type) {
    case 'start': {
      const metadata = update.metadata
      state.header.api = metadata.api
      state.header.provider = metadata.provider
      state.header.model = metadata.model
      state.header.timestamp = metadata.timestamp
      for (const key of ['responseModel', 'responseId', 'diagnostics', 'deferred', 'rawStopReason', 'endTurn']) {
        setOptional(state.header, key, metadata[key])
      }
      return
    }
    case 'metadata':
      for (const [key, value] of Object.entries(update.patch)) {
        if (value !== null) setOptional(state.header, key, value)
      }
      return
    case 'contentMetadata': {
      const metadata = update.metadata
      if (metadata.type === 'thinking') {
        setOptional(requireStreamBlock(state, update.contentIndex, 'thinking').base, 'redacted', metadata.redacted)
      } else {
        setOptional(requireStreamBlock(state, update.contentIndex, 'toolCall').base, 'namespace', metadata.namespace)
      }
      return
    }
    case 'textStart':
      state.content[update.contentIndex] ??= {
        kind: 'text',
        base: { type: 'text' },
        chunks: [],
      }
      return
    case 'textDelta':
      requireStreamBlock(state, update.contentIndex, 'text').chunks.push(update.delta)
      return
    case 'textEnd':
      setOptional(requireStreamBlock(state, update.contentIndex, 'text').base, 'textSignature', update.textSignature)
      return
    case 'thinkingStart':
      state.content[update.contentIndex] ??= {
        kind: 'thinking',
        base: { type: 'thinking' },
        chunks: [],
      }
      return
    case 'thinkingDelta':
      requireStreamBlock(state, update.contentIndex, 'thinking').chunks.push(update.delta)
      return
    case 'thinkingEnd':
      setOptional(requireStreamBlock(state, update.contentIndex, 'thinking').base, 'thinkingSignature', update.thinkingSignature)
      return
    case 'toolCallStart':
      state.content[update.contentIndex] ??= {
        kind: 'toolCall',
        base: { type: 'toolCall', id: update.id, name: update.name, arguments: null },
        argumentChunks: [],
      }
      return
    case 'toolCallDelta':
      requireStreamBlock(state, update.contentIndex, 'toolCall').argumentChunks.push(update.argumentsDelta)
      return
    case 'toolCallEnd': {
      const block = requireStreamBlock(state, update.contentIndex, 'toolCall')
      const rawArguments = block.argumentChunks.join('')
      try {
        block.base.arguments = rawArguments.length === 0 ? {} : JSON.parse(rawArguments)
      } catch {
        block.base.arguments = null
      }
      setOptional(block.base, 'thoughtSignature', update.thoughtSignature)
      return
    }
    case 'done':
      state.header.usage = structuredClone(update.usage)
      state.header.stopReason = update.reason
  }
}

function materializeStreamBlock(block: AssistantStreamBlock): Record<string, unknown> {
  switch (block.kind) {
    case 'text':
      return { ...structuredClone(block.base), text: block.chunks.join('') }
    case 'thinking':
      return { ...structuredClone(block.base), thinking: block.chunks.join('') }
    case 'toolCall':
    case 'static':
      return structuredClone(block.base)
  }
}

function materializeAssistantStream(state: AssistantStreamState): Record<string, unknown> {
  return {
    ...structuredClone(state.header),
    content: state.content.map((block, index) => {
      if (!block) throw new Error(`assistant stream content ${index} is missing`)
      return materializeStreamBlock(block)
    }),
  }
}

function defineLazy(target: Record<string, unknown>, key: string, get: () => unknown): void {
  Object.defineProperty(target, key, { enumerable: true, get })
}

function createStreamHookEvent(
  streamId: string,
  state: AssistantStreamState,
  update: StreamUpdate,
): Record<string, unknown> | undefined {
  if (update.type === 'metadata' || update.type === 'contentMetadata') return undefined
  let message: Record<string, unknown> | undefined
  let partial: Record<string, unknown> | undefined
  const ensureSnapshot = (): void => {
    if (partial) return
    partial = materializeAssistantStream(state)
    message = { ...partial }
  }
  const getMessage = (): Record<string, unknown> => {
    ensureSnapshot()
    return message as Record<string, unknown>
  }
  const getPartial = (): Record<string, unknown> => {
    ensureSnapshot()
    return partial as Record<string, unknown>
  }
  const assistantMessageEvent: Record<string, unknown> = (() => {
    switch (update.type) {
      case 'start': return { type: 'start' }
      case 'textStart': return { type: 'text_start', contentIndex: update.contentIndex }
      case 'textDelta': return { type: 'text_delta', contentIndex: update.contentIndex, delta: update.delta }
      case 'textEnd': {
        const projected: Record<string, unknown> = { type: 'text_end', contentIndex: update.contentIndex }
        defineLazy(projected, 'content', () => (getMessage().content as Record<string, unknown>[])[update.contentIndex]?.text ?? '')
        return projected
      }
      case 'thinkingStart': return { type: 'thinking_start', contentIndex: update.contentIndex }
      case 'thinkingDelta': return { type: 'thinking_delta', contentIndex: update.contentIndex, delta: update.delta }
      case 'thinkingEnd': {
        const projected: Record<string, unknown> = { type: 'thinking_end', contentIndex: update.contentIndex }
        defineLazy(projected, 'content', () => (getMessage().content as Record<string, unknown>[])[update.contentIndex]?.thinking ?? '')
        return projected
      }
      case 'toolCallStart': return {
        type: 'toolcall_start', contentIndex: update.contentIndex, id: update.id, toolName: update.name,
      }
      case 'toolCallDelta': return {
        type: 'toolcall_delta', contentIndex: update.contentIndex, delta: update.argumentsDelta,
      }
      case 'toolCallEnd': {
        const projected: Record<string, unknown> = { type: 'toolcall_end', contentIndex: update.contentIndex }
        defineLazy(projected, 'toolCall', () => (getMessage().content as Record<string, unknown>[])[update.contentIndex])
        return projected
      }
      case 'done': return {
        type: update.reason === 'error' || update.reason === 'aborted' ? 'error' : 'done',
        reason: update.reason,
      }
    }
  })()
  if (update.type === 'done') {
    defineLazy(assistantMessageEvent, assistantMessageEvent.type === 'error' ? 'error' : 'message', getPartial)
  } else {
    defineLazy(assistantMessageEvent, 'partial', getPartial)
  }
  const event: Record<string, unknown> = {
    type: 'message_update',
    streamId,
    update,
    assistantMessageEvent,
  }
  defineLazy(event, 'message', getMessage)
  return event
}

function callbackErrorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

function parseToolDefinition(value: unknown, path: string): ToolDefinition {
  const result = toolDefinitionSchema.safeParse(value)
  if (!result.success) {
    throw new Error(`registerTool received an invalid definition (${path}): ${z.prettifyError(result.error)}`, { cause: result.error })
  }
  const definition = result.data
  return {
    ...definition,
    execute: (toolCallId, input, signal, update, context) => callDynamic(definition.execute, definition, [toolCallId, input, signal, update, context]),
  }
}

function parseCommandOptions(value: unknown, path: string): CommandOptions {
  const result = commandOptionsSchema.safeParse(value)
  if (!result.success) {
    throw new Error(`registerCommand received invalid options (${path}): ${z.prettifyError(result.error)}`, { cause: result.error })
  }
  const options = result.data
  return {
    ...options,
    handler: (arguments_, context) => callDynamic(options.handler, options, [arguments_, context]),
  }
}

function extensionId(index: number, path: string): string {
  return `js:${index}:${basename(path).replace(/[^a-zA-Z0-9._-]/g, '-')}`
}

export class ExtensionHost {
  #generation = 0
  readonly #generations = new Map<string, GenerationState>()

  async dispatch(
    rawOperation: string,
    nativeContext?: NativeExtensionContext,
  ): Promise<string> {
    const operation = parseHostOperation(rawOperation)
    switch (operation.type) {
      case 'prepareGeneration':
        return JSON.stringify(await this.#prepareGeneration(operation.request))
      case 'invoke':
        return JSON.stringify(await this.#invoke(operation.invocation, nativeContext))
      case 'invokeHookBatch':
        return JSON.stringify(await this.#invokeHookBatch(operation.invocation, nativeContext))
      case 'invokeStreamHookBatch':
        return JSON.stringify(await this.#invokeStreamHookBatch(operation.invocation, nativeContext))
      case 'releaseStream':
        this.#releaseStream(operation.generationId, operation.streamId)
        return 'null'
      case 'cancel':
        this.#cancel(operation.invocationId)
        return 'null'
      case 'retireGeneration':
        this.#retireGeneration(operation.generationId)
        return 'null'
    }
  }

  async #prepareGeneration(request: GenerationRequest): Promise<GenerationManifest> {
    const generationId = `js-${++this.#generation}`
    const runtime = new ExtensionRuntime(request.cwd, () => {
      this.#requireGeneration(generationId)
    })
    const state: GenerationState = {
      callbacks: new Map(),
      active: new Map(),
      events: createExtensionEventBus(),
      mode: request.mode,
      projectTrusted: request.projectTrusted,
      runtime,
      flagValues: new Map(Object.entries(request.flagValues)),
      registeredFlags: new Map(),
      initializing: true,
      providerRegistrations: [],
      streams: new Map(),
    }
    this.#generations.set(generationId, state)
    try {
      const paths = request.extensionPaths
      const agentPlugins: AgentPluginManifest[] = []
      const providerPlugins: ProviderPluginManifest[] = []
      const sessionPlugins: SessionPluginManifest[] = []
      const diagnostics: ExtensionDiagnostic[] = []

      for (const [index, path] of paths.entries()) {
        const id = extensionId(index, path)
        const contribution: Contribution = {
          tools: [],
          commands: [],
          agentHooks: [],
          providerHooks: [],
          sessionHooks: [],
        }
        const api = this.#createExtensionApi(generationId, id, path, contribution, diagnostics)
        const jiti = createJiti(import.meta.url, {
          alias: compatibilityResolver.aliases,
          moduleCache: false,
          interopDefault: true,
        })
        const imported = await jiti.import<unknown>(path, { default: true })
        const factory = parseExternal(callableSchema, imported, `Extension default export (${path})`)
        await callDynamic(factory, undefined, [api])
        if (contribution.tools.length || contribution.commands.length || contribution.agentHooks.length) {
          agentPlugins.push({
            id,
            tools: contribution.tools,
            commands: contribution.commands,
            hooks: contribution.agentHooks,
          })
        }
        if (contribution.providerHooks.length) {
          providerPlugins.push({ id, hooks: contribution.providerHooks })
        }
        if (contribution.sessionHooks.length) {
          sessionPlugins.push({ id, hooks: contribution.sessionHooks })
        }
      }

      const unknownFlags = [...state.flagValues.keys()].filter(name => !state.registeredFlags.has(name))
      if (unknownFlags.length > 0) {
        throw new Error(`Unknown JavaScript extension flag${unknownFlags.length === 1 ? '' : 's'}: ${unknownFlags.map(name => `--${name}`).join(', ')}`)
      }

      state.initializing = false

      return {
        generationId,
        agentPlugins,
        providerPlugins,
        providerRegistrations: state.providerRegistrations,
        sessionPlugins,
        diagnostics,
      }
    } catch (error) {
      this.#retireGeneration(generationId)
      throw error
    }
  }

  #createExtensionApi(
    generationId: string,
    pluginId: string,
    path: string,
    contribution: Contribution,
    diagnostics: ExtensionDiagnostic[],
  ): ExtensionApi {
    const state = this.#requireGeneration(generationId)
    const registeredFlags = new Set<string>()
    const inactive = (feature: string): void => {
      if (diagnostics.some(diagnostic => diagnostic.pluginId === pluginId && diagnostic.feature === feature)) return
      diagnostics.push({
        pluginId,
        path,
        feature,
        status: 'inactive',
        message: `${feature} is recognized but inactive in the pi-rs JavaScript host`,
      })
    }
    return {
      registerTool: value => {
        const definition = parseToolDefinition(value, path)
        const callbackId = `${pluginId}:tool:${definition.name}`
        const prepareCallbackId = definition.prepareArguments === undefined
          ? undefined
          : `${pluginId}:tool:${definition.name}:prepareArguments`
        if (prepareCallbackId !== undefined) {
          this.#registerCallback(state, prepareCallbackId, async invocation => {
            const payload = parseExternal(
              toolPreparationPayloadSchema,
              invocation.payload,
              `JavaScript tool ${definition.name} preparation payload`,
            )
            return definition.prepareArguments?.(payload.input)
          })
        }
        this.#registerCallback(state, callbackId, async (invocation, signal, nativeContext) => {
          const payload = parseExternal(toolInvocationPayloadSchema, invocation.payload, `JavaScript tool ${definition.name} invocation payload`)
          const update = nativeContext?.update === undefined
            ? undefined
            : (partial: unknown): void => {
                const normalized = parseExternal(toolUpdateSchema, partial, `JavaScript tool ${definition.name} update`)
                nativeContext.update?.(JSON.stringify(normalized))
              }
          const result = await definition.execute(payload.context.toolCallId, payload.input, signal, update, this.#extensionContext(payload.context, generationId, signal, nativeContext, false))
          return normalizeToolResult(result, definition.name)
        })
        contribution.tools.push({
          callbackId,
          prepareCallbackId,
          name: definition.name,
          label: definition.label ?? definition.name,
          description: definition.description ?? '',
          parameters: cloneSchema(definition.parameters, definition.name),
          promptSnippet: definition.promptSnippet,
          promptGuidelines: definition.promptGuidelines ?? [],
          executionMode: definition.executionMode ?? 'parallel',
        })
      },
      on: (event, value) => {
        const registeredHandler = parseExternal(callableSchema, value, `pi.on(\"${event}\") handler (${path})`)
        const handler: HookHandler = (eventValue, context) => callDynamic(registeredHandler, undefined, [eventValue, context])
        const target = AGENT_HOOKS.has(event) ? contribution.agentHooks : PROVIDER_HOOKS.has(event) ? contribution.providerHooks : SESSION_HOOKS.has(event) ? contribution.sessionHooks : undefined
        if (!target) {
          if (KNOWN_HOOKS.has(event)) {
            inactive(`pi.on(\"${event}\")`)
            return
          }
          throw new Error(`pi.on(\"${event}\") is not a recognized Pi hook (${path})`)
        }
        const callbackId = `${pluginId}:hook:${event}:${target.length}`
        this.#registerCallback(state, callbackId, async (invocation, signal, nativeContext) => {
          const payload = parseExternal(hookInvocationPayloadSchema, invocation.payload, `JavaScript hook ${event} invocation payload`)
          // Zod validates the payload, but handlers must receive the exact
          // generation event object so mutations remain visible to the next
          // callback in registration order, as in Pi's extension runner.
          const eventValue = isRecord(invocation.payload.event) ? invocation.payload.event : payload.event
          const chainedSystemPrompt = event === 'before_agent_start' && typeof eventValue.systemPrompt === 'string'
            ? eventValue.systemPrompt
            : undefined
          const result = await handler(
            eventValue,
            this.#extensionContext(
              payload.context,
              generationId,
              signal,
              nativeContext,
              false,
              chainedSystemPrompt,
            ),
          )
          const resultRecord = isRecord(result) ? result : undefined
          if (event === 'before_agent_start' && resultRecord) {
            return parseExternal(
              beforeAgentStartResultSchema,
              resultRecord,
              `pi.on("before_agent_start") result (${path})`,
            )
          }
          if (event === 'tool_call') {
            return { ...(resultRecord ?? {}), input: eventValue.input }
          }
          if (event === 'before_provider_headers') {
            return parseExternal(
              providerHeadersSchema,
              eventValue.headers,
              `pi.on("before_provider_headers") mutated headers (${path})`,
            )
          }
          if (event === 'after_provider_response') return null
          return result ?? null
        })
        target.push({ name: event, callbackId })
      },
      registerCommand: (name, value) => {
        if (!name) throw new Error(`registerCommand requires a name and handler (${path})`)
        const options = parseCommandOptions(value, path)
        const callbackId = `${pluginId}:command:${name}`
        this.#registerCallback(state, callbackId, async (invocation, signal, nativeContext) => {
          const payload = parseExternal(commandInvocationPayloadSchema, invocation.payload, `JavaScript command ${name} invocation payload`)
          const context = this.#extensionContext(payload.context, generationId, signal, nativeContext, true) as ExtensionCommandContext
          const result = await options.handler(payload.arguments, context)
          if (typeof result === 'string') return { action: 'transform', text: result }
          return isRecord(result) && result.action ? result : { action: 'handled' }
        })
        contribution.commands.push({
          callbackId,
          name,
          description: options.description ?? '',
          argumentHint: options.argumentHint,
        })
      },
      registerShortcut: () => inactive('pi.registerShortcut'),
      registerFlag: (name, value) => {
        if (!name) throw new Error(`registerFlag requires a non-empty name (${path})`)
        const options = parseExternal(z.looseObject({
          type: z.enum(['boolean', 'string']),
          default: z.union([z.boolean(), z.string()]).optional(),
        }), value, `pi.registerFlag("${name}") options`)
        if (options.default !== undefined && typeof options.default !== options.type) {
          throw new Error(`pi.registerFlag("${name}") default must be ${options.type}`)
        }
        const existingType = state.registeredFlags.get(name)
        if (existingType !== undefined) {
          registeredFlags.add(name)
          return
        }
        registeredFlags.add(name)
        state.registeredFlags.set(name, options.type)
        if (state.flagValues.has(name)) {
          const value = state.flagValues.get(name)
          if (typeof value !== options.type) {
            throw new Error(`JavaScript extension flag --${name} requires a ${options.type} value`)
          }
        } else {
          state.flagValues.set(name, options.default)
        }
      },
      registerProvider: (nameOrProvider, value) => {
        if (typeof nameOrProvider !== 'string') {
          inactive('pi.registerProvider(Provider)')
          return
        }
        if (!nameOrProvider.trim()) throw new Error(`registerProvider requires a non-empty name (${path})`)
        if (value === undefined) throw new Error(`Provider config is required when registering by name (${path})`)
        const normalized = normalizeProviderConfig(value, path)
        for (const feature of normalized.inactive) inactive(`pi.registerProvider.${feature}`)
        if (Object.keys(normalized.config).length === 0 && normalized.inactive.length > 0) return
        const registration = {
          pluginId,
          path,
          name: nameOrProvider,
          config: normalized.config,
        }
        if (state.initializing) {
          state.providerRegistrations.push(registration)
        } else {
          state.runtime.notify({
            type: 'registerProvider',
            name: nameOrProvider,
            config: normalized.config,
          })
        }
      },
      registerMessageRenderer: () => inactive('pi.registerMessageRenderer'),
      registerMarkdownTransformer: () => inactive('pi.registerMarkdownTransformer'),
      registerEntryRenderer: () => inactive('pi.registerEntryRenderer'),
      sendMessage: (message, options) => state.runtime.notify({ type: 'sendMessage', message, options }),
      sendUserMessage: (content, options) => state.runtime.notify({ type: 'sendUserMessage', content, options }),
      appendEntry: (customType, data) => state.runtime.notify({ type: 'appendEntry', customType, data }),
      setSessionName: name => state.runtime.notify({ type: 'setSessionName', name }),
      setLabel: (entryId, label) => state.runtime.notify({ type: 'setLabel', entryId, label }),
      getSessionName: () => state.runtime.query(
        { type: 'sessionName' },
        optionalStringSchema,
        () => undefined,
      ),
      exec: (command, args, options) => state.runtime.exec(command, args, options),
      getActiveTools: () => state.runtime.query(
        { type: 'activeTools' },
        z.array(z.string()),
        () => [],
      ),
      getAllTools: () => state.runtime.query(
        { type: 'allTools' },
        z.array(jsonObjectSchema),
        () => [],
      ),
      setActiveTools: toolNames => state.runtime.notify({ type: 'setActiveTools', toolNames }),
      getCommands: () => state.runtime.query(
        { type: 'commands' },
        z.array(jsonObjectSchema),
        () => [],
      ),
      setModel: async model => {
        const provider = typeof model.provider === 'string' ? model.provider : undefined
        const modelId = typeof model.id === 'string' ? model.id : undefined
        if (!provider || !modelId) return false
        return state.runtime.request(
          { type: 'setModel', provider, modelId },
          z.boolean(),
        )
      },
      getThinkingLevel: () => state.runtime.query(
        { type: 'thinkingLevel' },
        z.string(),
        () => 'off',
      ),
      setThinkingLevel: level => state.runtime.notify({ type: 'setThinkingLevel', level }),
      unregisterProvider: name => {
        if (!name.trim()) throw new Error(`unregisterProvider requires a non-empty name (${path})`)
        if (state.initializing) {
          state.providerRegistrations.splice(
            0,
            state.providerRegistrations.length,
            ...state.providerRegistrations.filter(registration => registration.name !== name),
          )
        } else {
          state.runtime.notify({ type: 'unregisterProvider', name })
        }
      },
      getFlag: name => registeredFlags.has(name) ? state.flagValues.get(name) : undefined,
      events: state.events,
    }
  }

  #registerCallback(state: GenerationState, callbackId: string, callback: ExtensionCallback): void {
    if (state.callbacks.has(callbackId)) {
      throw new Error(`duplicate JavaScript callback id: ${callbackId}`)
    }
    state.callbacks.set(callbackId, callback)
  }

  #requireGeneration(generationId: string): GenerationState {
    const state = this.#generations.get(generationId)
    if (!state) throw new Error(`JavaScript generation is retired: ${generationId}`)
    return state
  }

  #extensionContext(
    context: Record<string, unknown>,
    generationId: string,
    signal: AbortSignal,
    nativeContext: NativeExtensionContext | undefined,
    command: boolean,
    chainedSystemPrompt?: string,
  ): ExtensionContext | ExtensionCommandContext {
    const parsedContext = extensionContextInputSchema.parse(context)
    const state = this.#requireGeneration(generationId)
    const contextFacade = createExtensionContext({
      payload: { ...context, ...parsedContext },
      nativeContext,
      mode: state.mode,
      projectTrusted: state.projectTrusted,
      signal,
      command,
      assertActive: () => {
        this.#requireGeneration(generationId)
      },
    })
    if (chainedSystemPrompt !== undefined) {
      contextFacade.getSystemPrompt = () => {
        this.#requireGeneration(generationId)
        return chainedSystemPrompt
      }
    }
    return contextFacade
  }

  async #invoke(
    invocation: Invocation,
    nativeContext: NativeExtensionContext | undefined,
  ): Promise<unknown> {
    const state = this.#requireGeneration(invocation.generationId)
    state.runtime.bind(nativeContext)
    const callback = state.callbacks.get(invocation.callbackId)
    if (!callback) throw new Error(`Unknown JavaScript callback: ${invocation.callbackId}`)
    const controller = new AbortController()
    state.active.set(invocation.invocationId, controller)
    try {
      return await callback(invocation, controller.signal, nativeContext)
    } finally {
      controller.abort()
      state.active.delete(invocation.invocationId)
    }
  }

  async #invokeHookBatch(
    invocation: HookBatchInvocation,
    nativeContext: NativeExtensionContext | undefined,
  ): Promise<{ errors: { callbackId: string; message: string }[] }> {
    const state = this.#requireGeneration(invocation.generationId)
    state.runtime.bind(nativeContext)
    const controller = new AbortController()
    state.active.set(invocation.invocationId, controller)
    const event = invocation.event
    const errors: { callbackId: string; message: string }[] = []
    try {
      for (const entry of invocation.callbacks) {
        if (controller.signal.aborted) break
        const callback = state.callbacks.get(entry.callbackId)
        if (!callback) {
          errors.push({
            callbackId: entry.callbackId,
            message: `Unknown JavaScript callback: ${entry.callbackId}`,
          })
          continue
        }
        const callbackInvocation: Invocation = {
          invocationId: invocation.invocationId,
          generationId: invocation.generationId,
          callbackId: entry.callbackId,
          kind: 'agentHook',
          payload: {
            hook: invocation.hook,
            context: entry.context,
            event,
          },
        }
        try {
          await callback(callbackInvocation, controller.signal, nativeContext)
        } catch (error) {
          errors.push({ callbackId: entry.callbackId, message: callbackErrorMessage(error) })
        }
      }
      return { errors }
    } finally {
      controller.abort()
      state.active.delete(invocation.invocationId)
    }
  }

  async #invokeStreamHookBatch(
    invocation: StreamHookBatchInvocation,
    nativeContext: NativeExtensionContext | undefined,
  ): Promise<{ errors: { callbackId: string; message: string }[] }> {
    const generation = this.#requireGeneration(invocation.generationId)
    generation.runtime.bind(nativeContext)
    let stream: AssistantStreamState
    if (invocation.initialMessage) {
      stream = createAssistantStream(invocation.initialMessage)
      generation.streams.set(invocation.streamId, stream)
    } else {
      const current = generation.streams.get(invocation.streamId)
      if (!current) {
        throw new Error(`Assistant stream ${invocation.streamId} has no initial message`)
      }
      stream = current
      applyStreamUpdate(stream, invocation.update)
    }
    const event = createStreamHookEvent(invocation.streamId, stream, invocation.update)
    if (!event) return { errors: [] }

    const controller = new AbortController()
    generation.active.set(invocation.invocationId, controller)
    const errors: { callbackId: string; message: string }[] = []
    try {
      for (const entry of invocation.callbacks) {
        if (controller.signal.aborted) break
        const callback = generation.callbacks.get(entry.callbackId)
        if (!callback) {
          errors.push({
            callbackId: entry.callbackId,
            message: `Unknown JavaScript callback: ${entry.callbackId}`,
          })
          continue
        }
        const callbackInvocation: Invocation = {
          invocationId: invocation.invocationId,
          generationId: invocation.generationId,
          callbackId: entry.callbackId,
          kind: 'agentHook',
          payload: {
            hook: 'message_update',
            context: entry.context,
            event,
          },
        }
        try {
          await callback(callbackInvocation, controller.signal, nativeContext)
        } catch (error) {
          errors.push({ callbackId: entry.callbackId, message: callbackErrorMessage(error) })
        }
      }
      return { errors }
    } finally {
      controller.abort()
      generation.active.delete(invocation.invocationId)
      if (invocation.update.type === 'done') generation.streams.delete(invocation.streamId)
    }
  }

  #cancel(invocationId: string): void {
    for (const state of this.#generations.values()) {
      const controller = state.active.get(invocationId)
      if (controller) {
        controller.abort()
        return
      }
    }
  }

  #releaseStream(generationId: string, streamId: string): void {
    this.#generations.get(generationId)?.streams.delete(streamId)
  }

  #retireGeneration(generationId: string): void {
    const state = this.#generations.get(generationId)
    if (!state) return
    for (const controller of state.active.values()) controller.abort()
    state.active.clear()
    state.callbacks.clear()
    state.streams.clear()
    state.events.clear()
    state.runtime.retire()
    this.#generations.delete(generationId)
  }
}
