/**
 * Generation-local MCP servers contributed by JavaScript extensions.
 *
 * The host validates registrations before they cross the native JSON boundary. Connections and
 * product policy remain owned by the Rust MCP generation; this module only owns extension identity,
 * replacement, and the public registration shape.
 */

export type McpExposure = 'codemode' | 'deferred' | 'direct' | 'hidden'

interface McpServerConfigBase extends Record<string, unknown> {
  enabled?: boolean
  exposure?: McpExposure
  description?: string
  toolExposure?: Record<string, McpExposure>
  timeout?: number
}

export interface McpStdioServerConfig extends McpServerConfigBase {
  type?: 'stdio'
  command: string
  args?: string[]
  env?: Record<string, string>
  cwd?: string
}

export interface McpOAuthConfig {
  clientId?: string
  clientSecret?: string
  callbackPort?: number
  callbackUrl?: string
  scope?: string
  clientName?: string
  clientRegistration?: 'auto' | 'dcr' | 'cimd'
  authServerMetadataUrl?: string
}

export interface McpHttpServerConfig extends McpServerConfigBase {
  type?: 'http' | 'streamable-http'
  url: string
  headers?: Record<string, string>
  oauth?: McpOAuthConfig
  auth?: { provider: string }
}

export type McpServerConfig = McpStdioServerConfig | McpHttpServerConfig

export interface RegisteredMcpServer {
  name: string
  config: McpServerConfig
  /** Path of the extension that owns this registration. */
  extensionPath: string
}

/** Fired after a late extension MCP mutation activates its replacement generation. */
export interface McpServersChangeEvent {
  type: 'mcp_servers_change'
  /** Every extension-registered MCP server after the change. */
  servers: RegisteredMcpServer[]
}

export interface McpServerRegistrationManifest {
  pluginId: string
  path: string
  name: string
  config: McpServerConfig
}

