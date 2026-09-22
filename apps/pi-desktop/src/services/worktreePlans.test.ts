import { beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { ask } from "@tauri-apps/plugin-dialog";
import { cancelWorktreePlan, confirmWorktreeDiscard, executeWorktreePlan, listManagedWorktrees, prepareWorktreePlan, removeWorktree, type WorktreePlanRequest } from "./tauri";

vi.mock("@tauri-apps/api/core", async (importOriginal) => ({
  ...await importOriginal<typeof import("@tauri-apps/api/core")>(), invoke: vi.fn(),
}));
vi.mock("@tauri-apps/plugin-dialog", async (importOriginal) => ({
  ...await importOriginal<typeof import("@tauri-apps/plugin-dialog")>(), ask: vi.fn(),
}));
beforeEach(() => { vi.clearAllMocks(); vi.mocked(invoke).mockResolvedValue(undefined); });

it("preserves checkout and session targets across the worktree planning IPC boundary", async () => {
  const request: WorktreePlanRequest = {
    parentId: "project", threadId: "saved-session", name: "Feature", copyAgentsMd: true, executionRootId: "api",
    checkouts: [
      { target: { kind: "checkout", key: "app-git", workdir: "/repo/app" }, branch: "feature/app", startPoint: "main" },
      { target: { kind: "checkout", key: "api-git", workdir: "/repo/api" }, branch: "feature/api", startPoint: "release" },
    ],
  };
  await prepareWorktreePlan(request);
  expect(invoke).toHaveBeenCalledWith("prepare_worktree_plan", { request });
  await executeWorktreePlan("prepared-id");
  expect(invoke).toHaveBeenCalledWith("create_worktree_plan", { planId: "prepared-id" });
  await cancelWorktreePlan("unused-id");
  expect(invoke).toHaveBeenCalledWith("discard_worktree_plan", { planId: "unused-id" });
  await listManagedWorktrees();
  expect(invoke).toHaveBeenCalledWith("list_managed_worktrees");
  await removeWorktree("managed-id");
  expect(invoke).toHaveBeenCalledWith("remove_worktree", { id: "managed-id" });
  await removeWorktree("confirmed-id", true);
  expect(invoke).toHaveBeenCalledWith("remove_worktree", { id: "confirmed-id", force: true });
});

it("uses a native confirmation that names tracked, untracked, and ignored data loss", async () => {
  vi.mocked(ask).mockResolvedValue(false);
  expect(await confirmWorktreeDiscard("Feature group")).toBe(false);
  expect(ask).toHaveBeenCalledWith(
    expect.stringMatching(/tracked changes, untracked files, and ignored files.*Feature group/s),
    expect.objectContaining({ kind: "warning", okLabel: "Discard changes and clean up", cancelLabel: "Cancel" }),
  );
  expect(invoke).not.toHaveBeenCalled();
});
