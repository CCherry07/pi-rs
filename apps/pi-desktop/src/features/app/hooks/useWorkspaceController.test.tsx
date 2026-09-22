// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ask, message } from "@tauri-apps/plugin-dialog";
import type { AppSettings, ProjectDefinition, WorkspaceInfo } from "../../../types";
import {
  addWorkspace,
  createWorkspaceProject,
  getWorkspaceProject,
  isWorkspacePathDir,
  listWorkspaces,
  pickWorkspacePaths,
  removeWorkspace,
  updateWorkspaceProject,
} from "../../../services/tauri";
import { useWorkspaceController } from "./useWorkspaceController";

vi.mock("@tauri-apps/plugin-dialog", () => ({
  ask: vi.fn(),
  message: vi.fn(),
}));

vi.mock("../../../services/tauri", () => ({
  addClone: vi.fn(),
  addWorkspace: vi.fn(),
  createWorkspaceProject: vi.fn(),
  getWorkspaceProject: vi.fn(),
  addWorkspaceFromGitUrl: vi.fn(),
  addWorktree: vi.fn(),
  isWorkspacePathDir: vi.fn(),
  listWorkspaces: vi.fn(),
  pickWorkspacePaths: vi.fn(),
  removeWorkspace: vi.fn(),
  removeWorktree: vi.fn(),
  renameWorktree: vi.fn(),
  renameWorktreeUpstream: vi.fn(),
  updateWorkspaceSettings: vi.fn(),
  updateWorkspaceProject: vi.fn(),
}));

const workspaceOne: WorkspaceInfo = {
  id: "ws-1",
  name: "workspace-one",
  path: "/tmp/ws-1",
  kind: "main",
  parentId: null,
  worktree: null,
  settings: { sidebarCollapsed: false, groupId: null },
};

const workspaceTwo: WorkspaceInfo = {
  id: "ws-2",
  name: "workspace-two",
  path: "/tmp/ws-2",
  kind: "main",
  parentId: null,
  worktree: null,
  settings: { sidebarCollapsed: false, groupId: null },
};

const baseAppSettings = {
  workspaceGroups: [],
} as unknown as AppSettings;

const projectTwo: ProjectDefinition = {
  id: workspaceTwo.id,
  name: "Existing project",
  roots: [
    { id: "frontend", name: "Frontend", path: workspaceOne.path, ownership: { kind: "external" } },
    { id: "backend", name: "Backend", path: workspaceTwo.path, ownership: { kind: "external" } },
  ],
  primaryRoot: "backend",
  executionDir: `${workspaceTwo.path}/src`,
};

function directoryId(prompt: ReturnType<typeof useWorkspaceController>["workspaceProjectPrompt"]["prompt"], path: string) {
  const root = prompt?.roots.find((root) => root.path === path);
  if (!root) throw new Error(`Missing directory: ${path}`);
  return root.id;
}

