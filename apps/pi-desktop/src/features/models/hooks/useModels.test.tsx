// @vitest-environment jsdom
import { useEffect } from "react";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { WorkspaceInfo } from "../../../types";
import { subscribePiEvents } from "../../../services/events";
import { getModelList } from "../../../services/tauri";
import { useModels } from "./useModels";

vi.mock("../../../services/events", () => ({
  subscribePiEvents: vi.fn(() => vi.fn()),
}));

vi.mock("../../../services/tauri", () => ({
  getModelList: vi.fn(),
}));

const workspace: WorkspaceInfo = {
  id: "workspace-1",
  name: "Workspace One",
  path: "/tmp/workspace-one",
  settings: { sidebarCollapsed: false },
};

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

function useSelectedModels(
  workspaceId: string | null,
  threadId: string | null = null,
) {
  const models = useModels({});
  const { selectCatalog } = models;
  useEffect(() => {
    selectCatalog(workspaceId, threadId);
  }, [selectCatalog, workspaceId, threadId]);
  return models;
}

function emitReplacement(workspaceId: string, threadId: string) {
  for (const [listener] of vi.mocked(subscribePiEvents).mock.calls) {
    listener({
      workspace_id: workspaceId,
      message: {
        method: "thread/replaced",
        params: { previousThreadId: threadId, thread: { id: threadId } },
      },
    });
  }
}

describe("useModels", () => {
  it("selects a compatible thinking level when the model changes", async () => {
    vi.mocked(getModelList).mockResolvedValue({
      data: [
        {
          id: "provider/reasoning",
          model: "reasoning",
          displayName: "Reasoning",
          supportedReasoningEfforts: [
            { reasoningEffort: "off", description: "Off" },
            { reasoningEffort: "high", description: "High" },
          ],
          defaultReasoningEffort: "high",
          isDefault: true,
        },
        {
          id: "provider/plain",
          model: "plain",
          displayName: "Plain",
          supportedReasoningEfforts: [
            { reasoningEffort: "off", description: "Off" },
          ],
          defaultReasoningEffort: "off",
          isDefault: false,
        },
      ],
    });

    const { result } = renderHook(() =>
      useSelectedModels(workspace.id, "thread"),
    );

    await waitFor(() => {
      expect(result.current.selectedModelId).toBe("provider/reasoning");
      expect(result.current.selectedEffort).toBe("high");
    });

    act(() => {
      result.current.setSelectedEffort("high");
      result.current.setSelectedModelId("provider/plain");
    });

    await waitFor(() => {
      expect(result.current.selectedModelId).toBe("provider/plain");
      expect(result.current.selectedEffort).toBe("off");
    });
  });
});

it("refreshes models after a same-ID runtime replacement", async () => {
  const before = {
    id: "provider/before",
    model: "before",
    displayName: "Before",
    isDefault: true,
  };
  const after = {
    id: "provider/after",
    model: "after",
    displayName: "After",
    isDefault: true,
  };
  vi.mocked(getModelList)
    .mockResolvedValueOnce({ data: [before] })
    .mockResolvedValue({ data: [after] });
  const { result } = renderHook(() =>
    useSelectedModels(workspace.id, "thread"),
  );
  await waitFor(() => expect(result.current.models[0]?.id).toBe(before.id));
  act(() => {
    emitReplacement(workspace.id, "thread");
  });
  await waitFor(() => expect(result.current.models[0]?.id).toBe(after.id));
});

it("queries the selected session and ignores replacements from other contexts", async () => {
  vi.mocked(getModelList).mockResolvedValue({
    data: [
      { id: "provider/current", model: "current", displayName: "Current" },
    ],
  });
  const { result } = renderHook(() =>
    useSelectedModels(workspace.id, "selected"),
  );
  await waitFor(() => expect(result.current.models).toHaveLength(1));
  expect(getModelList).toHaveBeenCalledExactlyOnceWith(
    workspace.id,
    "selected",
  );
  act(() => {
    emitReplacement(workspace.id, "other-thread");
    emitReplacement("other-workspace", "selected");
  });
  expect(getModelList).toHaveBeenCalledOnce();
});

it("refreshes a reloaded draft only while that draft remains selected", async () => {
  vi.mocked(getModelList).mockResolvedValue({
    data: [
      { id: "provider/current", model: "current", displayName: "Current" },
    ],
  });
  const { result, rerender } = renderHook(
    ({ workspaceId }) => useSelectedModels(workspaceId),
    { initialProps: { workspaceId: workspace.id } },
  );
  await waitFor(() => expect(result.current.models).toHaveLength(1));
  await act(async () => {
    await result.current.refreshModels(workspace.id, null);
  });
  expect(getModelList).toHaveBeenCalledTimes(2);
  rerender({ workspaceId: "other" });
  await waitFor(() => expect(getModelList).toHaveBeenCalledWith("other", null));
  await act(async () => {
    await result.current.refreshModels(workspace.id, null);
  });
  expect(getModelList).toHaveBeenCalledTimes(3);
});

it("ignores an old catalog arriving after selection changes", async () => {
  let finish!: (value: unknown) => void;
  vi.mocked(getModelList)
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockResolvedValue({
      data: [{ id: "provider/new", model: "new", displayName: "New" }],
    });
  const { result, rerender } = renderHook(
    ({ threadId }) => useSelectedModels(workspace.id, threadId),
    { initialProps: { threadId: "old" } },
  );
  await waitFor(() =>
    expect(getModelList).toHaveBeenCalledWith(workspace.id, "old"),
  );
  rerender({ threadId: "new" });
  await waitFor(() =>
    expect(result.current.models[0]?.id).toBe("provider/new"),
  );
  await act(async () => {
    finish({
      data: [{ id: "provider/old", model: "old", displayName: "Old" }],
    });
  });
  expect(result.current.models[0]?.id).toBe("provider/new");
});

it("lets a reload supersede an in-flight catalog read", async () => {
  let finish!: (value: unknown) => void;
  vi.mocked(getModelList)
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockResolvedValue({
      data: [
        { id: "provider/reloaded", model: "reloaded", displayName: "Reloaded" },
      ],
    });
  const { result } = renderHook(() =>
    useSelectedModels(workspace.id, "thread"),
  );
  await waitFor(() => expect(getModelList).toHaveBeenCalledOnce());
  act(() => {
    emitReplacement(workspace.id, "thread");
  });
  await waitFor(() =>
    expect(result.current.models[0]?.id).toBe("provider/reloaded"),
  );
  await act(async () => {
    finish({
      data: [{ id: "provider/old", model: "old", displayName: "Old" }],
    });
  });
  expect(result.current.models[0]?.id).toBe("provider/reloaded");
});
