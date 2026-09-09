import type { ComposerSendIntent, ServiceTier } from "@/types";

export type SendMessageOptions = {
  skipPromptExpansion?: boolean;
  model?: string | null;
  effort?: string | null;
  serviceTier?: ServiceTier | null | undefined;
  sendIntent?: ComposerSendIntent;
};

type ResolveSendMessageOptionsArgs = {
  options?: SendMessageOptions;
  defaults: {
    model?: string | null;
    effort?: string | null;
    serviceTier?: ServiceTier | null | undefined;
    steerEnabled: boolean;
    isProcessing: boolean;
    activeTurnId: string | null;
  };
};

export type ResolvedSendMessageOptions = {
  resolvedModel?: string | null;
  resolvedEffort?: string | null;
  resolvedServiceTier?: ServiceTier | null | undefined;
  sendIntent: ComposerSendIntent;
  shouldSteer: boolean;
  requestMode: "start" | "steer";
};

export type TurnStartPayload = {
  model?: string | null;
  effort?: string | null;
  serviceTier?: ServiceTier | null | undefined;
  images?: string[];
};

export function isStaleSteerTurnError(message: string): boolean {
  const normalized = message.trim().toLowerCase();
  if (!normalized) {
    return false;
  }
  if (normalized.includes("no active turn")) {
    return true;
  }
  return normalized.includes("active turn") && normalized.includes("not found");
}

export function resolveSendMessageOptions({
  options,
  defaults,
}: ResolveSendMessageOptionsArgs): ResolvedSendMessageOptions {
  const resolvedModel =
    options?.model !== undefined ? options.model : defaults.model;
  const resolvedEffort =
    options?.effort !== undefined ? options.effort : defaults.effort;
  const resolvedServiceTier =
    options?.serviceTier !== undefined ? options.serviceTier : defaults.serviceTier;
  const sendIntent = options?.sendIntent ?? "default";
  const canSteerCurrentTurn =
    defaults.isProcessing && defaults.steerEnabled && Boolean(defaults.activeTurnId);
  const shouldSteer =
    sendIntent === "steer"
      ? canSteerCurrentTurn
      : sendIntent === "queue"
        ? false
        : canSteerCurrentTurn;

  return {
    resolvedModel,
    resolvedEffort,
    resolvedServiceTier,
    sendIntent,
    shouldSteer,
    requestMode: shouldSteer ? "steer" : "start",
  };
}

export function buildTurnStartPayload({
  model,
  effort,
  serviceTier,
  images,
}: {
  model?: string | null;
  effort?: string | null;
  serviceTier?: ServiceTier | null | undefined;
  images: string[];
}): TurnStartPayload {
  const payload: TurnStartPayload = {
    model,
    effort,
    images,
  };
  if (serviceTier !== undefined) {
    payload.serviceTier = serviceTier;
  }
  return payload;
}
