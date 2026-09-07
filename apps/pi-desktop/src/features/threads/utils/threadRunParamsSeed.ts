import type { ServiceTier } from "@/types";
import type { ThreadRunParams } from "./threadStorage";
import { makeThreadRunParamsKey } from "./threadStorage";

export const NO_THREAD_SCOPE_SUFFIX = "__no_thread__";

export type PendingNewThreadSeed = {
  workspaceId: string;
  serviceTier: ServiceTier | null | undefined;
};

type ResolveThreadRunStateInput = {
  workspaceId: string;
  threadId: string | null;
  lastComposerModelId: string | null;
  lastComposerReasoningEffort: string | null;
  stored: ThreadRunParams | null;
  noThreadStored: ThreadRunParams | null;
};

type ResolvedThreadRunState = {
  scopeKey: string;
  preferredModelId: string | null;
  preferredEffort: string | null;
  preferredServiceTier: ServiceTier | null | undefined;
};

type ThreadRunSeedPatch = {
  modelId: string | null;
  effort: string | null;
  serviceTier: ServiceTier | null | undefined;
};

export function createPendingThreadSeed(options: {
  activeThreadId: string | null;
  activeWorkspaceId: string | null;
  selectedServiceTier: ServiceTier | null | undefined;
}): PendingNewThreadSeed | null {
  const { activeThreadId, activeWorkspaceId, selectedServiceTier } = options;
  if (activeThreadId || !activeWorkspaceId) {
    return null;
  }
  return {
    workspaceId: activeWorkspaceId,
    serviceTier: selectedServiceTier,
  };
}

export function resolveThreadRunState(
  input: ResolveThreadRunStateInput,
): ResolvedThreadRunState {
  const {
    workspaceId,
    threadId,
    lastComposerModelId,
    lastComposerReasoningEffort,
    stored,
    noThreadStored,
  } = input;

  if (!threadId) {
    return {
      scopeKey: `${workspaceId}:${NO_THREAD_SCOPE_SUFFIX}`,
      preferredModelId: stored?.modelId ?? lastComposerModelId ?? null,
      preferredEffort: stored?.effort ?? lastComposerReasoningEffort ?? null,
      preferredServiceTier: stored?.serviceTier,
    };
  }

  return {
    scopeKey: makeThreadRunParamsKey(workspaceId, threadId),
    preferredModelId: stored?.modelId ?? lastComposerModelId ?? null,
    preferredEffort: stored?.effort ?? lastComposerReasoningEffort ?? null,
    preferredServiceTier:
      stored?.serviceTier !== undefined
        ? stored.serviceTier
        : noThreadStored?.serviceTier,
  };
}

export function buildThreadRunSeedPatch(options: {
  workspaceId: string;
  selectedModelId: string | null;
  resolvedEffort: string | null;
  pendingSeed: PendingNewThreadSeed | null;
}): ThreadRunSeedPatch {
  const {
    workspaceId,
    selectedModelId,
    resolvedEffort,
    pendingSeed,
  } = options;

  const pendingForWorkspace =
    pendingSeed && pendingSeed.workspaceId === workspaceId ? pendingSeed : null;

  return {
    modelId: selectedModelId,
    effort: resolvedEffort,
    serviceTier: pendingForWorkspace ? pendingForWorkspace.serviceTier : undefined,
  };
}
