import { commandsFromThread } from "@utils/desktopCommands";
import { useCallback, useRef } from "react";
import type { Dispatch, MutableRefObject } from "react";
import type {
  DebugEntry,
  ThreadListSortKey,
  ThreadSummary,
  WorkspaceInfo,
} from "@/types";
import {
  archiveThread as archiveThreadService,
  forkThread as forkThreadService,
  listThreads as listThreadsService,
  resumeThread as resumeThreadService,
  startThread as startThreadService,
} from "@services/tauri";
import { getThreadTimestamp } from "@utils/threadItems";
import { extractThreadRunMetadata } from "@threads/utils/threadRunMetadata";
import { contextInheritanceFromThread } from "@threads/utils/threadContextInheritance";
import {
  buildThreadSummaryFromThread,
  extractThreadFromResponse,
} from "@threads/utils/threadSummary";
import { asString, normalizeTokenUsage } from "@threads/utils/threadNormalize";
import { getParentThreadIdFromThread } from "@threads/utils/threadRpc";
import { saveThreadActivity } from "@threads/utils/threadStorage";
import {
  buildResumeHydrationPlan,
  buildWorkspaceThreadListState,
} from "@threads/utils/threadActionHelpers";
import type { ThreadAction, ThreadState } from "./useThreadsReducer";

const THREAD_LIST_TARGET_COUNT = 20;
const FORK_NAME_SUFFIX = /\s+\(\d+\)$/;

function nextForkThreadName(sourceName: string, threads: ThreadSummary[]) {
  const baseName =
    sourceName.trim().replace(FORK_NAME_SUFFIX, "") || "New Agent";
  const existingNames = new Set(threads.map((thread) => thread.name.trim()));
  let index = 1;
  while (existingNames.has(`${baseName} (${index})`)) {
    index += 1;
  }
  return `${baseName} (${index})`;
}

type UseThreadActionsOptions = {
  dispatch: Dispatch<ThreadAction>;
  itemsByThread: ThreadState["itemsByThread"];
  threadsByWorkspace: ThreadState["threadsByWorkspace"];
  activeThreadIdByWorkspace: ThreadState["activeThreadIdByWorkspace"];
  activeTurnIdByThread: ThreadState["activeTurnIdByThread"];
  threadParentById: ThreadState["threadParentById"];
  threadListCursorByWorkspace: ThreadState["threadListCursorByWorkspace"];
  threadStatusById: ThreadState["threadStatusById"];
  threadSortKey: ThreadListSortKey;
  getReplacementRevision?: (threadId: string) => number;
  onDebug?: (entry: DebugEntry) => void;
  getCustomName: (workspaceId: string, threadId: string) => string | undefined;
  renameThread: (workspaceId: string, threadId: string, name: string) => void;
  threadActivityRef: MutableRefObject<Record<string, Record<string, number>>>;
  loadedThreadsRef: MutableRefObject<Record<string, boolean>>;
  replaceOnResumeRef: MutableRefObject<Record<string, boolean>>;
  applyCollabThreadLinksFromThread: (
    workspaceId: string,
    threadId: string,
    thread: Record<string, unknown>,
  ) => void;
  updateThreadParent: (parentId: string, childIds: string[]) => void;
  onSubagentThreadDetected: (workspaceId: string, threadId: string) => void;
  onThreadRunMetadataDetected?: (
    workspaceId: string,
    threadId: string,
    metadata: { modelId: string | null; effort: string | null },
  ) => void;
};

