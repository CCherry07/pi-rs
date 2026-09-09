import { useCallback, useRef } from "react";
import type { Dispatch } from "react";
import type { ThreadAction } from "./useThreadsReducer";

type UseThreadStatusOptions = {
  dispatch: Dispatch<ThreadAction>;
};

export function useThreadStatus({ dispatch }: UseThreadStatusOptions) {
  // Updated synchronously, including lifecycle events batched before a React render.
  const revisions = useRef<Record<string, number>>({});
  const getStatusRevision = useCallback((threadId: string) => revisions.current[threadId] ?? 0, []);
  const markProcessing = useCallback(
    (threadId: string, isProcessing: boolean) => {
      revisions.current[threadId] = (revisions.current[threadId] ?? 0) + 1;
      dispatch({
        type: "markProcessing",
        threadId,
        isProcessing,
        timestamp: Date.now(),
      });
    },
    [dispatch],
  );

  const setActiveTurnId = useCallback(
    (threadId: string, turnId: string | null) => {
      revisions.current[threadId] = (revisions.current[threadId] ?? 0) + 1;
      dispatch({ type: "setActiveTurnId", threadId, turnId });
    },
    [dispatch],
  );

  return { markProcessing, setActiveTurnId, getStatusRevision };
}
