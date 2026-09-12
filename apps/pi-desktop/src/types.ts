export type WorkspaceSettings = {
  sidebarCollapsed: boolean;
  sortOrder?: number | null;
  groupId?: string | null;
  cloneSourceWorkspaceId?: string | null;
  gitRoot?: string | null;
  launchScript?: string | null;
  launchScripts?: LaunchScriptEntry[] | null;
  worktreeSetupScript?: string | null;
  worktreesFolder?: string | null;
};

export type LaunchScriptIconId =
  | "play"
  | "build"
  | "debug"
  | "wrench"
  | "terminal"
  | "code"
  | "server"
  | "database"
  | "package"
  | "test"
  | "lint"
  | "dev"
  | "git"
  | "config"
  | "logs";

export type LaunchScriptEntry = {
  id: string;
  script: string;
  icon: LaunchScriptIconId;
  label?: string | null;
};

export type WorkspaceGroup = {
  id: string;
  name: string;
  sortOrder?: number | null;
  copiesFolder?: string | null;
};

export type WorkspaceKind = "main" | "worktree";

export type WorktreeInfo = {
  branch: string;
};

export type WorkspaceInfo = {
  id: string;
  name: string;
  path: string;
  kind?: WorkspaceKind;
  parentId?: string | null;
  worktree?: WorktreeInfo | null;
  settings: WorkspaceSettings;
};

export type PiEvent = {
  workspace_id: string;
  message: Record<string, unknown>;
};

export type TrayRecentThreadEntry = {
  workspaceId: string;
  workspaceLabel: string;
  threadId: string;
  threadLabel: string;
  updatedAt: number;
};

export type TrayOpenThreadPayload = {
  workspaceId: string;
  threadId: string;
};

export type Message = {
  id: string;
  role: "user" | "assistant";
  text: string;
};

export type CollabAgentRef = {
  threadId: string;
  nickname?: string;
  role?: string;
};

export type CollabAgentStatus = CollabAgentRef & {
  status: string;
  totalTokens?: number;
};

export type ConversationItem =
  | {
      id: string;
      entryId?: string;
      kind: "message";
      role: "user" | "assistant";
      text: string;
      images?: string[];
    }
  | {
      id: string;
      kind: "userInput";
      status: "answered";
      questions: {
        id: string;
        header: string;
        question: string;
        answers: string[];
      }[];
    }
  | { id: string; kind: "reasoning"; summary: string; content: string }
  | { id: string; kind: "diff"; title: string; diff: string; status?: string }
  | {
      id: string;
      kind: "explore";
      status: "exploring" | "explored";
      entries: { kind: "read" | "search" | "list" | "run"; label: string; detail?: string }[];
    }
  | {
      id: string;
      kind: "tool";
      toolType: string;
      title: string;
      detail: string;
      images?: string[];
      status?: string;
      output?: string;
      durationMs?: number | null;
      changes?: { path: string; kind?: string; diff?: string }[];
      collabSender?: CollabAgentRef;
      collabReceiver?: CollabAgentRef;
      collabReceivers?: CollabAgentRef[];
      collabStatuses?: CollabAgentStatus[];
      collabTask?: string;
    };

export type SubagentContextOrigin = {
  mode: "fresh" | "fork";
  parentThreadId: string;
  parentEntryId: string | null;
  snapshotEntryId: string | null;
};

export type ThreadContextInheritance = {
  origin: SubagentContextOrigin;
  inheritedItems: ConversationItem[] | null;
};

export type ThreadSummary = {
  id: string;
  name: string;
  updatedAt: number;
  createdAt?: number;
  messageCount?: number;
  modelId?: string | null;
  effort?: string | null;
  isSubagent?: boolean;
  subagentNickname?: string | null;
  subagentRole?: string | null;
};

export type ThreadListSortKey = "created_at" | "updated_at";
export type ThreadListOrganizeMode =
  | "by_project"
  | "by_project_activity"
  | "threads_only";

export type ServiceTier = "fast" | "flex";
export type ThemePreference = "system" | "light" | "dark" | "dim";
export type UiLocale = "system" | "en" | "zh-CN";
export type FollowUpMessageBehavior = "queue" | "steer";
export type ComposerSendIntent = "default" | "queue" | "steer";
export type SendMessageResult = {
  status: "sent" | "completed" | "handled" | "queued" | "blocked" | "steer_failed";
};

export type ComposerEditorPreset = "default" | "helpful" | "smart";

export type ComposerEditorSettings = {
  preset: ComposerEditorPreset;
  expandFenceOnSpace: boolean;
  expandFenceOnEnter: boolean;
  fenceLanguageTags: boolean;
  fenceWrapSelection: boolean;
  autoWrapPasteMultiline: boolean;
  autoWrapPasteCodeLike: boolean;
  continueListOnShiftEnter: boolean;
};

export type OpenAppTarget = {
  id: string;
  label: string;
  kind: "app" | "command" | "finder";
  appName?: string | null;
  command?: string | null;
  args: string[];
};

