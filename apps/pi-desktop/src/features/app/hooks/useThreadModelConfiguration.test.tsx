// @vitest-environment jsdom
import { useEffect } from "react";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { configureThread, getModelList } from "@services/tauri";
import { subscribePiEvents } from "@services/events";
import { useModels } from "@/features/models/hooks/useModels";
import { useThreadModelConfiguration } from "./useThreadModelConfiguration";

vi.mock("@services/tauri", () => ({
  configureThread: vi.fn(),
  getModelList: vi.fn(),
}));
vi.mock("@services/events", () => ({
  subscribePiEvents: vi.fn(() => vi.fn()),
}));

const model = (id: string, effort: string = "low", isDefault = true) => ({
  id,
  model: id,
  displayName: id,
  isDefault,
  supportedReasoningEfforts: [
    { reasoningEffort: "low" },
    { reasoningEffort: "high" },
  ],
  defaultReasoningEffort: effort,
});

function useHandoff(
  threadId: string,
  preferredModelId: string | null = null,
  preferredEffort: string | null = null,
) {
  const models = useModels({
    preferredModelId,
    preferredEffort,
    selectionKey: threadId,
  });
  const { selectCatalog } = models;
  useEffect(() => {
    selectCatalog("workspace", threadId);
  }, [selectCatalog, threadId]);
  useThreadModelConfiguration({
    workspaceId: "workspace",
    threadId,
    model: models.selectedModel?.id ?? null,
    effort: models.selectedEffort,
    isCatalogCurrent: models.isCatalogCurrent,
  });
  return models;
}

function reload(threadId: string) {
  const listener = vi.mocked(subscribePiEvents).mock.calls.slice(-1)[0][0];
  listener({
    workspace_id: "workspace",
    message: {
      method: "thread/replaced",
      params: { previousThreadId: threadId, thread: { id: threadId } },
    },
  });
}

beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(configureThread).mockResolvedValue({});
  vi.mocked(subscribePiEvents).mockReturnValue(vi.fn());
});
afterEach(cleanup);

it("never configures a newly selected thread with the previous thread's model", async () => {
  let finish!: (value: unknown) => void;
  vi.mocked(getModelList)
    .mockResolvedValueOnce({ data: [model("alpha")] })
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
  const { rerender } = renderHook(({ threadId }) => useHandoff(threadId), {
    initialProps: { threadId: "A" },
  });
  await waitFor(() =>
    expect(configureThread).toHaveBeenCalledWith("workspace", "A", {
      model: "alpha",
      effort: "low",
    }),
  );
  vi.mocked(configureThread).mockClear();
  rerender({ threadId: "B" });
  await waitFor(() =>
    expect(getModelList).toHaveBeenCalledWith("workspace", "B"),
  );
  expect(configureThread).not.toHaveBeenCalled();
  await act(async () => {
    finish({ data: [model("beta", "high")] });
  });
  expect(configureThread).toHaveBeenCalledExactlyOnceWith("workspace", "B", {
    model: "beta",
    effort: "high",
  });
});

it("honors the current native model and thinking after reload over cached preferences", async () => {
  vi.mocked(getModelList)
    .mockResolvedValueOnce({
      data: [model("alpha"), model("beta", "high", false)],
    })
    .mockResolvedValue({
      data: [model("alpha", "low", false), model("beta", "high")],
    });
  const { result } = renderHook(() => useHandoff("thread", "alpha", "low"));
  await waitFor(() => expect(result.current.selectedModelId).toBe("alpha"));
  vi.mocked(configureThread).mockClear();
  act(() => {
    reload("thread");
  });
  await waitFor(() => expect(result.current.selectedModelId).toBe("beta"));
  expect(configureThread).toHaveBeenCalledExactlyOnceWith(
    "workspace",
    "thread",
    { model: "beta", effort: "high" },
  );
});

it("blocks automatic writes throughout a pending or failed catalog refresh", async () => {
  let fail!: (reason: Error) => void;
  vi.mocked(getModelList)
    .mockResolvedValueOnce({
      data: [model("alpha"), model("beta", "high", false)],
    })
    .mockImplementationOnce(
      () =>
        new Promise((_, reject) => {
          fail = reject;
        }),
    );
  const { result } = renderHook(() => useHandoff("thread"));
  await waitFor(() => expect(result.current.selectedModelId).toBe("alpha"));
  vi.mocked(configureThread).mockClear();
  act(() => {
    reload("thread");
  });
  act(() => {
    result.current.setSelectedModelId("beta");
    result.current.setSelectedEffort("high");
  });
  expect(configureThread).not.toHaveBeenCalled();
  await act(async () => {
    fail(new Error("catalog unavailable"));
  });
  expect(configureThread).not.toHaveBeenCalled();
  act(() => {
    result.current.setSelectedEffort("low");
  });
  expect(configureThread).not.toHaveBeenCalled();
});

it("preserves a newer explicit model and thinking choice made during the read", async () => {
  let finish!: (value: unknown) => void;
  const data = [model("alpha"), model("beta", "high", false)];
  vi.mocked(getModelList)
    .mockResolvedValueOnce({ data })
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
  const { result } = renderHook(() => useHandoff("thread", "alpha", "low"));
  await waitFor(() => expect(result.current.selectedModelId).toBe("alpha"));
  vi.mocked(configureThread).mockClear();
  act(() => {
    reload("thread");
  });
  act(() => {
    result.current.setSelectedModelId("beta");
    result.current.setSelectedEffort("high");
  });
  expect(configureThread).not.toHaveBeenCalled();
  await act(async () => {
    finish({ data });
  });
  expect(result.current.selectedModelId).toBe("beta");
  expect(result.current.selectedEffort).toBe("high");
  expect(configureThread).toHaveBeenCalledExactlyOnceWith(
    "workspace",
    "thread",
    { model: "beta", effort: "high" },
  );
});

it("does not replace an unlisted native selection with the first listed model", async () => {
  vi.mocked(getModelList).mockResolvedValue({
    data: [model("alpha", "low", false)],
  });
  const { result } = renderHook(() => useHandoff("thread", "alpha", "low"));
  await waitFor(() => expect(result.current.models).toHaveLength(1));
  expect(result.current.selectedModelId).toBeNull();
  expect(result.current.selectedEffort).toBeNull();
  expect(configureThread).not.toHaveBeenCalled();
});

it("does not let a pending thinking-only edit restore the previous native model", async () => {
  let finish!: (value: unknown) => void;
  vi.mocked(getModelList)
    .mockResolvedValueOnce({
      data: [model("alpha"), model("beta", "high", false)],
    })
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
  const { result } = renderHook(() => useHandoff("thread", "alpha", "low"));
  await waitFor(() => expect(result.current.selectedModelId).toBe("alpha"));
  vi.mocked(configureThread).mockClear();
  act(() => {
    reload("thread");
  });
  act(() => {
    result.current.setSelectedEffort("low");
  });
  await act(async () => {
    finish({ data: [model("alpha", "high", false), model("beta", "high")] });
  });
  expect(configureThread).toHaveBeenCalledExactlyOnceWith(
    "workspace",
    "thread",
    { model: "beta", effort: "low" },
  );
});
