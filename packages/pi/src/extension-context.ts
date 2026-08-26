import { z } from 'zod'

import type {
  PiExtensionCommandContext,
  PiExtensionContext,
  PiModelRegistry,
  PiReadonlySessionManager,
} from './extension-api.js'
import { parseJson, type HostMode } from './extension-protocol.js'
import type { NativeExtensionContext } from './native-binding.js'

const stringSchema = z.string()
const booleanSchema = z.boolean()
const nullableStringSchema = z.string().nullable()
const optionalStringSchema = z.string().nullable().transform(value => value ?? undefined)
const unknownArraySchema = z.array(z.unknown())
const recordArraySchema = z.array(z.looseObject({}))
const optionalRecordSchema = z.looseObject({}).nullable().transform(value => value ?? undefined)
const replacementSchema = z.strictObject({ cancelled: z.boolean() })
const contextUsageSchema = z.strictObject({
  tokens: z.number().nullable(),
  contextWindow: z.number(),
  percent: z.number().nullable(),
}).nullable().transform(value => value ?? undefined)
const promptOptionsSchema = z.looseObject({})

type ContextOperation = Record<string, unknown> & { type: string }

interface ContextClient {
  query<T>(operation: ContextOperation, schema: z.ZodType<T>, fallback: () => T): T
  notify(operation: ContextOperation): void
  request<T>(operation: ContextOperation, schema: z.ZodType<T>): Promise<T>
}

class NativeContextClient implements ContextClient {
  readonly #nativeContext: NativeExtensionContext | undefined

  constructor(nativeContext: NativeExtensionContext | undefined) {
    this.#nativeContext = nativeContext
  }

