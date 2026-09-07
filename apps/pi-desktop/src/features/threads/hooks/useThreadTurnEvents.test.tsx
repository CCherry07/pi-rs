// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { useThreadTurnEvents } from "./useThreadTurnEvents";

function setup() {
  const dispatch = vi.fn();
  const recordThreadActivity = vi.fn();
  const setThreadLoaded = vi.fn();
  const { result } = renderHook(() =>
    useThreadTurnEvents({
      dispatch,
      planByThreadRef: { current: {} },
      getCustomName: () => undefined,
      isThreadHidden: () => false,
      setThreadLoaded,
      markProcessing: vi.fn(),
      setActiveTurnId: vi.fn(),
      getActiveTurnId: () => null,
      pendingInterruptsRef: { current: new Set() },
      pushThreadErrorMessage: vi.fn(),
      safeMessageActivity: vi.fn(),
      recordThreadActivity,
    }),
  );
  return { dispatch, recordThreadActivity, result, setThreadLoaded };
}

describe("useThreadTurnEvents", () => {
  it("restores archived thread title and summary metadata from the event", () => {
    const { dispatch, recordThreadActivity, result, setThreadLoaded } = setup();
    const onThreadUnarchived = result.current.onThreadUnarchived as (
      workspaceId: string,
      threadId: string,
      thread: Record<string, unknown>,
    ) => void;

    act(() => {
      onThreadUnarchived("workspace-1", "thread-1", {
        id: "thread-1",
        preview: "Original session title",
        model: "gpt-5.5",
        messageCount: 6,
        createdAt: 1_788_800_000_100,
        updatedAt: 1_788_800_000_123,
      });
    });

    expect(dispatch).toHaveBeenCalledWith({
      type: "setThreadName",
      workspaceId: "workspace-1",
      threadId: "thread-1",
      name: "Original session title",
    });
    expect(dispatch).toHaveBeenCalledWith({
      type: "setThreadTimestamp",
      workspaceId: "workspace-1",
      threadId: "thread-1",
      timestamp: 1_788_800_000_123,
    });
    expect(dispatch).toHaveBeenCalledWith({
      type: "mergeThreadSummary",
      workspaceId: "workspace-1",
      threadId: "thread-1",
      patch: expect.objectContaining({
        createdAt: 1_788_800_000_100,
        messageCount: 6,
        modelId: "gpt-5.5",
      }),
    });
    expect(recordThreadActivity).toHaveBeenCalledWith(
      "workspace-1",
      "thread-1",
      1_788_800_000_123,
    );
    expect(setThreadLoaded).toHaveBeenCalledWith("thread-1", false);
  });
});
