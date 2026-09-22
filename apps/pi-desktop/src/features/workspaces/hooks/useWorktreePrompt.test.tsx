// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { WorkspaceInfo, WorkspaceSpec } from "../../../types";
import type { GitInventory } from "../../git/gitContext";
import { cancelWorktreePlan, listGitBranches, listGitCheckouts, prepareWorktreePlan, type WorktreePlanPreview } from "../../../services/tauri";
import { useWorktreePrompt } from "./useWorktreePrompt";

vi.mock("../../../services/tauri", () => ({
  cancelWorktreePlan: vi.fn(), listGitBranches: vi.fn(), listGitCheckouts: vi.fn(), prepareWorktreePlan: vi.fn(),
}));

const parentWorkspace: WorkspaceInfo = {
  id: "ws-1", name: "Parent", path: "/repo/app/src", kind: "main",
  settings: { sidebarCollapsed: false },
};
const source: WorkspaceSpec = {
  roots: [
    { id: "app", name: "App", path: "/repo/app/src", ownership: { kind: "external" } },
    { id: "api", name: "API", path: "/repo/api", ownership: { kind: "external" } },
    { id: "docs", name: "Docs", path: "/repo/docs", ownership: { kind: "external" } },
  ], primaryRoot: "app", executionDir: "/repo/app/src",
};
const inventory: GitInventory = {
  workspace: source,
  checkouts: [
    { key: "app-checkout", workdir: "/repo/app", gitDir: "/repo/app/.git", commonDir: "/repo/app/.git", rootIds: ["app"] },
    { key: "api-checkout", workdir: "/repo/api", gitDir: "/repo/api/.git", commonDir: "/repo/api/.git", rootIds: ["api"] },
  ], directoryRootIds: ["docs"], defaultCheckoutKey: "app-checkout", errors: [],
};
const preview: WorktreePlanPreview = {
  id: "plan-1", parentId: parentWorkspace.id, name: "Example", source,
  workspace: { ...source, roots: source.roots.map((root) => root.id === "app" ? {
    ...root, path: "/worktrees/example/app/src", ownership: { kind: "managedWorktree", sourceRoot: root.id },
  } : root), executionDir: "/worktrees/example/app/src" },
  checkouts: [{ sourceWorkdir: "/repo/app", destination: "/worktrees/example/app", branch: "pi/example", startOid: "a".repeat(40), rootIds: ["app"] }],
  warnings: [],
};
const worktreeWorkspace: WorkspaceInfo = {
  ...parentWorkspace, id: "wt-1", kind: "worktree", name: "Example", parentId: parentWorkspace.id,
  path: preview.workspace.executionDir, worktree: { branch: "pi/example", managed: true, checkoutCount: 1 },
};
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}
function setup() {
  const addWorktreeAgent = vi.fn().mockResolvedValue(worktreeWorkspace);
  const updateWorkspaceSettings = vi.fn().mockResolvedValue(parentWorkspace);
  const onSelectWorkspace = vi.fn();
  const hook = renderHook(() => useWorktreePrompt({ addWorktreeAgent, updateWorkspaceSettings, onSelectWorkspace }));
  return { ...hook, addWorktreeAgent, updateWorkspaceSettings, onSelectWorkspace };
}
beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(cancelWorktreePlan).mockResolvedValue();
  vi.mocked(listGitCheckouts).mockResolvedValue(inventory);
  vi.mocked(listGitBranches).mockResolvedValue([{ name: "main" }]);
  vi.mocked(prepareWorktreePlan).mockResolvedValue(preview);
});

