import { useCallback, useEffect, useLayoutEffect, useRef } from "react";
import type { Dispatch, MutableRefObject, SetStateAction } from "react";
import type {
  AppSettings,
  ComposerSendIntent,
  ServiceTier,
} from "@/types";
import { useThreadRunParams } from "@threads/hooks/useThreadRunParams";
import {
  buildThreadRunSeedPatch,
  createPendingThreadSeed,
  NO_THREAD_SCOPE_SUFFIX,
  resolveThreadRunState,
  type PendingNewThreadSeed,
} from "@threads/utils/threadRunParamsSeed";
import { makeThreadRunParamsKey } from "@threads/utils/threadStorage";
import { useThreadRunOrchestration } from "./useThreadRunOrchestration";

type SetState<T> = Dispatch<SetStateAction<T>>;

type PersistThreadRunParams = (
  patch: {
    modelId?: string | null;
    effort?: string | null;
    serviceTier?: ServiceTier | null | undefined;
  },
) => void;

type UseThreadSelectionHandlersOrchestrationParams = {
  appSettingsLoading: boolean;
  setAppSettings: SetState<AppSettings>;
  queueSaveSettings: (next: AppSettings) => Promise<AppSettings | void>;
  activeThreadIdRef: MutableRefObject<string | null>;
  setSelectedModelId: (id: string | null) => void;
  setSelectedEffort: (effort: string | null) => void;
  setSelectedServiceTier: (tier: ServiceTier | null | undefined) => void;
  persistThreadRunParams: PersistThreadRunParams;
};

type UseThreadRunBootstrapOrchestrationParams = {
  activeWorkspaceId: string | null | undefined;
};

type UseThreadRunSyncOrchestrationParams = {
  activeWorkspaceId: string | null | undefined;
  activeThreadId: string | null;
  appSettings: Pick<
    AppSettings,
    "lastComposerModelId" | "lastComposerReasoningEffort"
  >;
  threadRunParamsVersion: number;
  getThreadRunParams: ReturnType<typeof useThreadRunParams>["getThreadRunParams"];
  patchThreadRunParams: ReturnType<typeof useThreadRunParams>["patchThreadRunParams"];
  setThreadRunSelectionKey: SetState<string | null>;
  setPreferredModelId: SetState<string | null>;
  setPreferredEffort: SetState<string | null>;
  setPreferredServiceTier: SetState<ServiceTier | null | undefined>;
  activeThreadIdRef: MutableRefObject<string | null>;
  pendingNewThreadSeedRef: MutableRefObject<PendingNewThreadSeed | null>;
  selectedModelId: string | null;
  resolvedEffort: string | null;
  selectedServiceTier: ServiceTier | null | undefined;
};

type MainTab = "home" | "projects" | "chat" | "git" | "log";

type SendOrQueueHandler = (
  text: string,
  images: string[],
  submitIntent?: ComposerSendIntent,
) => Promise<void>;

type UseThreadUiOrchestrationParams = {
  activeWorkspaceId: string | null | undefined;
  activeThreadId: string | null;
  selectedServiceTier: ServiceTier | null | undefined;
  pendingNewThreadSeedRef: MutableRefObject<PendingNewThreadSeed | null>;
  runWithDraftStart: (runner: () => Promise<void>) => Promise<void>;
  handleComposerSend: SendOrQueueHandler;
  clearDraftState: () => void;
  exitDiffView: () => void;
  resetPullRequestSelection: () => void;
  selectWorkspace: (workspaceId: string) => void;
  setActiveThreadId: (threadId: string | null, workspaceId?: string) => void;
  setActiveTab: SetState<MainTab>;
  isCompact: boolean;
  removeThread: (workspaceId: string, threadId: string) => void;
  clearDraftForThread: (threadId: string) => void;
  removeImagesForThread: (threadId: string) => void;
};

export function useThreadRunBootstrapOrchestration({
  activeWorkspaceId,
}: UseThreadRunBootstrapOrchestrationParams) {
  const activeWorkspaceIdForParamsRef = useRef<string | null>(activeWorkspaceId ?? null);

  useEffect(() => {
    activeWorkspaceIdForParamsRef.current = activeWorkspaceId ?? null;
  }, [activeWorkspaceId]);

  return useThreadRunOrchestration({ activeWorkspaceIdForParamsRef });
}

