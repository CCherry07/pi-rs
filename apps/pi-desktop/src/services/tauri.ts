import { gitRequestArgs, type GitRequest, type GitInventory, type GitTarget } from "@/features/git/gitContext";
import { invoke } from "@tauri-apps/api/core";
import type { DesktopSessionRef } from "@pi-rs/desktop-sdk";
import type { DesktopExtensionCatalog } from "../features/extensions/types";

export async function getDesktopExtensions(workspaceId: string): Promise<DesktopExtensionCatalog> {
  const catalog = await invoke<Omit<DesktopExtensionCatalog, "extensions"> & {
    extensions: (DesktopExtensionCatalog["extensions"][number] & { project?: boolean })[];
  }>("pi_desktop_extensions", { workspaceId });
  return {
    ...catalog,
    extensions: catalog.extensions.map(source => ({
      ...source,
      scope: source.project ? "project" : "global",
    })),
  };
}

export type DesktopWidgetSnapshot = {
  widgets: Record<string, unknown>;
  versions: Record<string, number>;
  scopeToken?: string | null;
};

export function getDesktopWidgets(workspaceId: string, threadId: string): Promise<DesktopWidgetSnapshot> {
  return invoke("pi_get_desktop_widgets", { workspaceId, threadId });
}

export function observeDesktopSession(
  workspaceId: string,
  threadId: string,
  reference: DesktopSessionRef,
): Promise<{ thread: { id: string } }> {
  return invoke("pi_observe_desktop_session", { workspaceId, threadId, reference });
}

export async function runDesktopCommand(
  workspaceId: string,
  threadId: string,
  name: string,
  args: string,
  scopeToken: string,
): Promise<void> {
  await invoke("pi_desktop_command", { workspaceId, threadId, name, args, scopeToken });
}

export type McpScope = "global" | "project";
export type McpDocument = {
  path: string; content: string; revision: string; writable: boolean;
  projectTrusted: boolean; diagnostic: string | null;
  servers: { name: string; scope: McpScope; transport: string; enabled: boolean }[];
};
export function readMcpConfig(workspaceId: string | null, scope: McpScope): Promise<McpDocument> {
  return invoke("pi_mcp_read", { workspaceId, scope });
}
export function saveMcpConfig(workspaceId: string | null, scope: McpScope, revision: string, content: string): Promise<McpDocument> {
  return invoke("pi_mcp_save", { workspaceId, scope, revision, content });
}
export function testMcpConnection(workspaceId: string | null, name: string): Promise<string[]> {
  return invoke("pi_mcp_test", { workspaceId, name });
}
import { ask, open, save } from "@tauri-apps/plugin-dialog";
import type { Options as NotificationOptions } from "@tauri-apps/plugin-notification";
import i18n from "../i18n";
import type {
  AppSettings,
  DictationModelStatus,
  DictationSessionState,
  TrayRecentThreadEntry,
  WorkspaceInfo,
  WorkspaceSettings,
  WorkspaceSpec,
  WorkspaceRoot,
} from "../types";
import type {
  GitFileDiff,
  GitFileStatus,
  GitCommitDiff,
  GitHubIssuesResponse,
  GitHubPullRequestComment,
  GitHubPullRequestDiff,
  GitHubPullRequestsResponse,
  GitLogResponse,
} from "../types";

function isMissingTauriInvokeError(error: unknown) {
  return (
    error instanceof TypeError &&
    (error.message.includes("reading 'invoke'") ||
      error.message.includes("reading \"invoke\""))
  );
}

export async function pickWorkspacePath(): Promise<string | null> {
  const selection = await open({ directory: true, multiple: false });
  if (!selection || Array.isArray(selection)) {
    return null;
  }
  return selection;
}

export async function pickWorkspacePaths(): Promise<string[]> {
  const selection = await open({ directory: true, multiple: true });
  if (!selection) {
    return [];
  }
  return Array.isArray(selection) ? selection : [selection];
}

export async function pickImageFiles(): Promise<string[]> {
  const selection = await open({
    multiple: true,
    filters: [
      {
        name: "Images",
        extensions: [
          "png",
          "jpg",
          "jpeg",
          "gif",
          "webp",
          "bmp",
          "tiff",
          "tif",
          "heic",
          "heif",
        ],
      },
    ],
  });
  if (!selection) {
    return [];
  }
  return Array.isArray(selection) ? selection : [selection];
}

