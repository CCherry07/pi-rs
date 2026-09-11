import { useCallback, useRef, type Dispatch } from "react";
import { readThread } from "@services/tauri";
import { buildItemsFromThread } from "@utils/threadItems";
import { normalizeTokenUsage } from "../utils/threadNormalize";
import { contextInheritanceFromThread } from "../utils/threadContextInheritance";
import { getResumedTurnState } from "../utils/threadRpc";
import { extractThreadFromResponse } from "../utils/threadSummary";
import type { ThreadAction, ThreadState } from "./useThreadsReducer";

type UseThreadConversationLoaderOptions = {
  state: ThreadState;
  dispatch: Dispatch<ThreadAction>;
  getStatusRevision: (threadId: string) => number;
};

export function useThreadConversationLoader({
  state,
  dispatch,
  getStatusRevision,
}: UseThreadConversationLoaderOptions) {
  const stateRef = useRef(state);
  stateRef.current = state;
  const inFlight = useRef(new Map<string, { threadId: string; promise: Promise<void> }>());
  const replacementRevisions = useRef(new Map<string, number>());
  const getReplacementRevision = useCallback((threadId: string) => replacementRevisions.current.get(threadId) ?? 0, []);
  const invalidateThread = useCallback((threadId: string) => {
    replacementRevisions.current.set(threadId, (replacementRevisions.current.get(threadId) ?? 0) + 1);
    for (const [key, request] of inFlight.current) {
      if (request.threadId === threadId) inFlight.current.delete(key);
    }
  }, []);

  const loadThread = useCallback((workspaceId: string, threadId: string): Promise<void> => {
    if (!workspaceId.trim() || !threadId.trim()) {
      return Promise.reject(new Error("A workspace and thread are required to read a conversation snapshot."));
    }
    const key = JSON.stringify([workspaceId, threadId]);
    const pending = inFlight.current.get(key);
    if (pending) return pending.promise;

    const replacementRevision = replacementRevisions.current.get(threadId) ?? 0;
    const atRequest = stateRef.current;
    const statusRevision = getStatusRevision(threadId);
    const request = (async () => {
      const response = await readThread(workspaceId, threadId);
      // A reload/context replacement is authoritative, not another merge source.
      if ((replacementRevisions.current.get(threadId) ?? 0) !== replacementRevision) return;
      const thread = extractThreadFromResponse(response);
      if (!thread || Array.isArray(thread) || thread.id !== threadId || !Array.isArray(thread.turns)) {
        throw new Error("The requested conversation snapshot is missing or invalid.");
      }
      const turn = getResumedTurnState(thread);
      const rawStatus = thread.status;
      const statusType = typeof rawStatus === "object" && rawStatus !== null
        ? (rawStatus as Record<string, unknown>).type
        : rawStatus;
      const hasStatus = statusType === "active" || statusType === "idle" ||
        turn.activeTurnId !== null || turn.confidentNoActiveTurn;
      const statusUnchanged = statusRevision === getStatusRevision(threadId);
      const rawUsage = thread.tokenUsage ?? thread.token_usage;
      dispatch({
        type: "hydrateThreadItems",
        threadId,
        items: buildItemsFromThread(thread),
        itemsAtRequest: atRequest.itemsByThread[threadId] ?? [],
        contextInheritance: {
          atRequest: atRequest.contextInheritanceByThread[threadId],
          value: contextInheritanceFromThread(thread),
        },
        ...(statusUnchanged && hasStatus ? {
          activity: {
            statusAtRequest: atRequest.threadStatusById[threadId],
            activeTurnIdAtRequest: atRequest.activeTurnIdByThread[threadId],
            isProcessing: statusType === "active" || turn.activeTurnId !== null,
            turnId: turn.activeTurnId,
            timestamp: turn.activeTurnStartedAtMs ?? Date.now(),
          },
        } : {}),
        ...(statusUnchanged && rawUsage && typeof rawUsage === "object" && !Array.isArray(rawUsage) ? {
          usage: {
            atRequest: atRequest.tokenUsageByThread[threadId],
            value: normalizeTokenUsage(rawUsage as Record<string, unknown>),
          },
        } : {}),
      });
    })().finally(() => {
      if (inFlight.current.get(key)?.promise === request) inFlight.current.delete(key);
    });
    inFlight.current.set(key, { threadId, promise: request });
    return request;
  }, [dispatch, getStatusRevision]);

  return { loadThread, invalidateThread, getReplacementRevision };
}