const SERVER_NAME = /^[A-Za-z0-9_-]+$/
const LOOPBACK_HOSTS = new Set(['localhost', '127.0.0.1', '[::1]'])
const EXPOSURES = new Set<McpExposure>(['codemode', 'deferred', 'direct', 'hidden'])
const EXPOSURE_ALIASES: Readonly<Record<string, McpExposure>> = {
  'codemode-deferred': 'codemode',
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function isStringRecord(value: unknown): value is Record<string, string> {
  return isRecord(value) && Object.values(value).every(entry => typeof entry === 'string')
}

function isExposure(value: unknown): value is McpExposure {
  return typeof value === 'string' && EXPOSURES.has(value as McpExposure)
}

function normalizeExposure(value: unknown): unknown {
  return typeof value === 'string' ? (EXPOSURE_ALIASES[value] ?? value) : value
}

function invalid(name: string, message: string, path: string): never {
  throw new Error(`pi.registerMcpServer(${JSON.stringify(name)}) config is invalid: ${message} (${path})`)
}

function checkBaseFields(name: string, config: Record<string, unknown>, path: string): void {
  if (config.enabled !== undefined && typeof config.enabled !== 'boolean') {
    invalid(name, 'enabled must be a boolean', path)
  }
  if (config.description !== undefined && typeof config.description !== 'string') {
    invalid(name, 'description must be a string', path)
  }
  if (config.timeout !== undefined && (
    typeof config.timeout !== 'number'
    || !Number.isFinite(config.timeout)
    || !(config.timeout > 0)
  )) {
    invalid(name, 'timeout must be a positive number of seconds', path)
  }
  const exposure = normalizeExposure(config.exposure)
  if (exposure !== undefined && !isExposure(exposure)) {
    invalid(name, 'exposure must be one of "codemode", "deferred", "direct", "hidden"', path)
  }
  const toolExposure = config.toolExposure
  if (toolExposure !== undefined) {
    if (!isRecord(toolExposure)) invalid(name, 'toolExposure must map tool names to exposures', path)
    for (const [tool, raw] of Object.entries(toolExposure)) {
      if (!isExposure(normalizeExposure(raw))) {
        invalid(name, `toolExposure ${JSON.stringify(tool)} must be one of "codemode", "deferred", "direct", "hidden"`, path)
      }
    }
  }
}

function loopbackHttpUrl(value: string): URL | undefined {
  if (!URL.canParse(value)) return undefined
  const parsed = new URL(value)
  return parsed.protocol === 'http:'
    && LOOPBACK_HOSTS.has(parsed.hostname)
    && !parsed.username
    && !parsed.password
    ? parsed
    : undefined
}

function checkOAuth(name: string, raw: unknown, path: string): void {
  if (raw === undefined) return
  if (!isRecord(raw)) invalid(name, 'oauth must be an object', path)
  if (raw.clientId !== undefined && (typeof raw.clientId !== 'string' || raw.clientId.length === 0)) {
    invalid(name, 'oauth.clientId must be a non-empty string', path)
  }
  if (raw.clientSecret !== undefined && typeof raw.clientSecret !== 'string') {
    invalid(name, 'oauth.clientSecret must be a string', path)
  }
  const callbackPort = raw.callbackPort
  if (callbackPort !== undefined && (
    typeof callbackPort !== 'number'
    || !Number.isInteger(callbackPort)
    || callbackPort < 1
    || callbackPort > 65_535
  )) {
    invalid(name, 'oauth.callbackPort must be a port number', path)
  }
  if (raw.callbackUrl !== undefined) {
    if (typeof raw.callbackUrl !== 'string') {
      invalid(name, 'oauth.callbackUrl must be an http URI on localhost, 127.0.0.1, or [::1] without query or fragment', path)
    }
    const callback = loopbackHttpUrl(raw.callbackUrl)
    if (!callback || callback.search || callback.hash) {
      invalid(name, 'oauth.callbackUrl must be an http URI on localhost, 127.0.0.1, or [::1] without query or fragment', path)
    }
    if (callback.port && callbackPort !== undefined && Number(callback.port) !== callbackPort) {
      invalid(name, 'oauth.callbackUrl and oauth.callbackPort name different ports', path)
    }
  }
  if (raw.scope !== undefined && typeof raw.scope !== 'string') {
    invalid(name, 'oauth.scope must be a string', path)
  }
  if (raw.clientName !== undefined && (typeof raw.clientName !== 'string' || !raw.clientName.trim())) {
    invalid(name, 'oauth.clientName must be a non-empty string', path)
  }
  const registration = raw.clientRegistration
  if (registration !== undefined && registration !== 'auto' && registration !== 'dcr' && registration !== 'cimd') {
    invalid(name, 'oauth.clientRegistration must be "auto", "dcr", or "cimd"', path)
  }
  if (registration === 'cimd') {
    if (raw.clientId !== undefined || raw.clientName !== undefined) {
      invalid(name, 'oauth.clientRegistration "cimd" cannot be combined with oauth.clientId or oauth.clientName', path)
    }
    const callback = typeof raw.callbackUrl === 'string' ? loopbackHttpUrl(raw.callbackUrl) : undefined
    if (raw.callbackUrl !== undefined && (!callback || callback.hostname === '[::1]' || callback.pathname !== '/callback')) {
      invalid(name, 'oauth.clientRegistration "cimd" requires oauth.callbackUrl on localhost or 127.0.0.1 with path /callback', path)
    }
  }
  if (raw.authServerMetadataUrl !== undefined) {
    const metadata = typeof raw.authServerMetadataUrl === 'string' && URL.canParse(raw.authServerMetadataUrl)
      ? new URL(raw.authServerMetadataUrl)
      : undefined
    if (!metadata
      || metadata.username
      || metadata.password
      || metadata.hash
      || (metadata.protocol !== 'https:' && !(metadata.protocol === 'http:' && LOOPBACK_HOSTS.has(metadata.hostname)))) {
      invalid(name, 'oauth.authServerMetadataUrl must be an https URL, or http on localhost, 127.0.0.1, or [::1]', path)
    }
  }
}

function checkHttp(name: string, config: Record<string, unknown>, path: string): void {
  const { url } = config
  const parsed = typeof url === 'string' && URL.canParse(url) ? new URL(url) : undefined
  if (!parsed
    || !['http:', 'https:'].includes(parsed.protocol)
    || parsed.username
    || parsed.password
    || parsed.hash) {
    invalid(name, 'url must be an http or https URL without credentials or a fragment', path)
  }
  if (config.headers !== undefined && !isStringRecord(config.headers)) {
    invalid(name, 'headers must map names to strings', path)
  }
  checkOAuth(name, config.oauth, path)
  if (config.auth !== undefined) {
    if (!isRecord(config.auth)
      || typeof config.auth.provider !== 'string'
      || !config.auth.provider.trim()) {
      invalid(name, 'auth.provider must be a provider name', path)
    }
    if (parsed.protocol !== 'https:' && !LOOPBACK_HOSTS.has(parsed.hostname)) {
      invalid(name, 'auth requires an https URL, or http on localhost, 127.0.0.1, or [::1]', path)
    }
  }
}

function checkStdio(name: string, config: Record<string, unknown>, path: string): void {
  if (typeof config.command !== 'string' || !config.command.trim()) {
    invalid(name, 'command must be a non-empty string', path)
  }
  if (config.args !== undefined && !(Array.isArray(config.args) && config.args.every(entry => typeof entry === 'string'))) {
    invalid(name, 'args must be an array of strings', path)
  }
  if (config.env !== undefined && !isStringRecord(config.env)) {
    invalid(name, 'env must map names to strings', path)
  }
  if (config.cwd !== undefined && typeof config.cwd !== 'string') {
    invalid(name, 'cwd must be a string', path)
  }
  if (config.oauth !== undefined || config.auth !== undefined) {
    invalid(name, 'oauth and auth are only valid for HTTP servers', path)
  }
}

function jsonClone<T>(value: T, name: string, path: string): T {
  try {
    const encoded = JSON.stringify(value)
    if (encoded === undefined) invalid(name, 'config must be JSON serializable', path)
    return JSON.parse(encoded) as T
  } catch (error) {
    throw new Error(`pi.registerMcpServer(${JSON.stringify(name)}) config must be JSON serializable (${path})`, { cause: error })
  }
}

/** Validate and clone one extension registration before it enters a generation manifest. */
export function normalizeMcpServerConfig(name: string, raw: unknown, path: string): McpServerConfig {
  if (!SERVER_NAME.test(name)) {
    throw new Error(`invalid MCP server name ${JSON.stringify(name)} (use letters, digits, "_" and "-") (${path})`)
  }
  if (!isRecord(raw)) invalid(name, 'server config must be an object', path)
  const config: Record<string, unknown> = { ...raw }
  config.exposure = normalizeExposure(config.exposure)
  if (isRecord(config.toolExposure)) {
    config.toolExposure = Object.fromEntries(
      Object.entries(config.toolExposure).map(([tool, exposure]) => [tool, normalizeExposure(exposure)]),
    )
  }
  checkBaseFields(name, config, path)
  if (config.type === 'sse') {
    invalid(name, 'legacy SSE transport is not supported; use a Streamable HTTP endpoint', path)
  }
  if (typeof config.url === 'string' && (config.type === undefined || config.type === 'http' || config.type === 'streamable-http')) {
    checkHttp(name, config, path)
    return jsonClone(config, name, path) as unknown as McpHttpServerConfig
  }
  if (typeof config.command === 'string' && (config.type === undefined || config.type === 'stdio')) {
    checkStdio(name, config, path)
    return jsonClone(config, name, path) as unknown as McpStdioServerConfig
  }
  invalid(name, 'server needs either "command" (stdio) or "url" (streamable HTTP)', path)
}

export function mcpNamespace(name: string): string {
  return name.replaceAll('-', '_')
}

/** Registration-order-preserving registry with extension ownership and namespace collision checks. */
export class McpServerRegistry {
  readonly #servers = new Map<string, McpServerRegistrationManifest>()

  register(pluginId: string, path: string, name: string, raw: unknown): void {
    const config = normalizeMcpServerConfig(name, raw, path)
    const existing = this.#servers.get(name)
    if (existing && existing.path !== path) {
      throw new Error(`MCP server ${JSON.stringify(name)} is already registered by ${existing.path}`)
    }
    const namespace = mcpNamespace(name)
    const collision = [...this.#servers.values()].find(server => server.name !== name && mcpNamespace(server.name) === namespace)
    if (collision) {
      throw new Error(`MCP server namespace ${JSON.stringify(namespace)} from ${JSON.stringify(name)} conflicts with ${JSON.stringify(collision.name)} registered by ${collision.path}`)
    }
    const registration: McpServerRegistrationManifest = {
      pluginId,
      path,
      name,
      config,
    }
    this.#servers.set(name, registration)
  }

  unregister(name: string, path: string): boolean {
    if (this.#servers.get(name)?.path !== path) return false
    this.#servers.delete(name)
    return true
  }

  get(name: string, path: string): McpServerRegistrationManifest | undefined {
    const server = this.#servers.get(name)
    return server?.path === path ? jsonClone(server, server.name, server.path) : undefined
  }

  list(): McpServerRegistrationManifest[] {
    return [...this.#servers.values()].map(server => jsonClone(server, server.name, server.path))
  }

  publicList(): RegisteredMcpServer[] {
    return this.list().map(({ name, config, path }) => ({ name, config, extensionPath: path }))
  }
}
