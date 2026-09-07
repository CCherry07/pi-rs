import { describe, expect, it } from "vitest";

import { getThreadStatusClass, getWorkspaceHomeThreadState } from "./threadStatus";

describe("threadStatus", () => {
  it("prioritizes pending user input over processing state", () => {
    expect(
      getThreadStatusClass(
        { isProcessing: true, hasUnread: false },
        true,
      ),
    ).toBe("unread");
  });

  it("maps thread status to workspace home labels and classes", () => {
    expect(
      getWorkspaceHomeThreadState({
        isProcessing: true,
        hasUnread: false,
      }),
    ).toEqual({
      status: "running",
      stateClass: "is-running",
      isRunning: true,
    });

    expect(
      getWorkspaceHomeThreadState({
        isProcessing: false,
        hasUnread: false,
      }),
    ).toEqual({
      status: "idle",
      stateClass: "is-idle",
      isRunning: false,
    });
  });

});
