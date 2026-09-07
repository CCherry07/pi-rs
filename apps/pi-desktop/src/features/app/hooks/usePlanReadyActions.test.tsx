// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { WorkspaceInfo } from "@/types";
import { usePlanReadyActions } from "@app/hooks/usePlanReadyActions";

const connectedWorkspace: WorkspaceInfo = {
  id: "ws-1",
  name: "Workspace",
  path: "/tmp/workspace",
  settings: { sidebarCollapsed: false },
};

function renderPlanReadyActions(activeWorkspace: WorkspaceInfo = connectedWorkspace) {
  const sendUserMessageToThread = vi.fn().mockResolvedValue(undefined);
  const hook = renderHook(() =>
    usePlanReadyActions({
      activeWorkspace,
      activeThreadId: "thread-1",
      sendUserMessageToThread,
    }),
  );
  return { ...hook, sendUserMessageToThread };
}

describe("usePlanReadyActions", () => {
  it("sends the plan acceptance message", async () => {
    const { result, sendUserMessageToThread } = renderPlanReadyActions();

    await act(async () => {
      await result.current.handlePlanAccept();
    });

    expect(sendUserMessageToThread).toHaveBeenCalledWith(
      connectedWorkspace,
      "thread-1",
      "[[cm_plan_ready:accept]] Implement this plan.",
      [],
    );
  });

  it("trims and sends plan changes", async () => {
    const { result, sendUserMessageToThread } = renderPlanReadyActions();

    await act(async () => {
      await result.current.handlePlanSubmitChanges("  Add tests  ");
    });

    expect(sendUserMessageToThread).toHaveBeenCalledWith(
      connectedWorkspace,
      "thread-1",
      "[[cm_plan_ready:changes]] Update the plan with these changes:\n\nAdd tests",
      [],
    );
  });
});
