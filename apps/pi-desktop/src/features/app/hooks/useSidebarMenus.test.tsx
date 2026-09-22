/** @vitest-environment jsdom */
import type { MouseEvent as ReactMouseEvent } from "react";
import { renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { WorkspaceInfo } from "../../../types";
import { useSidebarMenus } from "./useSidebarMenus";
import { fileManagerName } from "../../../utils/platformPaths";

const menuNew = vi.hoisted(() =>
  vi.fn(async ({ items }) => ({ popup: vi.fn(), items })),
);
const menuItemNew = vi.hoisted(() => vi.fn(async (options) => options));

vi.mock("@tauri-apps/api/menu", () => ({
  Menu: { new: menuNew },
  MenuItem: { new: menuItemNew },
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ scaleFactor: () => 1 }),
}));

vi.mock("@tauri-apps/api/dpi", () => ({
  LogicalPosition: class LogicalPosition {
    x: number;
    y: number;
    constructor(x: number, y: number) {
      this.x = x;
      this.y = y;
    }
  },
}));

const revealItemInDir = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/plugin-opener", () => ({
  revealItemInDir: (...args: unknown[]) => revealItemInDir(...args),
}));

vi.mock("../../../services/toasts", () => ({
  pushErrorToast: vi.fn(),
}));

