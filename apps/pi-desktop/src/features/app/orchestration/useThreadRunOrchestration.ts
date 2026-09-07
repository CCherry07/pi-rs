import { useCallback, useMemo, useRef, useState } from "react";
import type { Dispatch, MutableRefObject, SetStateAction } from "react";
import type { ServiceTier } from "@/types";
import { useThreadRunParams } from "@threads/hooks/useThreadRunParams";
import {
  type PendingNewThreadSeed,
  NO_THREAD_SCOPE_SUFFIX,
} from "@threads/utils/threadRunParamsSeed";

type ThreadRunOrchestration = {
  preferredModelId: string | null;
  setPreferredModelId: Dispatch<SetStateAction<string | null>>;
  preferredEffort: string | null;
  setPreferredEffort: Dispatch<SetStateAction<string | null>>;
  preferredServiceTier: ServiceTier | null | undefined;
  setPreferredServiceTier: Dispatch<SetStateAction<ServiceTier | null | undefined>>;
  threadRunSelectionKey: string | null;
  setThreadRunSelectionKey: Dispatch<SetStateAction<string | null>>;
  threadRunParamsVersion: number;
  getThreadRunParams: ReturnType<typeof useThreadRunParams>["getThreadRunParams"];
  patchThreadRunParams: ReturnType<typeof useThreadRunParams>["patchThreadRunParams"];
  persistThreadRunParams: (patch: {
    modelId?: string | null;
    effort?: string | null;
    serviceTier?: ServiceTier | null | undefined;
  }) => void;
  activeThreadIdRef: MutableRefObject<string | null>;
  pendingNewThreadSeedRef: MutableRefObject<PendingNewThreadSeed | null>;
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
  const [preferredServiceTier, setPreferredServiceTier] = useState<
    ServiceTier | null | undefined
  >(undefined);
  const [threadRunSelectionKey, setThreadRunSelectionKey] = useState<string | null>(
    null,
  );
  const activeThreadIdRef = useRef<string | null>(null);
  const pendingNewThreadSeedRef = useRef<PendingNewThreadSeed | null>(null);

  const persistThreadRunParams = useCallback(
    (patch: {
      modelId?: string | null;
      effort?: string | null;
      serviceTier?: ServiceTier | null | undefined;
    }) => {
      const workspaceId = activeWorkspaceIdForParamsRef.current;
      const threadId = activeThreadIdRef.current ?? NO_THREAD_SCOPE_SUFFIX;
      if (!workspaceId) {
        return;
      }
      patchThreadRunParams(workspaceId, threadId, patch);
      if (
        activeThreadIdRef.current &&
        Object.prototype.hasOwnProperty.call(patch, "serviceTier")
      ) {
        patchThreadRunParams(workspaceId, NO_THREAD_SCOPE_SUFFIX, {
          serviceTier: patch.serviceTier,
        });
      }
    },
    [activeWorkspaceIdForParamsRef, patchThreadRunParams],
  );

  return useMemo(
    () => ({
      preferredModelId,
      setPreferredModelId,
      preferredEffort,
      setPreferredEffort,
      preferredServiceTier,
      setPreferredServiceTier,
      threadRunSelectionKey,
      setThreadRunSelectionKey,
      threadRunParamsVersion,
      getThreadRunParams,
      patchThreadRunParams,
      persistThreadRunParams,
      activeThreadIdRef,
      pendingNewThreadSeedRef,
    }),
    [
      preferredEffort,
      preferredModelId,
      preferredServiceTier,
      threadRunSelectionKey,
      threadRunParamsVersion,
      getThreadRunParams,
      patchThreadRunParams,
      persistThreadRunParams,
    ],
  );
}
