import { z } from 'zod'

const jsonObjectSchema = z.looseObject({})
const hostModeSchema = z.enum(['tui', 'print', 'json', 'rpc'])
const invocationKindSchema = z.enum(['tool', 'toolPrepareArguments', 'command', 'agentHook', 'providerHook', 'sessionHook'])
export const toolExecutionModeSchema = z.enum(['parallel', 'sequential'])

const generationRequestSchema = z.strictObject({
  projectTrusted: z.boolean(),
  extensionPaths: z.array(z.string()),
  mode: hostModeSchema,
  cwd: z.string().default(process.cwd()),
  flagValues: z.record(z.string(), z.union([z.boolean(), z.string()])).default({}),
})

const invocationSchema = z.strictObject({
  invocationId: z.string(),
  generationId: z.string(),
  callbackId: z.string(),
  kind: invocationKindSchema,
  payload: jsonObjectSchema,
})

const hookBatchInvocationSchema = z.strictObject({
  invocationId: z.string(),
  generationId: z.string(),
  hook: z.string(),
  callbacks: z.array(z.strictObject({
    callbackId: z.string(),
    context: jsonObjectSchema,
  })),
  event: jsonObjectSchema,
})

const responseMetadataPatchSchema = z.strictObject({
  responseModel: z.string().nullable(),
  responseId: z.string().nullable(),
  diagnostics: z.array(z.unknown()).nullable(),
  deferred: jsonObjectSchema.nullable(),
  rawStopReason: z.string().nullable(),
  endTurn: z.boolean().nullable(),
})

const contentMetadataSchema = z.discriminatedUnion('type', [
  z.strictObject({ type: z.literal('thinking'), redacted: z.boolean().nullable() }),
  z.strictObject({ type: z.literal('toolCall'), namespace: z.string().nullable() }),
])

const streamUpdateSchema = z.discriminatedUnion('type', [
  z.strictObject({ type: z.literal('start'), metadata: jsonObjectSchema }),
  z.strictObject({ type: z.literal('metadata'), patch: responseMetadataPatchSchema }),
  z.strictObject({
    type: z.literal('contentMetadata'),
    contentIndex: z.number().int().nonnegative(),
    metadata: contentMetadataSchema,
  }),
  z.strictObject({ type: z.literal('textStart'), contentIndex: z.number().int().nonnegative() }),
  z.strictObject({
    type: z.literal('textDelta'),
    contentIndex: z.number().int().nonnegative(),
    delta: z.string(),
  }),
  z.strictObject({
    type: z.literal('textEnd'),
    contentIndex: z.number().int().nonnegative(),
    textSignature: z.string().nullable(),
  }),
  z.strictObject({ type: z.literal('thinkingStart'), contentIndex: z.number().int().nonnegative() }),
  z.strictObject({
    type: z.literal('thinkingDelta'),
    contentIndex: z.number().int().nonnegative(),
    delta: z.string(),
  }),
  z.strictObject({
    type: z.literal('thinkingEnd'),
    contentIndex: z.number().int().nonnegative(),
    thinkingSignature: z.string().nullable(),
  }),
  z.strictObject({
    type: z.literal('toolCallStart'),
    contentIndex: z.number().int().nonnegative(),
    id: z.string(),
    name: z.string(),
  }),
  z.strictObject({
    type: z.literal('toolCallDelta'),
    contentIndex: z.number().int().nonnegative(),
    argumentsDelta: z.string(),
  }),
  z.strictObject({
    type: z.literal('toolCallEnd'),
    contentIndex: z.number().int().nonnegative(),
    thoughtSignature: z.string().nullable(),
  }),
  z.strictObject({
    type: z.literal('done'),
    reason: z.enum(['stop', 'pending', 'length', 'toolUse', 'error', 'aborted', 'deferred']),
    usage: jsonObjectSchema,
  }),
])

const streamHookBatchInvocationSchema = z.strictObject({
  invocationId: z.string(),
  generationId: z.string(),
  callbacks: z.array(z.strictObject({
    callbackId: z.string(),
    context: jsonObjectSchema,
  })),
  streamId: z.string(),
  initialMessage: jsonObjectSchema.optional(),
  update: streamUpdateSchema,
})

