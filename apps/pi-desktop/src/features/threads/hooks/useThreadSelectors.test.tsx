// @vitest-environment jsdom
import { renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { ConversationItem } from "@/types";
import { useThreadSelectors } from "./useThreadSelectors";

const messageItem: ConversationItem = {
  id: "item-1",
  kind: "message",
  role: "user",
  text: "Hello",
};

describe("useThreadSelectors", () => {
  it("returns active thread id and items for the active workspace", () => {
    const { result } = renderHook(() =>
      useThreadSelectors({
        activeWorkspaceId: "workspace-1",
        activeThreadIdByWorkspace: { "workspace-1": "thread-1" },
        itemsByThread: { "thread-1": [messageItem] },
        threadsByWorkspace: {},
      }),
    );

    expect(result.current.activeThreadId).toBe("thread-1");
    expect(result.current.activeItems).toEqual([messageItem]);
  });

  it("returns null and empty items when there is no active workspace", () => {
    const { result } = renderHook(() =>
      useThreadSelectors({
        activeWorkspaceId: null,
        activeThreadIdByWorkspace: { "workspace-1": "thread-1" },
        itemsByThread: { "thread-1": [messageItem] },
        threadsByWorkspace: {},
      }),
    );

    expect(result.current.activeThreadId).toBeNull();
    expect(result.current.activeItems).toEqual([]);
  });

  it("returns empty items when the active thread has no entries", () => {
    const { result } = renderHook(() =>
      useThreadSelectors({
        activeWorkspaceId: "workspace-1",
        activeThreadIdByWorkspace: { "workspace-1": "thread-2" },
        itemsByThread: {},
        threadsByWorkspace: {},
      }),
    );

    expect(result.current.activeThreadId).toBe("thread-2");
    expect(result.current.activeItems).toEqual([]);
  });

  it("enriches collab tool items from active workspace thread metadata", () => {
    const collabItem: ConversationItem = {
      id: "collab-1",
      kind: "tool",
      toolType: "collabToolCall",
      title: "Collab: spawn_agent",
      detail: "From thread-parent → thread-child",
      status: "completed",
      output: "Investigate the issue\n\nthread-child: completed",
      collabSender: { threadId: "thread-parent" },
      collabReceiver: { threadId: "thread-child" },
      collabReceivers: [{ threadId: "thread-child" }],
      collabStatuses: [{ threadId: "thread-child", status: "completed" }],
    };

    const { result } = renderHook(() =>
      useThreadSelectors({
        activeWorkspaceId: "workspace-1",
        activeThreadIdByWorkspace: { "workspace-1": "thread-parent" },
        itemsByThread: { "thread-parent": [collabItem] },
        threadsByWorkspace: {
          "workspace-1": [
            {
              id: "thread-child",
              name: "Review helper",
              updatedAt: 1,
              isSubagent: true,
              subagentNickname: "Atlas",
              subagentRole: "reviewer",
            },
          ],
        },
      }),
    );

    expect(result.current.activeItems).toEqual([
      {
        ...collabItem,
        detail: "From thread-parent → Atlas [reviewer]",
        output: "Investigate the issue\n\nAtlas [reviewer]: completed",
        collabReceiver: {
          threadId: "thread-child",
          nickname: "Atlas",
          role: "reviewer",
        },
        collabReceivers: [
          {
            threadId: "thread-child",
            nickname: "Atlas",
            role: "reviewer",
          },
        ],
        collabStatuses: [
          {
            threadId: "thread-child",
            nickname: "Atlas",
            role: "reviewer",
            status: "completed",
          },
        ],
      },
    ]);
  });
});
