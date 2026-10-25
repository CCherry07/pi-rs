import { describe, expect, it } from "vitest";
import {
  initialState,
  threadReducer,
  type ThreadAction,
} from "./useThreadsReducer";
import type { ConversationItem } from "@/types";

describe("streaming item identity", () => {
  it("retains historical rows across text and reasoning deltas", () => {
    const history: ConversationItem[] = Array.from(
      { length: 199 },
      (_, index) => ({
        id: `history-${index}`,
        kind: "message",
        role: "user",
        text: `Message ${index}`,
      }),
    );
    const answer: ConversationItem = {
      id: "answer",
      kind: "message",
      role: "assistant",
      text: "Hello",
    };
    const reasoning: ConversationItem = {
      id: "reasoning",
      kind: "reasoning",
      summary: "",
      content: "Thinking",
    };
    const initial = {
      ...initialState,
      itemsByThread: { thread: [...history, answer, reasoning] },
    };
    const delta: ThreadAction = {
      type: "appendAgentDelta",
      workspaceId: "workspace",
      threadId: "thread",
      itemId: "answer",
      delta: " world",
      hasCustomName: true,
    };
    const updated = threadReducer(initial, delta);
    const final = threadReducer(updated, {
      type: "appendReasoningContent",
      threadId: "thread",
      itemId: "reasoning",
      delta: " through this",
    });
    history.forEach((item, index) =>
      expect(final.itemsByThread.thread[index]).toBe(item),
    );
    expect(updated.itemsByThread.thread[200]).toBe(reasoning);
    expect(final.itemsByThread.thread[199]).toBe(
      updated.itemsByThread.thread[199],
    );
    expect(final.itemsByThread.thread[199]).toMatchObject({
      text: "Hello world",
    });
    expect(threadReducer(updated, { ...delta, delta: "Hello world" })).toBe(
      updated,
    );
  });

  it("preserves history limits when a stream introduces a new item", () => {
    const state = {
      ...initialState,
      maxItemsPerThread: 2,
      itemsByThread: {
        thread: [
          {
            id: "old",
            kind: "message",
            role: "user",
            text: "Old",
          } as ConversationItem,
          {
            id: "recent",
            kind: "message",
            role: "user",
            text: "Recent",
          } as ConversationItem,
        ],
      },
    };
    const updated = threadReducer(state, {
      type: "appendAgentDelta",
      workspaceId: "workspace",
      threadId: "thread",
      itemId: "answer",
      delta: "Hello",
      hasCustomName: true,
    });
    expect(updated.itemsByThread.thread.map((item) => item.id)).toEqual([
      "recent",
      "answer",
    ]);
    expect(updated.itemsByThread.thread[0]).toBe(state.itemsByThread.thread[1]);
  });
});

const replacement: Extract<ThreadAction, { type: "replaceThread" }> = {
  type: "replaceThread",
  workspaceId: "workspace",
  previousThreadId: "old",
  threadId: "new",
  items: [
    {
      id: "answer",
      kind: "message",
      role: "assistant",
      text: "Restored answer",
    },
  ],
  commands: [{ name: "skill:review", description: "Review changes" }],
  isProcessing: false,
  turnId: null,
  timestamp: 100,
};

