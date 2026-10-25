const PI_PACKAGE_SCOPES = [
  '@earendil-works',
  '@mariozechner',
] as const

const PI_COMPATIBILITY_ENTRYPOINTS = [
  'pi-coding-agent',
  'pi-agent-core',
  'pi-ai/providers/all',
  'pi-ai/compat',
  'pi-ai/oauth',
  'pi-ai',
] as const

const TYPEBOX_PACKAGE_NAMES = ['typebox', '@sinclair/typebox'] as const
const TYPEBOX_PACKAGE_SUBPATHS = [
  '/compile',
  '/error',
  '/format',
  '/guard',
  '/schema',
  '/system',
  '/type',
  '/value',
  '',
] as const

export interface CompatibilityModules {
  pi: object
  tui: object
}

/** Resolves extension peers to the runtimes owned by the host. */
export class CompatibilityResolver {
  readonly aliases: Readonly<Record<string, string>>
  readonly virtualModules: Readonly<Record<string, object>>

  constructor(require: NodeRequire, modules: CompatibilityModules) {
    // Share host SDK helpers so callbacks retain the owning generation context.
    this.virtualModules = Object.freeze(Object.fromEntries([
      ['@pi-rs/cli', modules.pi],
      ...PI_PACKAGE_SCOPES.map(scope => [`${scope}/pi-tui`, modules.tui]),
      ...PI_PACKAGE_SCOPES.flatMap(scope =>
        PI_COMPATIBILITY_ENTRYPOINTS.map(
          (entrypoint) => [`${scope}/${entrypoint}`, modules.pi],
        ),
      ),
    ]))
    const entries: [string, string][] = [
      ...TYPEBOX_PACKAGE_NAMES.flatMap((packageName) =>
        TYPEBOX_PACKAGE_SUBPATHS.map(
          (subpath): [string, string] => [
            `${packageName}${subpath}`,
            require.resolve(`typebox${subpath}`),
          ],
        ),
      ),
    ]

    // Jiti aliases are prefix mappings. Subpaths must precede package roots or
    // `@sinclair/typebox/value` becomes `<root entry>/value`.
    entries.sort(([left], [right]) => right.length - left.length)
    this.aliases = Object.freeze(Object.fromEntries(entries))
  }
}
