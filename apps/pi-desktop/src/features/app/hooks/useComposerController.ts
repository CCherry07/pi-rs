import { useCallback, useMemo, useState } from "react";
import type {
  ComposerSendIntent,
  FollowUpMessageBehavior,
  QueuedMessage,
  SendMessageResult,
  WorkspaceInfo,
} from "../../../types";
import { useComposerImages } from "../../composer/hooks/useComposerImages";
import { useQueuedSend } from "../../threads/hooks/useQueuedSend";

export function useComposerController({
  activeThreadId,
  activeTurnId,
  activeWorkspaceId,
  activeWorkspace,
  isProcessing,
  queueFlushPaused = false,
  steerEnabled,
  followUpMessageBehavior,
  startThreadForWorkspace,
  sendUserMessage,
  sendUserMessageToThread,
  startFork,
  startResume,
  startCompact,
  startFast,
  startStatus,
}: {
  activeThreadId: string | null;
  activeTurnId: string | null;
  activeWorkspaceId: string | null;
  activeWorkspace: WorkspaceInfo | null;
  isProcessing: boolean;
  queueFlushPaused?: boolean;
  steerEnabled: boolean;
  followUpMessageBehavior: FollowUpMessageBehavior;
  startThreadForWorkspace: (
    workspaceId: string,
    options?: { activate?: boolean },
  ) => Promise<string | null>;
  sendUserMessage: (
    text: string,
    images?: string[],
    options?: { sendIntent?: ComposerSendIntent },
  ) => Promise<{ status: "sent" | "blocked" | "steer_failed" }>;
  sendUserMessageToThread: (
    workspace: WorkspaceInfo,
    threadId: string,
    text: string,
    images?: string[],
  ) => Promise<void | SendMessageResult>;
  startFork: (text: string) => Promise<void>;
  startResume: (text: string) => Promise<void>;
  startCompact: (text: string) => Promise<void>;
  startFast: (text: string) => Promise<void>;
  startStatus: (text: string) => Promise<void>;
}) {
  const [composerDraftsByThread, setComposerDraftsByThread] = useState<
    Record<string, string>
  >({});
  const [prefillDraft, setPrefillDraft] = useState<QueuedMessage | null>(null);
  const [composerInsert, setComposerInsert] = useState<QueuedMessage | null>(
    null,
  );

  const {
    activeImages,
    attachImages,
    pickImages,
    removeImage,
    clearActiveImages,
    setImagesForThread,
    removeImagesForThread,
  } = useComposerImages({ activeThreadId, activeWorkspaceId });

  const {
    activeQueue,
    handleSend,
    queueMessage,
    steerQueuedMessage,
    removeQueuedMessage,
  } = useQueuedSend({
    activeThreadId,
    activeTurnId,
    isProcessing,
    queueFlushPaused,
    steerEnabled,
    followUpMessageBehavior,
    activeWorkspace,
    startThreadForWorkspace,
    sendUserMessage,
    sendUserMessageToThread,
    startFork,
    startResume,
    startCompact,
    startFast,
    startStatus,
    clearActiveImages,
  });

  const activeDraft = useMemo(
    () =>
      activeThreadId ? composerDraftsByThread[activeThreadId] ?? "" : "",
    [activeThreadId, composerDraftsByThread],
  );

  const handleDraftChange = useCallback(
    (next: string) => {
      if (!activeThreadId) {
        return;
      }
      setComposerDraftsByThread((prev) => ({
        ...prev,
        [activeThreadId]: next,
      }));
    },
    [activeThreadId],
  );

  const handleSendPrompt = useCallback(
    (text: string) => {
      if (!text.trim()) {
        return;
      }
      void handleSend(text, []);
    },
    [handleSend],
  );

  const handleSteerQueued = useCallback(
    (item: QueuedMessage) => {
      if (!activeThreadId) {
        return;
      }
      void steerQueuedMessage(activeThreadId, item);
    },
    [activeThreadId, steerQueuedMessage],
  );

  const handleEditQueued = useCallback(
    (item: QueuedMessage) => {
      if (!activeThreadId) {
        return;
      }
      removeQueuedMessage(activeThreadId, item.id);
      setImagesForThread(activeThreadId, item.images ?? []);
      setPrefillDraft(item);
    },
    [activeThreadId, removeQueuedMessage, setImagesForThread],
  );

  const handleDeleteQueued = useCallback(
    (id: string) => {
      if (!activeThreadId) {
        return;
      }
      removeQueuedMessage(activeThreadId, id);
    },
    [activeThreadId, removeQueuedMessage],
  );

  const clearDraftForThread = useCallback((threadId: string) => {
    setComposerDraftsByThread((prev) => {
      if (!(threadId in prev)) {
        return prev;
      }
      const { [threadId]: _, ...rest } = prev;
      return rest;
    });
  }, []);

  return {
    activeImages,
    attachImages,
    pickImages,
    removeImage,
    clearActiveImages,
    setImagesForThread,
    removeImagesForThread,
    activeQueue,
    handleSend,
    queueMessage,
    removeQueuedMessage,
    prefillDraft,
    setPrefillDraft,
    composerInsert,
    setComposerInsert,
    activeDraft,
    handleDraftChange,
    handleSendPrompt,
    handleSteerQueued,
    handleEditQueued,
    handleDeleteQueued,
    clearDraftForThread,
  };
}
