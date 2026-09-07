// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { WorkspaceInfo } from "../../../types";
import { useSystemNotificationThreadLinks } from "./useSystemNotificationThreadLinks";

function makeWorkspace(overrides: Partial<WorkspaceInfo> = {}): WorkspaceInfo {
  return {
    id: "ws-1",
    name: "Workspace",
    path: "/tmp/workspace",
    settings: { sidebarCollapsed: false },
    ...overrides,
  };
}

describe("useSystemNotificationThreadLinks", () => {
  it("navigates to the thread when the app regains focus", async () => {
    const workspace = makeWorkspace();
    const workspacesById = new Map([[workspace.id, workspace]]);

    const refreshWorkspaces = vi.fn(async () => [workspace]);
    const openThreadLink = vi.fn();

    const { result } = renderHook(() =>
      useSystemNotificationThreadLinks({
        hasLoadedWorkspaces: true,
        workspacesById,
        refreshWorkspaces,
        openThreadLink,
      }),
    );

    act(() => {
      result.current.recordPendingThreadLink("ws-1", "t-1");
    });

    await act(async () => {
      window.dispatchEvent(new Event("focus"));
      await Promise.resolve();
    });

    expect(openThreadLink).toHaveBeenCalledWith("ws-1", "t-1");
    expect(refreshWorkspaces).not.toHaveBeenCalled();
  });

  it("navigates immediately when openThreadLinkOrQueue is used after load", async () => {
    const workspace = makeWorkspace();
    const workspacesById = new Map([[workspace.id, workspace]]);

    const refreshWorkspaces = vi.fn(async () => [workspace]);
    const openThreadLink = vi.fn();

    const { result } = renderHook(() =>
      useSystemNotificationThreadLinks({
        hasLoadedWorkspaces: true,
        workspacesById,
        refreshWorkspaces,
        openThreadLink,
      }),
    );

    await act(async () => {
      result.current.openThreadLinkOrQueue("ws-1", "t-2");
      await Promise.resolve();
    });

    expect(openThreadLink).toHaveBeenCalledWith("ws-1", "t-2");
  });
});
