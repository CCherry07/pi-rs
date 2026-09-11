import type { ThreadAction, ThreadState } from "../useThreadsReducer";

export function reduceThreadSnapshots(
  state: ThreadState,
  action: ThreadAction,
): ThreadState {
  switch (action.type) {
    case "setThreadContextInheritance":
      return {
        ...state,
        contextInheritanceByThread: {
          ...state.contextInheritanceByThread,
          [action.threadId]: action.contextInheritance,
        },
      };
    case "setThreadCommands":
      return {
        ...state,
        commandsByThread: { ...state.commandsByThread, [action.threadId]: action.commands },
      };
    case "setLastAgentMessage":
      if (
        state.lastAgentMessageByThread[action.threadId]?.timestamp >= action.timestamp
      ) {
        return state;
      }
      return {
        ...state,
        lastAgentMessageByThread: {
          ...state.lastAgentMessageByThread,
          [action.threadId]: { text: action.text, timestamp: action.timestamp },
        },
      };
    case "setThreadTokenUsage":
      return {
        ...state,
        tokenUsageByThread: {
          ...state.tokenUsageByThread,
          [action.threadId]: action.tokenUsage,
        },
      };
    case "setThreadTurnDiff":
      return {
        ...state,
        turnDiffByThread: {
          ...state.turnDiffByThread,
          [action.threadId]: action.diff,
        },
      };
    case "setThreadPlan":
      return {
        ...state,
        planByThread: {
          ...state.planByThread,
          [action.threadId]: action.plan,
        },
      };
    case "clearThreadPlan":
      return {
        ...state,
        planByThread: {
          ...state.planByThread,
          [action.threadId]: null,
        },
      };
    default:
      return state;
  }
}
