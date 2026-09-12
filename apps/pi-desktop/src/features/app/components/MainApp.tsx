import { lazy, useCallback, useEffect, useMemo, useRef, useState } from "react";
import successSoundUrl from "@/assets/success-notification.mp3";
import errorSoundUrl from "@/assets/error-notification.mp3";
import { MainAppShell } from "@app/components/MainAppShell";
import { useThreads } from "@threads/hooks/useThreads";
import { ThreadConversationsContext } from "@threads/contexts/ThreadConversations";
import { usePullRequestComposer } from "@/features/git/hooks/usePullRequestComposer";
import { useAutoExitEmptyDiff } from "@/features/git/hooks/useAutoExitEmptyDiff";
import { isMissingRepo } from "@/features/git/utils/repoErrors";
import { useModels } from "@/features/models/hooks/useModels";
import { useCustomPrompts } from "@/features/prompts/hooks/useCustomPrompts";
import { useBranchSwitcherShortcut } from "@/features/git/hooks/useBranchSwitcherShortcut";
import { useRenameWorktreePrompt } from "@/features/workspaces/hooks/useRenameWorktreePrompt";
import { useLayoutController } from "@app/hooks/useLayoutController";
import { useUpdaterController } from "@app/hooks/useUpdaterController";
import { useErrorToasts } from "@/features/notifications/hooks/useErrorToasts";
import { useComposerShortcuts } from "@/features/composer/hooks/useComposerShortcuts";
import { useComposerMenuActions } from "@/features/composer/hooks/useComposerMenuActions";
import { useComposerEditorState } from "@/features/composer/hooks/useComposerEditorState";
import { useMainAppComposerWorkspaceState } from "@app/hooks/useMainAppComposerWorkspaceState";
import { useMainAppGitState } from "@app/hooks/useMainAppGitState";
import { useMainAppLayoutSurfaces } from "@app/hooks/useMainAppLayoutSurfaces";
import { useMainAppLayoutNodes } from "@app/hooks/useMainAppLayoutNodes";
import { useWorkspaceFromUrlPrompt } from "@/features/workspaces/hooks/useWorkspaceFromUrlPrompt";
import { useWorkspaceController } from "@app/hooks/useWorkspaceController";
import { useWorkspaceSelection } from "@/features/workspaces/hooks/useWorkspaceSelection";
import { usePlanReadyActions } from "@app/hooks/usePlanReadyActions";
import { useThreadRows } from "@app/hooks/useThreadRows";
import { useInterruptShortcut } from "@app/hooks/useInterruptShortcut";
import { useArchiveShortcut } from "@app/hooks/useArchiveShortcut";
import { useCopyThread } from "@threads/hooks/useCopyThread";
import { useTerminalController } from "@/features/terminal/hooks/useTerminalController";
import { useWorkspaceLaunchScript } from "@app/hooks/useWorkspaceLaunchScript";
import { useWorkspaceLaunchScripts } from "@app/hooks/useWorkspaceLaunchScripts";
import { useWorktreeSetupScript } from "@app/hooks/useWorktreeSetupScript";
import { effectiveCommitMessageModelId } from "@/features/git/utils/commitMessageModelSelection";
import { useMainAppModals } from "@app/hooks/useMainAppModals";
import { useMainAppDisplayNodes } from "@app/hooks/useMainAppDisplayNodes";
import { useMainAppPromptActions } from "@app/hooks/useMainAppPromptActions";
import { useMainAppShellProps } from "@app/hooks/useMainAppShellProps";
import { useMainAppSidebarMenuOrchestration } from "@app/hooks/useMainAppSidebarMenuOrchestration";
import { useMainAppSettingsActions } from "@app/hooks/useMainAppSettingsActions";
import { useMainAppThreadRunState } from "@app/hooks/useMainAppThreadRunState";
import { useMainAppWorktreeState } from "@app/hooks/useMainAppWorktreeState";
import { useMainAppWorkspaceActions } from "@app/hooks/useMainAppWorkspaceActions";
import { useMainAppWorkspaceLifecycle } from "@app/hooks/useMainAppWorkspaceLifecycle";
import { useMainAppMobileThreadRefresh } from "@app/hooks/useMainAppMobileThreadRefresh";
import type {
  ComposerEditorSettings,
  ServiceTier,
  WorkspaceInfo,
} from "@/types";
import { useOpenAppIcons } from "@app/hooks/useOpenAppIcons";
import { useNewAgentDraft } from "@app/hooks/useNewAgentDraft";
import { useSystemNotificationThreadLinks } from "@app/hooks/useSystemNotificationThreadLinks";
import { useThreadListSortKey } from "@app/hooks/useThreadListSortKey";
import { useThreadListActions } from "@app/hooks/useThreadListActions";
import { useTrayRecentThreads } from "@app/hooks/useTrayRecentThreads";
import { useTauriEvent } from "@app/hooks/useTauriEvent";
import { useAppBootstrapOrchestration } from "@app/bootstrap/useAppBootstrapOrchestration";
import {
  useThreadRunBootstrapOrchestration,
  useThreadRunSyncOrchestration,
  useThreadSelectionHandlersOrchestration,
  useThreadUiOrchestration,
} from "@app/orchestration/useThreadOrchestration";
import {
  useWorkspaceInsightsOrchestration,
  useWorkspaceOrderingOrchestration,
} from "@app/orchestration/useWorkspaceOrchestration";
import { useAppShellOrchestration } from "@app/orchestration/useLayoutOrchestration";
import { subscribeTrayOpenThread } from "@services/events";
import { useThreadModelConfiguration } from "@app/hooks/useThreadModelConfiguration";

const SettingsView = lazy(() =>
  import("@settings/components/SettingsView").then((module) => ({
    default: module.SettingsView,
  })),
);