describe("context inheritance reduction", () => {
  const inheritance = {
    origin: {
      mode: "fork" as const,
      parentThreadId: "parent",
      parentEntryId: "cutoff",
      snapshotEntryId: "seed",
    },
    inheritedItems: [
      {
        id: "inherited",
        kind: "message" as const,
        role: "assistant" as const,
        text: "Parent snapshot",
      },
    ],
  };

  it("replaces provenance atomically with own history and keeps inherited content out of normal items", () => {
    const state = threadReducer(initialState, {
      ...replacement,
      contextInheritance: inheritance,
    });
    expect(state.contextInheritanceByThread.new).toBe(inheritance);
    expect(state.itemsByThread.new).toEqual(replacement.items);
    expect(state.lastAgentMessageByThread).toEqual({});
  });

  it("clears inherited metadata on empty replacement and plain snapshot reset", () => {
    const seeded = threadReducer(initialState, {
      ...replacement,
      contextInheritance: inheritance,
    });
    const replaced = threadReducer(seeded, {
      ...replacement,
      previousThreadId: "new",
      items: [],
    });
    expect(replaced.contextInheritanceByThread.new).toBeNull();
    expect(replaced.itemsByThread.new).toEqual([]);
    const cleared = threadReducer(seeded, {
      type: "setThreadItems",
      threadId: "new",
      items: [],
    });
    expect(cleared.contextInheritanceByThread.new).toBeNull();
  });

  it("removes inherited snapshots together with the thread", () => {
    const seeded = threadReducer(initialState, {
      ...replacement,
      contextInheritance: inheritance,
    });
    const removed = threadReducer(seeded, {
      type: "removeThread",
      workspaceId: "workspace",
      threadId: "new",
    });
    expect(removed.contextInheritanceByThread.new).toBeUndefined();
  });
});

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
    const state = threadReducer(
      {
        ...initialState,
        activeThreadIdByWorkspace: { workspace: "old", other: "unrelated" },
      },
      replacement,
    );
    expect(state.activeThreadIdByWorkspace).toEqual({
      workspace: "new",
      other: "unrelated",
    });
    expect(state.itemsByThread.new).toEqual(replacement.items);
    expect(state.commandsByThread.new).toEqual(replacement.commands);
    expect(state.threadStatusById.new.isProcessing).toBe(false);
    expect(state.activeTurnIdByThread.new).toBeNull();
  });

  it("refreshes a background session without changing the user's selection", () => {
    const state = threadReducer(
      {
        ...initialState,
        activeThreadIdByWorkspace: { workspace: "unrelated" },
      },
      replacement,
    );
    expect(state.activeThreadIdByWorkspace.workspace).toBe("unrelated");
    expect(state.commandsByThread.new).toEqual(replacement.commands);
  });

  it("keeps one stable provider error notice across repeated replacements", () => {
    const errorNotice = {
      id: "error-2-0",
      kind: "tool" as const,
      toolType: "notice",
      title: "[error]",
      detail: "Provider unavailable",
      status: "failed",
    };
    let state = threadReducer(initialState, {
      type: "addNotice",
      threadId: "old",
      itemId: "transient-notice",
      text: "Plugin warning",
      level: "warning",
    });
    const snapshot = {
      ...replacement,
      threadId: "old",
      items: [...replacement.items, errorNotice],
    };
    state = threadReducer(state, snapshot);
    state = threadReducer(state, snapshot);
    state = threadReducer(state, snapshot);
    expect(
      state.itemsByThread.old.filter((item) => item.id === "error-2-0"),
    ).toEqual([errorNotice]);
    expect(
      state.itemsByThread.old.filter((item) => item.id === "transient-notice"),
    ).toHaveLength(1);
    expect(state.itemsByThread.old).toHaveLength(3);
  });

  it("reloads the same identity from snapshot while preserving displayed notices", () => {
    let state = threadReducer(initialState, {
      type: "addNotice",
      threadId: "old",
      itemId: "notice-1",
      text: "Plugin warning",
      level: "warning",
    });
    state = threadReducer(state, {
      ...replacement,
      threadId: "old",
      isProcessing: true,
      turnId: "active-run",
    });
    expect(state.itemsByThread.old).toEqual([
      ...replacement.items,
      {
        id: "notice-1",
        kind: "tool",
        toolType: "notice",
        title: "[warning]",
        detail: "Plugin warning",
        status: "completed",
      },
    ]);
    expect(state.threadStatusById.old.isProcessing).toBe(true);
    expect(state.activeTurnIdByThread.old).toBe("active-run");
    expect(state.commandsByThread.old).toEqual(replacement.commands);
    expect(state.lastAgentMessageByThread).toEqual({});
  });
});
