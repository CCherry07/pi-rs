import type { ThreadRunParams } from "./threadStorage";
import { makeThreadRunParamsKey } from "./threadStorage";

export const NO_THREAD_SCOPE_SUFFIX = "__no_thread__";

export function resolveThreadRunState(input: {
  workspaceId: string;
  threadId: string | null;
  lastComposerModelId: string | null;
  lastComposerReasoningEffort: string | null;
  stored: ThreadRunParams | null;
}) {
  return {
    scopeKey: makeThreadRunParamsKey(
      input.workspaceId,
      input.threadId ?? NO_THREAD_SCOPE_SUFFIX,
    ),
    preferredModelId:
      input.stored?.modelId ?? input.lastComposerModelId ?? null,
    preferredEffort:
      input.stored?.effort ?? input.lastComposerReasoningEffort ?? null,
  };
}
