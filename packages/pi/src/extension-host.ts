import { existsSync } from 'node:fs'
import { createRequire } from 'node:module'
import { basename, dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { createJiti } from 'jiti'
import { z } from 'zod'

import { CompatibilityResolver } from './compatibility-resolver.js'
import { createExtensionContext } from './extension-context.js'
import type { PiExtensionCommandContext, PiExtensionContext } from './extension-api.js'
import type { NativeExtensionContext } from './native-binding.js'
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
const callableSchema = z.custom<(...arguments_: never[]) => unknown>(value => typeof value === 'function', 'expected a function')
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
const PROVIDER_HOOKS = new Set(['before_provider_request'])
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
  'before_provider_headers',
  'after_provider_response',
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
  prepareArguments?: unknown
  execute(toolCallId: string, input: unknown, signal: AbortSignal, update: undefined, context: ExtensionContext): unknown | Promise<unknown>
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
  registerFlag(): void
  registerProvider(): void
  registerMessageRenderer(): void
  registerMarkdownTransformer(): void
  registerEntryRenderer(): void
  sendMessage(): void
  sendUserMessage(): void
  appendEntry(): void
  setSessionName(): void
  setLabel(): void
  getSessionName(): undefined
  exec(): Promise<{ stdout: string; stderr: string; code: number; killed: boolean }>
  getActiveTools(): never[]
  getAllTools(): never[]
  setActiveTools(): void
  getCommands(): never[]
  setModel(): Promise<false>
  getThinkingLevel(): 'off'
  setThinkingLevel(): void
  unregisterProvider(): void
  getFlag(): undefined
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
    const state: GenerationState = {
      callbacks: new Map(),
      active: new Map(),
      events: createExtensionEventBus(),
      mode: request.mode,
      projectTrusted: request.projectTrusted,
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

      return { generationId, agentPlugins, providerPlugins, sessionPlugins, diagnostics }
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
        if (definition.prepareArguments !== undefined) {
          inactive(`prepareArguments for JavaScript tool ${definition.name}`)
        }
        const callbackId = `${pluginId}:tool:${definition.name}`
        this.#registerCallback(state, callbackId, async (invocation, signal, nativeContext) => {
          const payload = parseExternal(toolInvocationPayloadSchema, invocation.payload, `JavaScript tool ${definition.name} invocation payload`)
          const result = await definition.execute(payload.context.toolCallId, payload.input, signal, undefined, this.#extensionContext(payload.context, generationId, signal, nativeContext, false))
          return normalizeToolResult(result, definition.name)
        })
        contribution.tools.push({
          callbackId,
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
          const eventValue = payload.event
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
      registerFlag: () => inactive('pi.registerFlag'),
      registerProvider: () => inactive('pi.registerProvider'),
      registerMessageRenderer: () => inactive('pi.registerMessageRenderer'),
      registerMarkdownTransformer: () => inactive('pi.registerMarkdownTransformer'),
      registerEntryRenderer: () => inactive('pi.registerEntryRenderer'),
      sendMessage: () => inactive('pi.sendMessage'),
      sendUserMessage: () => inactive('pi.sendUserMessage'),
      appendEntry: () => inactive('pi.appendEntry'),
      setSessionName: () => inactive('pi.setSessionName'),
      setLabel: () => inactive('pi.setLabel'),
      getSessionName: () => {
        inactive('pi.getSessionName')
        return undefined
      },
      exec: async () => {
        inactive('pi.exec')
        return {
          stdout: '',
          stderr: 'pi.exec is inactive in the pi-rs JavaScript host',
          code: 1,
          killed: false,
        }
      },
      getActiveTools: () => {
        inactive('pi.getActiveTools')
        return []
      },
      getAllTools: () => {
        inactive('pi.getAllTools')
        return []
      },
      setActiveTools: () => inactive('pi.setActiveTools'),
      getCommands: () => {
        inactive('pi.getCommands')
        return []
      },
      setModel: async () => {
        inactive('pi.setModel')
        return false
      },
      getThinkingLevel: () => {
        inactive('pi.getThinkingLevel')
        return 'off'
      },
      setThinkingLevel: () => inactive('pi.setThinkingLevel'),
      unregisterProvider: () => inactive('pi.unregisterProvider'),
      getFlag: () => {
        inactive('pi.getFlag')
        return undefined
      },
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

  #cancel(invocationId: string): void {
    for (const state of this.#generations.values()) {
      const controller = state.active.get(invocationId)
      if (controller) {
        controller.abort()
        return
      }
    }
  }

  #retireGeneration(generationId: string): void {
    const state = this.#generations.get(generationId)
    if (!state) return
    for (const controller of state.active.values()) controller.abort()
    state.active.clear()
    state.callbacks.clear()
    state.events.clear()
    this.#generations.delete(generationId)
  }
}
