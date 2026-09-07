import { useCallback } from "react";
import type { ThreadRunParams } from "@threads/utils/threadStorage";

type ThreadRunParamsPatch = Partial<
  Pick<ThreadRunParams, "modelId" | "effort" | "serviceTier">
>;

type ThreadRunMetadata = {
  modelId: string | null;
  effort: string | null;
};

type UseMainAppThreadRunStateArgs = {
  getThreadRunParams: (
    workspaceId: string,
    threadId: string,
  ) => ThreadRunParams | null;
  patchThreadRunParams: (
    workspaceId: string,
    threadId: string,
    patch: ThreadRunParamsPatch,
  ) => void;
};

export function useMainAppThreadRunState({
  getThreadRunParams,
  patchThreadRunParams,
}: UseMainAppThreadRunStateArgs) {
  const handleThreadRunMetadataDetected = useCallback(
    (workspaceId: string, threadId: string, metadata: ThreadRunMetadata) => {
      if (!workspaceId || !threadId) {
        return;
      }

      const modelId =
        typeof metadata.modelId === "string" && metadata.modelId.trim().length > 0
          ? metadata.modelId.trim()
          : null;
      const effort =
        typeof metadata.effort === "string" && metadata.effort.trim().length > 0
          ? metadata.effort.trim().toLowerCase()
          : null;
      if (!modelId && !effort) {
        return;
      }

      const current = getThreadRunParams(workspaceId, threadId);
      const patch: ThreadRunParamsPatch = {};
      if (modelId && !current?.modelId) {
        patch.modelId = modelId;
      }
      if (effort && !current?.effort) {
        patch.effort = effort;
      }
      if (Object.keys(patch).length === 0) {
        return;
      }
      patchThreadRunParams(workspaceId, threadId, patch);
    },
    [getThreadRunParams, patchThreadRunParams],
  );

  return { handleThreadRunMetadataDetected };
}
