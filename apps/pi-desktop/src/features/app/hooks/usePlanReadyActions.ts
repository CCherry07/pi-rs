import { useCallback } from "react";
import type { SendMessageResult, WorkspaceInfo } from "../../../types";
import {
  makePlanReadyAcceptMessage,
  makePlanReadyChangesMessage,
} from "../../../utils/internalPlanReadyMessages";

type SendUserMessageToThread = (
  workspace: WorkspaceInfo,
  threadId: string,
  message: string,
  imageIds: string[],
) => Promise<void | SendMessageResult>;

type UsePlanReadyActionsOptions = {
  activeWorkspace: WorkspaceInfo | null;
  activeThreadId: string | null;
  sendUserMessageToThread: SendUserMessageToThread;
};

export function usePlanReadyActions({
  activeWorkspace,
  activeThreadId,
  sendUserMessageToThread,
}: UsePlanReadyActionsOptions) {
  const handlePlanAccept = useCallback(async () => {
    if (!activeWorkspace || !activeThreadId) {
      return;
    }

    await sendUserMessageToThread(
      activeWorkspace,
      activeThreadId,
      makePlanReadyAcceptMessage(),
      [],
    );
  }, [
    activeThreadId,
    activeWorkspace,
    sendUserMessageToThread,
  ]);

  const handlePlanSubmitChanges = useCallback(
    async (changes: string) => {
      const trimmed = changes.trim();
      if (!activeWorkspace || !activeThreadId || !trimmed) {
        return;
      }

      const message = makePlanReadyChangesMessage(trimmed);
      await sendUserMessageToThread(
        activeWorkspace,
        activeThreadId,
        message,
        [],
      );
    },
    [
      activeThreadId,
      activeWorkspace,
      sendUserMessageToThread,
    ],
  );

  return {
    handlePlanAccept,
    handlePlanSubmitChanges,
  };
}
