// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import type { WorkspaceInfo } from "../../../types";
import { executeWorktreeDelivery, getWorktreeDelivery, previewWorktreeDelivery, type DeliveryAttempt, type DeliveryOverview, type DeliveryPreview } from "../../../services/tauri";
import { useMainAppGitState } from "./useMainAppGitState";

const refresh = vi.hoisted(() => ({ status: vi.fn(), diffs: vi.fn(), log: vi.fn(), branches: vi.fn(), select: vi.fn(), inventory: vi.fn(), reset: vi.fn() }));
vi.mock("../../../services/tauri", () => ({ getWorktreeDelivery: vi.fn(), previewWorktreeDelivery: vi.fn(), executeWorktreeDelivery: vi.fn(), inspectWorktreeDeliveryAttempt: vi.fn(), finishWorktreeDeliveryAttempt: vi.fn() }));
vi.mock("@/features/git/hooks/useGitCheckouts", () => ({ useGitCheckouts: (workspace: WorkspaceInfo) => ({
  gitWorkspace: { ...workspace, gitScope: "selected-api", gitWorkdir: "/managed/api" },
  options: [{ value: "api", label: "/managed/api" }], selected: "api", workdir: "/managed/api", select: refresh.select, refresh: refresh.inventory,
}) }));
vi.mock("@app/hooks/useGitPanelController", () => ({ useGitPanelController: () => ({
  gitStatus: { error: null, files: [], branchName: "feature" }, gitPanelMode: "diff", diffSource: "local", shouldLoadDiffs: false,
  refreshGitStatus: refresh.status, refreshGitDiffs: refresh.diffs, refreshGitLog: refresh.log,
}) }));
vi.mock("@app/hooks/useGitHubPanelController", () => ({ useGitHubPanelController: () => ({ resetGitHubPanelState: refresh.reset }) }));
vi.mock("@/features/git/hooks/useGitRemote", () => ({ useGitRemote: () => ({}) }));
vi.mock("@/features/git/hooks/useGitBranches", () => ({ useGitBranches: () => ({ branches: [], refreshBranches: refresh.branches }) }));
vi.mock("@/features/git/hooks/useGitActions", () => ({ useGitActions: () => ({}) }));
vi.mock("@app/hooks/useGitCommitController", () => ({ useGitCommitController: () => ({}) }));
vi.mock("@app/hooks/useSyncSelectedDiffPath", () => ({ useSyncSelectedDiffPath: () => {} }));

it("refreshes Git data after delivery without changing the main panel's selected repository", async () => {
  const workspace: WorkspaceInfo = { id: "managed", name: "Group", path: "/managed/app", kind: "worktree", worktree: { branch: "feature", managed: true }, settings: { sidebarCollapsed: false } };
  const overview: DeliveryOverview = { workspaceId: "managed", name: "Group", sharedRoots: [], attempts: [], checkouts: [{
    key: "app", workdir: "/managed/app", originWorkdir: "/original/app", rootIds: ["app"], createdBranch: "feature", startOid: "base",
    head: { oid: "source", branch: "feature" }, changes: [], changesTruncated: false, targetBranches: [{ name: "main", oid: "target" }],
    defaultTargetBranch: "main", error: null, warnings: [],
  }] };
  const preview: DeliveryPreview = { workspaceId: "managed", checkoutKey: "app", sourceWorkdir: "/managed/app", targetWorkdir: "/original/app",
    source: { oid: "source", branch: "feature" }, target: { oid: "target", branch: "main" }, sourceChanges: [], targetChanges: [],
    sourceChangesTruncated: false, targetChangesTruncated: false, blockers: [], warnings: [], comparison: {
      sourceOid: "source", targetOid: "target", mergeBaseOids: ["target"], ahead: 1, behind: 0, kind: "fastForward", commits: [], files: [], conflicts: [], warnings: [], commitsTruncated: false, filesTruncated: false,
    },
  };
  const attempt: DeliveryAttempt = { id: "attempt", checkoutKey: "app", sourceOid: "source", targetOid: "target", targetBranch: "main", resultOid: "source", status: "completed", createdAt: "now", updatedAt: "now", error: null };
  vi.mocked(getWorktreeDelivery).mockResolvedValue(overview);
  vi.mocked(previewWorktreeDelivery).mockResolvedValue(preview);
  vi.mocked(executeWorktreeDelivery).mockResolvedValue(attempt);
  const { result } = renderHook(() => useMainAppGitState({ activeWorkspace: workspace, activeThreadId: "session", activeItems: [], activeTab: "chat", tabletTab: "chat", isCompact: false, isTablet: false,
    setActiveTab: vi.fn(), appSettings: { preloadGitDiffs: false, gitDiffIgnoreWhitespaceChanges: false, splitChatDiffView: false }, addDebugEntry: vi.fn(), commitMessageModelId: null }));
  await act(async () => { result.current.worktreeDelivery.open(); });
  await act(async () => { await result.current.worktreeDelivery.preview(); });
  vi.mocked(getWorktreeDelivery).mockResolvedValue({ ...overview, attempts: [attempt] });
  await act(async () => { await result.current.worktreeDelivery.execute(); });
  expect(executeWorktreeDelivery).toHaveBeenCalledWith("managed", "session", expect.objectContaining({ checkoutKey: "app" }));
  for (const action of [refresh.status, refresh.diffs, refresh.log, refresh.branches]) expect(action).toHaveBeenCalledTimes(1);
  expect(refresh.select).not.toHaveBeenCalled();
  expect(refresh.inventory).not.toHaveBeenCalled();
  expect(result.current.repositories.selected).toBe("api");
  expect(result.current.worktreeDelivery.state?.checkoutKey).toBe("app");
});
