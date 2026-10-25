import { useCallback, useMemo } from "react";
import type { Dispatch, MutableRefObject } from "react";
import type {
  PiEvent,
  CollabAgentRef,
  DebugEntry,
} from "@/types";
import { getPiEventRawMethod } from "@utils/piEvents";
import { useThreadItemEvents } from "./useThreadItemEvents";
import { useThreadTurnEvents } from "./useThreadTurnEvents";
import type { ThreadAction } from "./useThreadsReducer";

type ThreadEventHandlersOptions = {
  activeThreadId: string | null;
  dispatch: Dispatch<ThreadAction>;
  getCustomName: (workspaceId: string, threadId: string) => string | undefined;
  isThreadHidden: (workspaceId: string, threadId: string) => boolean;
  setThreadLoaded: (threadId: string, isLoaded: boolean) => void;
  markProcessing: (threadId: string, isProcessing: boolean) => void;
  setActiveTurnId: (threadId: string, turnId: string | null) => void;
  getActiveTurnId: (threadId: string) => string | null;
  safeMessageActivity: () => void;
  recordThreadActivity: (
    workspaceId: string,
    threadId: string,
    timestamp?: number,
  ) => void;
  onUserMessageCreated?: (
    workspaceId: string,
    threadId: string,
    text: string,
  ) => void | Promise<void>;
  pushThreadErrorMessage: (threadId: string, message: string) => void;
  onDebug?: (entry: DebugEntry) => void;
  applyCollabThreadLinks: (
    workspaceId: string,
    threadId: string,
    item: Record<string, unknown>,
  ) => void;
  hydrateSubagentThreads?: (
    workspaceId: string,
    receivers: CollabAgentRef[],
  ) => void | Promise<void>;
  pendingInterruptsRef: MutableRefObject<Set<string>>;
};

export function useThreadEventHandlers({
  activeThreadId,
  dispatch,
  getCustomName,
  isThreadHidden,
  setThreadLoaded,
  markProcessing,
  setActiveTurnId,
  getActiveTurnId,
  safeMessageActivity,
  recordThreadActivity,
  onUserMessageCreated,
  pushThreadErrorMessage,
  onDebug,
  applyCollabThreadLinks,
  hydrateSubagentThreads,
  pendingInterruptsRef,
}: ThreadEventHandlersOptions) {

  const {
    onAgentMessageDelta,
    onAgentMessageCompleted,
    onItemStarted,
    onItemCompleted,
    onReasoningTextDelta,
  } = useThreadItemEvents({
    activeThreadId,
    dispatch,
    getCustomName,
    markProcessing,
    safeMessageActivity,
    recordThreadActivity,
    applyCollabThreadLinks,
    hydrateSubagentThreads,
    onUserMessageCreated,
  });

  const {
    onThreadStarted,
    onThreadNameUpdated,
    onThreadArchived,
    onThreadUnarchived,
    onTurnStarted,
    onTurnCompleted,
    onThreadStatusChanged,
    onThreadTokenUsageUpdated,
    onTurnError,
  } = useThreadTurnEvents({
    dispatch,
    getCustomName,
    isThreadHidden,
    setThreadLoaded,
    markProcessing,
    setActiveTurnId,
    getActiveTurnId,
    pendingInterruptsRef,
    pushThreadErrorMessage,
    safeMessageActivity,
    recordThreadActivity,
  });

  const onPiEvent = useCallback(
    (event: PiEvent) => {
      const method = getPiEventRawMethod(event) ?? "";
      onDebug?.({
        id: `${Date.now()}-server-event`,
        timestamp: Date.now(),
        source: "event",
        label: method || "event",
        payload: event,
      });
    },
    [onDebug],
  );

  const handlers = useMemo(
    () => ({
      onPiEvent,
      onAgentMessageDelta,
      onAgentMessageCompleted,
      onItemStarted,
      onItemCompleted,
      onReasoningTextDelta,
      onThreadStarted,
      onThreadNameUpdated,
      onThreadArchived,
      onThreadUnarchived,
      onTurnStarted,
      onTurnCompleted,
      onThreadStatusChanged,
      onThreadTokenUsageUpdated,
      onTurnError,
    }),
    [
      onPiEvent,
      onAgentMessageDelta,
      onAgentMessageCompleted,
      onItemStarted,
      onItemCompleted,
      onReasoningTextDelta,
      onThreadStarted,
      onThreadNameUpdated,
      onThreadArchived,
      onThreadUnarchived,
      onTurnStarted,
      onTurnCompleted,
      onThreadStatusChanged,
      onThreadTokenUsageUpdated,
      onTurnError,
    ],
  );

  return handlers;
}
