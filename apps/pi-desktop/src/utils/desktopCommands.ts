// Desktop actions win name collisions in BOTH completion and dispatch.
export const DESKTOP_COMMANDS = ["compact", "reload"] as const;
export type DesktopCommand = (typeof DESKTOP_COMMANDS)[number];

export function resolveDesktopCommand(text: string): DesktopCommand | null {
  const token = /^\/(\S+)(?:\s|$)/.exec(text.trim())?.[1]?.toLowerCase();
  return DESKTOP_COMMANDS.find((name) => name === token) ?? null;
}

export function isDesktopCommandName(name: string): boolean {
  // /prompts:* is the Desktop custom-prompt namespace, expanded before submit.
  return resolveDesktopCommand(`/${name}`) !== null || name.startsWith("prompts:");
}

export type RuntimeCommand = {
  name: string;
  description: string;
  argumentHint?: string | null;
};

export function commandsFromThread(thread: Record<string, unknown>): RuntimeCommand[] {
  if (!Array.isArray(thread.commands)) return [];
  return thread.commands.filter((command): command is RuntimeCommand =>
    command !== null && typeof command === "object" &&
    typeof command.name === "string" && typeof command.description === "string",
  );
}
