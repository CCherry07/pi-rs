import { createContext } from "react";
import type { ThreadState } from "../hooks/useThreadsReducer";

export type ThreadConversationsSource = {
  itemsByThread: ThreadState["itemsByThread"];
  contextInheritanceByThread?: ThreadState["contextInheritanceByThread"];
  threadStatusById: ThreadState["threadStatusById"];
  tokenUsageByThread: ThreadState["tokenUsageByThread"];
  loadThread: (workspaceId: string, threadId: string) => Promise<void>;
};

export const ThreadConversationsContext = createContext<ThreadConversationsSource | null>(null);