export async function exportMarkdownFile(
  content: string,
  defaultFileName = "plan.md",
): Promise<string | null> {
  const selection = await save({
    title: i18n.t("plan.exportDialogTitle", { ns: "messages" }),
    defaultPath: defaultFileName,
    filters: [
      {
        name: "Markdown",
        extensions: ["md"],
      },
    ],
  });
  if (!selection) {
    return null;
  }
  await invoke("write_text_file", { path: selection, content });
  return selection;
}

export async function listWorkspaces(): Promise<WorkspaceInfo[]> {
  try {
    return await invoke<WorkspaceInfo[]>("list_workspaces");
  } catch (error) {
    if (isMissingTauriInvokeError(error)) {
      // In non-Tauri environments (e.g., Electron/web previews), the invoke
      // bridge may be missing. Treat this as "no workspaces" instead of crashing.
      console.warn("Tauri invoke bridge unavailable; returning empty workspaces list.");
      return [];
    }
    throw error;
  }
}

export type TextFileResponse = {
  exists: boolean;
  content: string;
  truncated: boolean;
};

export type GlobalAgentsResponse = TextFileResponse;
export type AgentMdResponse = TextFileResponse;
type FileScope = "workspace" | "global";
type FileKind = "agents";

async function fileRead(
  scope: FileScope,
  kind: FileKind,
  workspaceId?: string,
): Promise<TextFileResponse> {
  return invoke<TextFileResponse>("file_read", { scope, kind, workspaceId });
}

async function fileWrite(
  scope: FileScope,
  kind: FileKind,
  content: string,
  workspaceId?: string,
): Promise<void> {
  return invoke("file_write", { scope, kind, workspaceId, content });
}

export async function readImageAsDataUrl(path: string): Promise<string> {
  return invoke<string>("read_image_as_data_url", { path });
}

export async function readGlobalAgentsMd(): Promise<GlobalAgentsResponse> {
  return fileRead("global", "agents");
}

export async function writeGlobalAgentsMd(content: string): Promise<void> {
  return fileWrite("global", "agents", content);
}

export async function addWorkspace(path: string): Promise<WorkspaceInfo> {
  return invoke<WorkspaceInfo>("add_workspace", { path });
}

export type CreateWorkspaceProjectInput = {
  name: string;
  paths: string[];
  primaryPath: string;
};

export async function createWorkspaceProject(input: CreateWorkspaceProjectInput): Promise<WorkspaceInfo> {
  return invoke<WorkspaceInfo>("create_workspace_project", { ...input });
}

export async function addWorkspaceFromGitUrl(
  url: string,
  destinationPath: string,
  targetFolderName: string | null,
): Promise<WorkspaceInfo> {
  return invoke<WorkspaceInfo>("add_workspace_from_git_url", {
    url,
    destinationPath,
    targetFolderName,
  });
}

export async function isWorkspacePathDir(path: string): Promise<boolean> {
  return invoke<boolean>("is_workspace_path_dir", { path });
}

export async function addClone(
  sourceWorkspaceId: string,
  copiesFolder: string,
  copyName: string,
): Promise<WorkspaceInfo> {
  return invoke<WorkspaceInfo>("add_clone", {
    sourceWorkspaceId,
    copiesFolder,
    copyName,
  });
}

export type WorktreePlanRequest = {
  parentId: string;
  threadId?: string | null;
  name: string;
  copyAgentsMd: boolean;
  executionRootId: string | null;
  checkouts: Array<{
    target: Extract<GitTarget, { kind: "checkout" }>;
    branch: string;
    startPoint: string;
  }>;
};

export type WorktreePlanPreview = {
  id: string;
  parentId: string;
  name: string;
  source: WorkspaceSpec;
  workspace: WorkspaceSpec;
  checkouts: Array<{
    sourceWorkdir: string;
    destination: string;
    branch: string;
    startOid: string;
    rootIds: string[];
  }>;
  warnings: string[];
};

export function prepareWorktreePlan(request: WorktreePlanRequest): Promise<WorktreePlanPreview> {
  return invoke("prepare_worktree_plan", { request });
}

function notifyWorktreeGroupsChanged() {
  if (typeof window !== "undefined") window.dispatchEvent(new Event("pi-worktree-groups-changed"));
}

