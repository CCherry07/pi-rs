import type { TurnPlan } from "@/types";
import {
  getParentThreadIdFromThread,
  getSubagentMetadataFromThread,
  isSubagentThreadSource,
  shouldHideSubagentThreadFromSidebar,
} from "@threads/utils/threadRpc";
import { asString } from "@threads/utils/threadNormalize";

export function normalizeThreadStatusType(status: Record<string, unknown>): string {
  const typeRaw = status.type ?? status.statusType ?? status.status_type;
  if (typeof typeRaw !== "string") {
    return "";
  }
  return typeRaw
    .trim()
    .toLowerCase()
    .replace(/[\s_-]/g, "");
}

export function getLiveThreadSubagentSummaryPatch(thread: Record<string, unknown>) {
  const metadata = getSubagentMetadataFromThread(thread);
  const nickname = asString(metadata.nickname ?? "").trim();
  const role = asString(metadata.role ?? "").trim();
  const hasParentThread = Boolean(getParentThreadIdFromThread(thread));
  const isSubagent =
    isSubagentThreadSource(thread.source) || hasParentThread || Boolean(nickname || role);

  if (!isSubagent) {
    return null;
  }

  return {
    isSubagent: true as const,
    ...(nickname ? { subagentNickname: nickname } : {}),
    ...(role ? { subagentRole: role } : {}),
  };
}

export function getThreadStartAction(
  workspaceId: string,
  threadId: string,
  thread: Record<string, unknown>,
  isThreadHidden: (workspaceId: string, threadId: string) => boolean,
) {
  if (isThreadHidden(workspaceId, threadId)) {
    return "skip" as const;
  }
  if (shouldHideSubagentThreadFromSidebar(thread.source)) {
    return "hide" as const;
  }
  return "continue" as const;
}

export function shouldIgnoreOrphanSubagentThread(thread: Record<string, unknown>) {
  const sourceParentId = getParentThreadIdFromThread(thread);
  const subagentSummaryPatch = getLiveThreadSubagentSummaryPatch(thread);
  if (isSubagentThreadSource(thread.source) && !sourceParentId && !subagentSummaryPatch) {
    return true;
  }
  return false;
}

export function shouldClearCompletedPlanForThread(
  planByThreadRef: { current: Record<string, TurnPlan | null> },
  threadId: string,
  turnId: string,
) {
  const plan = planByThreadRef.current[threadId];
  if (!plan || plan.steps.length === 0) {
    return false;
  }
  if (turnId && plan.turnId !== turnId) {
    return false;
  }
  return plan.steps.every((step) => step.status === "completed");
}

export function resetThreadTurnState(
  refs: {
    hasOptimisticActiveTurnByThreadRef: { current: Record<string, boolean> };
    immediateActiveTurnIdByThreadRef: { current: Record<string, string | null> };
    pendingInterruptsRef: { current: Set<string> };
  },
  threadId: string,
) {
  refs.hasOptimisticActiveTurnByThreadRef.current[threadId] = false;
  refs.immediateActiveTurnIdByThreadRef.current[threadId] = null;
  refs.pendingInterruptsRef.current.delete(threadId);
}