describe("useSidebarMenus", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it.each(["workspace", "clone"])("edits the clicked %s even when another context menu is opened", async (kind) => {
    const onEditWorkspace = vi.fn();
    const onDeleteWorkspace = vi.fn();
    const onReloadWorkspaceThreads = vi.fn();
    const { result } = renderHook(() => useSidebarMenus({
      onAddAgent: vi.fn(),
      onAddWorktreeAgent: vi.fn(),
      onAddCloneAgent: vi.fn(),
      onDeleteThread: vi.fn(),
      onSyncThread: vi.fn(),
      onPinThread: vi.fn(),
      onUnpinThread: vi.fn(),
      isThreadPinned: vi.fn(() => false),
      onRenameThread: vi.fn(),
      onReloadWorkspaceThreads,
      onEditWorkspace,
      onDeleteWorkspace,
      onDeleteWorktree: vi.fn(),
    }));
    const event = {
      preventDefault: vi.fn(), stopPropagation: vi.fn(), clientX: 12, clientY: 34,
    } as unknown as ReactMouseEvent;
    const target: WorkspaceInfo = {
      id: "clicked-workspace", name: "Clicked", path: "/tmp/clicked",
      settings: { sidebarCollapsed: false },
    };
    if (kind === "clone") {
      await result.current.showCloneMenu(event, target);
    } else {
      await result.current.showWorkspaceMenu(event, target);
    }
    const menuArgs = menuNew.mock.calls[0]?.[0];
    const editItem = menuArgs.items.find(
      (item: { text: string }) => item.text === "Edit workspace…",
    );
    expect(editItem).toBeDefined();
    expect(onEditWorkspace).not.toHaveBeenCalled();
    expect(event.preventDefault).toHaveBeenCalledTimes(1);
    expect(event.stopPropagation).toHaveBeenCalledTimes(1);

    await result.current.showWorkspaceMenu(event, { ...target, id: "another-workspace" });
    await editItem.action();
    expect(onEditWorkspace).toHaveBeenCalledExactlyOnceWith("clicked-workspace");
    expect(onDeleteWorkspace).not.toHaveBeenCalled();
    expect(onReloadWorkspaceThreads).not.toHaveBeenCalled();
  });

  it("routes hover creation and management actions to the workspace they describe", async () => {
    const onAddAgent = vi.fn();
    const onAddWorktreeAgent = vi.fn();
    const onAddCloneAgent = vi.fn();
    const onEditWorkspace = vi.fn();
    const onReloadWorkspaceThreads = vi.fn();
    const onDeleteWorkspace = vi.fn();
    const { result } = renderHook(() => useSidebarMenus({
      onAddAgent,
      onAddWorktreeAgent,
      onAddCloneAgent,
      onDeleteThread: vi.fn(),
      onSyncThread: vi.fn(),
      onPinThread: vi.fn(),
      onUnpinThread: vi.fn(),
      isThreadPinned: vi.fn(() => false),
      onRenameThread: vi.fn(),
      onReloadWorkspaceThreads,
      onEditWorkspace,
      onDeleteWorkspace,
      onDeleteWorktree: vi.fn(),
    }));
    const workspace: WorkspaceInfo = {
      id: "workspace-1", name: "Project", path: "/tmp/project",
      settings: { sidebarCollapsed: false },
    };
    const actions = result.current.getWorkspaceActions(workspace);
    // Opening another panel must not redirect callbacks from the first one.
    result.current.getWorkspaceActions({ ...workspace, id: "workspace-2" });
    for (const id of ["new-agent", "new-worktree-agent", "new-clone-agent", "edit", "reload", "delete"]) {
      const action = actions.find((entry) => entry.id === id);
      expect(action).toBeDefined();
      await action?.onSelect();
    }

    expect(onAddAgent).toHaveBeenCalledExactlyOnceWith(workspace);
    expect(onAddWorktreeAgent).toHaveBeenCalledExactlyOnceWith(workspace);
    expect(onAddCloneAgent).toHaveBeenCalledExactlyOnceWith(workspace);
    expect(onEditWorkspace).toHaveBeenCalledExactlyOnceWith(workspace.id);
    expect(onReloadWorkspaceThreads).toHaveBeenCalledExactlyOnceWith(workspace.id);
    expect(onDeleteWorkspace).toHaveBeenCalledExactlyOnceWith(workspace.id);
  });

  it.each(["worktree", "clone"] as const)("routes hover delete for a %s through its existing handler", async (kind) => {
    const onDeleteWorkspace = vi.fn();
    const onDeleteWorktree = vi.fn();
    const onEditWorkspace = vi.fn();
    const { result } = renderHook(() => useSidebarMenus({
      onAddAgent: vi.fn(),
      onAddWorktreeAgent: vi.fn(),
      onAddCloneAgent: vi.fn(),
      onDeleteThread: vi.fn(),
      onSyncThread: vi.fn(),
      onPinThread: vi.fn(),
      onUnpinThread: vi.fn(),
      isThreadPinned: vi.fn(() => false),
      onRenameThread: vi.fn(),
      onReloadWorkspaceThreads: vi.fn(),
      onEditWorkspace,
      onDeleteWorkspace,
      onDeleteWorktree,
    }));
    const workspace: WorkspaceInfo = {
      id: "child-workspace", name: "Child", path: "/tmp/child",
      settings: { sidebarCollapsed: false },
    };
    const actions = kind === "worktree"
      ? result.current.getWorktreeActions(workspace)
      : result.current.getCloneActions(workspace);
    const deleteAction = actions.find((action) => action.id === "delete");
    expect(deleteAction).toBeDefined();
    await deleteAction?.onSelect();
    expect(kind === "worktree" ? onDeleteWorktree : onDeleteWorkspace)
      .toHaveBeenCalledExactlyOnceWith(workspace.id);
    expect(kind === "worktree" ? onDeleteWorkspace : onDeleteWorktree)
      .not.toHaveBeenCalled();
    expect(onEditWorkspace).not.toHaveBeenCalled();
    expect(actions.some((action) => action.id === "edit")).toBe(kind === "clone");
  });

  it("adds a show in file manager option for worktrees", async () => {
    const onDeleteThread = vi.fn();
    const onSyncThread = vi.fn();
    const onPinThread = vi.fn();
    const onUnpinThread = vi.fn();
    const isThreadPinned = vi.fn(() => false);
    const onRenameThread = vi.fn();
    const onReloadWorkspaceThreads = vi.fn();
    const onDeleteWorkspace = vi.fn();
    const onDeleteWorktree = vi.fn();

    const { result } = renderHook(() =>
      useSidebarMenus({
        onAddAgent: vi.fn(),
        onAddWorktreeAgent: vi.fn(),
        onAddCloneAgent: vi.fn(),
        onDeleteThread,
        onSyncThread,
        onPinThread,
        onUnpinThread,
        isThreadPinned,
        onRenameThread,
        onReloadWorkspaceThreads,
        onEditWorkspace: vi.fn(),
        onDeleteWorkspace,
        onDeleteWorktree,
      }),
    );

    const worktree: WorkspaceInfo = {
      id: "worktree-1",
      name: "feature/test",
      path: "/tmp/worktree-1",
      kind: "worktree",
      settings: {
        sidebarCollapsed: false,
        worktreeSetupScript: "",
      },
      worktree: { branch: "feature/test" },
    };

    const event = {
      preventDefault: vi.fn(),
      stopPropagation: vi.fn(),
      clientX: 12,
      clientY: 34,
    } as unknown as ReactMouseEvent;

    await result.current.showWorktreeMenu(event, worktree);

    const menuArgs = menuNew.mock.calls[0]?.[0];
    expect(menuArgs.items.some(
      (item: { text: string }) => item.text === "Edit workspace…",
    )).toBe(false);
    const revealItem = menuArgs.items.find(
      (item: { text: string }) => item.text === `Show in ${fileManagerName()}`,
    );

    expect(revealItem).toBeDefined();
    await revealItem.action();
    expect(revealItemInDir).toHaveBeenCalledWith("/tmp/worktree-1");
  });
});
