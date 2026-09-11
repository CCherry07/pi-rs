import * as Sentry from '@sentry/react'

const ERROR_MESSAGES = {
  'file-link-open': 'Failed to open file link',
  'open-app-menu': 'Failed to open workspace in target app',
  'desktop-error': 'Desktop error (details withheld for privacy)',
} as const

type ErrorFeature = keyof typeof ERROR_MESSAGES
type TargetKind = 'app' | 'command' | 'finder'
type TransportFactory = NonNullable<Sentry.BrowserOptions['transport']>
type Envelope = Parameters<ReturnType<TransportFactory>['send']>[0]
type EnvelopeItem<T> = T extends [unknown, Array<infer Item>] ? Item : never
type PrivateCounter = { name: string; type: 'counter'; value: 1 }

const COUNTERS = new Set(['app_open', 'agent_created', 'prompt_sent', 'thread_switched', 'workspace_added', 'workspace_switched', 'worktree_agent_created', 'clone_agent_created'])

function errorFeature(value: unknown): ErrorFeature {
  return value === 'file-link-open' || value === 'open-app-menu' ? value : 'desktop-error'
}

function targetKind(value: unknown): TargetKind | undefined {
  return value === 'app' || value === 'command' || value === 'finder' ? value : undefined
}

function record(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : undefined
}

function eventId(value: unknown): string | undefined {
  return typeof value === 'string' && /^[a-f0-9]{32}$/.test(value) ? value : undefined
}

// Rebuild from static categories, rather than trying to recognize every secret in text.
// Stack frames, messages, breadcrumbs, requests and scope data can all contain user data.
function privateErrorEvent(input: unknown): Sentry.ErrorEvent {
  const event = record(input)
  const tags = record(event?.tags)
  const feature = errorFeature(tags?.feature)
  const kind = targetKind(tags?.target_kind)
  return {
    type: undefined,
    event_id: eventId(event?.event_id),
    level: 'error',
    platform: 'javascript',
    release: __APP_VERSION__,
    exception: { values: [{ type: 'DesktopError', value: ERROR_MESSAGES[feature] }] },
    fingerprint: [feature],
    tags: { feature, ...(kind ? { target_kind: kind } : {}) },
  }
}

function privateCounter(input: unknown): PrivateCounter | null {
  const metric = record(input)
  if (!metric || typeof metric.name !== 'string' || !COUNTERS.has(metric.name) || metric.type !== 'counter' || metric.value !== 1) {
    return null
  }
  return { name: metric.name, type: 'counter', value: 1 }
}

function privateEnvelope(envelope: Envelope): Envelope {
  const items: EnvelopeItem<Envelope>[] = []
  for (const [header, payload] of envelope[1]) {
    if (header.type === 'event') {
      items.push([{ type: 'event' }, privateErrorEvent(payload)])
    } else if (header.type === 'trace_metric') {
      const rawMetrics = record(payload)?.items
      if (!Array.isArray(rawMetrics)) {
        continue
      }
      const metrics = rawMetrics.flatMap(raw => {
        const metric = privateCounter(raw)
        return metric ? [{ ...metric, timestamp: Date.now() / 1000, trace_id: '' }] : []
      })
      if (metrics.length > 0) {
        items.push([
          {
            type: 'trace_metric',
            item_count: metrics.length,
            content_type: 'application/vnd.sentry.items.trace-metric+json',
          },
          {
            version: 2,
            ingest_settings: { infer_ip: 'never', infer_user_agent: 'never' },
            items: metrics,
          },
        ])
      }
    }
  }
  // Drop attachments and every other envelope type, including SDK-internal diagnostics.
  // SDKs can add scope attributes AFTER beforeSendMetric, or bypass beforeSend on failure.
  return [{}, items] as Envelope
}

function privateTransport(makeTransport: TransportFactory): TransportFactory {
  return options => {
    const transport = makeTransport(options)
    return {
      send(envelope) {
        const safe = privateEnvelope(envelope)
        return safe[1].length > 0 ? transport.send(safe) : Promise.resolve({ statusCode: 200 })
      },
      flush: timeout => transport.flush(timeout),
    }
  }
}

export function desktopSentryOptions(dsn: string | undefined, makeTransport: TransportFactory = Sentry.makeFetchTransport): Sentry.BrowserOptions {
  return {
    dsn,
    enabled: Boolean(dsn),
    release: __APP_VERSION__,
    sendDefaultPii: false,
    defaultIntegrations: false,
    integrations: [Sentry.globalHandlersIntegration()],
    maxBreadcrumbs: 0,
    beforeBreadcrumb: () => null,
    enableLogs: false,
    beforeSendLog: () => null,
    tracesSampleRate: 0,
    beforeSendTransaction: () => null,
    sendClientReports: false,
    beforeSend: (event, hint) => {
      hint.attachments = []
      return privateErrorEvent(event)
    },
    beforeSendMetric: privateCounter,
    transport: privateTransport(makeTransport),
  }
}

// Callers retain raw diagnostics only in the local toast, never in Sentry or console.
export function reportOpenFailure(feature: Exclude<ErrorFeature, 'desktop-error'>, kind: TargetKind): void {
  Sentry.captureEvent(privateErrorEvent({ tags: { feature, target_kind: kind } }))
}
