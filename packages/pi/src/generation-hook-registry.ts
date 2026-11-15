export interface HookRegistrationSnapshot {
  pluginId: string
  callbackId: string
}

interface HookRegistration extends HookRegistrationSnapshot {
  active: boolean
}

/** Generation-local subscriptions, grouped in extension load order. */
export class GenerationHookRegistry {
  readonly #registrations: Map<string, Map<string, HookRegistration[]>>

  constructor(pluginIds: readonly string[]) {
    // Reserve every extension's position, including those with no initial hooks.
    this.#registrations = new Map(pluginIds.map(pluginId => [pluginId, new Map()]))
  }

  subscribe(pluginId: string, hook: string, callbackId: string): () => void {
    const hooks = this.#registrations.get(pluginId)
    if (!hooks) throw new Error(`Unknown JavaScript extension: ${pluginId}`)
    const registration: HookRegistration = { pluginId, callbackId, active: true }
    const registrations = hooks.get(hook) ?? []
    registrations.push(registration)
    hooks.set(hook, registrations)
    return () => { registration.active = false }
  }

  snapshot(hook: string): HookRegistrationSnapshot[] {
    return [...this.#registrations.values()].flatMap(hooks =>
      (hooks.get(hook) ?? [])
        .filter(registration => registration.active)
        .map(({ pluginId, callbackId }) => ({ pluginId, callbackId })),
    )
  }

  clear(): void {
    this.#registrations.clear()
  }
}