export function useThreadActions({
  dispatch,
  itemsByThread,
  threadsByWorkspace,
  activeThreadIdByWorkspace,
  activeTurnIdByThread,
  threadParentById,
  threadListCursorByWorkspace,
  threadStatusById,
  threadSortKey,
  getReplacementRevision,
  onDebug,
  getCustomName,
  renameThread,
  threadActivityRef,
  loadedThreadsRef,
  replaceOnResumeRef,
  applyCollabThreadLinksFromThread,
  updateThreadParent,
  onSubagentThreadDetected,
  onThreadRunMetadataDetected,
}: UseThreadActionsOptions) {
  const listingRevisions = useRef(new Map<string, number>());
  const resumeInFlightByThreadRef = useRef<Record<string, number>>({});
  const threadStatusByIdRef = useRef(threadStatusById);
  const activeTurnIdByThreadRef = useRef(activeTurnIdByThread);
  threadStatusByIdRef.current = threadStatusById;
  activeTurnIdByThreadRef.current = activeTurnIdByThread;

  const applyThreadMetadata = useCallback(
    (
      workspaceId: string,
      threadId: string,
      thread: Record<string, unknown>,
      options?: { notifySubagent?: boolean },
    ) => {
      const runMetadata = extractThreadRunMetadata(thread);
      if (runMetadata.modelId || runMetadata.effort) {
        onThreadRunMetadataDetected?.(workspaceId, threadId, runMetadata);
      }
      const sourceParentId = getParentThreadIdFromThread(thread);
      if (sourceParentId) {
        updateThreadParent(sourceParentId, [threadId]);
        if (options?.notifySubagent) {
          onSubagentThreadDetected(workspaceId, threadId);
        }
      }
    },
    [onSubagentThreadDetected, onThreadRunMetadataDetected, updateThreadParent],
  );

  const dispatchPreviewMessage = useCallback(
    (threadId: string, text: string, timestamp: number) => {
      dispatch({
        type: "setLastAgentMessage",
        threadId,
        text,
        timestamp,
      });
    },
    [dispatch],
  );

  const extractThreadId = useCallback(
    (response: Record<string, unknown> | null | undefined) => {
      const thread = extractThreadFromResponse(response);
      return String(thread?.id ?? "");
    },
    [],
  );

  const startThreadForWorkspace = useCallback(
    async (workspaceId: string, options?: { activate?: boolean }) => {
      const shouldActivate = options?.activate !== false;
      onDebug?.({
        id: `${Date.now()}-client-thread-start`,
        timestamp: Date.now(),
        source: "client",
        label: "thread/start",
        payload: { workspaceId },
      });
      try {
        const response = await startThreadService(workspaceId);
        onDebug?.({
          id: `${Date.now()}-server-thread-start`,
          timestamp: Date.now(),
          source: "server",
          label: "thread/start response",
          payload: response,
        });
        const threadId = extractThreadId(response);
        if (threadId) {
          const thread = extractThreadFromResponse(response);
          if (thread) {
            dispatch({
              type: "setThreadCommands",
              threadId,
              commands: commandsFromThread(thread),
            });
            dispatch({
              type: "setThreadContextInheritance",
              threadId,
              contextInheritance: contextInheritanceFromThread(thread),
            });
          }
          dispatch({ type: "ensureThread", workspaceId, threadId });
          if (shouldActivate) {
            dispatch({ type: "setActiveThreadId", workspaceId, threadId });
          }
          loadedThreadsRef.current[threadId] = true;
          return threadId;
        }
        return null;
      } catch (error) {
        onDebug?.({
          id: `${Date.now()}-client-thread-start-error`,
          timestamp: Date.now(),
          source: "error",
          label: "thread/start error",
          payload: error instanceof Error ? error.message : String(error),
        });
        throw error;
      }
    },
    [dispatch, extractThreadId, loadedThreadsRef, onDebug],
  );

  const resumeThreadForWorkspace = useCallback(
    async (
      workspaceId: string,
      threadId: string,
      force = false,
      replaceLocal = false,
    ) => {
      if (!threadId) {
        return null;
      }
      if (!force && loadedThreadsRef.current[threadId]) {
        return threadId;
      }
      const status = threadStatusByIdRef.current[threadId];
      if (
        status?.isProcessing &&
        loadedThreadsRef.current[threadId] &&
        !force
      ) {
        onDebug?.({
          id: `${Date.now()}-client-thread-resume-skipped`,
          timestamp: Date.now(),
          source: "client",
          label: "thread/resume skipped",
          payload: { workspaceId, threadId, reason: "active-turn" },
        });
        return threadId;
      }
      onDebug?.({
        id: `${Date.now()}-client-thread-resume`,
        timestamp: Date.now(),
        source: "client",
        label: "thread/resume",
        payload: { workspaceId, threadId },
      });
      const inFlightCount =
        (resumeInFlightByThreadRef.current[threadId] ?? 0) + 1;
      resumeInFlightByThreadRef.current[threadId] = inFlightCount;
      if (inFlightCount === 1) {
        dispatch({ type: "setThreadResumeLoading", threadId, isLoading: true });
      }
      const replacementRevision = getReplacementRevision?.(threadId);
      try {
        const response = (await resumeThreadService(
          workspaceId,
          threadId,
        )) as Record<string, unknown> | null;
        if (getReplacementRevision?.(threadId) !== replacementRevision)
          return threadId;
        onDebug?.({
          id: `${Date.now()}-server-thread-resume`,
          timestamp: Date.now(),
          source: "server",
          label: "thread/resume response",
          payload: response,
        });
        const thread = extractThreadFromResponse(response);
        if (thread) {
          const contextInheritance = contextInheritanceFromThread(thread);
          dispatch({
            type: "setThreadContextInheritance",
            threadId,
            contextInheritance,
          });
          dispatch({
            type: "setThreadCommands",
            threadId,
            commands: commandsFromThread(thread),
          });
          dispatch({ type: "ensureThread", workspaceId, threadId });
          const tokenUsage = thread.tokenUsage ?? thread.token_usage;
          if (tokenUsage && typeof tokenUsage === "object") {
            dispatch({
              type: "setThreadTokenUsage",
              threadId,
              tokenUsage: normalizeTokenUsage(
                tokenUsage as Record<string, unknown>,
              ),
            });
          }
          applyThreadMetadata(workspaceId, threadId, thread, {
            notifySubagent: true,
          });
          applyCollabThreadLinksFromThread(workspaceId, threadId, thread);
          const localItems = itemsByThread[threadId] ?? [];
          const shouldReplace =
            replaceLocal || replaceOnResumeRef.current[threadId] === true;
          if (shouldReplace) {
            replaceOnResumeRef.current[threadId] = false;
          }
          const hydrationPlan = buildResumeHydrationPlan({
            thread,
            workspaceId,
            threadId,
            replaceLocal: shouldReplace,
            localItems,
            localStatus: threadStatusByIdRef.current[threadId],
            localActiveTurnId:
              activeTurnIdByThreadRef.current[threadId] ?? null,
            getCustomName,
          });
          if (!hydrationPlan.shouldHydrate) {
            loadedThreadsRef.current[threadId] = true;
            return threadId;
          }
          if (hydrationPlan.keepLocalProcessing) {
            onDebug?.({
              id: `${Date.now()}-client-thread-resume-keep-processing`,
              timestamp: Date.now(),
              source: "client",
              label: "thread/resume keep-processing",
              payload: { workspaceId, threadId },
            });
          }
          dispatch({
            type: "markProcessing",
            threadId,
            isProcessing: hydrationPlan.shouldMarkProcessing,
            timestamp: hydrationPlan.processingTimestamp,
          });
          dispatch({
            type: "setActiveTurnId",
            threadId,
            turnId: hydrationPlan.resumedActiveTurnId,
          });
          dispatch({
            type: "setThreadItems",
            threadId,
            items: hydrationPlan.mergedItems,
            contextInheritance,
          });
          if (hydrationPlan.threadName) {
            dispatch({
              type: "setThreadName",
              workspaceId,
              threadId,
              name: hydrationPlan.threadName,
            });
          }
          if (
            hydrationPlan.lastMessageText &&
            hydrationPlan.lastMessageTimestamp !== null
          ) {
            dispatchPreviewMessage(
              threadId,
              hydrationPlan.lastMessageText,
              hydrationPlan.lastMessageTimestamp,
            );
          }
        }
        loadedThreadsRef.current[threadId] = true;
        return threadId;
      } catch (error) {
        onDebug?.({
          id: `${Date.now()}-client-thread-resume-error`,
          timestamp: Date.now(),
          source: "error",
          label: "thread/resume error",
          payload: error instanceof Error ? error.message : String(error),
        });
        return null;
      } finally {
        const nextCount = Math.max(
          0,
          (resumeInFlightByThreadRef.current[threadId] ?? 1) - 1,
        );
        if (nextCount === 0) {
          delete resumeInFlightByThreadRef.current[threadId];
          dispatch({
            type: "setThreadResumeLoading",
            threadId,
            isLoading: false,
          });
        } else {
          resumeInFlightByThreadRef.current[threadId] = nextCount;
        }
      }
    },
    [
      applyThreadMetadata,
      applyCollabThreadLinksFromThread,
      dispatchPreviewMessage,
      dispatch,
      getCustomName,
      getReplacementRevision,
      itemsByThread,
      loadedThreadsRef,
      onDebug,
      replaceOnResumeRef,
    ],
  );

  const forkThreadForWorkspace = useCallback(
    async (
      workspaceId: string,
      threadId: string,
      entryId: string,
      options?: { activate?: boolean },
    ) => {
      if (!threadId) {
        return null;
      }
      const shouldActivate = options?.activate !== false;
      const workspaceThreads = threadsByWorkspace[workspaceId] ?? [];
      const sourceName =
        getCustomName(workspaceId, threadId) ??
        workspaceThreads.find((thread) => thread.id === threadId)?.name ??
        "New Agent";
      const forkedThreadName = nextForkThreadName(sourceName, workspaceThreads);
      onDebug?.({
        id: `${Date.now()}-client-thread-fork`,
        timestamp: Date.now(),
        source: "client",
        label: "thread/fork",
        payload: { workspaceId, threadId, entryId },
      });
      try {
        const response = await forkThreadService(
          workspaceId,
          threadId,
          entryId,
        );
        onDebug?.({
          id: `${Date.now()}-server-thread-fork`,
          timestamp: Date.now(),
          source: "server",
          label: "thread/fork response",
          payload: response,
        });
        const forkedThreadId = extractThreadId(response);
        if (!forkedThreadId) {
          return null;
        }
        dispatch({
          type: "ensureThread",
          workspaceId,
          threadId: forkedThreadId,
        });
        if (shouldActivate) {
          dispatch({
            type: "setActiveThreadId",
            workspaceId,
            threadId: forkedThreadId,
          });
        }
        updateThreadParent(threadId, [forkedThreadId]);
        loadedThreadsRef.current[forkedThreadId] = false;
        await resumeThreadForWorkspace(workspaceId, forkedThreadId, true, true);
        renameThread(workspaceId, forkedThreadId, forkedThreadName);
        return forkedThreadId;
      } catch (error) {
        onDebug?.({
          id: `${Date.now()}-client-thread-fork-error`,
          timestamp: Date.now(),
          source: "error",
          label: "thread/fork error",
          payload: error instanceof Error ? error.message : String(error),
        });
        return null;
      }
    },
    [
      dispatch,
      extractThreadId,
      getCustomName,
      loadedThreadsRef,
      onDebug,
      renameThread,
      resumeThreadForWorkspace,
      threadsByWorkspace,
      updateThreadParent,
    ],
  );

  const refreshThread = useCallback(
    async (workspaceId: string, threadId: string) => {
      if (!threadId) {
        return null;
      }
      replaceOnResumeRef.current[threadId] = true;
      return resumeThreadForWorkspace(workspaceId, threadId, true, true);
    },
    [replaceOnResumeRef, resumeThreadForWorkspace],
  );

  const resetWorkspaceThreads = useCallback(
    (workspaceId: string) => {
      const threadIds = new Set<string>();
      const list = threadsByWorkspace[workspaceId] ?? [];
      list.forEach((thread) => threadIds.add(thread.id));
      const activeThread = activeThreadIdByWorkspace[workspaceId];
      if (activeThread) {
        threadIds.add(activeThread);
      }
      threadIds.forEach((threadId) => {
        loadedThreadsRef.current[threadId] = false;
      });
    },
    [activeThreadIdByWorkspace, loadedThreadsRef, threadsByWorkspace],
  );

  const buildThreadSummary = useCallback(
    (
      workspaceId: string,
      thread: Record<string, unknown>,
      fallbackIndex: number,
    ): ThreadSummary | null =>
      buildThreadSummaryFromThread({
        workspaceId,
        thread,
        fallbackIndex,
        getCustomName,
      }),
    [getCustomName],
  );

  const listThreadsForWorkspaces = useCallback(
    async (
      workspaces: WorkspaceInfo[],
      options?: { preserveState?: boolean; sortKey?: ThreadListSortKey },
    ) => {
      const targets = [
        ...new Map(
          workspaces.map((workspace) => [workspace.id, workspace]),
        ).values(),
      ];
      if (!targets.length) return;
      const requestedSortKey = options?.sortKey ?? threadSortKey;
      const revisions = new Map(
        targets.map(({ id }) => {
          const revision = (listingRevisions.current.get(id) ?? 0) + 1;
          listingRevisions.current.set(id, revision);
          if (!options?.preserveState) {
            dispatch({
              type: "setThreadListLoading",
              workspaceId: id,
              isLoading: true,
            });
            dispatch({
              type: "setThreadListCursor",
              workspaceId: id,
              cursor: null,
            });
          }
          return [id, revision];
        }),
      );
      const isCurrent = (id: string) =>
        listingRevisions.current.get(id) === revisions.get(id);
      try {
        const pages = await listThreadsService(
          targets.map(({ id }) => ({ workspaceId: id })),
          { limit: THREAD_LIST_TARGET_COUNT, sortKey: requestedSortKey },
        );
        const pagesByWorkspace = new Map(
          pages.map((page) => [page.workspaceId, page]),
        );
        const nextThreadActivity = { ...threadActivityRef.current };
        let didChangeActivity = false;
        for (const workspace of targets) {
          if (!isCurrent(workspace.id)) continue;
          const page = pagesByWorkspace.get(workspace.id);
          if (!page)
            throw new Error(`Missing session page for ${workspace.id}`);
          const list = buildWorkspaceThreadListState({
            workspaceId: workspace.id,
            matchingThreads: page.data,
            activityByThread: nextThreadActivity[workspace.id] ?? {},
            requestedSortKey,
            buildThreadSummary,
            activeThreadId: activeThreadIdByWorkspace[workspace.id],
            existingThreadIds: (threadsByWorkspace[workspace.id] ?? []).map(
              (thread) => thread.id,
            ),
            threadStatusById,
            threadParentById,
            threadListTargetCount: THREAD_LIST_TARGET_COUNT,
          });
          list.uniqueThreads.forEach((thread) => {
            applyThreadMetadata(workspace.id, String(thread.id), thread, {
              notifySubagent: true,
            });
          });
          if (list.didChangeActivity) {
            nextThreadActivity[workspace.id] = list.nextActivityByThread;
            didChangeActivity = true;
          }
          dispatch({
            type: "setThreads",
            workspaceId: workspace.id,
            threads: list.summaries,
            sortKey: requestedSortKey,
            preserveAnchors: true,
          });
          dispatch({
            type: "setThreadListCursor",
            workspaceId: workspace.id,
            cursor: page.nextCursor,
          });
          list.previewUpdates.forEach(({ threadId, text, timestamp }) =>
            dispatchPreviewMessage(threadId, text, timestamp),
          );
        }
        if (didChangeActivity) {
          threadActivityRef.current = nextThreadActivity;
          saveThreadActivity(nextThreadActivity);
        }
      } catch (error) {
        onDebug?.({
          id: `${Date.now()}-client-thread-list-error`,
          timestamp: Date.now(),
          source: "error",
          label: "thread/list error",
          payload: String(error),
        });
      } finally {
        targets.forEach(({ id }) => {
          if (!isCurrent(id)) return;
          dispatch({
            type: "setThreadListLoading",
            workspaceId: id,
            isLoading: false,
          });
          dispatch({
            type: "setThreadListPaging",
            workspaceId: id,
            isLoading: false,
          });
        });
      }
    },
    [
      activeThreadIdByWorkspace,
      applyThreadMetadata,
      buildThreadSummary,
      dispatch,
      dispatchPreviewMessage,
      onDebug,
      threadActivityRef,
      threadParentById,
      threadSortKey,
      threadStatusById,
      threadsByWorkspace,
    ],
  );

  const listThreadsForWorkspace = useCallback(
    (
      workspace: WorkspaceInfo,
      options?: { preserveState?: boolean; sortKey?: ThreadListSortKey },
    ) => listThreadsForWorkspaces([workspace], options),
    [listThreadsForWorkspaces],
  );

  const loadOlderThreadsForWorkspace = useCallback(
    async (workspace: WorkspaceInfo) => {
      const cursor = threadListCursorByWorkspace[workspace.id];
      if (!cursor) return;
      const revision = (listingRevisions.current.get(workspace.id) ?? 0) + 1;
      listingRevisions.current.set(workspace.id, revision);
      const isCurrent = () =>
        listingRevisions.current.get(workspace.id) === revision;
      dispatch({
        type: "setThreadListPaging",
        workspaceId: workspace.id,
        isLoading: true,
      });
      try {
        const [page] = await listThreadsService(
          [{ workspaceId: workspace.id, cursor }],
          { limit: THREAD_LIST_TARGET_COUNT, sortKey: threadSortKey },
        );
        if (!isCurrent()) return;
        if (!page || page.workspaceId !== workspace.id)
          throw new Error("Missing session page");
        const existing = threadsByWorkspace[workspace.id] ?? [];
        const ids = new Set(existing.map((thread) => thread.id));
        const additions: ThreadSummary[] = [];
        for (const thread of page.data) {
          const id = String(thread.id ?? "");
          if (!id || ids.has(id)) continue;
          applyThreadMetadata(workspace.id, id, thread);
          const summary = buildThreadSummary(
            workspace.id,
            thread,
            existing.length + additions.length,
          );
          if (!summary) continue;
          additions.push(summary);
          ids.add(id);
          const text = asString(thread.preview).trim();
          if (text)
            dispatchPreviewMessage(id, text, getThreadTimestamp(thread));
        }
        if (additions.length)
          dispatch({
            type: "appendThreads",
            workspaceId: workspace.id,
            threads: additions,
          });
        dispatch({
          type: "setThreadListCursor",
          workspaceId: workspace.id,
          cursor: page.nextCursor,
        });
      } catch (error) {
        onDebug?.({
          id: `${Date.now()}-client-thread-list-older-error`,
          timestamp: Date.now(),
          source: "error",
          label: "thread/list older error",
          payload: String(error),
        });
      } finally {
        if (isCurrent()) {
          dispatch({
            type: "setThreadListPaging",
            workspaceId: workspace.id,
            isLoading: false,
          });
          dispatch({
            type: "setThreadListLoading",
            workspaceId: workspace.id,
            isLoading: false,
          });
        }
      }
    },
    [
      applyThreadMetadata,
      buildThreadSummary,
      dispatch,
      dispatchPreviewMessage,
      onDebug,
      threadListCursorByWorkspace,
      threadSortKey,
      threadsByWorkspace,
    ],
  );

  const archiveThread = useCallback(
    async (workspaceId: string, threadId: string) => {
      try {
        await archiveThreadService(workspaceId, threadId);
      } catch (error) {
        onDebug?.({
          id: `${Date.now()}-client-thread-archive-error`,
          timestamp: Date.now(),
          source: "error",
          label: "thread/archive error",
          payload: error instanceof Error ? error.message : String(error),
        });
      }
    },
    [onDebug],
  );

  return {
    startThreadForWorkspace,
    forkThreadForWorkspace,
    resumeThreadForWorkspace,
    refreshThread,
    resetWorkspaceThreads,
    listThreadsForWorkspaces,
    listThreadsForWorkspace,
    loadOlderThreadsForWorkspace,
    archiveThread,
  };
}