export async function executeWorktreePlan(planId: string): Promise<WorkspaceInfo> {
  try {
    return await invoke("create_worktree_plan", { planId });
  } finally {
    notifyWorktreeGroupsChanged();
  }
}

export function cancelWorktreePlan(planId: string): Promise<void> {
  return invoke("discard_worktree_plan", { planId });
}

export type ManagedWorktreeSummary = {
  id: string;
  name: string;
  status: "prepared" | "creating" | "ready" | "cleanupRequired" | "removing";
  errors: string[];
  memberCount: number;
};

export function listManagedWorktrees(): Promise<ManagedWorktreeSummary[]> {
  return invoke("list_managed_worktrees");
}

export type DeliveryChange = { path: string; indexStatus: string; worktreeStatus: string };
export type DeliveryCheckout = {
  key: string;
  workdir: string;
  originWorkdir: string;
  rootIds: string[];
  createdBranch: string;
  startOid: string;
  head: { oid: string; branch: string | null } | null;
  changes: DeliveryChange[];
  changesTruncated: boolean;
  targetBranches: { name: string; oid: string }[];
  defaultTargetBranch: string | null;
  warnings: string[];
  error: string | null;
};
export type DeliveryOverview = {
  workspaceId: string;
  name: string;
  checkouts: DeliveryCheckout[];
  sharedRoots: WorkspaceRoot[];
  attempts: DeliveryAttempt[];
};
export type DeliveryExecutionRequest = {
  attemptId: string;
  checkoutKey: string;
  targetBranch: string;
  sourceOid: string;
  targetOid: string;
};
export type DeliveryAttempt = {
  id: string;
  checkoutKey: string;
  sourceOid: string;
  targetOid: string;
  targetBranch: string;
  resultOid: string | null;
  status: "preparing" | "applying" | "completed" | "unchanged" | "readyToFinish" | "needsAttention";
  createdAt: string;
  updatedAt: string;
  error: string | null;
};
export type MergeComparison = {
  sourceOid: string;
  targetOid: string;
  mergeBaseOids: string[];
  ahead: number;
  behind: number;
  kind: "upToDate" | "fastForward" | "mergeable" | "conflicts" | "unrelated" | "unsupported";
  commits: { oid: string; summary: string }[];
  commitsTruncated: boolean;
  files: { path: string; status: string; oldPath: string | null }[];
  filesTruncated: boolean;
  conflicts: string[];
  warnings: string[];
};
export type DeliveryPreview = {
  workspaceId: string;
  checkoutKey: string;
  sourceWorkdir: string;
  targetWorkdir: string;
  source: { oid: string; branch: string | null };
  target: { oid: string; branch: string };
  sourceChanges: DeliveryChange[];
  targetChanges: DeliveryChange[];
  sourceChangesTruncated: boolean;
  targetChangesTruncated: boolean;
  comparison: MergeComparison;
  blockers: string[];
  warnings: string[];
};

export function getWorktreeDelivery(workspaceId: string, threadId: string | null): Promise<DeliveryOverview> {
  return invoke("get_worktree_delivery", { workspaceId, threadId });
}

export function previewWorktreeDelivery(
  workspaceId: string,
  threadId: string | null,
  checkoutKey: string,
  targetBranch: string,
): Promise<DeliveryPreview> {
  return invoke("preview_worktree_delivery", { workspaceId, threadId, checkoutKey, targetBranch });
}

export function executeWorktreeDelivery(
  workspaceId: string, threadId: string | null, request: DeliveryExecutionRequest,
): Promise<DeliveryAttempt> {
  return invoke("execute_worktree_delivery", { workspaceId, threadId, request });
}

export function inspectWorktreeDeliveryAttempt(
  workspaceId: string, threadId: string | null, attemptId: string,
): Promise<DeliveryAttempt> {
  return invoke("inspect_worktree_delivery_attempt", { workspaceId, threadId, attemptId });
}

export function finishWorktreeDeliveryAttempt(
  workspaceId: string, threadId: string | null, attemptId: string,
): Promise<DeliveryAttempt> {
  return invoke("finish_worktree_delivery_attempt", { workspaceId, threadId, attemptId });
}

export type WorktreeSetupStatus = {
  shouldRun: boolean;
  script: string | null;
};

export async function getWorktreeSetupStatus(
  workspaceId: string,
): Promise<WorktreeSetupStatus> {
  return invoke<WorktreeSetupStatus>("worktree_setup_status", { workspaceId });
}