describe("useWorkspaceController dialogs", () => {
  beforeEach(() => {
    vi.resetAllMocks();
    window.localStorage.clear();
  });

  function renderController() {
    return renderHook(() => useWorkspaceController({
      appSettings: baseAppSettings,
      addDebugEntry: vi.fn(),
      queueSaveSettings: vi.fn(async (next) => next),
    }));
  }

  it("opens a draft panel and creates one project with every directory and the selected primary", async () => {
    vi.mocked(listWorkspaces).mockResolvedValue([]);
    vi.mocked(pickWorkspacePaths).mockResolvedValue([workspaceOne.path, workspaceTwo.path]);
    vi.mocked(createWorkspaceProject).mockResolvedValue(workspaceTwo);
    const { result } = renderController();
    await act(async () => {});
    let creation!: Promise<WorkspaceInfo | null>;
    act(() => { creation = result.current.addWorkspace(); });
    expect(result.current.workspaceProjectPrompt.prompt?.roots.map((root) => root.path)).toEqual([]);
    expect(pickWorkspacePaths).not.toHaveBeenCalled();
    expect(createWorkspaceProject).not.toHaveBeenCalled();

    await act(async () => { await result.current.workspaceProjectPrompt.chooseDirectories(); });
    expect(result.current.workspaceProjectPrompt.prompt).toMatchObject({
      name: "ws-1", primaryRootId: directoryId(result.current.workspaceProjectPrompt.prompt, workspaceOne.path),
    });
    act(() => {
      result.current.workspaceProjectPrompt.updateName("  Full stack  ");
      result.current.workspaceProjectPrompt.updatePrimaryRoot(directoryId(result.current.workspaceProjectPrompt.prompt, workspaceTwo.path));
    });
    await act(async () => { await result.current.workspaceProjectPrompt.confirm(); });
    expect(await creation).toEqual(workspaceTwo);
    expect(createWorkspaceProject).toHaveBeenCalledExactlyOnceWith({
      name: "Full stack", paths: [workspaceOne.path, workspaceTwo.path], primaryPath: workspaceTwo.path,
    });
    expect(addWorkspace).not.toHaveBeenCalled();
    expect(result.current.workspaces).toEqual([workspaceTwo]);
    expect(result.current.activeWorkspaceId).toBe(workspaceTwo.id);
    expect(result.current.workspaceProjectPrompt.prompt).toBeNull();
  });

  it("deduplicates selections and keeps a valid primary when directories are removed", async () => {
    vi.mocked(listWorkspaces).mockResolvedValue([]);
    vi.mocked(pickWorkspacePaths).mockResolvedValue([workspaceOne.path, `${workspaceOne.path}/`, workspaceTwo.path]);
    const { result } = renderController();
    await act(async () => {});
    let creation!: Promise<WorkspaceInfo | null>;
    act(() => { creation = result.current.addWorkspace(); });
    await act(async () => { await result.current.workspaceProjectPrompt.chooseDirectories(); });
    expect(result.current.workspaceProjectPrompt.prompt?.roots.map((root) => root.path)).toEqual([workspaceOne.path, workspaceTwo.path]);
    act(() => { result.current.workspaceProjectPrompt.removeDirectory(directoryId(result.current.workspaceProjectPrompt.prompt, workspaceOne.path)); });
    expect(result.current.workspaceProjectPrompt.prompt).toMatchObject({
      name: "ws-2", roots: [expect.objectContaining({ path: workspaceTwo.path })], primaryRootId: directoryId(result.current.workspaceProjectPrompt.prompt, workspaceTwo.path),
    });
    act(() => {
      result.current.workspaceProjectPrompt.updateName("My workspace");
      result.current.workspaceProjectPrompt.removeDirectory(directoryId(result.current.workspaceProjectPrompt.prompt, workspaceTwo.path));
    });
    expect(result.current.workspaceProjectPrompt.prompt).toMatchObject({
      name: "My workspace", roots: [], primaryRootId: null,
    });
    await act(async () => { await result.current.workspaceProjectPrompt.confirm(); });
    expect(createWorkspaceProject).not.toHaveBeenCalled();
    await act(async () => { await result.current.workspaceProjectPrompt.chooseDirectories(); });
    expect(result.current.workspaceProjectPrompt.prompt?.name).toBe("My workspace");
    act(() => { result.current.workspaceProjectPrompt.cancel(); });
    expect(await creation).toBeNull();
    expect(result.current.workspaces).toEqual([]);
  });

  it("retains a failed draft for retry and prevents duplicate submissions or dismissal while saving", async () => {
    vi.mocked(listWorkspaces).mockResolvedValue([workspaceOne]);
    vi.mocked(pickWorkspacePaths).mockResolvedValue([workspaceTwo.path]);
    vi.mocked(createWorkspaceProject).mockRejectedValueOnce(new Error("Directory unavailable"));
    const { result } = renderController();
    await act(async () => {});
    let creation!: Promise<WorkspaceInfo | null>;
    act(() => { creation = result.current.addWorkspace(); });
    await act(async () => { await result.current.workspaceProjectPrompt.chooseDirectories(); });
    await act(async () => { await result.current.workspaceProjectPrompt.confirm(); });
    expect(result.current.workspaceProjectPrompt.prompt).toMatchObject({
      name: "ws-2", roots: [expect.objectContaining({ path: workspaceTwo.path })], error: "Directory unavailable", isBusy: false,
    });
    expect(result.current.workspaces).toEqual([workspaceOne]);
    let finish!: (workspace: WorkspaceInfo) => void;
    vi.mocked(createWorkspaceProject).mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }));
    let saving!: Promise<void>;
    act(() => {
      saving = result.current.workspaceProjectPrompt.confirm();
      void result.current.workspaceProjectPrompt.confirm();
      result.current.workspaceProjectPrompt.cancel();
      result.current.workspaceProjectPrompt.removeDirectory(directoryId(result.current.workspaceProjectPrompt.prompt, workspaceTwo.path));
    });
    expect(createWorkspaceProject).toHaveBeenCalledTimes(2);
    expect(result.current.workspaceProjectPrompt.prompt).toMatchObject({ isBusy: true, roots: [expect.objectContaining({ path: workspaceTwo.path })] });
    await act(async () => { finish(workspaceTwo); await saving; });
    expect(await creation).toEqual(workspaceTwo);
    expect(result.current.workspaces).toEqual([workspaceOne, workspaceTwo]);
  });

  it("ignores a folder picker result from a dismissed panel", async () => {
    vi.mocked(listWorkspaces).mockResolvedValue([]);
    let finishPicking!: (paths: string[]) => void;
    vi.mocked(pickWorkspacePaths).mockReturnValueOnce(new Promise((resolve) => { finishPicking = resolve; }));
    const { result, unmount } = renderController();
    await act(async () => {});
    let creation!: Promise<WorkspaceInfo | null>;
    act(() => { creation = result.current.addWorkspace(); });
    let picking!: Promise<void>;
    act(() => { picking = result.current.workspaceProjectPrompt.chooseDirectories(); });
    act(() => { result.current.workspaceProjectPrompt.cancel(); });
    expect(await creation).toBeNull();
    let nextCreation!: Promise<WorkspaceInfo | null>;
    act(() => { nextCreation = result.current.addWorkspace(); });
    await act(async () => { finishPicking([workspaceOne.path]); await picking; });
    expect(result.current.workspaceProjectPrompt.prompt?.roots.map((root) => root.path)).toEqual([]);
    unmount();
    expect(await nextCreation).toBeNull();
    expect(createWorkspaceProject).not.toHaveBeenCalled();
  });

  it("shows picker errors in the panel and permits another selection", async () => {
    vi.mocked(listWorkspaces).mockResolvedValue([]);
    vi.mocked(pickWorkspacePaths).mockRejectedValueOnce(new Error("Picker failed"));
    const { result } = renderController();
    await act(async () => {});
    act(() => { void result.current.addWorkspace(); });
    await act(async () => { await result.current.workspaceProjectPrompt.chooseDirectories(); });
    expect(result.current.workspaceProjectPrompt.prompt).toMatchObject({ error: "Picker failed", isChoosing: false });
    vi.mocked(pickWorkspacePaths).mockResolvedValueOnce([]);
    await act(async () => { await result.current.workspaceProjectPrompt.chooseDirectories(); });
    expect(result.current.workspaceProjectPrompt.prompt).toMatchObject({ error: null, roots: [] });
    act(() => { result.current.workspaceProjectPrompt.cancel(); });
  });

  it("edits the clicked project without selecting it or replacing its root identities", async () => {
    vi.mocked(listWorkspaces).mockResolvedValue([workspaceOne, { ...workspaceTwo, project: projectTwo }]);
    vi.mocked(getWorkspaceProject).mockResolvedValue(projectTwo);
    vi.mocked(updateWorkspaceProject).mockImplementation(async (project) => project);
    vi.mocked(pickWorkspacePaths).mockResolvedValue(["/tmp/docs"]);
    const { result } = renderController();
    await act(async () => {});
    act(() => { result.current.setActiveWorkspaceId(workspaceOne.id); });
    await act(async () => { result.current.editWorkspace(workspaceTwo.id); });
    expect(getWorkspaceProject).toHaveBeenCalledWith(workspaceTwo.id);
    expect(result.current.workspaceProjectPrompt.prompt).toMatchObject({
      mode: "edit", name: "Existing project", primaryRootId: "backend",
      roots: projectTwo.roots, isLoading: false,
    });
    act(() => { result.current.workspaceProjectPrompt.updateName("Renamed project"); });
    await act(async () => { await result.current.workspaceProjectPrompt.chooseDirectories(); });
    await act(async () => { await result.current.workspaceProjectPrompt.confirm(); });
    expect(createWorkspaceProject).not.toHaveBeenCalled();
    expect(updateWorkspaceProject).toHaveBeenCalledExactlyOnceWith({
      ...projectTwo,
      name: "Renamed project",
      roots: [...projectTwo.roots, {
        id: expect.any(String), name: "docs", path: "/tmp/docs", ownership: { kind: "external" },
      }],
    });
    expect(result.current.activeWorkspaceId).toBe(workspaceOne.id);
    expect(result.current.workspaces).toHaveLength(2);
    expect(result.current.workspaces[1]).toMatchObject({
      id: workspaceTwo.id, name: "Renamed project", path: projectTwo.executionDir,
    });
    expect(result.current.workspaceProjectPrompt.prompt).toBeNull();
  });

  it("resets the execution directory when editing changes the primary root, and retries failures", async () => {
    vi.mocked(listWorkspaces).mockResolvedValue([workspaceTwo]);
    vi.mocked(getWorkspaceProject).mockResolvedValue(projectTwo);
    vi.mocked(updateWorkspaceProject).mockRejectedValueOnce(new Error("Project could not be saved"));
    const { result } = renderController();
    await act(async () => {});
    await act(async () => { result.current.editWorkspace(workspaceTwo.id); });
    act(() => { result.current.workspaceProjectPrompt.removeDirectory(directoryId(result.current.workspaceProjectPrompt.prompt, workspaceTwo.path)); });
    await act(async () => { await result.current.workspaceProjectPrompt.confirm(); });
    const expected = {
      ...projectTwo, roots: [projectTwo.roots[0]], primaryRoot: "frontend", executionDir: null,
    };
    expect(updateWorkspaceProject).toHaveBeenCalledWith(expected);
    expect(result.current.workspaceProjectPrompt.prompt).toMatchObject({
      error: "Project could not be saved", primaryRootId: "frontend", isBusy: false,
    });
    expect(result.current.workspaces).toEqual([workspaceTwo]);
    vi.mocked(updateWorkspaceProject).mockResolvedValueOnce(expected);
    await act(async () => { await result.current.workspaceProjectPrompt.confirm(); });
    expect(result.current.workspaces[0]).toMatchObject({ id: workspaceTwo.id, path: workspaceOne.path });
    expect(result.current.workspaceProjectPrompt.prompt).toBeNull();
  });

  it("preserves distinct root identities even when existing roots share a path", async () => {
    const original: ProjectDefinition = {
      ...projectTwo,
      roots: [
        { ...projectTwo.roots[0], path: workspaceTwo.path },
        { ...projectTwo.roots[1], path: workspaceTwo.path },
        { ...projectTwo.roots[1], id: "alias", path: `${workspaceTwo.path}/` },
      ],
    };
    vi.mocked(listWorkspaces).mockResolvedValue([workspaceTwo]);
    vi.mocked(getWorkspaceProject).mockResolvedValue(original);
    vi.mocked(updateWorkspaceProject).mockImplementation(async (project) => project);
    const { result } = renderController();
    await act(async () => {});
    await act(async () => { result.current.editWorkspace(workspaceTwo.id); });
    act(() => { result.current.workspaceProjectPrompt.updateName("Renamed"); });
    await act(async () => { await result.current.workspaceProjectPrompt.confirm(); });
    expect(updateWorkspaceProject).toHaveBeenLastCalledWith({ ...original, name: "Renamed" });
    await act(async () => { result.current.editWorkspace(workspaceTwo.id); });
    act(() => { result.current.workspaceProjectPrompt.removeDirectory("frontend"); });
    await act(async () => { await result.current.workspaceProjectPrompt.confirm(); });
    expect(updateWorkspaceProject).toHaveBeenLastCalledWith({ ...original, roots: original.roots.slice(1) });
  });

  it("keeps loading failures recoverable and discards edits on cancel", async () => {
    vi.mocked(listWorkspaces).mockResolvedValue([workspaceTwo]);
    vi.mocked(getWorkspaceProject).mockRejectedValueOnce(new Error("Read failed"));
    const { result } = renderController();
    await act(async () => {});
    await act(async () => { result.current.editWorkspace(workspaceTwo.id); });
    expect(result.current.workspaceProjectPrompt.prompt).toMatchObject({ mode: "edit", error: "Read failed", isLoading: false });
    await act(async () => { await result.current.workspaceProjectPrompt.confirm(); });
    expect(updateWorkspaceProject).not.toHaveBeenCalled();
    vi.mocked(getWorkspaceProject).mockResolvedValueOnce(projectTwo);
    await act(async () => { result.current.workspaceProjectPrompt.retryLoad(); });
    expect(result.current.workspaceProjectPrompt.prompt?.name).toBe(projectTwo.name);
    act(() => {
      result.current.workspaceProjectPrompt.updateName("Unsaved name");
      result.current.workspaceProjectPrompt.cancel();
    });
    expect(result.current.workspaces).toEqual([workspaceTwo]);
    expect(updateWorkspaceProject).not.toHaveBeenCalled();
  });

  it("does not overwrite another panel with a cancelled edit load", async () => {
    vi.mocked(listWorkspaces).mockResolvedValue([workspaceTwo]);
    let finish!: (project: ProjectDefinition) => void;
    vi.mocked(getWorkspaceProject).mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }));
    const { result } = renderController();
    await act(async () => {});
    act(() => { result.current.editWorkspace(workspaceTwo.id); });
    expect(result.current.workspaceProjectPrompt.prompt?.isLoading).toBe(true);
    act(() => {
      result.current.workspaceProjectPrompt.cancel();
      void result.current.addWorkspace();
    });
    await act(async () => { finish(projectTwo); });
    expect(result.current.workspaceProjectPrompt.prompt).toMatchObject({ mode: "create", name: "", roots: [] });
    act(() => { result.current.workspaceProjectPrompt.cancel(); });
  });

  it("shows add-workspaces summary in controller layer", async () => {
    vi.mocked(listWorkspaces).mockResolvedValue([workspaceOne]);
    vi.mocked(pickWorkspacePaths).mockResolvedValue([workspaceOne.path, workspaceTwo.path]);
    vi.mocked(isWorkspacePathDir).mockResolvedValue(true);
    vi.mocked(addWorkspace).mockResolvedValue(workspaceTwo);

    const { result } = renderHook(() =>
      useWorkspaceController({
        appSettings: baseAppSettings,
        addDebugEntry: vi.fn(),
        queueSaveSettings: vi.fn(async (next) => next),
      }),
    );

    await act(async () => {
      await Promise.resolve();
    });

    let added: WorkspaceInfo | null = null;
    await act(async () => {
      added = await result.current.addWorkspacesFromPaths([workspaceOne.path, workspaceTwo.path]);
    });

    expect(added).toMatchObject({ id: workspaceTwo.id });
    expect(message).toHaveBeenCalledTimes(1);
    const [summary] = vi.mocked(message).mock.calls[0];
    expect(String(summary)).toContain("Skipped 1 already added workspace");
  });

  it("confirms workspace deletion and reports service errors", async () => {
    vi.mocked(listWorkspaces).mockResolvedValue([workspaceOne]);
    vi.mocked(ask).mockResolvedValue(true);
    vi.mocked(removeWorkspace).mockRejectedValue(new Error("delete failed"));

    const { result } = renderHook(() =>
      useWorkspaceController({
        appSettings: baseAppSettings,
        addDebugEntry: vi.fn(),
        queueSaveSettings: vi.fn(async (next) => next),
      }),
    );

    await act(async () => {
      await Promise.resolve();
    });

    await act(async () => {
      await result.current.removeWorkspace(workspaceOne.id);
    });

    expect(ask).toHaveBeenCalledTimes(1);
    expect(removeWorkspace).toHaveBeenCalledWith(workspaceOne.id);
    expect(message).toHaveBeenCalledTimes(1);
    const [, options] = vi.mocked(message).mock.calls[0];
    expect(options).toEqual(
      expect.objectContaining({ title: "Delete workspace failed", kind: "error" }),
    );
  });

});
