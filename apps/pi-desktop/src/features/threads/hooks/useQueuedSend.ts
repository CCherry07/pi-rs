import { useCallback, useEffect, useMemo, useState } from "react";
import type {
  ComposerSendIntent,
  FollowUpMessageBehavior,
  QueuedMessage,
  SendMessageResult,
  WorkspaceInfo,
} from "@/types";

type UseQueuedSendOptions = {
  activeThreadId: string | null;
  activeTurnId: string | null;
  isProcessing: boolean;
  queueFlushPaused?: boolean;
  steerEnabled: boolean;
  followUpMessageBehavior: FollowUpMessageBehavior;
  activeWorkspace: WorkspaceInfo | null;
  startThreadForWorkspace: (
    workspaceId: string,
    options?: { activate?: boolean },
  ) => Promise<string | null>;
  sendUserMessage: (
    text: string,
    images?: string[],
    options?: { sendIntent?: ComposerSendIntent },
  ) => Promise<SendMessageResult>;
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
  clearActiveImages: () => void;
};

type UseQueuedSendResult = {
  queuedByThread: Record<string, QueuedMessage[]>;
  activeQueue: QueuedMessage[];
  handleSend: (
    text: string,
    images?: string[],
    submitIntent?: ComposerSendIntent,
  ) => Promise<void>;
  queueMessage: (
    text: string,
    images?: string[],
  ) => Promise<void>;
  steerQueuedMessage: (threadId: string, item: QueuedMessage) => Promise<void>;
  removeQueuedMessage: (threadId: string, messageId: string) => void;
};

type SlashCommandKind =
  | "compact"
  | "fast"
  | "fork"
  | "new"
  | "resume"
  | "status";

function parseSlashCommand(text: string): SlashCommandKind | null {
  if (/^\/fork\b/i.test(text)) {
    return "fork";
  }
  if (/^\/fast\b/i.test(text)) {
    return "fast";
  }
  if (/^\/compact\b/i.test(text)) {
    return "compact";
  }
  if (/^\/new\b/i.test(text)) {
    return "new";
  }
  if (/^\/resume\b/i.test(text)) {
    return "resume";
  }
  if (/^\/status\b/i.test(text)) {
    return "status";
  }
  return null;
}