export async function markWorktreeSetupRan(workspaceId: string): Promise<void> {
  return invoke("worktree_setup_mark_ran", { workspaceId });
}

export async function updateWorkspaceSettings(
  id: string,
  settings: WorkspaceSettings,
): Promise<WorkspaceInfo> {
  return invoke<WorkspaceInfo>("update_workspace_settings", { id, settings });
}

export async function removeWorkspace(id: string): Promise<void> {
  return invoke("remove_workspace", { id });
}

export async function removeWorktree(id: string, force?: boolean): Promise<void> {
  try {
    return await invoke("remove_worktree", force === undefined ? { id } : { id, force });
  } finally {
    notifyWorktreeGroupsChanged();
  }
}

export function confirmWorktreeDiscard(name: string): Promise<boolean> {
  return ask(i18n.t("worktree.discardConfirm", { ns: "workspaces", name }), {
    title: i18n.t("worktree.discardTitle", { ns: "workspaces" }),
    kind: "warning",
    okLabel: i18n.t("worktree.discardAndCleanup", { ns: "workspaces" }),
    cancelLabel: i18n.t("actions.cancel", { ns: "common" }),
  });
}

export async function renameWorktree(
  id: string,
  branch: string,
): Promise<WorkspaceInfo> {
  return invoke<WorkspaceInfo>("rename_worktree", { id, branch });
}

export async function renameWorktreeUpstream(
  id: string,
  oldBranch: string,
  newBranch: string,
): Promise<void> {
  return invoke("rename_worktree_upstream", { id, oldBranch, newBranch });
}

export async function applyWorktreeChanges(workspaceId: string): Promise<void> {
  return invoke("apply_worktree_changes", { workspaceId });
}

export async function openWorkspaceIn(
  path: string,
  options: {
    appName?: string | null;
    command?: string | null;
    args?: string[];
    line?: number | null;
    column?: number | null;
  },
): Promise<void> {
  return invoke("open_workspace_in", {
    path,
    app: options.appName ?? null,
    command: options.command ?? null,
    args: options.args ?? [],
    line: options.line ?? null,
    column: options.column ?? null,
  });
}

export async function getOpenAppIcon(appName: string): Promise<string | null> {
  return invoke<string | null>("get_open_app_icon", { appName });
}

export async function startThread(workspaceId: string) {
  return invoke<any>("pi_start_thread", { workspaceId });
}

export async function prepareThread(workspaceId: string) {
  return invoke<{ thread: Record<string, unknown> }>("pi_start_thread", {
    workspaceId, prepareOnly: true,
  });
}

export async function forkThread(workspaceId: string, threadId: string, entryId: string) {
  return invoke<any>("pi_fork_thread", { workspaceId, threadId, entryId });
}

export async function compactThread(workspaceId: string, threadId: string) {
  return invoke<any>("pi_compact_thread", { workspaceId, threadId });
}

export async function reloadThread(workspaceId: string, threadId?: string | null) {
  return invoke<{ thread: Record<string, unknown> }>("pi_reload_thread", {
    workspaceId,
    threadId: threadId ?? null,
  });
}

function isInlineImageUrl(image: string) {
  return (
    image.startsWith("data:") ||
    image.startsWith("http://") ||
    image.startsWith("https://")
  );
}

async function convertImagesToDataUrls(images: string[]): Promise<string[]> {
  return Promise.all(
    images.map(async (image) => {
      if (isInlineImageUrl(image)) {
        return image;
      }
      return readImageAsDataUrl(image);
    }),
  );
}

async function normalizeImagesForRpc(images?: string[]): Promise<string[] | null> {
  if (images == null) {
    return null;
  }
  if (images.length === 0) {
    return [];
  }
  const hasPathImages = images.some((image) => !isInlineImageUrl(image));
  if (!hasPathImages) {
    return images;
  }
  try {
    return await convertImagesToDataUrls(images);
  } catch (error) {
    if (isMissingTauriInvokeError(error)) {
      return images;
    }
    throw error;
  }
}

