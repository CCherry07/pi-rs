import type { RuntimeCommand } from "@utils/desktopCommands";
import type {
  ConversationItem,
  ThreadListSortKey,
  ThreadContextInheritance,
  ThreadSummary,
  ThreadTokenUsage,
} from "@/types";
import { CHAT_SCROLLBACK_DEFAULT } from "@utils/chatScrollback";
import { reduceThreadItems } from "./threadReducer/threadItemsSlice";
import { reduceThreadLifecycle } from "./threadReducer/threadLifecycleSlice";
import { reduceThreadConfig } from "./threadReducer/threadConfigSlice";
import { reduceThreadSnapshots } from "./threadReducer/threadSnapshotSlice";

type ThreadActivityStatus = {
  isProcessing: boolean;
  hasUnread: boolean;
  processingStartedAt: number | null;
  lastDurationMs: number | null;
};

export type ThreadState = {
  activeThreadIdByWorkspace: Record<string, string | null>;
  itemsByThread: Record<string, ConversationItem[]>;
  historyPreviewByThread: Record<string, boolean>;
  contextInheritanceByThread: Record<string, ThreadContextInheritance | null>;
  commandsByThread: Record<string, RuntimeCommand[]>;
  maxItemsPerThread: number | null;
  threadsByWorkspace: Record<string, ThreadSummary[]>;
  hiddenThreadIdsByWorkspace: Record<string, Record<string, true>>;
  threadParentById: Record<string, string>;
  threadStatusById: Record<string, ThreadActivityStatus>;
  threadResumeLoadingById: Record<string, boolean>;
  threadListLoadingByWorkspace: Record<string, boolean>;
  threadListPagingByWorkspace: Record<string, boolean>;
  threadListCursorByWorkspace: Record<string, string | null>;
  threadSortKeyByWorkspace: Record<string, ThreadListSortKey>;
  activeTurnIdByThread: Record<string, string | null>;
  tokenUsageByThread: Record<string, ThreadTokenUsage>;
  lastAgentMessageByThread: Record<string, { text: string; timestamp: number }>;
};

export type ThreadAction =
  | {
      type: "hydrateThreadPreview";
      threadId: string;
      items: ConversationItem[];
      contextInheritance: ThreadContextInheritance | null;
    }
  | {
      type: "hydrateResumedThread";
      threadId: string;
      items: ConversationItem[];
      contextInheritance: ThreadContextInheritance | null;
      replaceLocal: boolean;
      isProcessing: boolean;
      timestamp: number;
      turnId: string | null;
    }
  | { type: "setActiveThreadId"; workspaceId: string; threadId: string | null }
  | {
      type: "replaceThread";
      workspaceId: string;
      previousThreadId: string;
      threadId: string;
      items: ConversationItem[];
      contextInheritance?: ThreadContextInheritance | null;
      commands: RuntimeCommand[];
      isProcessing: boolean;
      turnId: string | null;
      timestamp: number;
    }
  | { type: "setThreadCommands"; threadId: string; commands: RuntimeCommand[] }
  | {
      type: "setThreadContextInheritance";
      threadId: string;
      contextInheritance: ThreadContextInheritance | null;
    }
  | { type: "setMaxItemsPerThread"; maxItemsPerThread: number | null }
  | { type: "ensureThread"; workspaceId: string; threadId: string }
  | { type: "hideThread"; workspaceId: string; threadId: string }
  | { type: "removeThread"; workspaceId: string; threadId: string }
  | { type: "setThreadParent"; threadId: string; parentId: string }
  | {
      type: "markProcessing";
      threadId: string;
      isProcessing: boolean;
      timestamp: number;
    }
  | { type: "markUnread"; threadId: string; hasUnread: boolean }
  | { type: "addAssistantMessage"; threadId: string; text: string }
  | {
      type: "addNotice";
      threadId: string;
      itemId: string;
      text: string;
      level: string;
    }
  | {
      type: "setThreadName";
      workspaceId: string;
      threadId: string;
      name: string;
    }
  | {
      type: "mergeThreadSummary";
      workspaceId: string;
      threadId: string;
      patch: Partial<
        Pick<
          ThreadSummary,
          | "messageCount"
          | "modelId"
          | "effort"
          | "isSubagent"
          | "subagentNickname"
          | "subagentRole"
          | "createdAt"
        >
      >;
    }
  | {
      type: "setThreadTimestamp";
      workspaceId: string;
      threadId: string;
      timestamp: number;
    }
  | {
      type: "appendAgentDelta";
      workspaceId: string;
      threadId: string;
      itemId: string;
      delta: string;
      hasCustomName: boolean;
    }
  | {
      type: "completeAgentMessage";
      workspaceId: string;
      threadId: string;
      itemId: string;
      text: string;
      hasCustomName: boolean;
    }
  | {
      type: "upsertItem";
      workspaceId: string;
      threadId: string;
      item: ConversationItem;
      hasCustomName?: boolean;
    }
  | {
      type: "setThreadItems";
      threadId: string;
      items: ConversationItem[];
      contextInheritance?: ThreadContextInheritance | null;
    }
  | {
      type: "appendReasoningContent";
      threadId: string;
      itemId: string;
      delta: string;
    }
  | {
      type: "setThreads";
      workspaceId: string;
      threads: ThreadSummary[];
      sortKey: ThreadListSortKey;
      preserveAnchors?: boolean;
    }
  | { type: "appendThreads"; workspaceId: string; threads: ThreadSummary[] }
  | {
      type: "setThreadListLoading";
      workspaceId: string;
      isLoading: boolean;
    }
  | {
      type: "setThreadResumeLoading";
      threadId: string;
      isLoading: boolean;
    }
  | {
      type: "setThreadListPaging";
      workspaceId: string;
      isLoading: boolean;
    }
  | {
      type: "setThreadListCursor";
      workspaceId: string;
      cursor: string | null;
    }
  | {
      type: "setThreadTokenUsage";
      threadId: string;
      tokenUsage: ThreadTokenUsage;
    }
  | { type: "setActiveTurnId"; threadId: string; turnId: string | null }
  | {
      type: "setLastAgentMessage";
      threadId: string;
      text: string;
      timestamp: number;
    };