export default function MainApp() {
  const {
    appSettings,
    setAppSettings,
    appSettingsLoading,
    reduceTransparency,
    setReduceTransparency,
    scaleShortcutTitle,
    scaleShortcutText,
    queueSaveSettings,
    dictationModel,
    dictationState,
    dictationLevel,
    dictationTranscript,
    dictationError,
    dictationHint,
    dictationReady,
    handleToggleDictation,
    cancelDictation,
    clearDictationTranscript,
    clearDictationError,
    clearDictationHint,
    debugOpen,
    setDebugOpen,
    debugEntries,
    showDebugButton,
    addDebugEntry,
    handleCopyDebug,
    clearDebugEntries,
    shouldReduceTransparency,
  } = useAppBootstrapOrchestration();
  const {
    threadListSortKey,
    setThreadListSortKey,
    threadListOrganizeMode,
    setThreadListOrganizeMode,
  } = useThreadListSortKey();
  const [activeTab, setActiveTab] = useState<
    "home" | "projects" | "chat" | "git" | "log"
  >("chat");
  const tabletTab =
    activeTab === "projects" || activeTab === "home" ? "chat" : activeTab;
  const {
    workspaces,
    workspaceGroups,
    groupedWorkspaces,
    getWorkspaceGroupName,
    ungroupedLabel,
    activeWorkspace,
    activeWorkspaceId,
    setActiveWorkspaceId,
    addWorkspace,
    addWorkspaceFromPath,
    addWorkspaceFromGitUrl,
    addWorkspacesFromPaths,
    addCloneAgent,
    addWorktreeAgent,
    updateWorkspaceSettings,
    createWorkspaceGroup,
    renameWorkspaceGroup,
    moveWorkspaceGroup,
    deleteWorkspaceGroup,
    assignWorkspaceGroup,
    removeWorkspace,
    removeWorktree,
    renameWorktree,
    renameWorktreeUpstream,
    deletingWorktreeIds,
    hasLoaded,
    refreshWorkspaces,
  } = useWorkspaceController({
    appSettings,
    addDebugEntry,
    queueSaveSettings,
  });
  const updaterEnabled = true;

  const workspacesById = useMemo(
    () => new Map(workspaces.map((workspace) => [workspace.id, workspace])),
    [workspaces],
  );
  const {
    threadRunParamsVersion,
    getThreadRunParams,
    patchThreadRunParams,
    preferredModelId,
    setPreferredModelId,
    preferredEffort,
    setPreferredEffort,
    preferredServiceTier,
    setPreferredServiceTier,
    threadRunSelectionKey,
    setThreadRunSelectionKey,
    activeThreadIdRef,
    pendingNewThreadSeedRef,
    persistThreadRunParams,
  } = useThreadRunBootstrapOrchestration({
    activeWorkspaceId,
  });
  const {
    appRef,
    isResizing,
    sidebarWidth,
    chatDiffSplitPositionPercent,
    rightPanelWidth,
    onSidebarResizeStart,
    onChatDiffSplitPositionResizeStart,
    onRightPanelResizeStart,
    planPanelHeight,
    onPlanPanelResizeStart,
    terminalPanelHeight,
    onTerminalPanelResizeStart,
    debugPanelHeight,
    onDebugPanelResizeStart,
    isCompact,
    isTablet,
    isPhone,
    sidebarCollapsed,
    rightPanelCollapsed,
    collapseSidebar,
    expandSidebar,
    collapseRightPanel,
    expandRightPanel,
    terminalOpen,
    handleDebugClick,
    handleToggleTerminal,
    openTerminal,
    closeTerminal: closeTerminalPanel,
  } = useLayoutController({
    activeWorkspaceId,
    setActiveTab,
    setDebugOpen,
    toggleDebugPanelShortcut: appSettings.toggleDebugPanelShortcut,
    toggleTerminalShortcut: appSettings.toggleTerminalShortcut,
  });
  const sidebarToggleProps = {
    isCompact,
    sidebarCollapsed,
    rightPanelCollapsed,
    onCollapseSidebar: collapseSidebar,
    onExpandSidebar: expandSidebar,
    onCollapseRightPanel: collapseRightPanel,
    onExpandRightPanel: expandRightPanel,
  };
  const composerInputRef = useRef<HTMLTextAreaElement | null>(null);
  const workspaceHomeTextareaRef = useRef<HTMLTextAreaElement | null>(null);

  const getWorkspaceName = useCallback(
    (workspaceId: string) => workspacesById.get(workspaceId)?.name,
    [workspacesById],
  );

  const recordPendingThreadLinkRef = useRef<
    (workspaceId: string, threadId: string) => void
  >(() => {});

  const { errorToasts, dismissErrorToast } = useErrorToasts();
  const queueGitStatusRefreshRef = useRef<() => void>(() => {});
  const handleThreadMessageActivity = useCallback(() => {
    queueGitStatusRefreshRef.current();
  }, []);

  const {
    models,
    selectedModel,
    selectedModelId,
    setSelectedModelId,
    reasoningSupported,
    reasoningOptions,
    selectedEffort,
    setSelectedEffort,
    refreshModels,
    selectCatalog,
    isCatalogCurrent,
  } = useModels({
    onDebug: addDebugEntry,
    preferredModelId,
    preferredEffort,
    selectionKey: threadRunSelectionKey,
  });

  const [selectedServiceTier, setSelectedServiceTier] = useState<
    ServiceTier | null | undefined
  >(undefined);
  useEffect(() => {
    setSelectedServiceTier(preferredServiceTier);
  }, [preferredServiceTier, threadRunSelectionKey]);

  const {
    handleSelectModel,
    handleSelectEffort,
    handleSelectServiceTier,
  } = useThreadSelectionHandlersOrchestration({
    appSettingsLoading,
    setAppSettings,
    queueSaveSettings,
    activeThreadIdRef,
    setSelectedModelId,
    setSelectedEffort,
    setSelectedServiceTier,
    persistThreadRunParams,
  });
  const commitMessageModelId = useMemo(
    () => effectiveCommitMessageModelId(models, appSettings.commitMessageModelId),
    [models, appSettings.commitMessageModelId],
  );

  const composerShortcuts = {
    modelShortcut: appSettings.composerModelShortcut,
    reasoningShortcut: appSettings.composerReasoningShortcut,
    models,
    selectedModelId,
    onSelectModel: handleSelectModel,
    reasoningOptions,
    selectedEffort,
    onSelectEffort: handleSelectEffort,
    selectedServiceTier: selectedServiceTier ?? null,
    reasoningSupported,
  };

  useComposerShortcuts({
    textareaRef: composerInputRef,
    ...composerShortcuts,
  });

  useComposerShortcuts({
    textareaRef: workspaceHomeTextareaRef,
    ...composerShortcuts,
  });

  useComposerMenuActions({
    models,
    selectedModelId,
    onSelectModel: handleSelectModel,
    reasoningOptions,
    selectedEffort,
    onSelectEffort: handleSelectEffort,
    reasoningSupported,
    onFocusComposer: () => composerInputRef.current?.focus(),
  });

  const {
    prompts,
    createPrompt,
    updatePrompt,
    deletePrompt,
    movePrompt,
    getWorkspacePromptsDir,
    getGlobalPromptsDir,
  } = useCustomPrompts({ activeWorkspace, onDebug: addDebugEntry });
  const resolvedModel = selectedModel?.id ?? null;
  const resolvedEffort = reasoningSupported
    ? reasoningOptions.includes(selectedEffort ?? "")
      ? selectedEffort
      : selectedModel?.defaultReasoningEffort ?? reasoningOptions[0] ?? null
    : null;

  const { handleThreadRunMetadataDetected } = useMainAppThreadRunState({
    getThreadRunParams,
    patchThreadRunParams,
  });

  const {
    setActiveThreadId,
    activeThreadId,
    activeItems,
    conversationSource,
    threadsByWorkspace,
    threadParentById,
    isSubagentThread,
    threadStatusById,
    threadResumeLoadingById,
    threadListLoadingByWorkspace,
    threadListPagingByWorkspace,
    threadListCursorByWorkspace,
    activeTurnIdByThread,
    tokenUsageByThread,
    planByThread,
    lastAgentMessageByThread,
    pinnedThreadsVersion,
    interruptTurn,
    removeThread,
    pinThread,
    unpinThread,
    isThreadPinned,
    getPinTimestamp,
    renameThread,
    startThreadForWorkspace,
    listThreadsForWorkspaces,
    listThreadsForWorkspace,
    loadOlderThreadsForWorkspace,
    resetWorkspaceThreads,
    refreshThread,
    sendUserMessage,
    sendUserMessageToThread,
    forkMessage,
    startCompact,
    startReload,
    reloadCurrentSession,
    runtimeCommands,
  } = useThreads({
    activeWorkspace,
    onDebug: addDebugEntry,
    model: resolvedModel,
    effort: resolvedEffort,
    serviceTier: selectedServiceTier,
    steerEnabled: appSettings.steerEnabled,
    threadTitleAutogenerationEnabled: appSettings.threadTitleAutogenerationEnabled,
    chatHistoryScrollbackItems: appSettingsLoading
      ? null
      : appSettings.chatHistoryScrollbackItems,
    customPrompts: prompts,
    onMessageActivity: handleThreadMessageActivity,
    onDraftReloaded: refreshModels,
    threadSortKey: threadListSortKey,
    onThreadRunMetadataDetected: handleThreadRunMetadataDetected,
  });
  const modelSessionLoading = Boolean(
    activeThreadId && threadResumeLoadingById[activeThreadId],
  );
  useEffect(() => {
    selectCatalog(
      modelSessionLoading ? null : activeWorkspace?.id ?? null,
      activeThreadId,
    );
  }, [activeWorkspace?.id, activeThreadId, modelSessionLoading, selectCatalog]);

  const skills = runtimeCommands.filter((command) => command.name.startsWith("skill:"))
    .map((command) => ({ name: command.name.slice(6), description: command.description, path: "" }));

  useThreadModelConfiguration({
    workspaceId: activeWorkspace?.id ?? null,
    threadId: activeThreadId,
    model: resolvedModel,
    effort: resolvedEffort,
    isCatalogCurrent,
    isSessionLoading: modelSessionLoading,
    onDebug: addDebugEntry,
  });
  const { mobileThreadRefreshLoading, handleMobileThreadRefresh } =
    useMainAppMobileThreadRefresh({
      activeWorkspace,
      activeThreadId,
      startThreadForWorkspace,
      refreshThread,
    });
  const {
    updaterState,
    startUpdate,
    dismissUpdate,
    postUpdateNotice,
    dismissPostUpdateNotice,
    handleTestNotificationSound,
    handleTestSystemNotification,
  } = useUpdaterController({
    enabled: updaterEnabled,
    autoCheckOnMount:
      !appSettingsLoading && appSettings.automaticAppUpdateChecksEnabled,
    notificationSoundsEnabled: appSettings.notificationSoundsEnabled,
    systemNotificationsEnabled: appSettings.systemNotificationsEnabled,
    subagentSystemNotificationsEnabled:
      appSettings.subagentSystemNotificationsEnabled,
    isSubagentThread,
    getWorkspaceName,
    onThreadNotificationSent: (workspaceId, threadId) =>
      recordPendingThreadLinkRef.current(workspaceId, threadId),
    onDebug: addDebugEntry,
    successSoundUrl,
    errorSoundUrl,
  });
  const gitState = useMainAppGitState({
    activeWorkspace,
    activeWorkspaceId,
    activeItems,
    activeTab,
    tabletTab,
    isCompact,
    isTablet,
    setActiveTab,
    appSettings: {
      preloadGitDiffs: appSettings.preloadGitDiffs,
      gitDiffIgnoreWhitespaceChanges: appSettings.gitDiffIgnoreWhitespaceChanges,
      splitChatDiffView: appSettings.splitChatDiffView,
    },
    addDebugEntry,
    updateWorkspaceSettings,
    commitMessageModelId,
  });
  const {
    activeWorkspaceRef,
    activeWorkspaceIdRef,
    queueGitStatusRefresh,
    alertError,
    centerMode,
    setCenterMode,
    selectedDiffPath,
    setSelectedDiffPath,
    gitPanelMode,
    setGitPanelMode,
    gitDiffViewStyle,
    setGitDiffViewStyle,
    filePanelMode,
    selectedPullRequest,
    setSelectedPullRequest,
    diffSource,
    setDiffSource,
    gitStatus,
    shouldLoadDiffs,
    activeDiffs,
    activeDiffLoading,
    activeDiffError,
    shouldLoadGitHubPanelData,
    handleGitIssuesChange,
    handleGitPullRequestsChange,
    handleGitPullRequestDiffsChange,
    handleGitPullRequestCommentsChange,
    refreshGitRemote,
    branches,
    currentBranch,
    isBranchSwitcherEnabled,
    handleCheckoutBranch,
    handleCreateGitHubRepo,
    createGitHubRepoLoading,
    handleInitGitRepo,
    initGitRepoLoading,
  } = gitState;
  queueGitStatusRefreshRef.current = queueGitStatusRefresh;
  const { isExpanded: composerEditorExpanded, toggleExpanded: toggleComposerEditorExpanded } =
    useComposerEditorState();

  const composerEditorSettings = useMemo<ComposerEditorSettings>(
    () => ({
      preset: appSettings.composerEditorPreset,
      expandFenceOnSpace: appSettings.composerFenceExpandOnSpace,
      expandFenceOnEnter: appSettings.composerFenceExpandOnEnter,
      fenceLanguageTags: appSettings.composerFenceLanguageTags,
      fenceWrapSelection: appSettings.composerFenceWrapSelection,
      autoWrapPasteMultiline: appSettings.composerFenceAutoWrapPasteMultiline,
      autoWrapPasteCodeLike: appSettings.composerFenceAutoWrapPasteCodeLike,
      continueListOnShiftEnter: appSettings.composerListContinuation,
    }),
    [
      appSettings.composerEditorPreset,
      appSettings.composerFenceExpandOnSpace,
      appSettings.composerFenceExpandOnEnter,
      appSettings.composerFenceLanguageTags,
      appSettings.composerFenceWrapSelection,
      appSettings.composerFenceAutoWrapPasteMultiline,
      appSettings.composerFenceAutoWrapPasteCodeLike,
      appSettings.composerListContinuation,
    ],
  );

  useThreadRunSyncOrchestration({
    activeWorkspaceId,
    activeThreadId,
    appSettings: {
      lastComposerModelId: appSettings.lastComposerModelId,
      lastComposerReasoningEffort: appSettings.lastComposerReasoningEffort,
    },
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
  });

  const { handleSetThreadListSortKey, handleRefreshAllWorkspaceThreads } =
    useThreadListActions({
      threadListSortKey,
      setThreadListSortKey,
      workspaces,
      refreshWorkspaces,
      listThreadsForWorkspaces,
      resetWorkspaceThreads,
    });

  const {
    newAgentDraftWorkspaceId,
    startingDraftThreadWorkspaceId,
    isDraftModeForActiveWorkspace: isNewAgentDraftMode,
    startNewAgentDraft,
    clearDraftState,
    clearDraftStateIfDifferentWorkspace,
    runWithDraftStart,
  } = useNewAgentDraft({
    activeWorkspace,
    activeWorkspaceId,
    activeThreadId,
  });
  const { getThreadRows } = useThreadRows(threadParentById);

  useTrayRecentThreads({
    workspaces,
    threadsByWorkspace,
    isSubagentThread,
  });

  useAutoExitEmptyDiff({
    centerMode,
    autoExitEnabled: diffSource === "local",
    activeDiffCount: activeDiffs.length,
    activeDiffLoading,
    activeDiffError,
    activeThreadId,
    isCompact,
    setCenterMode,
    setSelectedDiffPath,
    setActiveTab,
  });

  const { handleCopyThread } = useCopyThread({
    activeItems,
    onDebug: addDebugEntry,
  });

  const {
    renamePrompt: renameWorktreePrompt,
    notice: renameWorktreeNotice,
    upstreamPrompt: renameWorktreeUpstreamPrompt,
    confirmUpstream: confirmRenameWorktreeUpstream,
    openRenamePrompt: openRenameWorktreePrompt,
    handleRenameChange: handleRenameWorktreeChange,
    handleRenameCancel: handleRenameWorktreeCancel,
    handleRenameConfirm: handleRenameWorktreeConfirm,
  } = useRenameWorktreePrompt({
    workspaces,
    activeWorkspaceId,
    renameWorktree,
    renameWorktreeUpstream,
    onRenameSuccess: (workspace) => {
      resetWorkspaceThreads(workspace.id);
      void listThreadsForWorkspace(workspace);
      if (activeThreadId && activeWorkspaceId === workspace.id) {
        void refreshThread(workspace.id, activeThreadId);
      }
    },
  });

  const handleOpenRenameWorktree = useCallback(() => {
    if (activeWorkspace) {
      openRenameWorktreePrompt(activeWorkspace.id);
    }
  }, [activeWorkspace, openRenameWorktreePrompt]);

  const {
    terminalTabs,
    activeTerminalId,
    onSelectTerminal,
    onNewTerminal,
    onCloseTerminal,
    terminalState,
    ensureTerminalWithTitle,
    restartTerminalSession,
    requestTerminalFocus,
  } = useTerminalController({
    activeWorkspaceId,
    activeWorkspace,
    terminalOpen,
    onCloseTerminalPanel: closeTerminalPanel,
    onDebug: addDebugEntry,
  });

  const ensureLaunchTerminal = useCallback(
    (workspaceId: string) => ensureTerminalWithTitle(workspaceId, "launch", "Launch"),
    [ensureTerminalWithTitle],
  );

  const openTerminalWithFocus = useCallback(() => {
    if (!activeWorkspaceId) {
      return;
    }
    requestTerminalFocus();
    openTerminal();
  }, [activeWorkspaceId, openTerminal, requestTerminalFocus]);

  const handleToggleTerminalWithFocus = useCallback(() => {
    if (!activeWorkspaceId) {
      return;
    }
    if (!terminalOpen) {
      requestTerminalFocus();
    }
    handleToggleTerminal();
  }, [
    activeWorkspaceId,
    handleToggleTerminal,
    requestTerminalFocus,
    terminalOpen,
  ]);

  const launchScriptState = useWorkspaceLaunchScript({
    activeWorkspace,
    updateWorkspaceSettings,
    openTerminal: openTerminalWithFocus,
    ensureLaunchTerminal,
    restartLaunchSession: restartTerminalSession,
    terminalState,
    activeTerminalId,
  });

  const launchScriptsState = useWorkspaceLaunchScripts({
    activeWorkspace,
    updateWorkspaceSettings,
    openTerminal: openTerminalWithFocus,
    ensureLaunchTerminal: (workspaceId, entry, title) => {
      const label = entry.label?.trim() || entry.icon;
      return ensureTerminalWithTitle(
        workspaceId,
        `launch:${entry.id}`,
        title || `Launch ${label}`,
      );
    },
    restartLaunchSession: restartTerminalSession,
    terminalState,
    activeTerminalId,
  });

  const worktreeSetupScriptState = useWorktreeSetupScript({
    ensureTerminalWithTitle,
    restartTerminalSession,
    openTerminal,
    onDebug: addDebugEntry,
  });

  const handleWorktreeCreated = useCallback(
    async (worktree: WorkspaceInfo, _parentWorkspace?: WorkspaceInfo) => {
      await worktreeSetupScriptState.maybeRunWorktreeSetupScript(worktree);
    },
    [worktreeSetupScriptState],
  );

  const { exitDiffView, selectWorkspace, selectHome } = useWorkspaceSelection({
    workspaces,
    isCompact,
    activeWorkspaceId,
    setActiveTab,
    setActiveWorkspaceId,
    updateWorkspaceSettings,
    setCenterMode,
    setSelectedDiffPath,
  });

  const resolveCloneProjectContext = useCallback(
    (workspace: WorkspaceInfo) => {
      const groupId = workspace.settings.groupId ?? null;
      const group = groupId
        ? appSettings.workspaceGroups.find((entry) => entry.id === groupId)
        : null;
      return {
        groupId,
        copiesFolder: group?.copiesFolder ?? null,
      };
    },
    [appSettings.workspaceGroups],
  );

  const { handleMoveWorkspace } = useWorkspaceOrderingOrchestration({
    workspaces,
    workspacesById,
    updateWorkspaceSettings,
  });

  const {
    handleSelectOpenAppId,
    handleToggleAutomaticAppUpdateChecks,
    persistProjectCopiesFolder,
  } = useMainAppSettingsActions({
    appSettings,
    setAppSettings,
    queueSaveSettings,
  });

  const openAppIconById = useOpenAppIcons(appSettings.openAppTargets);

  const {
    workspaceFromUrlPrompt,
    openWorkspaceFromUrlPrompt,
    closeWorkspaceFromUrlPrompt,
    chooseWorkspaceFromUrlDestinationPath,
    submitWorkspaceFromUrlPrompt,
    updateWorkspaceFromUrlUrl,
    updateWorkspaceFromUrlTargetFolderName,
    clearWorkspaceFromUrlDestinationPath,
    canSubmitWorkspaceFromUrlPrompt,
  } = useWorkspaceFromUrlPrompt({
    onSubmit: async (url, destinationPath, targetFolderName) => {
      await handleAddWorkspaceFromGitUrl(url, destinationPath, targetFolderName);
    },
  });

  const { appModalsProps, modalActions } = useMainAppModals({
    settingsViewComponent: SettingsView,
    workspaces,
    workspaceGroups,
    groupedWorkspaces,
    ungroupedLabel,
    activeWorkspace,
    setActiveWorkspaceId,
    branches,
    currentBranch,
    threadRename: {
      threadsByWorkspace,
      renameThread,
    },
    git: {
      checkoutBranch: handleCheckoutBranch,
      initGitRepo: handleInitGitRepo,
      createGitHubRepo: handleCreateGitHubRepo,
      refreshGitRemote,
      initGitRepoLoading,
      createGitHubRepoLoading,
    },
    workspacePrompts: {
      addWorktreeAgent,
      addCloneAgent,
      updateWorkspaceSettings,
      selectWorkspace,
      handleWorktreeCreated,
      resolveCloneProjectContext,
      persistProjectCopiesFolder,
      onCompactActivate: isCompact ? () => setActiveTab("chat") : undefined,
      onWorkspacePromptError: (message, kind) => {
        addDebugEntry({
          id: `${Date.now()}-client-add-${kind}-error`,
          timestamp: Date.now(),
          source: "error",
          label: `${kind}/add error`,
          payload: message,
        });
      },
      openWorkspaceFromUrlPrompt,
      workspaceFromUrl: {
        workspaceFromUrlPrompt,
        workspaceFromUrlCanSubmit: canSubmitWorkspaceFromUrlPrompt,
        onWorkspaceFromUrlPromptUrlChange: updateWorkspaceFromUrlUrl,
        onWorkspaceFromUrlPromptTargetFolderNameChange:
          updateWorkspaceFromUrlTargetFolderName,
        onWorkspaceFromUrlPromptChooseDestinationPath:
          chooseWorkspaceFromUrlDestinationPath,
        onWorkspaceFromUrlPromptClearDestinationPath:
          clearWorkspaceFromUrlDestinationPath,
        onWorkspaceFromUrlPromptCancel: closeWorkspaceFromUrlPrompt,
        onWorkspaceFromUrlPromptConfirm: submitWorkspaceFromUrlPrompt,
      },
    },
    settings: {
      pluginSession: activeWorkspace ? {
        workspaceId: activeWorkspace.id,
        workspaceName: activeWorkspace.name,
        threadId: activeThreadId,
        isProcessing: Boolean(activeThreadId && threadStatusById[activeThreadId]?.isProcessing),
        reload: reloadCurrentSession,
      } : undefined,
      handleMoveWorkspace,
      removeWorkspace,
      createWorkspaceGroup,
      renameWorkspaceGroup,
      moveWorkspaceGroup,
      deleteWorkspaceGroup,
      assignWorkspaceGroup,
      reduceTransparency,
      setReduceTransparency,
      appSettings,
      openAppIconById,
      queueSaveSettings,
      handleToggleAutomaticAppUpdateChecks,
      updateWorkspaceSettings,
      scaleShortcutTitle,
      scaleShortcutText,
      handleTestNotificationSound,
      handleTestSystemNotification,
      dictationModel,
    },
  });

  useBranchSwitcherShortcut({
    shortcut: appSettings.branchSwitcherShortcut,
    isEnabled: isBranchSwitcherEnabled,
    onTrigger: modalActions.openBranchSwitcher,
  });

  const handleRenameThread = useCallback(
    (workspaceId: string, threadId: string) => {
      modalActions.openRenamePrompt(workspaceId, threadId);
    },
    [modalActions],
  );

  const showHome = !activeWorkspace;
  const {
    latestAgentRuns,
    isLoadingLatestAgents,
  } = useWorkspaceInsightsOrchestration({
    workspaces,
    hasLoaded,
    threadsByWorkspace,
    lastAgentMessageByThread,
    threadStatusById,
    threadListLoadingByWorkspace,
    getWorkspaceGroupName,
  });

  const activeTokenUsage = activeThreadId
    ? tokenUsageByThread[activeThreadId] ?? null
    : null;
  const activePlan = activeThreadId
    ? planByThread[activeThreadId] ?? null
    : null;
  const hasActivePlan = Boolean(
    activePlan && (activePlan.steps.length > 0 || activePlan.explanation)
  );
  const composerWorkspaceState = useMainAppComposerWorkspaceState({
    view: {
      activeTab,
      tabletTab,
      centerMode,
      isCompact,
      isTablet,
      rightPanelCollapsed,
      filePanelMode,
    },
    workspace: {
      activeWorkspace,
      activeWorkspaceId,
      isNewAgentDraftMode,
      startingDraftThreadWorkspaceId,
      threadsByWorkspace,
    },
    thread: {
      activeThreadId,
      activeItems,
      threadStatusById,
      activeTurnIdByThread,
    },
    settings: {
      steerEnabled: appSettings.steerEnabled,
      followUpMessageBehavior: appSettings.followUpMessageBehavior,
      pauseQueuedMessagesWhenResponseRequired:
        appSettings.pauseQueuedMessagesWhenResponseRequired,
    },
    models: {
      models,
      selectedModelId,
      resolvedEffort,
      selectedServiceTier,
    },
    refs: {
      composerInputRef,
      workspaceHomeTextareaRef,
    },
    actions: {
      startThreadForWorkspace,
      sendUserMessage,
      sendUserMessageToThread,
      seedThreadRunParams: patchThreadRunParams,
      startCompact,
      startReload,
      addWorktreeAgent,
      handleWorktreeCreated,
      addDebugEntry,
    },
  });
  const {
    files,
    setFileAutocompleteActive,
    showWorkspaceHome,
    showComposer,
    canInterrupt,
    recentThreadInstances,
    recentThreadsUpdatedAt,
    removeImagesForThread,
    handleSend,
    setPrefillDraft,
    clearDraftForThread,
    workspaceHomeState,
    agentMdState,
  } = composerWorkspaceState;
  const {
    runs: workspaceRuns,
    draft: workspacePrompt,
    runMode: workspaceRunMode,
    modelSelections: workspaceModelSelections,
    error: workspaceRunError,
    isSubmitting: workspaceRunSubmitting,
    setDraft: setWorkspacePrompt,
    setRunMode: setWorkspaceRunMode,
    toggleModelSelection: toggleWorkspaceModelSelection,
    setModelCount: setWorkspaceModelCount,
    startRun: startWorkspaceRun,
  } = workspaceHomeState;
  const {
    content: agentMdContent,
    exists: agentMdExists,
    truncated: agentMdTruncated,
    isLoading: agentMdLoading,
    isSaving: agentMdSaving,
    error: agentMdError,
    isDirty: agentMdDirty,
    setContent: setAgentMdContent,
    refresh: refreshAgentMd,
    save: saveAgentMd,
  } = agentMdState;
  const promptActions = useMainAppPromptActions({
    activeWorkspace,
    startThreadForWorkspace,
    sendUserMessageToThread,
    alertError,
    createPrompt,
    updatePrompt,
    deletePrompt,
    movePrompt,
    getWorkspacePromptsDir,
    getGlobalPromptsDir,
  });
  const worktreeState = useMainAppWorktreeState({
    activeWorkspace,
    workspacesById,
    renameWorktreePrompt,
    renameWorktreeNotice,
    renameWorktreeUpstreamPrompt,
    confirmRenameWorktreeUpstream,
    handleOpenRenameWorktree,
    handleRenameWorktreeChange,
    handleRenameWorktreeCancel,
    handleRenameWorktreeConfirm,
  });
  const { baseWorkspaceRef } = worktreeState;

  useMainAppWorkspaceLifecycle({
    activeTab,
    isTablet,
    setActiveTab,
    workspaces,
    hasLoaded,
    listThreadsForWorkspaces,
  });

  const {
    handleAddWorkspace,
    handleAddWorkspaceFromGitUrl,
    handleAddAgent,
    handleAddWorktreeAgent,
    handleAddCloneAgent,
    dropTargetRef: workspaceDropTargetRef,
    isDragOver: isWorkspaceDropActive,
    handleDragOver: handleWorkspaceDragOver,
    handleDragEnter: handleWorkspaceDragEnter,
    handleDragLeave: handleWorkspaceDragLeave,
    handleDrop: handleWorkspaceDrop,
  } = useMainAppWorkspaceActions({
    workspaceActions: {
      isCompact,
      addWorkspace,
      addWorkspaceFromPath,
      addWorkspaceFromGitUrl,
      addWorkspacesFromPaths,
      setActiveThreadId,
      setActiveTab,
      exitDiffView,
      selectWorkspace,
      onStartNewAgentDraft: startNewAgentDraft,
      openWorktreePrompt: modalActions.openWorktreePrompt,
      openClonePrompt: modalActions.openClonePrompt,
      composerInputRef,
      onDebug: addDebugEntry,
    },
  });

  useInterruptShortcut({
    isEnabled: canInterrupt,
    shortcut: appSettings.interruptShortcut,
    onTrigger: () => {
      void interruptTurn();
    },
  });

  const {
    handleSelectPullRequest,
    resetPullRequestSelection,
  } = usePullRequestComposer({
    isCompact,
    setSelectedPullRequest,
    setDiffSource,
    setSelectedDiffPath,
    setCenterMode,
    setGitPanelMode,
    setPrefillDraft,
    setActiveTab,
  });

  const {
    handleComposerSendWithDraftStart,
    handleSelectWorkspaceInstance,
    handleOpenThreadLink,
    handleArchiveActiveThread,
  } = useThreadUiOrchestration({
    activeWorkspaceId,
    activeThreadId,
    selectedServiceTier,
    pendingNewThreadSeedRef,
    runWithDraftStart,
    handleComposerSend: handleSend,
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
  });

  const handleOpenThreadLinkFromExternal = useCallback(
    (workspaceId: string, threadId: string) => {
      setActiveTab("chat");
      handleOpenThreadLink(threadId, workspaceId);
    },
    [handleOpenThreadLink, setActiveTab],
  );

  const { recordPendingThreadLink, openThreadLinkOrQueue } =
    useSystemNotificationThreadLinks({
      hasLoadedWorkspaces: hasLoaded,
      workspacesById,
      refreshWorkspaces,
      openThreadLink: handleOpenThreadLinkFromExternal,
    });

  useTauriEvent(
    subscribeTrayOpenThread,
    ({ workspaceId, threadId }: { workspaceId: string; threadId: string }) => {
      openThreadLinkOrQueue(workspaceId, threadId);
    },
  );

  useEffect(() => {
    recordPendingThreadLinkRef.current = recordPendingThreadLink;
    return () => {
      recordPendingThreadLinkRef.current = () => {};
    };
  }, [recordPendingThreadLink]);

  const { handlePlanAccept, handlePlanSubmitChanges } = usePlanReadyActions({
    activeWorkspace,
    activeThreadId,
    sendUserMessageToThread,
  });

  const {
    showGitDetail,
    isThreadOpen,
    dropOverlayActive,
    dropOverlayText,
    appClassName,
    appStyle,
  } = useAppShellOrchestration({
    isCompact,
    isPhone,
    isTablet,
    sidebarCollapsed,
    rightPanelCollapsed,
    shouldReduceTransparency,
    isWorkspaceDropActive,
    centerMode,
    selectedDiffPath,
    showComposer,
    activeThreadId,
    sidebarWidth,
    chatDiffSplitPositionPercent,
    rightPanelWidth,
    planPanelHeight,
    terminalPanelHeight,
    debugPanelHeight,
    appSettings,
  });

  const sidebarMenuOrchestration = useMainAppSidebarMenuOrchestration({
    sidebarActions: {
      openSettings: modalActions.openSettings,
      resetPullRequestSelection,
      clearDraftState,
      clearDraftStateIfDifferentWorkspace,
      selectHome,
      exitDiffView,
      selectWorkspace,
      setActiveThreadId,
      workspacesById,
      updateWorkspaceSettings,
      removeThread,
      clearDraftForThread,
      removeImagesForThread,
      refreshThread,
      handleRenameThread,
      removeWorkspace,
      removeWorktree,
      loadOlderThreadsForWorkspace,
      listThreadsForWorkspace,
    },
    workspaceCycling: {
      workspaces,
      groupedWorkspaces,
      threadsByWorkspace,
      getThreadRows,
      getPinTimestamp,
      pinnedThreadsVersion,
      activeWorkspaceIdRef,
      activeThreadIdRef,
      exitDiffView,
      resetPullRequestSelection,
      selectWorkspace,
      setActiveThreadId,
    },
    appMenu: {
      activeWorkspaceRef,
      baseWorkspaceRef,
      onAddWorkspace: handleAddWorkspace,
      onAddWorkspaceFromUrl: openWorkspaceFromUrlPrompt,
      onAddAgent: handleAddAgent,
      onAddWorktreeAgent: handleAddWorktreeAgent,
      onAddCloneAgent: handleAddCloneAgent,
      onToggleDebug: handleDebugClick,
      onToggleTerminal: handleToggleTerminalWithFocus,
      sidebarCollapsed,
      rightPanelCollapsed,
      onExpandSidebar: expandSidebar,
      onCollapseSidebar: collapseSidebar,
      onExpandRightPanel: expandRightPanel,
      onCollapseRightPanel: collapseRightPanel,
    },
    appSettings,
    onDebug: addDebugEntry,
  });
  useArchiveShortcut({
    isEnabled: isThreadOpen,
    shortcut: appSettings.archiveThreadShortcut,
    onTrigger: handleArchiveActiveThread,
  });
  const showCompactChatThreadActions =
    Boolean(activeWorkspace) &&
    isCompact &&
    ((isPhone && activeTab === "chat") || (isTablet && tabletTab === "chat"));
  const showMobilePollingFetchStatus = false;
  const gitRootOverride = activeWorkspace?.settings.gitRoot;
  const hasGitRootOverride =
    typeof gitRootOverride === "string" && gitRootOverride.trim().length > 0;
  const showGitInitBanner =
    Boolean(activeWorkspace) && !hasGitRootOverride && isMissingRepo(gitStatus.error);
  const displayNodes = useMainAppDisplayNodes({
    showCompactChatThreadActions,
    handleMobileThreadRefresh,
    mobileThreadRefreshLoading,
    centerMode,
    gitDiffViewStyle,
    setGitDiffViewStyle,
    isCompact,
    rightPanelCollapsed,
    sidebarToggleProps,
    workspaceHomeProps: activeWorkspace
      ? {
          workspace: activeWorkspace,
          showGitInitBanner,
          initGitRepoLoading,
          onInitGitRepo: modalActions.openInitGitRepoPrompt,
          runs: workspaceRuns,
          recentThreadInstances,
          recentThreadsUpdatedAt,
          prompt: workspacePrompt,
          onPromptChange: setWorkspacePrompt,
          onStartRun: startWorkspaceRun,
          runMode: workspaceRunMode,
          onRunModeChange: setWorkspaceRunMode,
          models,
          selectedModelId,
          onSelectModel: setSelectedModelId,
          modelSelections: workspaceModelSelections,
          onToggleModel: toggleWorkspaceModelSelection,
          onModelCountChange: setWorkspaceModelCount,
          reasoningOptions,
          selectedEffort,
          onSelectEffort: setSelectedEffort,
          reasoningSupported,
          error: workspaceRunError,
          isSubmitting: workspaceRunSubmitting,
          activeWorkspaceId,
          activeThreadId,
          threadStatusById,
          onSelectInstance: handleSelectWorkspaceInstance,
          skills,
          runtimeCommands,
          prompts,
          files,
          onFileAutocompleteActiveChange: setFileAutocompleteActive,
          dictationEnabled: appSettings.dictationEnabled && dictationReady,
          dictationState,
          dictationLevel,
          onToggleDictation: handleToggleDictation,
          onCancelDictation: cancelDictation,
          onOpenDictationSettings: () => modalActions.openSettings("dictation"),
          dictationError,
          onDismissDictationError: clearDictationError,
          dictationHint,
          onDismissDictationHint: clearDictationHint,
          dictationTranscript,
          onDictationTranscriptHandled: clearDictationTranscript,
          textareaRef: workspaceHomeTextareaRef,
          agentMdContent,
          agentMdExists,
          agentMdTruncated,
          agentMdLoading,
          agentMdSaving,
          agentMdError,
          agentMdDirty,
          onAgentMdChange: setAgentMdContent,
          onAgentMdRefresh: () => {
            void refreshAgentMd();
          },
          onAgentMdSave: () => {
            void saveAgentMd();
          },
        }
      : null,
  });
  const { workspaceHomeNode } = displayNodes;
  const layoutSurfaces = useMainAppLayoutSurfaces({
    appSettings: {
      composerCodeBlockCopyUseModifier:
        appSettings.composerCodeBlockCopyUseModifier,
      showMessageFilePath: appSettings.showMessageFilePath,
      openAppTargets: appSettings.openAppTargets,
      selectedOpenAppId: appSettings.selectedOpenAppId,
      followUpMessageBehavior: appSettings.followUpMessageBehavior,
      dictationEnabled: appSettings.dictationEnabled,
      splitChatDiffView: appSettings.splitChatDiffView,
      gitDiffIgnoreWhitespaceChanges:
        appSettings.gitDiffIgnoreWhitespaceChanges,
    },
    workspaces,
    groupedWorkspaces,
    workspaceGroupsCount: workspaceGroups.length,
    deletingWorktreeIds,
    newAgentDraftWorkspaceId,
    startingDraftThreadWorkspaceId,
    threadsByWorkspace,
    threadParentById,
    threadStatusById,
    threadResumeLoadingById,
    threadListLoadingByWorkspace,
    threadListPagingByWorkspace,
    threadListCursorByWorkspace,
    pinnedThreadsVersion,
    threadListSortKey,
    onSetThreadListSortKey: handleSetThreadListSortKey,
    threadListOrganizeMode,
    onSetThreadListOrganizeMode: setThreadListOrganizeMode,
    onRefreshAllThreads: handleRefreshAllWorkspaceThreads,
    activeWorkspace,
    activeWorkspaceId,
    activeThreadId,
    activeThreadReadOnly:
      Boolean(activeWorkspaceId && activeThreadId) &&
      isSubagentThread(activeWorkspaceId ?? "", activeThreadId ?? ""),
    activeItems,
    onPlanAccept: handlePlanAccept,
    onPlanSubmitChanges: handlePlanSubmitChanges,
    activePlan,
    activeTokenUsage,
    latestAgentRuns,
    isLoadingLatestAgents,
    gitState,
    selectedServiceTier: selectedServiceTier ?? null,
    composerWorkspaceState,
    promptActions,
    worktreeState,
    sidebarHandlers: sidebarMenuOrchestration,
    displayNodes,
    threadPinning: {
      pinThread,
      unpinThread,
      isThreadPinned,
      getPinTimestamp,
    },
    workspaceDrop: {
      workspaceDropTargetRef,
      isWorkspaceDropActive: dropOverlayActive,
      workspaceDropText: dropOverlayText,
      onWorkspaceDragOver: handleWorkspaceDragOver,
      onWorkspaceDragEnter: handleWorkspaceDragEnter,
      onWorkspaceDragLeave: handleWorkspaceDragLeave,
      onWorkspaceDrop: handleWorkspaceDrop,
    },
    threadNavigation: {
      exitDiffView,
      clearDraftState,
      selectWorkspace,
      setActiveThreadId,
      resetPullRequestSelection,
      selectHome,
    },
    pullRequestComposer: {
      handleSelectPullRequest,
    },
    dictationUi: {
      onOpenDictationSettings: () => modalActions.openSettings('dictation'),
      dictationTranscript,
      dictationError,
      dictationHint,
    },
    openAppIconById,
    openInitGitRepoPrompt: modalActions.openInitGitRepoPrompt,
    handleAddWorkspace,
    openWorkspaceFromUrlPrompt,
    handleAddAgent,
    handleAddWorktreeAgent,
    handleAddCloneAgent,
    handleOpenThreadLink,
    onForkMessage: forkMessage,
    handleSelectOpenAppId,
    handleCopyThread,
    handleToggleTerminalWithFocus,
    launchScriptState,
    launchScriptsState,
    models,
    selectedModelId,
    onSelectModel: handleSelectModel,
    reasoningOptions,
    selectedEffort,
    onSelectEffort: handleSelectEffort,
    onSelectServiceTier: handleSelectServiceTier,
    reasoningSupported,
    skills,
    runtimeCommands,
    prompts,
    composerInputRef,
    composerEditorSettings,
    composerEditorExpanded,
    onToggleComposerEditorExpanded: toggleComposerEditorExpanded,
    dictationReady,
    dictationState,
    dictationLevel,
    onToggleDictation: handleToggleDictation,
    onCancelDictation: cancelDictation,
    clearDictationTranscript,
    clearDictationError,
    clearDictationHint,
    handleComposerSendWithDraftStart,
    interruptTurn,
    terminalOpen,
    debugOpen,
    debugEntries,
    terminalTabs,
    activeTerminalId,
    onSelectTerminal,
    onNewTerminal,
    onCloseTerminal,
    terminalState,
    onClearDebug: clearDebugEntries,
    onCopyDebug: handleCopyDebug,
    onResizeDebug: onDebugPanelResizeStart,
    onResizeTerminal: onTerminalPanelResizeStart,
    isCompact,
    isPhone,
    activeTab,
    setActiveTab,
    tabletTab,
    showMobilePollingFetchStatus,
    appModalsAboutOpen:
      appModalsProps.settingsOpen && appModalsProps.settingsSection === 'about',
    updaterState,
    startUpdate,
    dismissUpdate,
    postUpdateNotice,
    dismissPostUpdateNotice,
    errorToasts,
    dismissErrorToast,
    showDebugButton,
    handleDebugClick,
  });

  const {
    sidebarNode,
    messagesNode,
    composerNode,
    updateToastNode,
    errorToastsNode,
    homeNode,
    mainHeaderNode,
    desktopTopbarLeftNode,
    tabletNavNode,
    tabBarNode,
    gitDiffPanelNode,
    gitDiffViewerNode,
    planPanelNode,
    debugPanelNode,
    debugPanelFullNode,
    terminalDockNode,
    compactEmptyChatNode,
    compactEmptyGitNode,
    compactGitBackNode,
  } = useMainAppLayoutNodes(layoutSurfaces);

  const mainMessagesNode = showWorkspaceHome ? workspaceHomeNode : (
    <ThreadConversationsContext.Provider value={conversationSource}>
      {messagesNode}
    </ThreadConversationsContext.Provider>
  );
  const mainAppShellProps = useMainAppShellProps({
    shell: {
      appClassName,
      isResizing,
      appStyle,
      appRef,
      sidebarToggleProps,
      shouldLoadGitHubPanelData,
      appModalsProps,
    },
    gitHubPanelDataProps: {
      activeWorkspace,
      gitPanelMode,
      shouldLoadDiffs,
      diffSource,
      selectedPullRequestNumber: selectedPullRequest?.number ?? null,
      onIssuesChange: handleGitIssuesChange,
      onPullRequestsChange: handleGitPullRequestsChange,
      onPullRequestDiffsChange: handleGitPullRequestDiffsChange,
      onPullRequestCommentsChange: handleGitPullRequestCommentsChange,
    },
    appLayout: {
      isPhone,
      isTablet,
      showHome,
      showGitDetail,
      activeTab,
      tabletTab,
      centerMode,
      preloadGitDiffs: appSettings.preloadGitDiffs,
      splitChatDiffView: appSettings.splitChatDiffView,
      hasActivePlan: hasActivePlan,
      activeWorkspace: Boolean(activeWorkspace),
      sidebarNode,
      messagesNode: mainMessagesNode,
      composerNode,
      updateToastNode,
      errorToastsNode,
      homeNode,
      mainHeaderNode,
      tabletNavNode,
      tabBarNode,
      gitDiffPanelNode,
      gitDiffViewerNode,
      planPanelNode,
      debugPanelNode,
      debugPanelFullNode,
      terminalDockNode,
      compactEmptyChatNode,
      compactEmptyGitNode,
      compactGitBackNode,
      onSidebarResizeStart,
      onChatDiffSplitPositionResizeStart,
      onRightPanelResizeStart,
      onPlanPanelResizeStart,
    },
    topbar: {
      isCompact,
      desktopTopbarLeftNode,
    },
  });

  return <MainAppShell {...mainAppShellProps} />;
}
