// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { sendNotification } from "../../../services/tauri";
import { useAgentSystemNotifications } from "./useAgentSystemNotifications";

const usePiEventsMock = vi.fn();

vi.mock("../../../services/tauri", () => ({
  sendNotification: vi.fn(),
}));

vi.mock("../../app/hooks/usePiEvents", () => ({
  usePiEvents: (handlers: unknown) => usePiEventsMock(handlers),
}));

describe("useAgentSystemNotifications", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(sendNotification).mockResolvedValue();
  });

  it("mutes notifications for subagent threads when disabled", async () => {
    renderHook(() =>
      useAgentSystemNotifications({
        enabled: true,
        isWindowFocused: false,
        minDurationMs: 0,
        subagentNotificationsEnabled: false,
        isSubagentThread: (_workspaceId, threadId) => threadId === "child-thread",
      }),
    );

    const handlers = usePiEventsMock.mock.calls[
      usePiEventsMock.mock.calls.length - 1
    ]?.[0] as {
      onTurnStarted?: (workspaceId: string, threadId: string, turnId: string) => void;
      onTurnCompleted?: (workspaceId: string, threadId: string, turnId: string) => void;
    };

    act(() => {
      handlers.onTurnStarted?.("ws-1", "child-thread", "turn-1");
      handlers.onTurnCompleted?.("ws-1", "child-thread", "turn-1");
    });

    await act(async () => {
      await Promise.resolve();
    });

    expect(sendNotification).not.toHaveBeenCalled();
  });

  it("still notifies for non-subagent threads while muted", async () => {
    renderHook(() =>
      useAgentSystemNotifications({
        enabled: true,
        isWindowFocused: false,
        minDurationMs: 0,
        subagentNotificationsEnabled: false,
        isSubagentThread: (_workspaceId, threadId) => threadId === "child-thread",
      }),
    );

    const handlers = usePiEventsMock.mock.calls[
      usePiEventsMock.mock.calls.length - 1
    ]?.[0] as {
      onTurnStarted?: (workspaceId: string, threadId: string, turnId: string) => void;
      onTurnCompleted?: (workspaceId: string, threadId: string, turnId: string) => void;
    };

    act(() => {
      handlers.onTurnStarted?.("ws-1", "parent-thread", "turn-1");
      handlers.onTurnCompleted?.("ws-1", "parent-thread", "turn-1");
    });

    await act(async () => {
      await Promise.resolve();
    });

    expect(sendNotification).toHaveBeenCalledTimes(1);
    expect(vi.mocked(sendNotification).mock.calls[0]?.[2]).toMatchObject({
      extra: {
        workspaceId: "ws-1",
        threadId: "parent-thread",
      },
    });
  });
});