export async function sendUserMessage(
  workspaceId: string,
  threadId: string,
  text: string,
  options?: {
    model?: string | null;
    effort?: string | null;
    serviceTier?: "fast" | "flex" | null | undefined;
    images?: string[];
  },
) {
  const images = await normalizeImagesForRpc(options?.images);
  const payload: Record<string, unknown> = {
    workspaceId,
    threadId,
    text,
    model: options?.model ?? null,
    effort: options?.effort ?? null,
    images,
  };
  if (options?.serviceTier !== undefined) {
    payload.serviceTier = options.serviceTier;
  }
  return invoke("pi_send_user_message", payload);
}

export async function interruptTurn(
  workspaceId: string,
  threadId: string,
  turnId: string,
) {
  return invoke("pi_turn_interrupt", { workspaceId, threadId, turnId });
}

export async function steerTurn(
  workspaceId: string,
  threadId: string,
  turnId: string,
  text: string,
  images?: string[],
) {
  const normalizedImages = await normalizeImagesForRpc(images);
  const payload: Record<string, unknown> = {
    workspaceId,
    threadId,
    turnId,
    text,
    images: normalizedImages,
  };
  return invoke("pi_turn_steer", payload);
}

export async function getGitStatus(request: GitRequest): Promise<{
  repoRoot?: string;
  branchName: string;
  files: GitFileStatus[];
  stagedFiles: GitFileStatus[];
  unstagedFiles: GitFileStatus[];
  totalAdditions: number;
  totalDeletions: number;
}> {
  return invoke("get_git_status", { ...gitRequestArgs(request) });
}

export type InitGitRepoResponse =
  | { status: "initialized"; commitError?: string; target?: GitTarget }
  | { status: "already_initialized"; target?: GitTarget }
  | { status: "needs_confirmation"; entryCount: number };

export async function initGitRepo(
  request: GitRequest,
  branch: string,
  force = false,
): Promise<InitGitRepoResponse> {
  return invoke<InitGitRepoResponse>("init_git_repo", { ...gitRequestArgs(request), branch, force });
}

export type CreateGitHubRepoResponse =
  | { status: "ok"; repo: string; remoteUrl?: string | null }
  | {
      status: "partial";
      repo: string;
      remoteUrl?: string | null;
      pushError?: string | null;
      defaultBranchError?: string | null;
    };

export async function createGitHubRepo(
  request: GitRequest,
  repo: string,
  visibility: "private" | "public",
  branch?: string | null,
): Promise<CreateGitHubRepoResponse> {
  return invoke<CreateGitHubRepoResponse>("create_github_repo", {
    ...gitRequestArgs(request),
    repo,
    visibility,
    branch,
  });
}

export async function getGitDiffs(
  request: GitRequest,
): Promise<GitFileDiff[]> {
  return invoke("get_git_diffs", { ...gitRequestArgs(request) });
}

export async function getGitLog(
  request: GitRequest,
  limit = 40,
): Promise<GitLogResponse> {
  return invoke("get_git_log", { ...gitRequestArgs(request), limit });
}

export async function getGitCommitDiff(
  request: GitRequest,
  sha: string,
): Promise<GitCommitDiff[]> {
  return invoke("get_git_commit_diff", { ...gitRequestArgs(request), sha });
}

export async function getGitRemote(request: GitRequest): Promise<string | null> {
  return invoke("get_git_remote", { ...gitRequestArgs(request) });
}

export async function stageGitFile(request: GitRequest, path: string) {
  return invoke("stage_git_file", { ...gitRequestArgs(request), path });
}

export async function stageGitAll(request: GitRequest): Promise<void> {
  return invoke("stage_git_all", { ...gitRequestArgs(request) });
}

export async function unstageGitFile(request: GitRequest, path: string) {
  return invoke("unstage_git_file", { ...gitRequestArgs(request), path });
}

export async function revertGitFile(request: GitRequest, path: string) {
  return invoke("revert_git_file", { ...gitRequestArgs(request), path });
}

export async function revertGitAll(request: GitRequest) {
  return invoke("revert_git_all", { ...gitRequestArgs(request) });
}

export async function commitGit(
  request: GitRequest,
  message: string,
): Promise<void> {
  return invoke("commit_git", { ...gitRequestArgs(request), message });
}

export async function pushGit(request: GitRequest): Promise<void> {
  return invoke("push_git", { ...gitRequestArgs(request) });
}

export async function pullGit(request: GitRequest): Promise<void> {
  return invoke("pull_git", { ...gitRequestArgs(request) });
}

export async function fetchGit(request: GitRequest): Promise<void> {
  return invoke("fetch_git", { ...gitRequestArgs(request) });
}

