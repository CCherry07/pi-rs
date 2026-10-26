import { useCallback, useMemo, useRef, useState } from "react";
import type { Dispatch, MutableRefObject, SetStateAction } from "react";
import { useThreadRunParams } from "@threads/hooks/useThreadRunParams";
import {
  NO_THREAD_SCOPE_SUFFIX,
} from "@threads/utils/threadRunParamsSeed";

type ThreadRunOrchestration = {
  preferredModelId: string | null;
  setPreferredModelId: Dispatch<SetStateAction<string | null>>;
  preferredEffort: string | null;
  setPreferredEffort: Dispatch<SetStateAction<string | null>>;
  threadRunSelectionKey: string | null;
  setThreadRunSelectionKey: Dispatch<SetStateAction<string | null>>;
  threadRunParamsVersion: number;
  getThreadRunParams: ReturnType<typeof useThreadRunParams>["getThreadRunParams"];
  patchThreadRunParams: ReturnType<typeof useThreadRunParams>["patchThreadRunParams"];
  persistThreadRunParams: (patch: {
    modelId?: string | null;
    effort?: string | null;
  }) => void;
  activeThreadIdRef: MutableRefObject<string | null>;
};

type UseThreadRunOrchestrationParams = {
  activeWorkspaceIdForParamsRef: MutableRefObject<string | null>;
};

export function useThreadRunOrchestration({
  activeWorkspaceIdForParamsRef,
}: UseThreadRunOrchestrationParams): ThreadRunOrchestration {
  const {
    version: threadRunParamsVersion,
    getThreadRunParams,
    patchThreadRunParams,
  } = useThreadRunParams();
  const [preferredModelId, setPreferredModelId] = useState<string | null>(null);
  const [preferredEffort, setPreferredEffort] = useState<string | null>(null);
  const [threadRunSelectionKey, setThreadRunSelectionKey] = useState<string | null>(
    null,
  );
  const activeThreadIdRef = useRef<string | null>(null);

  const persistThreadRunParams = useCallback(
    (patch: {
      modelId?: string | null;
      effort?: string | null;
    }) => {
      const workspaceId = activeWorkspaceIdForParamsRef.current;
      const threadId = activeThreadIdRef.current ?? NO_THREAD_SCOPE_SUFFIX;
      if (!workspaceId) {
        return;
      }
      patchThreadRunParams(workspaceId, threadId, patch);
    },
    [activeWorkspaceIdForParamsRef, patchThreadRunParams],
  );

  return useMemo(
    () => ({
      preferredModelId,
      setPreferredModelId,
      preferredEffort,
      setPreferredEffort,
      threadRunSelectionKey,
      setThreadRunSelectionKey,
      threadRunParamsVersion,
      getThreadRunParams,
      patchThreadRunParams,
      persistThreadRunParams,
      activeThreadIdRef,
    }),
    [
      preferredEffort,
      preferredModelId,
      threadRunSelectionKey,
      threadRunParamsVersion,
      getThreadRunParams,
      patchThreadRunParams,
      persistThreadRunParams,
    ],
  );
}