const emptyItems: Record<string, ConversationItem[]> = {};

export const initialState: ThreadState = {
  historyPreviewByThread: {},
  activeThreadIdByWorkspace: {},
  itemsByThread: emptyItems,
  contextInheritanceByThread: {},
  commandsByThread: {},
  maxItemsPerThread: CHAT_SCROLLBACK_DEFAULT,
  threadsByWorkspace: {},
  hiddenThreadIdsByWorkspace: {},
  threadParentById: {},
  threadStatusById: {},
  threadResumeLoadingById: {},
  threadListLoadingByWorkspace: {},
  threadListPagingByWorkspace: {},
  threadListCursorByWorkspace: {},
  threadSortKeyByWorkspace: {},
  activeTurnIdByThread: {},
  tokenUsageByThread: {},
  lastAgentMessageByThread: {},
};

type ThreadSliceReducer = (
  state: ThreadState,
  action: ThreadAction,
) => ThreadState;

const threadSliceReducers: ThreadSliceReducer[] = [
  reduceThreadLifecycle,
  reduceThreadConfig,
  reduceThreadItems,
  reduceThreadSnapshots,
];

export function threadReducer(
  state: ThreadState,
  action: ThreadAction,
): ThreadState {
  const next = reduceThread(state, action);
  if (
    action.type !== "hydrateThreadPreview" &&
    action.type !== "addNotice" &&
    "threadId" in action &&
    action.threadId
  ) {
    const id = action.threadId;
    const startedTurn =
      (action.type === "markProcessing" && action.isProcessing) ||
      (action.type === "setActiveTurnId" && action.turnId !== null);
    if (
      state.historyPreviewByThread[id] !== undefined &&
      (startedTurn || next.itemsByThread[id] !== state.itemsByThread[id])
    ) {
      const historyPreviewByThread = { ...next.historyPreviewByThread };
      delete historyPreviewByThread[id];
      return { ...next, historyPreviewByThread };
    }
  }
  return next;
}

function reduceThread(state: ThreadState, action: ThreadAction): ThreadState {
  if (action.type === "hydrateThreadPreview") {
    // A slow disk snapshot must never replace live/optimistic messages.
    const current = state.itemsByThread[action.threadId] ?? [];
    if (current.some((item) => item.kind !== "tool" || item.toolType !== "notice")) {
      return state;
    }
    const next = reduceThreadItems(state, {
      ...action,
      type: "setThreadItems",
      items: [...action.items, ...current],
    });
    return {
      ...next,
      historyPreviewByThread: {
        ...next.historyPreviewByThread,
        [action.threadId]: !(
          state.threadStatusById[action.threadId]?.isProcessing ||
          state.activeTurnIdByThread[action.threadId]
        ),
      },
    };
  }
  if (action.type === "hydrateResumedThread") {
    // Only a preview is replaceable automatically. Events arriving since the
    // request began take precedence, even when React batches both updates.
    if (
      !action.replaceLocal &&
      !state.historyPreviewByThread[action.threadId] &&
      state.itemsByThread[action.threadId]?.length
    ) {
      return state;
    }
    const ids = new Set(action.items.map((item) => item.id));
    const notices = (state.itemsByThread[action.threadId] ?? []).filter(
      (item) => item.kind === "tool" && item.toolType === "notice" && !ids.has(item.id),
    );
    let next = reduceThreadItems(state, {
      ...action,
      type: "setThreadItems",
      items: [...action.items, ...notices],
    });
    next = reduceThreadLifecycle(next, { ...action, type: "markProcessing" });
    return reduceThreadLifecycle(next, { ...action, type: "setActiveTurnId" });
  }
  if (action.type === "replaceThread") {
    // Keep transient notices on reload without duplicating persisted snapshot errors.
    const snapshotIds = new Set(action.items.map((item) => item.id));
    const notices =
      action.previousThreadId === action.threadId
        ? (state.itemsByThread[action.threadId] ?? []).filter(
            (item) =>
              item.kind === "tool" &&
              item.toolType === "notice" &&
              !snapshotIds.has(item.id),
          )
        : [];
    let next = reduceThreadItems(state, {
      type: "setThreadItems",
      threadId: action.threadId,
      items: [...action.items, ...notices],
      contextInheritance: action.contextInheritance ?? null,
    });
    next = reduceThreadLifecycle(next, {
      type: "markProcessing",
      threadId: action.threadId,
      isProcessing: action.isProcessing,
      timestamp: action.timestamp,
    });
    return {
      ...next,
      commandsByThread: {
        ...next.commandsByThread,
        [action.threadId]: action.commands,
      },
      activeTurnIdByThread: {
        ...next.activeTurnIdByThread,
        [action.threadId]: action.turnId,
      },
      activeThreadIdByWorkspace: {
        ...next.activeThreadIdByWorkspace,
        [action.workspaceId]:
          state.activeThreadIdByWorkspace[action.workspaceId] ===
          action.previousThreadId
            ? action.threadId
            : (state.activeThreadIdByWorkspace[action.workspaceId] ?? null),
      },
    };
  }
  for (const reduceSlice of threadSliceReducers) {
    const nextState = reduceSlice(state, action);
    if (nextState !== state) {
      return nextState;
    }
  }
  return state;
}
