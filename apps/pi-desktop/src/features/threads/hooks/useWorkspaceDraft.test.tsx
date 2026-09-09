// @vitest-environment jsdom
import { StrictMode } from "react";
import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { prepareThread, reloadThread } from "@services/tauri";
import { useWorkspaceDraft } from "./useWorkspaceDraft";

vi.mock("@services/tauri", () => ({ prepareThread: vi.fn(), reloadThread: vi.fn() }));

const commands = [
  { name: "native", description: "Plugin action" },
  { name: "skill:startup", description: "Workspace startup checks" },
];

describe("workspace draft", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(prepareThread).mockResolvedValue({ thread: { id: "draft", commands } });
    vi.mocked(reloadThread).mockResolvedValue({ thread: { id: "draft", commands } });
  });

  it("loads plugin commands and skills before a thread is selected", async () => {
    const { result } = renderHook(() => useWorkspaceDraft("workspace", null), { wrapper: StrictMode });
    await waitFor(() => expect(result.current.commands).toEqual(commands));
    expect(prepareThread).toHaveBeenCalledWith("workspace");
  });

  it("does not prepare a draft while an existing thread is selected", async () => {
    const { result, rerender } = renderHook(
      ({ workspaceId, threadId }) => useWorkspaceDraft(workspaceId, threadId),
      { initialProps: { workspaceId: "workspace", threadId: "existing" as string | null } },
    );
    expect(prepareThread).not.toHaveBeenCalled();
    expect(result.current.commands).toEqual([]);
    rerender({ workspaceId: "workspace", threadId: null });
    await waitFor(() => expect(result.current.commands).toEqual(commands));
    rerender({ workspaceId: "workspace", threadId: "started" });
    expect(result.current.commands).toEqual([]);
  });

  it("ignores late results after switching workspaces or starting a thread", async () => {
    let resolveFirst!: (value: Awaited<ReturnType<typeof prepareThread>>) => void;
    vi.mocked(prepareThread).mockImplementationOnce(() => new Promise((resolve) => { resolveFirst = resolve; }));
    const { result, rerender } = renderHook(
      ({ workspaceId, threadId }) => useWorkspaceDraft(workspaceId, threadId),
      { initialProps: { workspaceId: "first", threadId: null as string | null } },
    );
    rerender({ workspaceId: "second", threadId: null });
    await waitFor(() => expect(result.current.commands).toEqual(commands));
    await act(async () => resolveFirst({ thread: { id: "old", commands: [{ name: "old", description: "Old workspace" }] } }));
    expect(result.current.commands).toEqual(commands);
    rerender({ workspaceId: "second", threadId: "started" });
    expect(result.current.commands).toEqual([]);
  });

  it("reports preparation errors without retaining an old catalog", async () => {
    vi.mocked(prepareThread).mockRejectedValueOnce(new Error("Cannot load workspace plugin"));
    const onDebug = vi.fn();
    const { result } = renderHook(() => useWorkspaceDraft("workspace", null, onDebug));
    await waitFor(() => expect(onDebug).toHaveBeenCalledWith(expect.objectContaining({
      source: "error", payload: "Cannot load workspace plugin",
    })));
    expect(result.current.commands).toEqual([]);
  });

  it("reloads the prepared generation and replaces its command catalog", async () => {
    const reloaded = [{ name: "native", description: "Reloaded plugin action" }];
    vi.mocked(reloadThread).mockResolvedValueOnce({
      thread: { id: "draft", commands: reloaded },
    });
    const { result } = renderHook(() => useWorkspaceDraft("workspace", null));
    await waitFor(() => expect(result.current.commands).toEqual(commands));

    await act(async () => {
      await result.current.reload();
    });

    expect(reloadThread).toHaveBeenCalledWith("workspace");
    expect(result.current.commands).toEqual(reloaded);
  });
});
