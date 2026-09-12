// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { PiEvent, WorkspaceInfo } from "@/types";
import { subscribePiEvents } from "@/services/events";
import { prepareThread, reloadThread, startThread } from "@services/tauri";
import { useThreads } from "./useThreads";

vi.mock("@/services/events", () => ({ subscribePiEvents: vi.fn(() => vi.fn()) }));
vi.mock("@services/tauri", () => ({
  prepareThread: vi.fn(), reloadThread: vi.fn(), startThread: vi.fn(),
  archiveThread: vi.fn(), readThread: vi.fn(), setThreadName: vi.fn(),
  forkThread: vi.fn(), listThreads: vi.fn(), listWorkspaces: vi.fn(), resumeThread: vi.fn(),
  interruptTurn: vi.fn(), sendUserMessage: vi.fn(), compactThread: vi.fn(),
}));
const workspace: WorkspaceInfo = { id: "workspace", name: "Workspace", path: "/project", settings: { sidebarCollapsed: false } };
const command = { name: "native", description: "Before reload" };
function emit(method: string, params: Record<string, unknown>) {
  const listener = vi.mocked(subscribePiEvents).mock.calls.slice(-1)[0][0];
  listener({ workspace_id: workspace.id, message: { method, params } } as PiEvent);
}
beforeEach(() => {
  vi.resetAllMocks();
  localStorage.clear();
  vi.mocked(subscribePiEvents).mockReturnValue(vi.fn());
  vi.mocked(prepareThread).mockResolvedValue({ thread: { id: "draft", commands: [command] } });
  vi.mocked(startThread).mockResolvedValue({ thread: { id: "thread", commands: [command], turns: [] } });
});
afterEach(cleanup);

it("Settings reload uses the existing thread replacement event to refresh commands and transcript", async () => {
  const { result } = renderHook(() => useThreads({ activeWorkspace: workspace }));
  await waitFor(() => expect(result.current.runtimeCommands).toEqual([command]));
  await act(async () => { await result.current.startThreadForWorkspace(workspace.id); });
  expect(result.current.activeThreadId).toBe("thread");
  const replacement = {
    id: "thread", status: { type: "idle" }, commands: [{ name: "native", description: "After reload" }],
    turns: [{ id: "turn", items: [{ id: "answer", type: "agentMessage", text: "Restored transcript" }] }],
  };
  vi.mocked(reloadThread).mockImplementationOnce(async () => {
    emit("thread/replaced", { previousThreadId: "thread", thread: replacement });
    return { thread: replacement };
  });
  await act(async () => { await result.current.reloadCurrentSession(); });
  expect(reloadThread).toHaveBeenCalledExactlyOnceWith("workspace", "thread");
  expect(result.current.runtimeCommands).toEqual(replacement.commands);
  expect(result.current.activeItems).toEqual(expect.arrayContaining([expect.objectContaining({ text: "Restored transcript" })]));
  expect(prepareThread).toHaveBeenCalledOnce();
});

it("Settings reload rejects failure without falsely replacing the active catalog, while composer preserves its error path", async () => {
  const { result } = renderHook(() => useThreads({ activeWorkspace: workspace }));
  await waitFor(() => expect(result.current.runtimeCommands).toEqual([command]));
  await act(async () => { await result.current.startThreadForWorkspace(workspace.id); });
  vi.mocked(reloadThread).mockRejectedValue(new Error("incompatible plugin"));
  await act(async () => { await expect(result.current.reloadCurrentSession()).rejects.toThrow("incompatible plugin"); });
  expect(result.current.runtimeCommands).toEqual([command]);
  await act(async () => { await result.current.startReload(); });
  expect(result.current.activeItems).toEqual(expect.arrayContaining([expect.objectContaining({ text: "incompatible plugin" })]));
});

it("Settings reload updates the existing prepared draft catalog without creating a visible thread", async () => {
  const onDraftReloaded = vi.fn();
  const { result } = renderHook(() =>
    useThreads({ activeWorkspace: workspace, onDraftReloaded }),
  );
  await waitFor(() => expect(result.current.runtimeCommands).toEqual([command]));
  const commands = [{ name: "reloaded", description: "Updated draft" }];
  vi.mocked(reloadThread).mockResolvedValue({ thread: { id: "draft", commands } });
  await act(async () => { await result.current.reloadCurrentSession(); });
  expect(reloadThread).toHaveBeenCalledExactlyOnceWith("workspace");
  expect(result.current.runtimeCommands).toEqual(commands);
  expect(result.current.activeThreadId).toBeNull();
  expect(startThread).not.toHaveBeenCalled();
  expect(onDraftReloaded).toHaveBeenCalledExactlyOnceWith(workspace.id, null);
});

it("rejects a retained Settings reload callback after the selected workspace changes", async () => {
  const { result, rerender } = renderHook(({ activeWorkspace }) => useThreads({ activeWorkspace }), { initialProps: { activeWorkspace: workspace } });
  await waitFor(() => expect(result.current.runtimeCommands).toEqual([command]));
  const reloadPrevious = result.current.reloadCurrentSession;
  rerender({ activeWorkspace: { ...workspace, id: "different" } });
  await act(async () => { await expect(reloadPrevious()).rejects.toThrow(/selection changed/); });
  expect(reloadThread).not.toHaveBeenCalled();
});
