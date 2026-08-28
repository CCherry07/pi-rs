import { spawn } from 'node:child_process'

import { z } from 'zod'

import type { NativeExtensionContext } from './native-binding.js'
import { parseJson } from './extension-protocol.js'

export interface ExecOptions {
  signal?: AbortSignal
  timeout?: number
  cwd?: string
}

export interface ExecResult {
  stdout: string
  stderr: string
  code: number
  killed: boolean
}

/**
 * Generation-local adapter for Pi facilities owned by the Rust session.
 * The native handle is rebound on every callback so retained extension API
 * closures follow session replacement while still expiring with generation.
 */
export class ExtensionRuntime {
  readonly #cwd: string
  readonly #assertActive: () => void
  #nativeContext: NativeExtensionContext | undefined

  constructor(cwd: string, assertActive: () => void) {
    this.#cwd = cwd
    this.#assertActive = assertActive
  }

  bind(nativeContext: NativeExtensionContext | undefined): void {
    if (nativeContext) this.#nativeContext = nativeContext
  }

  retire(): void {
    this.#nativeContext = undefined
  }

  query<T>(operation: Record<string, unknown>, schema: z.ZodType<T>, fallback: () => T): T {
    this.#assertActive()
    if (!this.#nativeContext) return fallback()
    const decoded = parseJson(
      this.#nativeContext.query(JSON.stringify(operation)),
      `native extension runtime ${String(operation.type)} result`,
    )
    const result = schema.safeParse(decoded)
    if (result.success) return result.data
    throw new Error(
      `native extension runtime ${String(operation.type)} returned an invalid value: ${z.prettifyError(result.error)}`,
      { cause: result.error },
    )
  }

  notify(operation: Record<string, unknown>): void {
    this.#assertActive()
    if (!this.#nativeContext) {
      throw new Error(`${String(operation.type)} requires an active native Pi session`)
    }
    this.#nativeContext.notify(JSON.stringify(operation))
  }

  async request<T>(operation: Record<string, unknown>, schema: z.ZodType<T>): Promise<T> {
    this.#assertActive()
    if (!this.#nativeContext) {
      throw new Error(`${String(operation.type)} requires an active native Pi session`)
    }
    const decoded = parseJson(
      await this.#nativeContext.request(JSON.stringify(operation)),
      `native extension runtime ${String(operation.type)} result`,
    )
    this.#assertActive()
    const result = schema.safeParse(decoded)
    if (result.success) return result.data
    throw new Error(
      `native extension runtime ${String(operation.type)} returned an invalid value: ${z.prettifyError(result.error)}`,
      { cause: result.error },
    )
  }

  exec(command: string, args: string[], options?: ExecOptions): Promise<ExecResult> {
    this.#assertActive()
    return execCommand(command, args, options?.cwd ?? this.#cwd, options)
  }
}

export function execCommand(
  command: string,
  args: string[],
  cwd: string,
  options?: ExecOptions,
): Promise<ExecResult> {
  return new Promise(resolve => {
    const child = spawn(command, args, {
      cwd,
      shell: false,
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    let stdout = ''
    let stderr = ''
    let killed = false
    let settled = false
    let timeout: NodeJS.Timeout | undefined
    let forceKill: NodeJS.Timeout | undefined

    const finish = (code: number): void => {
      if (settled) return
      settled = true
      if (timeout) clearTimeout(timeout)
      if (forceKill) clearTimeout(forceKill)
      options?.signal?.removeEventListener('abort', kill)
      resolve({ stdout, stderr, code, killed })
    }
    const kill = (): void => {
      if (killed || settled) return
      killed = true
      child.kill('SIGTERM')
      forceKill = setTimeout(() => child.kill('SIGKILL'), 5_000)
      forceKill.unref()
    }

    child.stdout?.on('data', chunk => { stdout += String(chunk) })
    child.stderr?.on('data', chunk => { stderr += String(chunk) })
    child.once('close', code => finish(code ?? 0))
    child.once('error', error => {
      if (!stderr) stderr = error.message
      finish(1)
    })

    if (options?.signal?.aborted) kill()
    else options?.signal?.addEventListener('abort', kill, { once: true })
    if (options?.timeout !== undefined && options.timeout > 0) {
      timeout = setTimeout(kill, options.timeout)
      timeout.unref()
    }
  })
}
