import { describe, expect, it } from "vitest";
import { initialState, threadReducer, type ThreadAction } from "./useThreadsReducer";

const replacement: Extract<ThreadAction, { type: "replaceThread" }> = {
  type: "replaceThread", workspaceId: "workspace", previousThreadId: "old", threadId: "new",
  items: [{ id: "answer", kind: "message", role: "assistant", text: "Restored answer" }],
  commands: [{ name: "skill:review", description: "Review changes" }],
  isProcessing: false, turnId: null, timestamp: 100,
};

describe("thread naming", () => {
  it("replaces the untitled placeholder with the first user message", () => {
    const state = threadReducer(
      {
        ...initialState,
        threadsByWorkspace: {
          workspace: [
            {
              id: "thread-1",
              name: "Untitled session",
              updatedAt: 0,
            },
          ],
        },
      },
      {
        type: "upsertItem",
        workspaceId: "workspace",
        threadId: "thread-1",
        item: {
          id: "message-1",
          kind: "message",
          role: "user",
          text: "Fix the desktop session title",
        },
        hasCustomName: false,
      },
    );

    expect(state.threadsByWorkspace.workspace[0]?.name).toBe(
      "Fix the desktop session title",
    );
  });
});

describe("replacement and notice reduction", () => {
  it("updates selected identity, history, status and commands together", () => {
    const state = threadReducer({
      ...initialState, activeThreadIdByWorkspace: { workspace: "old", other: "unrelated" },
    }, replacement);
    expect(state.activeThreadIdByWorkspace).toEqual({ workspace: "new", other: "unrelated" });
    expect(state.itemsByThread.new).toEqual(replacement.items);
    expect(state.commandsByThread.new).toEqual(replacement.commands);
    expect(state.threadStatusById.new.isProcessing).toBe(false);
    expect(state.activeTurnIdByThread.new).toBeNull();
  });

  it("refreshes a background session without changing the user's selection", () => {
    const state = threadReducer({
      ...initialState, activeThreadIdByWorkspace: { workspace: "unrelated" },
    }, replacement);
    expect(state.activeThreadIdByWorkspace.workspace).toBe("unrelated");
    expect(state.commandsByThread.new).toEqual(replacement.commands);
  });

  it("reloads the same identity from snapshot while preserving displayed notices", () => {
    let state = threadReducer(initialState, {
      type: "addNotice", threadId: "old", itemId: "notice-1", text: "Plugin warning", level: "warning",
    });
    state = threadReducer(state, {
      ...replacement, threadId: "old", isProcessing: true, turnId: "active-run",
    });
    expect(state.itemsByThread.old).toEqual([...replacement.items, {
      id: "notice-1", kind: "tool", toolType: "notice", title: "[warning]", detail: "Plugin warning", status: "completed",
    }]);
    expect(state.threadStatusById.old.isProcessing).toBe(true);
    expect(state.activeTurnIdByThread.old).toBe("active-run");
    expect(state.commandsByThread.old).toEqual(replacement.commands);
    expect(state.lastAgentMessageByThread).toEqual({});
  });
});
