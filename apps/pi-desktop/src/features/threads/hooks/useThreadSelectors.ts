import { useMemo } from "react";
import type { ConversationItem } from "@/types";
import type { ThreadState } from "./useThreadsReducer";
import { enrichConversationItemsWithThreads } from "@utils/threadItems";

const EMPTY_ITEMS: ConversationItem[] = [];

type UseThreadSelectorsOptions = {
  activeWorkspaceId: string | null;
  activeThreadIdByWorkspace: ThreadState["activeThreadIdByWorkspace"];
  itemsByThread: ThreadState["itemsByThread"];
  threadsByWorkspace: ThreadState["threadsByWorkspace"];
};

export function useThreadSelectors({
  activeWorkspaceId,
  activeThreadIdByWorkspace,
  itemsByThread,
  threadsByWorkspace,
}: UseThreadSelectorsOptions) {
  const activeThreadId = activeWorkspaceId
    ? (activeThreadIdByWorkspace[activeWorkspaceId] ?? null)
    : null;
  const items = activeThreadId
    ? (itemsByThread[activeThreadId] ?? EMPTY_ITEMS)
    : EMPTY_ITEMS;
  const activeWorkspaceThreads = activeWorkspaceId
    ? threadsByWorkspace[activeWorkspaceId]
    : undefined;
  const activeItems = useMemo(
    () =>
      enrichConversationItemsWithThreads(items, activeWorkspaceThreads ?? []),
    [items, activeWorkspaceThreads],
  );

  return { activeThreadId, activeItems };
}