describe("useWorktreePrompt", () => {
  it("derives branches from the name until they are manually edited", async () => {
    const { result, addWorktreeAgent } = setup();
    await act(async () => { result.current.openPrompt(parentWorkspace); });
    act(() => { result.current.updateName("My New Feature!"); });
    expect(result.current.worktreePrompt?.branch).toBe("pi/my-new-feature");
    act(() => { result.current.updateCheckout("api-checkout", { branch: "api/custom" }); });
    act(() => { result.current.updateName("Another Idea"); });
    expect(result.current.worktreePrompt?.checkouts.map((choice) => choice.branch)).toEqual(["pi/another-idea", "api/custom"]);
    act(() => { result.current.updateBranch("custom/branch-name"); });
    act(() => { result.current.updateName("Third Idea"); });
    expect(result.current.worktreePrompt?.branch).toBe("custom/branch-name");
    expect(addWorktreeAgent).not.toHaveBeenCalled();
  });

  it("does not override the generated branch when the name is cleared", async () => {
    const { result } = setup();
    await act(async () => { result.current.openPrompt(parentWorkspace); });
    const originalBranch = result.current.worktreePrompt?.branch;
    act(() => { result.current.updateName("  "); });
    expect(result.current.worktreePrompt?.branch).toBe(originalBranch);
  });

  it("reviews selected checkouts with independent refs and an explicit execution root", async () => {
    const { result, addWorktreeAgent, onSelectWorkspace } = setup();
    await act(async () => { result.current.openPrompt(parentWorkspace, "thread-saved"); });
    expect(listGitCheckouts).toHaveBeenCalledWith(parentWorkspace.id, "thread-saved");
    expect(listGitBranches).toHaveBeenCalledWith({ workspaceId: parentWorkspace.id, threadId: "thread-saved",
      target: { kind: "checkout", key: "api-checkout", workdir: "/repo/api" } });
    act(() => {
      result.current.updateName("Example");
      result.current.updateCheckout("api-checkout", { branch: "api/example", startPoint: "release" });
      result.current.updateExecutionRoot("api");
      result.current.updateCopyAgentsMd(false);
    });
    await act(async () => { await result.current.confirmPrompt(); });
    expect(addWorktreeAgent).not.toHaveBeenCalled();
    await act(async () => { await result.current.reviewPrompt(); });
    expect(prepareWorktreePlan).toHaveBeenCalledWith({
      parentId: parentWorkspace.id, threadId: "thread-saved", name: "Example", copyAgentsMd: false, executionRootId: "api",
      checkouts: [
        { target: { kind: "checkout", key: "app-checkout", workdir: "/repo/app" }, branch: "pi/example", startPoint: "HEAD" },
        { target: { kind: "checkout", key: "api-checkout", workdir: "/repo/api" }, branch: "api/example", startPoint: "release" },
      ],
    });
    expect(result.current.worktreePrompt?.plan).toEqual(preview);
    await act(async () => { await result.current.confirmPrompt(); });
    expect(addWorktreeAgent).toHaveBeenCalledWith(parentWorkspace, "pi/example", { displayName: "Example", copyAgentsMd: false, planId: "plan-1" });
    expect(onSelectWorkspace).toHaveBeenCalledWith("wt-1");
  });

  it("omits unselected repositories and preserves cwd by default", async () => {
    const { result } = setup();
    await act(async () => { result.current.openPrompt(parentWorkspace); });
    act(() => { result.current.updateCheckout("api-checkout", { selected: false }); });
    await act(async () => { await result.current.reviewPrompt(); });
    expect(vi.mocked(prepareWorktreePlan).mock.calls[0][0]).toMatchObject({ executionRootId: null, checkouts: [{ target: { key: "app-checkout" } }] });
  });

  it("saves the setup script before native preparation captures it and invalidates edited scripts", async () => {
    const { result, updateWorkspaceSettings } = setup();
    await act(async () => { result.current.openPrompt(parentWorkspace); });
    act(() => { result.current.updateSetupScript("pnpm install"); });
    await act(async () => { await result.current.reviewPrompt(); });
    expect(updateWorkspaceSettings).toHaveBeenCalledWith(parentWorkspace.id, {
      sidebarCollapsed: false, worktreeSetupScript: "pnpm install",
    });
    expect(updateWorkspaceSettings.mock.invocationCallOrder[0]).toBeLessThan(vi.mocked(prepareWorktreePlan).mock.invocationCallOrder[0]);
    act(() => { result.current.updateSetupScript("cargo build"); });
    expect(result.current.worktreePrompt?.plan).toBeNull();
    expect(cancelWorktreePlan).toHaveBeenCalledWith("plan-1");
  });

  it("discards late previews after the draft changes and never executes a stale plan", async () => {
    const pending = deferred<WorktreePlanPreview>();
    vi.mocked(prepareWorktreePlan).mockReturnValue(pending.promise);
    const { result, addWorktreeAgent } = setup();
    await act(async () => { result.current.openPrompt(parentWorkspace); });
    let reviewing!: Promise<void>;
    act(() => { reviewing = result.current.reviewPrompt(); });
    act(() => { result.current.updateCheckout("api-checkout", { selected: false }); });
    await act(async () => { pending.resolve(preview); await reviewing; });
    expect(result.current.worktreePrompt?.plan).toBeNull();
    expect(cancelWorktreePlan).toHaveBeenCalledWith(preview.id);
    await act(async () => { await result.current.confirmPrompt(); });
    expect(addWorktreeAgent).not.toHaveBeenCalled();
  });

  it("ignores inventory from an earlier visit to the same workspace", async () => {
    const pending = deferred<GitInventory>();
    vi.mocked(listGitCheckouts).mockReturnValueOnce(pending.promise);
    const { result } = setup();
    act(() => { result.current.openPrompt(parentWorkspace); });
    act(() => { result.current.cancelPrompt(); });
    await act(async () => { result.current.openPrompt(parentWorkspace); });
    await act(async () => { pending.resolve({ ...inventory, checkouts: [] }); });
    expect(result.current.worktreePrompt?.checkouts).toHaveLength(2);
  });

  it("cancels a prepared plan when its inputs change or the dialog closes", async () => {
    const { result } = setup();
    await act(async () => { result.current.openPrompt(parentWorkspace); });
    await act(async () => { await result.current.reviewPrompt(); });
    act(() => { result.current.updateExecutionRoot("docs"); });
    expect(result.current.worktreePrompt?.plan).toBeNull();
    expect(cancelWorktreePlan).toHaveBeenCalledWith("plan-1");
    await act(async () => { await result.current.reviewPrompt(); });
    act(() => { result.current.cancelPrompt(); });
    expect(cancelWorktreePlan).toHaveBeenCalledTimes(2);
  });
});