export function useThreadRunSyncOrchestration({
  activeWorkspaceId,
  activeThreadId,
  appSettings,
  threadRunParamsVersion,
  getThreadRunParams,
  patchThreadRunParams,
  setThreadRunSelectionKey,
  setPreferredModelId,
  setPreferredEffort,
  setPreferredServiceTier,
  activeThreadIdRef,
  pendingNewThreadSeedRef,
  selectedModelId,
  resolvedEffort,
  selectedServiceTier,
}: UseThreadRunSyncOrchestrationParams) {
  useLayoutEffect(() => {
    const workspaceId = activeWorkspaceId ?? null;
    const threadId = activeThreadId ?? null;
    activeThreadIdRef.current = threadId;

    if (!workspaceId) {
      return;
    }

    const stored = getThreadRunParams(
      workspaceId,
      threadId ?? NO_THREAD_SCOPE_SUFFIX,
    );
    const noThreadStored = getThreadRunParams(workspaceId, NO_THREAD_SCOPE_SUFFIX);
    const resolved = resolveThreadRunState({
      workspaceId,
      threadId,
      lastComposerModelId: appSettings.lastComposerModelId,
      lastComposerReasoningEffort: appSettings.lastComposerReasoningEffort,
      stored,
      noThreadStored,
    });

    setThreadRunSelectionKey(resolved.scopeKey);
    setPreferredModelId(resolved.preferredModelId);
    setPreferredEffort(resolved.preferredEffort);
    setPreferredServiceTier(resolved.preferredServiceTier);
  }, [
    activeThreadId,
    activeWorkspaceId,
    appSettings.lastComposerModelId,
    appSettings.lastComposerReasoningEffort,
    getThreadRunParams,
    setPreferredEffort,
    setPreferredModelId,
    setPreferredServiceTier,
    setThreadRunSelectionKey,
    threadRunParamsVersion,
    activeThreadIdRef,
    pendingNewThreadSeedRef,
  ]);

  const seededThreadParamsRef = useRef(new Set<string>());
  useEffect(() => {
    const workspaceId = activeWorkspaceId ?? null;
    const threadId = activeThreadId ?? null;
    if (!workspaceId || !threadId) {
      return;
    }

    const key = makeThreadRunParamsKey(workspaceId, threadId);
    if (seededThreadParamsRef.current.has(key)) {
      return;
    }

    const stored = getThreadRunParams(workspaceId, threadId);
    if (stored) {
      seededThreadParamsRef.current.add(key);
      return;
    }

    seededThreadParamsRef.current.add(key);
    const pendingSeed = pendingNewThreadSeedRef.current;
    patchThreadRunParams(
      workspaceId,
      threadId,
      buildThreadRunSeedPatch({
        workspaceId,
        selectedModelId,
        resolvedEffort,
        pendingSeed,
      }),
    );
    if (pendingSeed?.workspaceId === workspaceId) {
      pendingNewThreadSeedRef.current = null;
    }
  }, [
    activeThreadId,
    activeWorkspaceId,
    getThreadRunParams,
    patchThreadRunParams,
    resolvedEffort,
    selectedModelId,
    pendingNewThreadSeedRef,
  ]);

  useEffect(() => {
    const workspaceId = activeWorkspaceId ?? null;
    const threadId = activeThreadId ?? null;
    if (!workspaceId || !threadId || selectedServiceTier === undefined) {
      return;
    }

    const noThreadStored = getThreadRunParams(workspaceId, NO_THREAD_SCOPE_SUFFIX);
    if (noThreadStored?.serviceTier !== undefined) {
      return;
    }

    patchThreadRunParams(workspaceId, NO_THREAD_SCOPE_SUFFIX, {
      serviceTier: selectedServiceTier,
    });
  }, [
    activeThreadId,
    activeWorkspaceId,
    getThreadRunParams,
    patchThreadRunParams,
    selectedServiceTier,
  ]);
}

