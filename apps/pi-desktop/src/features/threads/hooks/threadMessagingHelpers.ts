import i18n from "@/i18n";
import type { ComposerSendIntent, ServiceTier } from "@/types";

export type SendMessageOptions = {
  skipPromptExpansion?: boolean;
  model?: string | null;
  effort?: string | null;
  serviceTier?: ServiceTier | null | undefined;
  sendIntent?: ComposerSendIntent;
};

type FastCommandAction = "toggle" | "on" | "off" | "status" | "invalid";

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

export function parseFastCommand(text: string): FastCommandAction {
  const arg = text.replace(/^\/fast\b/i, "").trim().toLowerCase();
  if (!arg) {
    return "toggle";
  }
  if (arg === "on") {
    return "on";
  }
  if (arg === "off") {
    return "off";
  }
  if (arg === "status") {
    return "status";
  }
  return "invalid";
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

export function buildStatusLines({
  model,
  serviceTier,
  effort,
}: {
  model?: string | null;
  serviceTier?: ServiceTier | null | undefined;
  effort?: string | null;
}): string[] {
  const defaultLabel = i18n.t("commands.status.default", { ns: "messages" });
  const lines = [
    i18n.t("commands.status.title", { ns: "messages" }),
    i18n.t("commands.status.model", {
      ns: "messages",
      value: model ?? defaultLabel,
    }),
    i18n.t("commands.status.fastMode", {
      ns: "messages",
      value: i18n.t(
        serviceTier === "fast" ? "commands.status.on" : "commands.status.off",
        { ns: "messages" },
      ),
    }),
    i18n.t("commands.status.reasoningEffort", {
      ns: "messages",
      value: effort ?? defaultLabel,
    }),
  ];
  return lines;
}