export async function syncGit(request: GitRequest): Promise<void> {
  return invoke("sync_git", { ...gitRequestArgs(request) });
}

export async function getGitHubIssues(
  request: GitRequest,
): Promise<GitHubIssuesResponse> {
  return invoke("get_github_issues", { ...gitRequestArgs(request) });
}

export async function getGitHubPullRequests(
  request: GitRequest,
): Promise<GitHubPullRequestsResponse> {
  return invoke("get_github_pull_requests", { ...gitRequestArgs(request) });
}

export async function getGitHubPullRequestDiff(
  request: GitRequest,
  prNumber: number,
): Promise<GitHubPullRequestDiff[]> {
  return invoke("get_github_pull_request_diff", {
    ...gitRequestArgs(request),
    prNumber,
  });
}

export async function getGitHubPullRequestComments(
  request: GitRequest,
  prNumber: number,
): Promise<GitHubPullRequestComment[]> {
  return invoke("get_github_pull_request_comments", {
    ...gitRequestArgs(request),
    prNumber,
  });
}

export async function checkoutGitHubPullRequest(
  request: GitRequest,
  prNumber: number,
): Promise<void> {
  return invoke("checkout_github_pull_request", {
    ...gitRequestArgs(request),
    prNumber,
  });
}

export async function getModelList(
  workspaceId: string,
  threadId: string | null = null,
) {
  return invoke<any>("pi_model_list", { workspaceId, threadId });
}

export async function configureThread(
  workspaceId: string,
  threadId: string,
  options: { model?: string | null; effort?: string | null },
) {
  return invoke("pi_configure_thread", {
    workspaceId,
    threadId,
    model: options.model ?? null,
    effort: options.effort ?? null,
  });
}

export async function generateRunMetadata(workspaceId: string, prompt: string) {
  return invoke<{ title: string; worktreeName: string }>("pi_generate_run_metadata", {
    workspaceId,
    prompt,
  });
}

export async function getSkillsList(workspaceId: string, threadId?: string) {
  return invoke<any>("pi_skills_list", { workspaceId, threadId });
}

export async function getPromptsList(workspaceId: string) {
  return invoke<any>("prompts_list", { workspaceId });
}

export async function getWorkspacePromptsDir(workspaceId: string) {
  return invoke<string>("prompts_workspace_dir", { workspaceId });
}

export async function getGlobalPromptsDir(workspaceId: string) {
  return invoke<string>("prompts_global_dir", { workspaceId });
}

export async function createPrompt(
  workspaceId: string,
  data: {
    scope: "workspace" | "global";
    name: string;
    description?: string | null;
    argumentHint?: string | null;
    content: string;
  },
) {
  return invoke<any>("prompts_create", {
    workspaceId,
    scope: data.scope,
    name: data.name,
    description: data.description ?? null,
    argumentHint: data.argumentHint ?? null,
    content: data.content,
  });
}

export async function updatePrompt(
  workspaceId: string,
  data: {
    path: string;
    name: string;
    description?: string | null;
    argumentHint?: string | null;
    content: string;
  },
) {
  return invoke<any>("prompts_update", {
    workspaceId,
    path: data.path,
    name: data.name,
    description: data.description ?? null,
    argumentHint: data.argumentHint ?? null,
    content: data.content,
  });
}

export async function deletePrompt(workspaceId: string, path: string) {
  return invoke<any>("prompts_delete", { workspaceId, path });
}

export async function movePrompt(
  workspaceId: string,
  data: { path: string; scope: "workspace" | "global" },
) {
  return invoke<any>("prompts_move", {
    workspaceId,
    path: data.path,
    scope: data.scope,
  });
}

export async function getAppSettings(): Promise<AppSettings> {
  return invoke<AppSettings>("get_app_settings");
}

export async function updateAppSettings(settings: AppSettings): Promise<AppSettings> {
  return invoke<AppSettings>("update_app_settings", { settings });
}

type MenuAcceleratorUpdate = {
  id: string;
  accelerator: string | null;
};

export async function setMenuAccelerators(
  updates: MenuAcceleratorUpdate[],
): Promise<void> {
  return invoke("menu_set_accelerators", { updates });
}

export async function setNativeUiLocale(locale: "en" | "zh-CN"): Promise<void> {
  try {
    await invoke("menu_set_locale", { locale });
  } catch (error) {
    if (isMissingTauriInvokeError(error)) {
      return;
    }
    throw error;
  }
}

