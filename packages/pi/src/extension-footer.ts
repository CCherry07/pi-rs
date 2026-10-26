import { execFile, execFileSync } from "node:child_process";

export interface FooterDataProvider {
  getGitBranch(): string | null;
  getExtensionStatuses(): ReadonlyMap<string, string>;
  getAvailableProviderCount(): number;
  onBranchChange(callback: () => void): () => void;
}

interface FooterSources {
  cwd(): string;
  statuses(): ReadonlyMap<string, string>;
  providerCount(): number;
  run(callback: () => void): void;
  report(error: unknown): void;
}

/** Read-only footer data, scoped to the component that subscribes to it. */
export class ExtensionFooter implements FooterDataProvider {
  readonly #listeners = new Set<() => void>();
  readonly #abort = new AbortController();
  #timer?: NodeJS.Timeout;
  #branch: string | null | undefined;
  #cwd?: string;
  #refreshing = false;
  #disposed = false;

  constructor(private readonly sources: FooterSources) {}

  getGitBranch(): string | null {
    if (this.#disposed) return this.#branch ?? null;
    const cwd = this.sources.cwd();
    if (this.#branch === undefined || cwd !== this.#cwd) {
      this.#cwd = cwd;
      const read = (args: string[]) => {
        try { return execFileSync("git", args, { cwd, encoding: "utf8", timeout: 1_000, maxBuffer: 16_384, stdio: ["ignore", "pipe", "ignore"], windowsHide: true }).trim(); }
        catch { return null; }
      };
      this.#branch = read(["symbolic-ref", "--quiet", "--short", "HEAD"])
        ?? (read(["rev-parse", "--verify", "HEAD"]) ? "detached" : null);
    }
    this.#timer ??= setInterval(() => { void this.#refresh(); }, 1_000).unref();
    return this.#branch;
  }

  getExtensionStatuses(): ReadonlyMap<string, string> { return new Map(this.sources.statuses()); }
  getAvailableProviderCount(): number { return this.sources.providerCount(); }
  onBranchChange(callback: () => void): () => void {
    if (this.#disposed) return () => {};
    this.getGitBranch();
    this.#listeners.add(callback);
    return () => { this.#listeners.delete(callback); };
  }

  async #refresh() {
    if (this.#disposed || this.#refreshing) return;
    this.#refreshing = true;
    try {
      const cwd = this.sources.cwd();
      const read = (args: string[]) => new Promise<string | null>(resolve => {
        execFile("git", args, { cwd, encoding: "utf8", timeout: 1_000, maxBuffer: 16_384, windowsHide: true, signal: this.#abort.signal }, (error, stdout) => resolve(error ? null : stdout.trim()));
      });
      const branch = await read(["symbolic-ref", "--quiet", "--short", "HEAD"])
        ?? (await read(["rev-parse", "--verify", "HEAD"]) ? "detached" : null);
      if (this.#disposed) return;
      if (branch !== this.#branch || cwd !== this.#cwd) {
        this.#branch = branch;
        this.#cwd = cwd;
        for (const listener of [...this.#listeners]) {
          if (this.#disposed) break;
          try { this.sources.run(listener); } catch (error) { this.sources.report(error); }
        }
      }
    } catch (error) {
      if (!this.#disposed) this.sources.report(error);
    } finally { this.#refreshing = false; }
  }

  dispose() {
    this.#disposed = true;
    clearInterval(this.#timer);
    this.#abort.abort();
    this.#listeners.clear();
  }
}