export function useThreadSelectionHandlersOrchestration({
  appSettingsLoading,
  setAppSettings,
  queueSaveSettings,
  activeThreadIdRef,
  setSelectedModelId,
  setSelectedEffort,
  setSelectedServiceTier,
  persistThreadRunParams,
}: UseThreadSelectionHandlersOrchestrationParams) {
  const handleSelectModel = useCallback(
    (id: string | null) => {
      setSelectedModelId(id);
      const hasActiveThread = Boolean(activeThreadIdRef.current);
      if (!appSettingsLoading && !hasActiveThread) {
        setAppSettings((current) => {
          if (current.lastComposerModelId === id) {
            return current;
          }
          const nextSettings = { ...current, lastComposerModelId: id };
          void queueSaveSettings(nextSettings);
          return nextSettings;
        });
      }
      persistThreadRunParams({ modelId: id });
    },
    [
      activeThreadIdRef,
      appSettingsLoading,
      persistThreadRunParams,
      queueSaveSettings,
      setAppSettings,
      setSelectedModelId,
    ],
  );

  const handleSelectEffort = useCallback(
    (raw: string | null) => {
      const next = typeof raw === "string" && raw.trim().length > 0 ? raw.trim() : null;
      setSelectedEffort(next);
      const hasActiveThread = Boolean(activeThreadIdRef.current);
      if (!appSettingsLoading && !hasActiveThread) {
        setAppSettings((current) => {
          if (current.lastComposerReasoningEffort === next) {
            return current;
          }
          const nextSettings = { ...current, lastComposerReasoningEffort: next };
          void queueSaveSettings(nextSettings);
          return nextSettings;
        });
      }
      persistThreadRunParams({ effort: next });
    },
    [
      activeThreadIdRef,
      appSettingsLoading,
      persistThreadRunParams,
      queueSaveSettings,
      setAppSettings,
      setSelectedEffort,
    ],
  );

  const handleSelectServiceTier = useCallback(
    (tier: ServiceTier | null | undefined) => {
      setSelectedServiceTier(tier);
      persistThreadRunParams({ serviceTier: tier });
    },
    [persistThreadRunParams, setSelectedServiceTier],
  );

  return {
    handleSelectModel,
    handleSelectEffort,
    handleSelectServiceTier,
  };
}

export function useThreadUiOrchestration({
  activeWorkspaceId,
  activeThreadId,
  selectedServiceTier,
  pendingNewThreadSeedRef,
  runWithDraftStart,
  handleComposerSend,
  clearDraftState,
  exitDiffView,
  resetPullRequestSelection,
  selectWorkspace,
  setActiveThreadId,
  setActiveTab,
  isCompact,
  removeThread,
  clearDraftForThread,
  removeImagesForThread,
}: UseThreadUiOrchestrationParams) {
  const rememberPendingNewThreadSeed = useCallback(() => {
    pendingNewThreadSeedRef.current = createPendingThreadSeed({
      activeThreadId: activeThreadId ?? null,
      activeWorkspaceId: activeWorkspaceId ?? null,
      selectedServiceTier,
    });
  }, [
    activeThreadId,
    activeWorkspaceId,
    pendingNewThreadSeedRef,
    selectedServiceTier,
  ]);

  const handleComposerSendWithDraftStart = useCallback(
    (
      text: string,
      images: string[],
      submitIntent?: ComposerSendIntent,
    ) => {
      rememberPendingNewThreadSeed();
      return runWithDraftStart(() => handleComposerSend(text, images, submitIntent));
    },
    [handleComposerSend, rememberPendingNewThreadSeed, runWithDraftStart],
  );

  const handleSelectWorkspaceInstance = useCallback(
    (workspaceId: string, threadId: string) => {
      exitDiffView();
      resetPullRequestSelection();
      clearDraftState();
      selectWorkspace(workspaceId);
      setActiveThreadId(threadId, workspaceId);
      if (isCompact) {
        setActiveTab("chat");
      }
    },
    [
      clearDraftState,
      exitDiffView,
      isCompact,
      resetPullRequestSelection,
      selectWorkspace,
      setActiveTab,
      setActiveThreadId,
    ],
  );

  const handleOpenThreadLink = useCallback(
    (threadId: string, workspaceId?: string | null) => {
      const targetWorkspaceId = workspaceId ?? activeWorkspaceId;
      if (!targetWorkspaceId) {
        return;
      }
      exitDiffView();
      resetPullRequestSelection();
      clearDraftState();
      if (targetWorkspaceId !== activeWorkspaceId) {
        selectWorkspace(targetWorkspaceId);
      }
      setActiveThreadId(threadId, targetWorkspaceId);
    },
    [
      activeWorkspaceId,
      clearDraftState,
      exitDiffView,
      resetPullRequestSelection,
      selectWorkspace,
      setActiveThreadId,
    ],
  );

  const handleArchiveActiveThread = useCallback(() => {
    if (!activeWorkspaceId || !activeThreadId) {
      return;
    }
    removeThread(activeWorkspaceId, activeThreadId);
    clearDraftForThread(activeThreadId);
    removeImagesForThread(activeThreadId);
  }, [
    activeThreadId,
    activeWorkspaceId,
    clearDraftForThread,
    removeImagesForThread,
    removeThread,
  ]);

  return {
    handleComposerSendWithDraftStart,
    handleSelectWorkspaceInstance,
    handleOpenThreadLink,
    handleArchiveActiveThread,
  };
}