export function useQueuedSend({
  activeThreadId,
  activeTurnId,
  isProcessing,
  queueFlushPaused = false,
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
}: UseQueuedSendOptions): UseQueuedSendResult {
  const [queuedByThread, setQueuedByThread] = useState<
    Record<string, QueuedMessage[]>
  >({});
  const [inFlightByThread, setInFlightByThread] = useState<
    Record<string, QueuedMessage | null>
  >({});
  const [hasStartedByThread, setHasStartedByThread] = useState<
    Record<string, boolean>
  >({});

  const activeQueue = useMemo(
    () => (activeThreadId ? queuedByThread[activeThreadId] ?? [] : []),
    [activeThreadId, queuedByThread],
  );

  const enqueueMessage = useCallback((threadId: string, item: QueuedMessage) => {
    setQueuedByThread((prev) => ({
      ...prev,
      [threadId]: [...(prev[threadId] ?? []), item],
    }));
  }, []);

  const removeQueuedMessage = useCallback(
    (threadId: string, messageId: string) => {
      setQueuedByThread((prev) => ({
        ...prev,
        [threadId]: (prev[threadId] ?? []).filter(
          (entry) => entry.id !== messageId,
        ),
      }));
    },
    [],
  );

  const prependQueuedMessage = useCallback((threadId: string, item: QueuedMessage) => {
    setQueuedByThread((prev) => ({
      ...prev,
      [threadId]: [item, ...(prev[threadId] ?? [])],
    }));
  }, []);

  const createQueuedItem = useCallback(
    (text: string, images: string[]): QueuedMessage => ({
      id: `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
      text,
      createdAt: Date.now(),
      images,
    }),
    [],
  );

  const runSlashCommand = useCallback(
    async (command: SlashCommandKind, trimmed: string) => {
      if (command === "fork") {
        await startFork(trimmed);
        return;
      }
      if (command === "resume") {
        await startResume(trimmed);
        return;
      }
      if (command === "compact") {
        await startCompact(trimmed);
        return;
      }
      if (command === "fast") {
        await startFast(trimmed);
        return;
      }
      if (command === "status") {
        await startStatus(trimmed);
        return;
      }
      if (command === "new" && activeWorkspace) {
        const threadId = await startThreadForWorkspace(activeWorkspace.id);
        const rest = trimmed.replace(/^\/new\b/i, "").trim();
        if (threadId && rest) {
          await sendUserMessageToThread(activeWorkspace, threadId, rest, []);
        }
      }
    },
    [
      activeWorkspace,
      sendUserMessageToThread,
      startFork,
      startResume,
      startCompact,
      startFast,
      startStatus,
      startThreadForWorkspace,
    ],
  );

  const handleSend = useCallback(
    async (
      text: string,
      images: string[] = [],
      submitIntent: ComposerSendIntent = "default",
    ) => {
      const trimmed = text.trim();
      const command = parseSlashCommand(trimmed);
      const nextImages = command ? [] : images;
      const canSteerCurrentTurn =
        isProcessing && steerEnabled && Boolean(activeTurnId);
      const effectiveIntent: ComposerSendIntent = !isProcessing
        ? "default"
        : submitIntent === "queue"
          ? "queue"
          : submitIntent === "steer"
            ? canSteerCurrentTurn
              ? "steer"
              : "queue"
            : followUpMessageBehavior === "steer" && canSteerCurrentTurn
              ? "steer"
              : "queue";
      if (!trimmed && nextImages.length === 0) {
        return;
      }
      if (isProcessing && activeThreadId && effectiveIntent === "queue") {
        const item = createQueuedItem(trimmed, nextImages);
        enqueueMessage(activeThreadId, item);
        clearActiveImages();
        return;
      }
      if (command) {
        await runSlashCommand(command, trimmed);
        clearActiveImages();
        return;
      }
      const sendResult = await sendUserMessage(trimmed, nextImages, {
          sendIntent: effectiveIntent,
        });
      if (
        sendResult.status === "steer_failed" &&
        activeThreadId &&
        isProcessing
      ) {
        enqueueMessage(activeThreadId, createQueuedItem(trimmed, nextImages));
      }
      clearActiveImages();
    },
    [
      activeThreadId,
      clearActiveImages,
      createQueuedItem,
      enqueueMessage,
      activeTurnId,
      followUpMessageBehavior,
      isProcessing,
      steerEnabled,
      runSlashCommand,
      sendUserMessage,
    ],
  );

  const queueMessage = useCallback(
    async (
      text: string,
      images: string[] = [],
    ) => {
      const trimmed = text.trim();
      const command = parseSlashCommand(trimmed);
      const nextImages = command ? [] : images;
      if (!trimmed && nextImages.length === 0) {
        return;
      }
      if (!activeThreadId) {
        return;
      }
      const item = createQueuedItem(trimmed, nextImages);
      enqueueMessage(activeThreadId, item);
      clearActiveImages();
    },
    [
      activeThreadId,
      clearActiveImages,
      createQueuedItem,
      enqueueMessage,
    ],
  );

  const steerQueuedMessage = useCallback(
    async (threadId: string, item: QueuedMessage) => {
      if (
        threadId !== activeThreadId ||
        !isProcessing ||
        !steerEnabled ||
        !activeTurnId
      ) {
        return;
      }
      removeQueuedMessage(threadId, item.id);
      await handleSend(item.text, item.images ?? [], "steer");
    },
    [
      activeThreadId,
      activeTurnId,
      handleSend,
      isProcessing,
      removeQueuedMessage,
      steerEnabled,
    ],
  );

  useEffect(() => {
    if (!activeThreadId) {
      return;
    }
    const inFlight = inFlightByThread[activeThreadId];
    if (!inFlight) {
      return;
    }
    if (isProcessing) {
      if (!hasStartedByThread[activeThreadId]) {
        setHasStartedByThread((prev) => ({
          ...prev,
          [activeThreadId]: true,
        }));
      }
      return;
    }
    if (hasStartedByThread[activeThreadId]) {
      setInFlightByThread((prev) => ({ ...prev, [activeThreadId]: null }));
      setHasStartedByThread((prev) => ({ ...prev, [activeThreadId]: false }));
    }
  }, [
    activeThreadId,
    hasStartedByThread,
    inFlightByThread,
    isProcessing,
  ]);

  useEffect(() => {
    if (!activeThreadId || isProcessing || queueFlushPaused) {
      return;
    }
    if (inFlightByThread[activeThreadId]) {
      return;
    }
    const queue = queuedByThread[activeThreadId] ?? [];
    if (queue.length === 0) {
      return;
    }
    const threadId = activeThreadId;
    const nextItem = queue[0];
    setInFlightByThread((prev) => ({ ...prev, [threadId]: nextItem }));
    setHasStartedByThread((prev) => ({ ...prev, [threadId]: false }));
    setQueuedByThread((prev) => ({
      ...prev,
      [threadId]: (prev[threadId] ?? []).slice(1),
    }));
    (async () => {
      try {
        const trimmed = nextItem.text.trim();
        const command = parseSlashCommand(trimmed);
        if (command) {
          await runSlashCommand(command, trimmed);
        } else {
          await sendUserMessage(nextItem.text, nextItem.images ?? []);
        }
      } catch {
        setInFlightByThread((prev) => ({ ...prev, [threadId]: null }));
        setHasStartedByThread((prev) => ({ ...prev, [threadId]: false }));
        prependQueuedMessage(threadId, nextItem);
      }
    })();
  }, [
    activeThreadId,
    inFlightByThread,
    isProcessing,
    queueFlushPaused,
    prependQueuedMessage,
    queuedByThread,
    runSlashCommand,
    sendUserMessage,
  ]);

  return {
    queuedByThread,
    activeQueue,
    handleSend,
    queueMessage,
    steerQueuedMessage,
    removeQueuedMessage,
  };
}