  query<T>(operation: ContextOperation, schema: z.ZodType<T>, fallback: () => T): T {
    if (!this.#nativeContext) return fallback()
    const value = parseJson(
      this.#nativeContext.query(JSON.stringify(operation)),
      `native extension context ${operation.type} result`,
    )
    return parseContextResult(schema, value, operation.type)
  }

  notify(operation: ContextOperation): void {
    this.#nativeContext?.notify(JSON.stringify(operation))
  }

  async request<T>(operation: ContextOperation, schema: z.ZodType<T>): Promise<T> {
    if (!this.#nativeContext) {
      throw new Error(`${operation.type} requires the native Pi extension context`)
    }
    const value = parseJson(
      await this.#nativeContext.request(JSON.stringify(operation)),
      `native extension context ${operation.type} result`,
    )
    return parseContextResult(schema, value, operation.type)
  }
}

function parseContextResult<T>(schema: z.ZodType<T>, value: unknown, operation: string): T {
  const result = schema.safeParse(value)
  if (result.success) return result.data
  throw new Error(`native extension context ${operation} returned an invalid value: ${z.prettifyError(result.error)}`, {
    cause: result.error,
  })
}

interface ExtensionContextOptions {
  payload: Record<string, unknown>
  nativeContext?: NativeExtensionContext
  mode: HostMode
  projectTrusted: boolean
  signal: AbortSignal
  command: boolean
  assertActive(): void
}

interface InactiveExtensionUI {
  select(): Promise<undefined>
  confirm(): Promise<boolean>
  input(): Promise<undefined>
  notify(message: unknown, level?: unknown): void
  onTerminalInput(): () => void
  setStatus(): void
  setWorkingMessage(): void
  setWorkingVisible(): void
  setWorkingIndicator(): void
  setHiddenThinkingLabel(): void
  setWidget(): void
  setFooter(): void
  setHeader(): void
  setTitle(): void
  custom(): Promise<undefined>
  pasteToEditor(): void
  setEditorText(): void
  getEditorText(): string
  editor(): Promise<undefined>
  addAutocompleteProvider(): void
  setEditorComponent(): void
  getEditorComponent(): undefined
  getAllThemes(): never[]
  getTheme(): undefined
  setTheme(): { success: false; error: string }
  getToolsExpanded(): boolean
  setToolsExpanded(): void
  readonly theme: undefined
}

function createInactiveExtensionUI(
  publishNotice: (message: string, level: 'info' | 'warning' | 'error') => void,
): InactiveExtensionUI {
  const noOp = (): void => {}
  return Object.freeze({
    select: async () => undefined,
    confirm: async () => false,
    input: async () => undefined,
    notify: (message: unknown, level?: unknown) => publishNotice(
      typeof message === 'string' ? message : String(message),
      level === 'warning' || level === 'error' ? level : 'info',
    ),
    onTerminalInput: () => noOp,
    setStatus: noOp,
    setWorkingMessage: noOp,
    setWorkingVisible: noOp,
    setWorkingIndicator: noOp,
    setHiddenThinkingLabel: noOp,
    setWidget: noOp,
    setFooter: noOp,
    setHeader: noOp,
    setTitle: noOp,
    custom: async () => undefined,
    pasteToEditor: noOp,
    setEditorText: noOp,
    getEditorText: () => '',
    editor: async () => undefined,
    addAutocompleteProvider: noOp,
    setEditorComponent: noOp,
    getEditorComponent: () => undefined,
    getAllThemes: () => [],
    getTheme: () => undefined,
    setTheme: () => ({ success: false as const, error: 'JavaScript extension UI is inactive in pi-rs' }),
    getToolsExpanded: () => false,
    setToolsExpanded: noOp,
    theme: undefined,
  })
}

interface NewSessionOptions {
  parentSession?: string
  setup?: unknown
  withSession?: (context: PiExtensionCommandContext) => unknown | Promise<unknown>
}

interface ReplacementOptions {
  withSession?: (context: PiExtensionCommandContext) => unknown | Promise<unknown>
}

interface ForkOptions extends ReplacementOptions {
  position?: 'before' | 'at'
}

interface NavigateTreeOptions {
  summarize?: boolean
  customInstructions?: string
  replaceInstructions?: boolean
  label?: string
}

export function createExtensionContext(
  options: ExtensionContextOptions,
): PiExtensionContext | PiExtensionCommandContext {
  const client = new NativeContextClient(options.nativeContext)
  const payloadSession = isRecord(options.payload.session) ? options.payload.session : undefined
  const fallbackCwd = typeof options.payload.cwd === 'string'
    ? options.payload.cwd
    : typeof payloadSession?.cwd === 'string'
      ? payloadSession.cwd
      : process.cwd()
  const query = <T>(operation: ContextOperation, schema: z.ZodType<T>, fallback: () => T): T => {
    options.assertActive()
    return client.query(operation, schema, fallback)
  }
  const notify = (operation: ContextOperation): void => {
    options.assertActive()
    client.notify(operation)
  }
  const request = async <T>(operation: ContextOperation, schema: z.ZodType<T>): Promise<T> => {
    options.assertActive()
    const result = await client.request(operation, schema)
    options.assertActive()
    return result
  }
  const ui = createInactiveExtensionUI((message, level) => notify({
    type: 'uiNotify',
    message,
    level,
  }))

  const sessionManager: PiReadonlySessionManager = {
    getCwd: () => query({ type: 'sessionCwd' }, stringSchema, () => fallbackCwd),
    getSessionDir: () => query({ type: 'sessionDir' }, stringSchema, () => ''),
    getSessionId: () => query({ type: 'sessionId' }, stringSchema, () => ''),
    getSessionFile: () => query({ type: 'sessionFile' }, optionalStringSchema, () => undefined),
    getLeafId: () => query({ type: 'sessionLeafId' }, nullableStringSchema, () => null),
    getLeafEntry: () => normalizeSessionEntry(query({ type: 'sessionLeafEntry' }, z.unknown(), () => undefined)),
    getEntry: id => normalizeSessionEntry(query({ type: 'sessionEntry', id }, z.unknown(), () => undefined)),
    getLabel: id => query({ type: 'sessionLabel', id }, optionalStringSchema, () => undefined),
    getBranch: fromId => query({ type: 'sessionBranch', ...(fromId === undefined ? {} : { fromId }) }, unknownArraySchema, () => []).map(normalizeSessionEntry),
    buildContextEntries: () => query({ type: 'sessionContextEntries' }, unknownArraySchema, () => []).map(normalizeSessionEntry),
    getHeader: () => normalizeSessionHeader(query({ type: 'sessionHeader' }, z.unknown(), () => null)),
    getEntries: () => query({ type: 'sessionEntries' }, unknownArraySchema, () => []).map(normalizeSessionEntry),
    getTree: () => query({ type: 'sessionTree' }, unknownArraySchema, () => []).map(normalizeSessionTreeNode),
    getSessionName: () => query({ type: 'sessionName' }, optionalStringSchema, () => undefined),
  }

  const models = (available: boolean): Record<string, unknown>[] => query(
    { type: available ? 'availableModels' : 'models' },
    recordArraySchema,
    () => [],
  )
  const modelRegistry: PiModelRegistry = {
    getAll: () => models(false),
    getAvailable: () => models(true),
    find: (provider, modelId) => models(false).find(model => model.provider === provider && model.id === modelId),
    hasConfiguredAuth: model => models(true).some(candidate => candidate.provider === model.provider && candidate.id === model.id),
    getProviderDisplayName: provider => query(
      { type: 'providerDisplayName', provider },
      stringSchema,
      () => provider,
    ),
  }

  const context = {
    ...options.payload,
    ui,
    sessionManager,
    modelRegistry,
    signal: options.signal,
    isIdle: () => query({ type: 'isIdle' }, booleanSchema, () => true),
    isProjectTrusted: () => query({ type: 'isProjectTrusted' }, booleanSchema, () => options.projectTrusted),
    abort: () => notify({ type: 'abort' }),
    hasPendingMessages: () => query({ type: 'hasPendingMessages' }, booleanSchema, () => false),
    shutdown: () => notify({ type: 'shutdown' }),
    getContextUsage: () => query({ type: 'contextUsage' }, contextUsageSchema, () => undefined),
    compact: (compactOptions?: { customInstructions?: string }) => notify({
      type: 'compact',
      ...(compactOptions?.customInstructions === undefined
        ? {}
        : { customInstructions: compactOptions.customInstructions }),
    }),
    getSystemPrompt: () => query({ type: 'systemPrompt' }, stringSchema, () => ''),
  } as Record<string, unknown>

  Object.defineProperties(context, {
    cwd: {
      enumerable: true,
      get: () => query({ type: 'cwd' }, stringSchema, () => fallbackCwd),
    },
    mode: {
      enumerable: true,
      get: () => {
        options.assertActive()
        return options.mode
      },
    },
    hasUI: {
      enumerable: true,
      get: () => {
        options.assertActive()
        return false
      },
    },
    model: {
      enumerable: true,
      get: () => query({ type: 'model' }, optionalRecordSchema, () => undefined),
    },
    scopedModels: {
      enumerable: true,
      get: () => query({ type: 'scopedModels' }, recordArraySchema, () => []),
    },
    thinkingLevel: {
      enumerable: true,
      get: () => query({ type: 'thinkingLevel' }, stringSchema.optional(), () => undefined),
    },
  })

  if (!options.command) return context as PiExtensionContext

  const commandContext = context as unknown as PiExtensionCommandContext
  commandContext.getSystemPromptOptions = () => query(
    { type: 'systemPromptOptions' },
    promptOptionsSchema,
    () => ({ cwd: fallbackCwd }),
  )
  commandContext.waitForIdle = async () => {
    await request({ type: 'waitForIdle' }, z.null())
  }
  commandContext.newSession = async (newSessionOptions?: NewSessionOptions) => {
    if (newSessionOptions?.setup !== undefined) {
      throw new Error('newSession({ setup }) is not supported by the pi-rs JavaScript host')
    }
    const result = await request({
      type: 'newSession',
      ...(newSessionOptions?.parentSession === undefined
        ? {}
        : { parentSession: newSessionOptions.parentSession }),
    }, replacementSchema)
    if (!result.cancelled) await newSessionOptions?.withSession?.(commandContext)
    return result
  }
  commandContext.fork = async (entryId: string, forkOptions?: ForkOptions) => {
    const result = await request({
      type: 'fork',
      entryId,
      position: forkOptions?.position ?? 'before',
    }, replacementSchema)
    if (!result.cancelled) await forkOptions?.withSession?.(commandContext)
    return result
  }
  commandContext.navigateTree = (targetId: string, navigateOptions?: NavigateTreeOptions) => request({
    type: 'navigateTree',
    targetId,
    summarize: navigateOptions?.summarize ?? false,
    ...(navigateOptions?.customInstructions === undefined
      ? {}
      : { customInstructions: navigateOptions.customInstructions }),
    replaceInstructions: navigateOptions?.replaceInstructions ?? false,
    ...(navigateOptions?.label === undefined ? {} : { label: navigateOptions.label }),
  }, replacementSchema)
  commandContext.switchSession = async (sessionPath: string, switchOptions?: ReplacementOptions) => {
    const result = await request({ type: 'switchSession', sessionPath }, replacementSchema)
    if (!result.cancelled) await switchOptions?.withSession?.(commandContext)
    return result
  }
  commandContext.reload = async () => {
    await request({ type: 'reload' }, z.null())
  }
  return commandContext
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function normalizeSessionEntry(value: unknown): unknown | undefined {
  if (!isRecord(value)) return undefined
  const entry = { ...value }
  delete entry.seq
  if (typeof entry.timestamp === 'number') {
    entry.timestamp = new Date(entry.timestamp).toISOString()
  }
  return entry
}

function normalizeSessionHeader(value: unknown): unknown | null {
  if (!isRecord(value)) return null
  if (value.kind !== 'header') return value
  return {
    type: 'session',
    version: 3,
    id: value.id,
    timestamp: typeof value.createdAt === 'number'
      ? new Date(value.createdAt).toISOString()
      : value.createdAt,
    cwd: value.cwd,
    ...(typeof value.legacyParentSessionPath === 'string'
      ? { parentSession: value.legacyParentSessionPath }
      : {}),
  }
}

function normalizeSessionTreeNode(value: unknown): unknown {
  if (!isRecord(value)) return value
  return {
    ...value,
    entry: normalizeSessionEntry(value.entry),
    children: Array.isArray(value.children)
      ? value.children.map(normalizeSessionTreeNode)
      : [],
  }
}
