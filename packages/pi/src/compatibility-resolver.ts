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

export const PI_TUI_PACKAGE_NAMES: readonly string[] = Object.freeze(
  PI_PACKAGE_SCOPES.map(scope => `${scope}/pi-tui`),
)

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

/** Resolves extension peers to the single runtime bundled by the host. */
export class CompatibilityResolver {
  readonly aliases: Readonly<Record<string, string>>

  constructor(require: NodeRequire, compatibilityModulePath: string) {
    const entries: [string, string][] = [
      ...PI_PACKAGE_SCOPES.flatMap(scope =>
        PI_COMPATIBILITY_ENTRYPOINTS.map(
          (entrypoint): [string, string] => [`${scope}/${entrypoint}`, compatibilityModulePath],
        ),
      ),
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
