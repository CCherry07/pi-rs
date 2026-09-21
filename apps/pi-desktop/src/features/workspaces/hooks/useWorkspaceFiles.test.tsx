/** @vitest-environment jsdom */
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { WorkspaceFileListing, WorkspaceInfo } from "../../../types";
import { getWorkspaceFiles } from "../../../services/tauri";
import { useWorkspaceFiles } from "./useWorkspaceFiles";

vi.mock("../../../services/tauri", () => ({ getWorkspaceFiles: vi.fn() }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

const workspace: WorkspaceInfo = {
  id: "project", name: "project", path: "/app", settings: { sidebarCollapsed: false },
  project: { id: "project", name: "project", primaryRoot: "app", roots: [{ id: "app", name: "app", path: "/app", ownership: { kind: "external" } }] },
};
function listing(path: string): WorkspaceFileListing {
  return { workspace: { roots: [{ id: "app", name: "app", path, ownership: { kind: "external" } }], primaryRoot: "app", executionDir: path }, files: [{ rootId: "app", path: "README.md" }], errors: [] };
}

describe("session-scoped file listings", () => {
  it("discards an old response after switching away and back to the same thread", async () => {
    let resolveFirst!: (value: WorkspaceFileListing) => void;
    vi.mocked(getWorkspaceFiles)
      .mockImplementationOnce(() => new Promise((resolve) => { resolveFirst = resolve; }))
      .mockResolvedValueOnce(listing("/second"))
      .mockResolvedValueOnce(listing("/latest"));
    const { result, rerender } = renderHook(({ threadId }) => useWorkspaceFiles({ activeWorkspace: workspace, threadId, pollingEnabled: false }), { initialProps: { threadId: "first" } });
    rerender({ threadId: "second" });
    expect(result.current.listing).toBeNull();
    await waitFor(() => expect(result.current.listing?.workspace.executionDir).toBe("/second"));
    rerender({ threadId: "first" });
    await waitFor(() => expect(result.current.listing?.workspace.executionDir).toBe("/latest"));
    await act(async () => resolveFirst(listing("/stale")));
    expect(result.current.listing?.workspace.executionDir).toBe("/latest");
  });

  it("refreshes drafts when supplemental roots change even if cwd does not", async () => {
    vi.mocked(getWorkspaceFiles).mockResolvedValue(listing("/app"));
    const { result, rerender } = renderHook(({ activeWorkspace, enabled }) => useWorkspaceFiles({ activeWorkspace, enabled, pollingEnabled: false }), { initialProps: { activeWorkspace: workspace, enabled: true } });
    await waitFor(() => expect(getWorkspaceFiles).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(result.current.isLoading).toBe(false));
    rerender({ activeWorkspace: workspace, enabled: false });
    rerender({ activeWorkspace: workspace, enabled: true });
    expect(result.current.isLoading).toBe(false);
    expect(getWorkspaceFiles).toHaveBeenCalledTimes(1);
    const changed: WorkspaceInfo = { ...workspace, project: { ...workspace.project!, roots: [...workspace.project!.roots, { id: "shared", name: "shared", path: "/shared", ownership: { kind: "external" } }] } };
    rerender({ activeWorkspace: changed, enabled: true });
    await waitFor(() => expect(getWorkspaceFiles).toHaveBeenCalledTimes(2));
  });
});
