import { useMemo, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import type {
  AppSettings,
  ConversationItem,
  DebugEntry,
  ModelOption,
  ServiceTier,
  ThreadSummary,
  WorkspaceInfo,
} from "@/types";
import { computePlanFollowupState } from "@/features/messages/utils/messageRenderUtils";
import { useComposerController } from "@app/hooks/useComposerController";
import { useComposerInsert } from "@app/hooks/useComposerInsert";
import { useWorkspaceFileListing } from "@app/hooks/useWorkspaceFileListing";
import { useWorkspaceAgentMd } from "@/features/workspaces/hooks/useWorkspaceAgentMd";
import { useWorkspaceHome } from "@/features/workspaces/hooks/useWorkspaceHome";

const RECENT_THREAD_LIMIT = 8;

type UseMainAppComposerWorkspaceStateArgs = {
  view: {
    centerMode: "chat" | "diff";
    isCompact: boolean;
    isTablet: boolean;
    activeTab: "home" | "projects" | "chat" | "git" | "log";
    tabletTab: "chat" | "git" | "log";
    filePanelMode: "git" | "files" | "prompts";
    rightPanelCollapsed: boolean;
  };
  workspace: {
    activeWorkspace: WorkspaceInfo | null;
    activeWorkspaceId: string | null;
    isNewAgentDraftMode: boolean;
    startingDraftThreadWorkspaceId: string | null;
    threadsByWorkspace: Record<string, ThreadSummary[]>;
  };
  thread: {
    activeThreadId: string | null;
    activeItems: ConversationItem[];
    activeTurnIdByThread: Record<string, string | null | undefined>;
    threadStatusById: Record<
      string,
      {
        isProcessing: boolean;
      }
    >;
  };
  settings: Pick<
    AppSettings,
    | "steerEnabled"
    | "followUpMessageBehavior"
    | "pauseQueuedMessagesWhenResponseRequired"
  >;
  models: {
    models: ModelOption[];
    selectedModelId: string | null;
    resolvedEffort: string | null;
    selectedServiceTier: ServiceTier | null | undefined;
  };
  refs: {
    composerInputRef: RefObject<HTMLTextAreaElement | null>;
    workspaceHomeTextareaRef: RefObject<HTMLTextAreaElement | null>;
  };
  actions: {
    addWorktreeAgent: Parameters<typeof useWorkspaceHome>[0]["addWorktreeAgent"];
    startThreadForWorkspace: Parameters<typeof useWorkspaceHome>[0]["startThreadForWorkspace"];
    sendUserMessage: Parameters<typeof useComposerController>[0]["sendUserMessage"];
    sendUserMessageToThread: Parameters<typeof useWorkspaceHome>[0]["sendUserMessageToThread"];
    seedThreadRunParams: NonNullable<
      Parameters<typeof useWorkspaceHome>[0]["seedThreadRunParams"]
    >;
    startCompact: Parameters<typeof useComposerController>[0]["startCompact"];
    startReload: Parameters<typeof useComposerController>[0]["startReload"];
    handleWorktreeCreated?: Parameters<typeof useWorkspaceHome>[0]["onWorktreeCreated"];
    addDebugEntry: (entry: DebugEntry) => void;
  };
};

export function useMainAppComposerWorkspaceState({
  view,
  workspace,
  thread,
  settings,
  models,
  refs,
  actions,
}: UseMainAppComposerWorkspaceStateArgs) {
  const { t } = useTranslation("app");
  const {
    centerMode,
    isCompact,
    isTablet,
    activeTab,
    tabletTab,
    filePanelMode,
    rightPanelCollapsed,
  } = view;
  const {
    activeWorkspace,
    activeWorkspaceId,
    isNewAgentDraftMode,
    startingDraftThreadWorkspaceId,
    threadsByWorkspace,
  } = workspace;
  const {
    activeThreadId,
    activeItems,
    activeTurnIdByThread,
    threadStatusById,
  } = thread;
  const {
    models: modelOptions,
    selectedModelId,
    resolvedEffort,
    selectedServiceTier,
  } = models;
  const { composerInputRef, workspaceHomeTextareaRef } = refs;
  const {
    addWorktreeAgent,
    startThreadForWorkspace,
    sendUserMessage,
    sendUserMessageToThread,
    seedThreadRunParams,
    startCompact,
    startReload,
    handleWorktreeCreated,
    addDebugEntry,
  } = actions;
  const showWorkspaceHome = Boolean(
    activeWorkspace && !activeThreadId && !isNewAgentDraftMode,
  );
  const showComposer =
    (!isCompact
      ? centerMode === "chat" || centerMode === "diff"
      : (isTablet ? tabletTab : activeTab) === "chat") && !showWorkspaceHome;

  const { files, isLoading: isFilesLoading, setFileAutocompleteActive } =
    useWorkspaceFileListing({
      activeWorkspace,
      activeWorkspaceId,
      filePanelMode,
      isCompact,
      isTablet,
      activeTab,
      tabletTab,
      rightPanelCollapsed,
      hasComposerSurface: showComposer || showWorkspaceHome,
      onDebug: addDebugEntry,
    });

  const canInterrupt = activeThreadId
    ? threadStatusById[activeThreadId]?.isProcessing ?? false
    : false;
  const isStartingDraftThread =
    Boolean(activeWorkspaceId) && startingDraftThreadWorkspaceId === activeWorkspaceId;
  const isProcessing =
    (activeThreadId ? threadStatusById[activeThreadId]?.isProcessing ?? false : false) ||
    isStartingDraftThread;
  const activeTurnId = activeThreadId ? activeTurnIdByThread[activeThreadId] ?? null : null;
  const steerAvailable = settings.steerEnabled && Boolean(activeTurnId);
  const isPlanReadyAwaitingResponse = useMemo(
    () =>
      computePlanFollowupState({
        threadId: activeThreadId,
        items: activeItems,
        isThinking: isProcessing,
        hasVisibleUserInputRequest: false,
      }).shouldShow,
    [
      activeItems,
      activeThreadId,
      isProcessing,
    ],
  );

  const queueFlushPaused = Boolean(
    settings.pauseQueuedMessagesWhenResponseRequired &&
      activeThreadId &&
      isPlanReadyAwaitingResponse,
  );

  const queuePausedReason =
    queueFlushPaused && isPlanReadyAwaitingResponse
        ? t("layout.queuePausedForPlan")
        : null;

  const composerState = useComposerController({
    activeThreadId,
    activeTurnId,
    activeWorkspaceId,
    isProcessing,
    queueFlushPaused,
    steerEnabled: settings.steerEnabled,
    followUpMessageBehavior: settings.followUpMessageBehavior,
    sendUserMessage,
    startCompact,
    startReload,
  });

  const workspaceHomeState = useWorkspaceHome({
    activeWorkspace,
    models: modelOptions,
    selectedModelId,
    effort: resolvedEffort,
    serviceTier: selectedServiceTier,
    seedThreadRunParams,
    addWorktreeAgent,
    startThreadForWorkspace,
    sendUserMessageToThread,
    reloadWorkspace: () => startReload("/reload"),
    onWorktreeCreated: handleWorktreeCreated,
  });

  const canInsertComposerText = showWorkspaceHome
    ? Boolean(activeWorkspace)
    : Boolean(activeThreadId);
  const handleInsertComposerText = useComposerInsert({
    isEnabled: canInsertComposerText,
    draftText: showWorkspaceHome ? workspaceHomeState.draft : composerState.activeDraft,
    onDraftChange: showWorkspaceHome
      ? workspaceHomeState.setDraft
      : composerState.handleDraftChange,
    textareaRef: showWorkspaceHome ? workspaceHomeTextareaRef : composerInputRef,
  });

  const { recentThreadInstances, recentThreadsUpdatedAt } = useMemo(() => {
    if (!activeWorkspaceId) {
      return { recentThreadInstances: [], recentThreadsUpdatedAt: null };
    }
    const threads = threadsByWorkspace[activeWorkspaceId] ?? [];
    if (threads.length === 0) {
      return { recentThreadInstances: [], recentThreadsUpdatedAt: null };
    }
    const sorted = [...threads].sort((a, b) => b.updatedAt - a.updatedAt);
    const slice = sorted.slice(0, RECENT_THREAD_LIMIT);
    const updatedAt = slice.reduce(
      (max, thread) => (thread.updatedAt > max ? thread.updatedAt : max),
      0,
    );
    const instances = slice.map((thread, index) => ({
      id: `recent-${thread.id}`,
      workspaceId: activeWorkspaceId,
      threadId: thread.id,
      modelId: null,
      modelLabel: thread.name?.trim() || t("tray.untitledThread"),
      sequence: index + 1,
    }));
    return {
      recentThreadInstances: instances,
      recentThreadsUpdatedAt: updatedAt > 0 ? updatedAt : null,
    };
  }, [activeWorkspaceId, t, threadsByWorkspace]);

  const agentMdState = useWorkspaceAgentMd({
    activeWorkspace,
    onDebug: addDebugEntry,
  });

  return {
    showWorkspaceHome,
    showComposer,
    files,
    isFilesLoading,
    setFileAutocompleteActive,
    canInterrupt,
    isProcessing,
    activeTurnId,
    steerAvailable,
    queuePausedReason,
    canInsertComposerText,
    handleInsertComposerText,
    recentThreadInstances,
    recentThreadsUpdatedAt,
    workspaceHomeState,
    agentMdState,
    ...composerState,
  };
}
