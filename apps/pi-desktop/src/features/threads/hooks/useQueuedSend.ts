import { useCallback, useEffect, useMemo, useState } from "react";
import { resolveDesktopCommand as parseSlashCommand, type DesktopCommand as SlashCommandKind } from "@/utils/desktopCommands";
import type {
  ComposerSendIntent,
  FollowUpMessageBehavior,
  QueuedMessage,
  SendMessageResult,
} from "@/types";

type UseQueuedSendOptions = {
  activeThreadId: string | null;
  activeTurnId: string | null;
  isProcessing: boolean;
  queueFlushPaused?: boolean;
  steerEnabled: boolean;
  followUpMessageBehavior: FollowUpMessageBehavior;
  sendUserMessage: (
    text: string,
    images?: string[],
    options?: { sendIntent?: ComposerSendIntent },
  ) => Promise<SendMessageResult>;
  startCompact: (text: string) => Promise<void>;
  startReload: (text: string) => Promise<void>;
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

export function useQueuedSend({
  activeThreadId,
  activeTurnId,
  isProcessing,
  queueFlushPaused = false,
  steerEnabled,
  followUpMessageBehavior,
  sendUserMessage,
  startCompact,
  startReload,
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
      if (command === "compact") {
        await startCompact(trimmed);
        return;
      }
      if (command === "reload") {
        await startReload(trimmed);
        return;
      }
    },
    [
      startCompact,
      startReload,
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
      // Consume this draft now: submit can await an entire provider turn.
      clearActiveImages();
      if (command) {
        await runSlashCommand(command, trimmed);
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
        const result = command
          ? (await runSlashCommand(command, trimmed), { status: "handled" })
          : await sendUserMessage(nextItem.text, nextItem.images ?? []);
        // Handled commands have no AgentStart/Settled cycle. A completed
        // submission may also settle before React observes isProcessing=true.
        if (result.status !== "sent") {
          setInFlightByThread((prev) => prev[threadId]?.id === nextItem.id
            ? { ...prev, [threadId]: null } : prev);
        }
      } catch {
        setInFlightByThread((prev) => prev[threadId]?.id === nextItem.id
          ? { ...prev, [threadId]: null } : prev);
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
