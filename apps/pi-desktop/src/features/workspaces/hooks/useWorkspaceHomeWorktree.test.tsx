// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import type { ModelOption, WorkspaceInfo, WorkspaceSpec } from "../../../types";
import { cancelWorktreePlan, generateRunMetadata, listGitBranches, listGitCheckouts, prepareWorktreePlan, type WorktreePlanPreview } from "../../../services/tauri";
import { useWorkspaceHome } from "./useWorkspaceHome";
import { useWorktreePrompt } from "./useWorktreePrompt";

vi.mock("../../../services/tauri", () => ({
  cancelWorktreePlan: vi.fn(), generateRunMetadata: vi.fn(), listGitBranches: vi.fn(), listGitCheckouts: vi.fn(), prepareWorktreePlan: vi.fn(),
}));
const parent: WorkspaceInfo = { id: "project", name: "Project", path: "/repo/app", settings: { sidebarCollapsed: false } };
const child: WorkspaceInfo = { ...parent, id: "managed", kind: "worktree", path: "/worktree/app", worktree: { branch: "feat/reviewed", managed: true } };
const workspace: WorkspaceSpec = {
  roots: [
    { id: "app", name: "App", path: "/repo/app", ownership: { kind: "external" } },
    { id: "api", name: "API", path: "/repo/api", ownership: { kind: "external" } },
  ], primaryRoot: "app", executionDir: "/repo/app",
};
const model: ModelOption = {
  id: "provider/model", model: "model", displayName: "Model", description: "", supportedReasoningEfforts: [], defaultReasoningEffort: "medium", isDefault: true,
};
const plan: WorktreePlanPreview = {
  id: "reviewed", parentId: parent.id, name: "Reviewed", source: workspace, workspace,
  checkouts: [{ sourceWorkdir: "/repo/app", destination: "/worktree/app", branch: "feat/reviewed", startOid: "abc", rootIds: ["app"] }], warnings: [],
};
beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(cancelWorktreePlan).mockResolvedValue();
  vi.mocked(generateRunMetadata).mockResolvedValue({ title: "Feature", worktreeName: "feat/reviewed" });
  vi.mocked(listGitBranches).mockResolvedValue([]);
  vi.mocked(listGitCheckouts).mockResolvedValue({
    workspace, checkouts: workspace.roots.map((root) => ({ key: `${root.id}-git`, workdir: root.path, gitDir: `${root.path}/.git`, commonDir: `${root.path}/.git`, rootIds: [root.id] })),
    directoryRootIds: [], defaultCheckoutKey: "app-git", errors: [],
  });
  vi.mocked(prepareWorktreePlan).mockResolvedValue(plan);
});
function setup() {
  const addWorktreeAgent = vi.fn().mockResolvedValue(child);
  const startThreadForWorkspace = vi.fn().mockResolvedValue("thread");
  const sendUserMessageToThread = vi.fn().mockResolvedValue(undefined);
  const onSelectWorkspace = vi.fn();
  const onWorktreeCreated = vi.fn();
  const hook = renderHook(() => {
    const prompt = useWorktreePrompt({ addWorktreeAgent, onSelectWorkspace, onWorktreeCreated, updateWorkspaceSettings: vi.fn().mockResolvedValue(parent) });
    const home = useWorkspaceHome({ activeWorkspace: parent, models: [model], selectedModelId: null, effort: "high", serviceTier: "fast",
      requestWorktree: prompt.requestWorktree, startThreadForWorkspace, sendUserMessageToThread });
    return { prompt, home };
  });
  act(() => {
    hook.result.current.home.setRunMode("worktree");
    hook.result.current.home.toggleModelSelection(model.id);
    hook.result.current.home.setDraft("Keep this intent");
  });
  return { ...hook, addWorktreeAgent, startThreadForWorkspace, sendUserMessageToThread, onSelectWorkspace, onWorktreeCreated };
}

it("routes a home run through multi-repository review before creating and sends the captured intent", async () => {
  const { result, addWorktreeAgent, startThreadForWorkspace, sendUserMessageToThread, onSelectWorkspace, onWorktreeCreated } = setup();
  let running!: Promise<boolean>;
  await act(async () => { running = result.current.home.startRun(["image-1"]); });
  expect(result.current.home.draft).toBe("Keep this intent");
  expect(result.current.prompt.worktreePrompt?.checkouts).toHaveLength(2);
  expect(addWorktreeAgent).not.toHaveBeenCalled();
  expect(startThreadForWorkspace).not.toHaveBeenCalled();
  await act(async () => { await result.current.prompt.reviewPrompt(); });
  expect(vi.mocked(prepareWorktreePlan).mock.calls[0][0].checkouts).toHaveLength(2);
  expect(addWorktreeAgent).not.toHaveBeenCalled();
  await act(async () => { await result.current.prompt.confirmPrompt(); expect(await running).toBe(true); });
  expect(addWorktreeAgent).toHaveBeenCalledWith(parent, "feat/reviewed", expect.objectContaining({ planId: "reviewed", activate: false }));
  expect(sendUserMessageToThread).toHaveBeenCalledWith(child, "thread", "Keep this intent", ["image-1"], { model: model.id, effort: "high", serviceTier: "fast" });
  expect(onWorktreeCreated).toHaveBeenCalledTimes(1);
  expect(onSelectWorkspace).not.toHaveBeenCalled();
  expect(result.current.home.draft).toBe("");
});

it("cancels pending instances without losing the draft, attachments, or model selections", async () => {
  const { result, addWorktreeAgent, startThreadForWorkspace } = setup();
  act(() => { result.current.home.setModelCount(model.id, 2); });
  let running!: Promise<boolean>;
  await act(async () => { running = result.current.home.startRun(["image-1"]); });
  await act(async () => { result.current.prompt.cancelPrompt(); expect(await running).toBe(false); });
  expect(result.current.prompt.worktreePrompt).toBeNull();
  expect(result.current.home.draft).toBe("Keep this intent");
  expect(result.current.home.modelSelections).toEqual({ [model.id]: 2 });
  expect(result.current.home.runs).toEqual([]);
  expect(result.current.home.isSubmitting).toBe(false);
  expect(addWorktreeAgent).not.toHaveBeenCalled();
  expect(startThreadForWorkspace).not.toHaveBeenCalled();
});