export async function getWorkspaceFiles(workspaceId: string, threadId?: string | null) {
  return invoke<import("../types").WorkspaceFileListing>("list_workspace_files", { workspaceId, ...(threadId ? { threadId } : {}) });
}

export async function readWorkspaceFile(
  workspaceId: string,
  file: import("../types").WorkspaceFileRef,
  threadId?: string | null,
): Promise<{ content: string; truncated: boolean }> {
  return invoke<{ content: string; truncated: boolean }>("read_workspace_file", {
    ...(threadId ? { threadId } : {}),
    workspaceId,
    rootId: file.rootId,
    path: file.path,
  });
}

export async function readAgentMd(workspaceId: string): Promise<AgentMdResponse> {
  return fileRead("workspace", "agents", workspaceId);
}

export async function writeAgentMd(workspaceId: string, content: string): Promise<void> {
  return fileWrite("workspace", "agents", content, workspaceId);
}

export async function listGitBranches(request: GitRequest) {
  return invoke<any>("list_git_branches", { ...gitRequestArgs(request) });
}

export async function checkoutGitBranch(request: GitRequest, name: string) {
  return invoke("checkout_git_branch", { ...gitRequestArgs(request), name });
}

export async function createGitBranch(request: GitRequest, name: string) {
  return invoke("create_git_branch", { ...gitRequestArgs(request), name });
}

function withModelId(modelId?: string | null) {
  return modelId ? { modelId } : {};
}

export async function getDictationModelStatus(
  modelId?: string | null,
): Promise<DictationModelStatus> {
  return invoke<DictationModelStatus>(
    "dictation_model_status",
    withModelId(modelId),
  );
}

export async function downloadDictationModel(
  modelId?: string | null,
): Promise<DictationModelStatus> {
  return invoke<DictationModelStatus>(
    "dictation_download_model",
    withModelId(modelId),
  );
}

export async function cancelDictationDownload(
  modelId?: string | null,
): Promise<DictationModelStatus> {
  return invoke<DictationModelStatus>(
    "dictation_cancel_download",
    withModelId(modelId),
  );
}

export async function removeDictationModel(
  modelId?: string | null,
): Promise<DictationModelStatus> {
  return invoke<DictationModelStatus>(
    "dictation_remove_model",
    withModelId(modelId),
  );
}

export async function startDictation(
  preferredLanguage: string | null,
): Promise<DictationSessionState> {
  return invoke("dictation_start", { preferredLanguage });
}

export async function requestDictationPermission(): Promise<boolean> {
  return invoke("dictation_request_permission");
}

export async function stopDictation(): Promise<DictationSessionState> {
  return invoke("dictation_stop");
}

export async function cancelDictation(): Promise<DictationSessionState> {
  return invoke("dictation_cancel");
}

export async function openTerminalSession(
  workspaceId: string,
  terminalId: string,
  cols: number,
  rows: number,
  threadId?: string | null,
): Promise<{ id: string }> {
  return invoke("terminal_open", { workspaceId, terminalId, cols, rows, ...(threadId ? { threadId } : {}) });
}

export async function writeTerminalSession(
  workspaceId: string,
  terminalId: string,
  data: string,
): Promise<void> {
  return invoke("terminal_write", { workspaceId, terminalId, data });
}

export async function resizeTerminalSession(
  workspaceId: string,
  terminalId: string,
  cols: number,
  rows: number,
): Promise<void> {
  return invoke("terminal_resize", { workspaceId, terminalId, cols, rows });
}

export async function closeTerminalSession(
  workspaceId: string,
  terminalId: string,
): Promise<void> {
  return invoke("terminal_close", { workspaceId, terminalId });
}

export async function listThreads(
  workspaceId: string,
  cursor?: string | null,
  limit?: number | null,
  sortKey?: "created_at" | "updated_at" | null,
) {
  return invoke<any>("pi_list_threads", { workspaceId, cursor, limit, sortKey });
}

export async function listArchivedThreads(
  workspaceId: string,
  cursor?: string | null,
  limit?: number | null,
  sortKey?: "created_at" | "updated_at" | null,
) {
  return invoke<any>("pi_list_archived_threads", { workspaceId, cursor, limit, sortKey });
}