const hostOperationSchema = z.discriminatedUnion('type', [
  z.strictObject({ type: z.literal('prepareGeneration'), request: generationRequestSchema }),
  z.strictObject({ type: z.literal('invoke'), invocation: invocationSchema }),
  z.strictObject({ type: z.literal('invokeHookBatch'), invocation: hookBatchInvocationSchema }),
  z.strictObject({ type: z.literal('invokeStreamHookBatch'), invocation: streamHookBatchInvocationSchema }),
  z.strictObject({ type: z.literal('releaseStream'), generationId: z.string(), streamId: z.string() }),
  z.strictObject({ type: z.literal('cancel'), invocationId: z.string() }),
  z.strictObject({ type: z.literal('retireGeneration'), generationId: z.string() }),
])

const hookManifestSchema = z.strictObject({
  name: z.string(),
  callbackId: z.string(),
})

const toolManifestSchema = z.strictObject({
  callbackId: z.string(),
  prepareCallbackId: z.string().optional(),
  name: z.string(),
  label: z.string(),
  description: z.string(),
  parameters: jsonObjectSchema,
  promptSnippet: z.string().optional(),
  promptGuidelines: z.array(z.string()),
  executionMode: toolExecutionModeSchema,
})

const commandManifestSchema = z.strictObject({
  callbackId: z.string(),
  name: z.string(),
  description: z.string(),
  argumentHint: z.string().optional(),
})

const providerRegistrationManifestSchema = z.strictObject({
  pluginId: z.string(),
  path: z.string(),
  name: z.string(),
  config: jsonObjectSchema,
})

const generationManifestSchema = z.strictObject({
  generationId: z.string(),
  agentPlugins: z.array(
    z.strictObject({
      id: z.string(),
      tools: z.array(toolManifestSchema),
      commands: z.array(commandManifestSchema),
      hooks: z.array(hookManifestSchema),
    }),
  ),
  providerPlugins: z.array(
    z.strictObject({
      id: z.string(),
      hooks: z.array(hookManifestSchema),
    }),
  ),
  providerRegistrations: z.array(providerRegistrationManifestSchema).default([]),
  sessionPlugins: z.array(
    z.strictObject({
      id: z.string(),
      hooks: z.array(hookManifestSchema),
    }),
  ),
  diagnostics: z.array(
    z.strictObject({
      pluginId: z.string(),
      path: z.string(),
      feature: z.string(),
      status: z.literal('inactive'),
      message: z.string(),
    }),
  ).default([]),
})

export type HostMode = z.infer<typeof hostModeSchema>
export type InvocationKind = z.infer<typeof invocationKindSchema>
export type ToolExecutionMode = z.infer<typeof toolExecutionModeSchema>
export type GenerationRequest = z.infer<typeof generationRequestSchema>
export type Invocation = z.infer<typeof invocationSchema>
export type HookBatchInvocation = z.infer<typeof hookBatchInvocationSchema>
export type StreamUpdate = z.infer<typeof streamUpdateSchema>
export type StreamHookBatchInvocation = z.infer<typeof streamHookBatchInvocationSchema>
export type HostOperation = z.infer<typeof hostOperationSchema>
export type HookManifest = z.infer<typeof hookManifestSchema>
export type ToolManifest = z.infer<typeof toolManifestSchema>
export type CommandManifest = z.infer<typeof commandManifestSchema>
export type AgentPluginManifest = z.infer<typeof generationManifestSchema>['agentPlugins'][number]
export type ProviderPluginManifest = z.infer<typeof generationManifestSchema>['providerPlugins'][number]
export type ProviderRegistrationManifest = z.infer<typeof providerRegistrationManifestSchema>
export type SessionPluginManifest = z.infer<typeof generationManifestSchema>['sessionPlugins'][number]
export type ExtensionDiagnostic = z.infer<typeof generationManifestSchema>['diagnostics'][number]
export type GenerationManifest = z.infer<typeof generationManifestSchema>

export function parseJson(raw: string, description = 'JSON value'): unknown {
  try {
    const decoded: unknown = JSON.parse(raw)
    return decoded
  } catch (error) {
    throw new Error(`${description} is not valid JSON`, { cause: error })
  }
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return jsonObjectSchema.safeParse(value).success
}

function parseWithSchema<T>(schema: z.ZodType<T>, value: unknown, description: string): T {
  const result = schema.safeParse(value)
  if (result.success) return result.data
  throw new Error(`${description} is invalid: ${z.prettifyError(result.error)}`, {
    cause: result.error,
  })
}

export function parseGenerationManifest(rawManifest: string): GenerationManifest {
  return parseWithSchema(generationManifestSchema, parseJson(rawManifest, 'generation manifest'), 'generation manifest')
}

export function parseHostOperation(rawOperation: string): HostOperation {
  return parseWithSchema(hostOperationSchema, parseJson(rawOperation, 'pi-rs host operation'), 'pi-rs host operation')
}
