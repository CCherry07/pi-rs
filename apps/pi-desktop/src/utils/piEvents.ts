import type { PiEvent } from "../types";

export const SUPPORTED_PI_EVENT_METHODS = [
  "error",
  "hook/completed",
  "hook/started",
  "item/agentMessage/delta",
  "item/commandExecution/outputDelta",
  "item/commandExecution/terminalInteraction",
  "item/completed",
  "item/fileChange/outputDelta",
  "item/plan/delta",
  "item/reasoning/summaryPartAdded",
  "item/reasoning/summaryTextDelta",
  "item/reasoning/textDelta",
  "item/started",
  "thread/archived",
  "thread/closed",
  "thread/deleted",
  "thread/name/updated",
  "thread/status/changed",
  "thread/started",
  "thread/tokenUsage/updated",
  "thread/unarchived",
  "turn/completed",
  "turn/diff/updated",
  "turn/plan/updated",
  "turn/started",
] as const;

export type SupportedPiEventMethod = (typeof SUPPORTED_PI_EVENT_METHODS)[number];

const SUPPORTED_METHOD_SET = new Set<string>(SUPPORTED_PI_EVENT_METHODS);

function getPiEventMessageObject(
  event: PiEvent,
): Record<string, unknown> | null {
  if (!event || typeof event !== "object") {
    return null;
  }
  const message = (event as { message?: unknown }).message;
  if (!message || typeof message !== "object" || Array.isArray(message)) {
    return null;
  }
  return message as Record<string, unknown>;
}

export function getPiEventRawMethod(event: PiEvent): string | null {
  const message = getPiEventMessageObject(event);
  if (!message) {
    return null;
  }
  const method = message.method;
  if (typeof method !== "string") {
    return null;
  }
  const trimmed = method.trim();
  return trimmed.length > 0 ? trimmed : null;
}

export function isSupportedPiEventMethod(
  method: string,
): method is SupportedPiEventMethod {
  return SUPPORTED_METHOD_SET.has(method);
}

export function getPiEventParams(event: PiEvent): Record<string, unknown> {
  const message = getPiEventMessageObject(event);
  if (!message) {
    return {};
  }
  const params = message.params;
  if (!params || typeof params !== "object" || Array.isArray(params)) {
    return {};
  }
  return params as Record<string, unknown>;
}