export type AppSettings = {
  composerModelShortcut: string | null;
  composerReasoningShortcut: string | null;
  interruptShortcut: string | null;
  newAgentShortcut: string | null;
  newWorktreeAgentShortcut: string | null;
  newCloneAgentShortcut: string | null;
  archiveThreadShortcut: string | null;
  toggleProjectsSidebarShortcut: string | null;
  toggleGitSidebarShortcut: string | null;
  branchSwitcherShortcut: string | null;
  toggleDebugPanelShortcut: string | null;
  toggleTerminalShortcut: string | null;
  cycleAgentNextShortcut: string | null;
  cycleAgentPrevShortcut: string | null;
  cycleWorkspaceNextShortcut: string | null;
  cycleWorkspacePrevShortcut: string | null;
  lastComposerModelId: string | null;
  lastComposerReasoningEffort: string | null;
  uiScale: number;
  theme: ThemePreference;
  uiLocale: UiLocale;
  showMessageFilePath: boolean;
  chatHistoryScrollbackItems: number | null;
  threadTitleAutogenerationEnabled: boolean;
  automaticAppUpdateChecksEnabled: boolean;
  uiFontFamily: string;
  codeFontFamily: string;
  codeFontSize: number;
  notificationSoundsEnabled: boolean;
  systemNotificationsEnabled: boolean;
  subagentSystemNotificationsEnabled: boolean;
  splitChatDiffView: boolean;
  preloadGitDiffs: boolean;
  gitDiffIgnoreWhitespaceChanges: boolean;
  commitMessagePrompt: string;
  commitMessageModelId: string | null;
  steerEnabled: boolean;
  followUpMessageBehavior: FollowUpMessageBehavior;
  pauseQueuedMessagesWhenResponseRequired: boolean;
  dictationEnabled: boolean;
  dictationModelId: string;
  dictationPreferredLanguage: string | null;
  dictationHoldKey: string | null;
  composerEditorPreset: ComposerEditorPreset;
  composerFenceExpandOnSpace: boolean;
  composerFenceExpandOnEnter: boolean;
  composerFenceLanguageTags: boolean;
  composerFenceWrapSelection: boolean;
  composerFenceAutoWrapPasteMultiline: boolean;
  composerFenceAutoWrapPasteCodeLike: boolean;
  composerListContinuation: boolean;
  composerCodeBlockCopyUseModifier: boolean;
  workspaceGroups: WorkspaceGroup[];
  globalWorktreesFolder: string | null;
  openAppTargets: OpenAppTarget[];
  selectedOpenAppId: string;
};

export type GitFileStatus = {
  path: string;
  status: string;
  additions: number;
  deletions: number;
};

export type GitFileDiff = {
  path: string;
  diff: string;
  oldLines?: string[];
  newLines?: string[];
  isBinary?: boolean;
  isImage?: boolean;
  oldImageData?: string | null;
  newImageData?: string | null;
  oldImageMime?: string | null;
  newImageMime?: string | null;
};

export type GitCommitDiff = {
  path: string;
  status: string;
  diff: string;
  oldLines?: string[];
  newLines?: string[];
  isBinary?: boolean;
  isImage?: boolean;
  oldImageData?: string | null;
  newImageData?: string | null;
  oldImageMime?: string | null;
  newImageMime?: string | null;
};

export type GitLogEntry = {
  sha: string;
  summary: string;
  author: string;
  timestamp: number;
};

export type GitLogResponse = {
  total: number;
  entries: GitLogEntry[];
  ahead: number;
  behind: number;
  aheadEntries: GitLogEntry[];
  behindEntries: GitLogEntry[];
  upstream: string | null;
};

export type GitHubIssue = {
  number: number;
  title: string;
  url: string;
  updatedAt: string;
};

export type GitHubIssuesResponse = {
  total: number;
  issues: GitHubIssue[];
};

export type GitHubUser = {
  login: string;
};

export type GitHubPullRequest = {
  number: number;
  title: string;
  url: string;
  updatedAt: string;
  createdAt: string;
  body: string;
  headRefName: string;
  baseRefName: string;
  isDraft: boolean;
  author: GitHubUser | null;
};

export type GitHubPullRequestsResponse = {
  total: number;
  pullRequests: GitHubPullRequest[];
};

export type GitHubPullRequestDiff = {
  path: string;
  status: string;
  diff: string;
};

export type GitHubPullRequestComment = {
  id: number;
  body: string;
  createdAt: string;
  url: string;
  author: GitHubUser | null;
};

export type ThreadTokenUsage = {
  totalTokens: number | null;
  contextTokens: number | null;
  modelContextWindow: number | null;
};

export type TurnPlanStepStatus = "pending" | "inProgress" | "completed";

export type TurnPlanStep = {
  step: string;
  status: TurnPlanStepStatus;
};

export type TurnPlan = {
  turnId: string;
  explanation: string | null;
  steps: TurnPlanStep[];
};

export type QueuedMessage = {
  id: string;
  text: string;
  createdAt: number;
  images?: string[];
};

export type ModelOption = {
  id: string;
  model: string;
  displayName: string;
  description: string;
  supportedReasoningEfforts: { reasoningEffort: string; description: string }[];
  defaultReasoningEffort: string | null;
  isDefault: boolean;
};

export type SkillOption = {
  name: string;
  path: string;
  description?: string;
};

export type CustomPromptOption = {
  name: string;
  path: string;
  description?: string;
  argumentHint?: string;
  content: string;
  scope?: "workspace" | "global";
};

export type BranchInfo = {
  name: string;
  lastCommit: number;
};

export type DebugEntry = {
  id: string;
  timestamp: number;
  source: "client" | "server" | "event" | "stderr" | "error";
  label: string;
  payload?: unknown;
};

export type TerminalStatus = "idle" | "connecting" | "ready" | "error";

export type DictationModelState = "missing" | "downloading" | "ready" | "error";

export type DictationDownloadProgress = {
  totalBytes?: number | null;
  downloadedBytes: number;
};

export type DictationModelStatus = {
  state: DictationModelState;
  modelId: string;
  progress?: DictationDownloadProgress | null;
  error?: string | null;
  path?: string | null;
};

export type DictationSessionState = "idle" | "listening" | "processing";

export type DictationEvent =
  | { type: "state"; state: DictationSessionState }
  | { type: "level"; value: number }
  | { type: "transcript"; text: string }
  | { type: "error"; message: string }
  | { type: "canceled"; message: string };

export type DictationTranscript = {
  id: string;
  text: string;
};
