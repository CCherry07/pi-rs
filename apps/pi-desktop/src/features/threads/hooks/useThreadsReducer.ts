import type { RuntimeCommand } from "@utils/desktopCommands";
import type {
  ConversationItem,
  ThreadListSortKey,
  ThreadContextInheritance,
  ThreadSummary,
  ThreadTokenUsage,
  TurnPlan,
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
  turnDiffByThread: Record<string, string>;
  tokenUsageByThread: Record<string, ThreadTokenUsage>;
  planByThread: Record<string, TurnPlan | null>;
  lastAgentMessageByThread: Record<string, { text: string; timestamp: number }>;
};

export type ThreadAction =
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
  | { type: "setThreadContextInheritance"; threadId: string; contextInheritance: ThreadContextInheritance | null }
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
  | { type: "addNotice"; threadId: string; itemId: string; text: string; level: string }
  | { type: "setThreadName"; workspaceId: string; threadId: string; name: string }
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
  | { type: "setThreadItems"; threadId: string; items: ConversationItem[]; contextInheritance?: ThreadContextInheritance | null }
  | {
      type: "hydrateThreadItems";
      threadId: string;
      items: ConversationItem[];
      itemsAtRequest: ConversationItem[];
      contextInheritance?: {
        atRequest: ThreadContextInheritance | null | undefined;
        value: ThreadContextInheritance | null;
      };
      activity?: {
        statusAtRequest: ThreadActivityStatus | undefined;
        activeTurnIdAtRequest: string | null | undefined;
        isProcessing: boolean;
        turnId: string | null;
        timestamp: number;
      };
      usage?: {
        atRequest: ThreadTokenUsage | undefined;
        value: ThreadTokenUsage;
      };
    }
  | {
      type: "appendReasoningSummary";
      threadId: string;
      itemId: string;
      delta: string;
    }
  | {
      type: "appendReasoningSummaryBoundary";
      threadId: string;
      itemId: string;
    }
  | { type: "appendReasoningContent"; threadId: string; itemId: string; delta: string }
  | { type: "appendPlanDelta"; threadId: string; itemId: string; delta: string }
  | { type: "appendToolOutput"; threadId: string; itemId: string; delta: string }
  | {
      type: "setThreads";
      workspaceId: string;
      threads: ThreadSummary[];
      sortKey: ThreadListSortKey;
      preserveAnchors?: boolean;
    }
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
  | { type: "setThreadTokenUsage"; threadId: string; tokenUsage: ThreadTokenUsage }
  | { type: "setActiveTurnId"; threadId: string; turnId: string | null }
  | { type: "setThreadTurnDiff"; threadId: string; diff: string }
  | { type: "setThreadPlan"; threadId: string; plan: TurnPlan | null }
  | { type: "clearThreadPlan"; threadId: string }
  | {
      type: "setLastAgentMessage";
      threadId: string;
      text: string;
      timestamp: number;
    };

const emptyItems: Record<string, ConversationItem[]> = {};

export const initialState: ThreadState = {
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
  turnDiffByThread: {},
  tokenUsageByThread: {},
  planByThread: {},
  lastAgentMessageByThread: {},
};

type ThreadSliceReducer = (state: ThreadState, action: ThreadAction) => ThreadState;

const threadSliceReducers: ThreadSliceReducer[] = [
  reduceThreadLifecycle,
  reduceThreadConfig,
  reduceThreadItems,
  reduceThreadSnapshots,
];

export function threadReducer(state: ThreadState, action: ThreadAction): ThreadState {
  if (action.type === "hydrateThreadItems") {
    let next = reduceThreadItems(state, action);
    const { activity, usage, contextInheritance, threadId } = action;
    if (contextInheritance && state.contextInheritanceByThread[threadId] === contextInheritance.atRequest) {
      next = reduceThreadSnapshots(next, {
        type: "setThreadContextInheritance", threadId, contextInheritance: contextInheritance.value,
      });
    }
    if (activity && state.threadStatusById[threadId] === activity.statusAtRequest &&
        state.activeTurnIdByThread[threadId] === activity.activeTurnIdAtRequest) {
      next = reduceThreadLifecycle(next, {
        type: "markProcessing", threadId,
        isProcessing: activity.isProcessing, timestamp: activity.timestamp,
      });
      next = reduceThreadLifecycle(next, {
        type: "setActiveTurnId", threadId, turnId: activity.turnId,
      });
    }
    const currentUsage = state.tokenUsageByThread[threadId];
    if (usage && currentUsage === usage.atRequest &&
        (currentUsage?.totalTokens == null ||
          (usage.value.totalTokens !== null && usage.value.totalTokens >= currentUsage.totalTokens))) {
      next = reduceThreadSnapshots(next, {
        type: "setThreadTokenUsage", threadId, tokenUsage: usage.value,
      });
    }
    return next;
  }
  if (action.type === "replaceThread") {
    // Keep transient notices on reload without duplicating persisted snapshot errors.
    const snapshotIds = new Set(action.items.map((item) => item.id));
    const notices = action.previousThreadId === action.threadId
      ? (state.itemsByThread[action.threadId] ?? []).filter(
          (item) => item.kind === "tool" && item.toolType === "notice" && !snapshotIds.has(item.id),
        )
      : [];
    let next = reduceThreadItems(state, {
      type: "setThreadItems", threadId: action.threadId, items: [...action.items, ...notices],
      contextInheritance: action.contextInheritance ?? null,
    });
    next = reduceThreadLifecycle(next, {
      type: "markProcessing", threadId: action.threadId,
      isProcessing: action.isProcessing, timestamp: action.timestamp,
    });
    return {
      ...next,
      commandsByThread: { ...next.commandsByThread, [action.threadId]: action.commands },
      activeTurnIdByThread: { ...next.activeTurnIdByThread, [action.threadId]: action.turnId },
      activeThreadIdByWorkspace: {
        ...next.activeThreadIdByWorkspace,
        [action.workspaceId]: state.activeThreadIdByWorkspace[action.workspaceId] === action.previousThreadId
          ? action.threadId : state.activeThreadIdByWorkspace[action.workspaceId] ?? null,
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
