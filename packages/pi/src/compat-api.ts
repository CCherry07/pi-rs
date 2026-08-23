import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

function packageVersion(): string {
  let directory = dirname(fileURLToPath(import.meta.url));
  for (;;) {
    const parent = dirname(directory);
    const manifestPath = join(directory, "package.json");
    try {
      const manifest = JSON.parse(readFileSync(manifestPath, "utf8")) as {
        name?: unknown;
        version?: unknown;
      };
      if (manifest.name === "@pi-rs/cli" && typeof manifest.version === "string") {
        return manifest.version;
      }
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    }
    if (parent === directory) return "0.0.0";
    directory = parent;
  }
}

/** Runtime exports commonly imported by Pi extensions. Type-only imports are
 * erased by jiti; this shim contains only runtime-neutral helpers. */
export const CONFIG_DIR_NAME = ".pi";
export const VERSION = packageVersion();

export function defineTool<T>(tool: T): T {
  return tool;
}

export function getAgentDir() {
  return process.env.PI_AGENT_DIR || join(homedir(), ".pi", "agent");
}

export function isToolCallEventType(
  toolName: string,
  event: { toolName?: string } | null | undefined,
): boolean {
  return event?.toolName === toolName;
}

export function isBashToolResult(event: { toolName?: string } | null | undefined): boolean {
  return event?.toolName === "bash";
}

export function isReadToolResult(event: { toolName?: string } | null | undefined): boolean {
  return event?.toolName === "read";
}

export function isEditToolResult(event: { toolName?: string } | null | undefined): boolean {
  return event?.toolName === "edit";
}

export function isWriteToolResult(event: { toolName?: string } | null | undefined): boolean {
  return event?.toolName === "write";
}

export function isGrepToolResult(event: { toolName?: string } | null | undefined): boolean {
  return event?.toolName === "grep";
}

export function isFindToolResult(event: { toolName?: string } | null | undefined): boolean {
  return event?.toolName === "find";
}

export function isLsToolResult(event: { toolName?: string } | null | undefined): boolean {
  return event?.toolName === "ls";
}