export async function resumeThread(workspaceId: string, threadId: string) {
  return invoke<any>("pi_resume_thread", { workspaceId, threadId });
}

export async function readThread(workspaceId: string, threadId: string) {
  return invoke<any>("pi_read_thread", { workspaceId, threadId });
}

export async function threadLiveSubscribe(workspaceId: string, threadId: string) {
  return invoke<any>("pi_thread_live_subscribe", { workspaceId, threadId });
}

export async function threadLiveUnsubscribe(workspaceId: string, threadId: string) {
  return invoke<any>("pi_thread_live_unsubscribe", { workspaceId, threadId });
}

export async function archiveThread(workspaceId: string, threadId: string) {
  return invoke<any>("pi_archive_thread", { workspaceId, threadId });
}

export async function unarchiveThread(workspaceId: string, threadId: string) {
  return invoke<any>("pi_unarchive_thread", { workspaceId, threadId });
}

export async function deleteThread(workspaceId: string, threadId: string) {
  return invoke<any>("pi_delete_thread", { workspaceId, threadId });
}

export async function setThreadName(
  workspaceId: string,
  threadId: string,
  name: string,
) {
  return invoke<any>("pi_set_thread_name", { workspaceId, threadId, name });
}

export async function setTrayRecentThreads(entries: TrayRecentThreadEntry[]) {
  return invoke<void>("set_tray_recent_threads", { entries });
}

export async function generateCommitMessage(
  request: GitRequest,
  commitMessageModelId: string | null,
): Promise<string> {
  return invoke("generate_commit_message", { ...gitRequestArgs(request), commitMessageModelId });
}

export type AppBuildType = "debug" | "release";

export async function getAppBuildType(): Promise<AppBuildType> {
  return invoke<AppBuildType>("app_build_type");
}

export async function sendNotification(
  title: string,
  body: string,
  options?: {
    id?: number;
    group?: string;
    actionTypeId?: string;
    sound?: string;
    autoCancel?: boolean;
    extra?: Record<string, unknown>;
  },
): Promise<void> {
  const macosDebugBuild = await invoke<boolean>("is_macos_debug_build").catch(
    () => false,
  );
  const attemptFallback = async () => {
    try {
      await invoke("send_notification_fallback", { title, body });
      return true;
    } catch (error) {
      console.warn("Notification fallback failed.", { error });
      return false;
    }
  };

  // In dev builds on macOS, the notification plugin can silently fail because
  // the process is not a bundled app. Prefer the native AppleScript fallback.
  if (macosDebugBuild) {
    await attemptFallback();
    return;
  }

  try {
    const notification = await import("@tauri-apps/plugin-notification");
    let permissionGranted = await notification.isPermissionGranted();
    if (!permissionGranted) {
      const permission = await notification.requestPermission();
      permissionGranted = permission === "granted";
      if (!permissionGranted) {
        console.warn("Notification permission not granted.", { permission });
        await attemptFallback();
        return;
      }
    }
    if (permissionGranted) {
      const payload: NotificationOptions = { title, body };
      if (options?.id !== undefined) {
        payload.id = options.id;
      }
      if (options?.group !== undefined) {
        payload.group = options.group;
      }
      if (options?.actionTypeId !== undefined) {
        payload.actionTypeId = options.actionTypeId;
      }
      if (options?.sound !== undefined) {
        payload.sound = options.sound;
      }
      if (options?.autoCancel !== undefined) {
        payload.autoCancel = options.autoCancel;
      }
      if (options?.extra !== undefined) {
        payload.extra = options.extra;
      }
      await notification.sendNotification(payload);
      return;
    }
  } catch (error) {
    console.warn("Notification plugin failed.", { error });
  }

  await attemptFallback();
}

export async function getWorkspaceProject(workspaceId: string): Promise<import("@/types").ProjectDefinition> {
  return invoke("get_workspace_project", { workspaceId });
}

export async function updateWorkspaceProject(project: import("@/types").ProjectDefinition): Promise<import("@/types").ProjectDefinition> {
  const result = await invoke<import("@/types").ProjectDefinition>("update_workspace_project", { project });
  window.dispatchEvent(new CustomEvent("pi-project-changed", { detail: project.id }));
  return result;
}

export async function listGitCheckouts(workspaceId: string, threadId: string | null, depth?: number): Promise<GitInventory> {
  return invoke("list_git_checkouts", { workspaceId, threadId, depth });
}
