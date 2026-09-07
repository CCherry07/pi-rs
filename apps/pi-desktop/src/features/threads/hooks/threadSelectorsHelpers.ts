import type { ConversationItem, ThreadSummary } from "@/types";
import { enrichConversationItemsWithThreads } from "@utils/threadItems";

export function getActiveThreadIdForWorkspace(
  activeWorkspaceId: string | null,
  activeThreadIdByWorkspace: Record<string, string | null | undefined>,
) {
  if (!activeWorkspaceId) {
    return null;
  }
  return activeThreadIdByWorkspace[activeWorkspaceId] ?? null;
}

export function getActiveItemsForThread({
  activeThreadId,
  itemsByThread,
  threads,
}: {
  activeThreadId: string | null;
  itemsByThread: Record<string, ConversationItem[]>;
  threads: ThreadSummary[] | undefined;
}) {
  if (!activeThreadId) {
    return [];
  }
  const items = itemsByThread[activeThreadId] ?? [];
  return enrichConversationItemsWithThreads(items, threads ?? []);
}
