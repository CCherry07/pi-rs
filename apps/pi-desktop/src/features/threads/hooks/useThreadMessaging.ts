import { useCallback } from "react";
import type { Dispatch, MutableRefObject } from "react";
import * as Sentry from "@sentry/react";
import i18n from "@/i18n";
import type {
  ComposerSendIntent,
  CustomPromptOption,
  DebugEntry,
  SendMessageResult,
  ServiceTier,
  WorkspaceInfo,
} from "@/types";
import {
  compactThread as compactThreadService,
  sendUserMessage as sendUserMessageService,
  steerTurn as steerTurnService,
  interruptTurn as interruptTurnService,
} from "@services/tauri";
import { expandCustomPromptText } from "@utils/customPrompts";
import { asString, extractRpcErrorMessage } from "@threads/utils/threadNormalize";
import type { ThreadAction, ThreadState } from "./useThreadsReducer";
import {
  buildStatusLines,
  buildTurnStartPayload,
  isStaleSteerTurnError,
  parseFastCommand,
  resolveSendMessageOptions,
  type SendMessageOptions,
} from "./threadMessagingHelpers";

type UseThreadMessagingOptions = {
  activeWorkspace: WorkspaceInfo | null;
  activeThreadId: string | null;
  model?: string | null;
  effort?: string | null;
  serviceTier?: ServiceTier | null | undefined;
  onSelectServiceTier?: (tier: ServiceTier | null | undefined) => void;
  steerEnabled: boolean;
  customPrompts: CustomPromptOption[];
  threadStatusById: ThreadState["threadStatusById"];
  activeTurnIdByThread: ThreadState["activeTurnIdByThread"];
  pendingInterruptsRef: MutableRefObject<Set<string>>;
  dispatch: Dispatch<ThreadAction>;
  getCustomName: (workspaceId: string, threadId: string) => string | undefined;
  markProcessing: (threadId: string, isProcessing: boolean) => void;
  setActiveTurnId: (threadId: string, turnId: string | null) => void;
  recordThreadActivity: (
    workspaceId: string,
    threadId: string,
    timestamp?: number,
  ) => void;
  safeMessageActivity: () => void;
  onDebug?: (entry: DebugEntry) => void;
  pushThreadErrorMessage: (threadId: string, message: string) => void;
  ensureThreadForActiveWorkspace: () => Promise<string | null>;
  refreshThread: (workspaceId: string, threadId: string) => Promise<string | null>;
  forkThreadForWorkspace: (
    workspaceId: string,
    threadId: string,
    options?: { activate?: boolean },
  ) => Promise<string | null>;
  updateThreadParent: (parentId: string, childIds: string[]) => void;
};

export function useThreadMessaging({
  activeWorkspace,
  activeThreadId,
  model,
  effort,
  serviceTier,
  onSelectServiceTier,
  steerEnabled,
  customPrompts,
  threadStatusById,
  activeTurnIdByThread,
  pendingInterruptsRef,
  dispatch,
  getCustomName,
  markProcessing,
  setActiveTurnId,
  recordThreadActivity,
  safeMessageActivity,
  onDebug,
  pushThreadErrorMessage,
  ensureThreadForActiveWorkspace,
  refreshThread,
  forkThreadForWorkspace,
  updateThreadParent,
}: UseThreadMessagingOptions) {
  const sendMessageToThread = useCallback(
    async (
      workspace: WorkspaceInfo,
      threadId: string,
      text: string,
      images: string[] = [],
      options?: SendMessageOptions,
    ): Promise<SendMessageResult> => {
      const messageText = text.trim();
      if (!messageText && images.length === 0) {
        return { status: "blocked" };
      }
      let finalText = messageText;
      if (!options?.skipPromptExpansion) {
        const promptExpansion = expandCustomPromptText(messageText, customPrompts);
        if (promptExpansion && "error" in promptExpansion) {
          pushThreadErrorMessage(threadId, promptExpansion.error);
          safeMessageActivity();
          return { status: "blocked" };
        }
        finalText = promptExpansion?.expanded ?? messageText;
      }
      const isProcessing = threadStatusById[threadId]?.isProcessing ?? false;
      const activeTurnId = activeTurnIdByThread[threadId] ?? null;
      const {
        resolvedModel,
        resolvedEffort,
        resolvedServiceTier,
        sendIntent,
        shouldSteer,
        requestMode,
      } = resolveSendMessageOptions({
        options,
        defaults: {
          model,
          effort,
          serviceTier,
          steerEnabled,
          isProcessing,
          activeTurnId,
        },
      });
      Sentry.metrics.count("prompt_sent", 1, {
        attributes: {
          workspace_id: workspace.id,
          thread_id: threadId,
          has_images: images.length > 0 ? "true" : "false",
          text_length: String(finalText.length),
          model: resolvedModel ?? "unknown",
          effort: resolvedEffort ?? "unknown",
          service_tier: resolvedServiceTier ?? "default",
          send_intent: sendIntent,
        },
      });
      const timestamp = Date.now();
      const customThreadName = getCustomName(workspace.id, threadId) ?? null;
      recordThreadActivity(workspace.id, threadId, timestamp);
      dispatch({
        type: "setThreadTimestamp",
        workspaceId: workspace.id,
        threadId,
        timestamp,
      });
      markProcessing(threadId, true);
      safeMessageActivity();
      onDebug?.({
        id: `${Date.now()}-${shouldSteer ? "client-turn-steer" : "client-turn-start"}`,
        timestamp: Date.now(),
        source: "client",
        label: shouldSteer ? "turn/steer" : "turn/start",
        payload: {
          workspaceId: workspace.id,
          threadId,
          turnId: activeTurnId,
          text: finalText,
          images,
          model: resolvedModel,
          effort: resolvedEffort,
          serviceTier: resolvedServiceTier,
          sendIntent,
          threadCustomName: customThreadName,
        },
      });
      try {
        const response: Record<string, unknown> = shouldSteer
          ? (await steerTurnService(
              workspace.id,
              threadId,
              activeTurnId ?? "",
              finalText,
              images,
            )) as Record<string, unknown>
          : (await sendUserMessageService(
            workspace.id,
            threadId,
            finalText,
            buildTurnStartPayload({
              model: resolvedModel,
              effort: resolvedEffort,
              serviceTier: resolvedServiceTier,
              images,
            }),
          )) as Record<string, unknown>;

        const rpcError = extractRpcErrorMessage(response);

        onDebug?.({
          id: `${Date.now()}-${requestMode === "steer" ? "server-turn-steer" : "server-turn-start"}`,
          timestamp: Date.now(),
          source: "server",
          label: requestMode === "steer" ? "turn/steer response" : "turn/start response",
          payload: response,
        });
        if (rpcError) {
          if (requestMode !== "steer") {
            markProcessing(threadId, false);
            setActiveTurnId(threadId, null);
            pushThreadErrorMessage(
              threadId,
              i18n.t("errors.turnStartFailedWithMessage", {
                ns: "messages",
                message: rpcError,
              }),
            );
            safeMessageActivity();
            return { status: "blocked" };
          }
          if (isStaleSteerTurnError(rpcError)) {
            markProcessing(threadId, false);
            setActiveTurnId(threadId, null);
          }
          pushThreadErrorMessage(
            threadId,
            i18n.t("errors.turnSteerFailedWithMessage", {
              ns: "messages",
              message: rpcError,
            }),
          );
          safeMessageActivity();
          return { status: "steer_failed" };
        }
        if (requestMode === "steer") {
          const result = (response?.result ?? response) as Record<string, unknown>;
          const steeredTurnId = asString(result?.turnId ?? result?.turn_id ?? "");
          if (steeredTurnId) {
            setActiveTurnId(threadId, steeredTurnId);
          }
          return { status: "sent" };
        }
        const result = (response?.result ?? response) as Record<string, unknown>;
        const turn = (result?.turn ?? response?.turn ?? null) as
          | Record<string, unknown>
          | null;
        const turnId = asString(turn?.id ?? "");
        if (!turnId) {
          markProcessing(threadId, false);
          setActiveTurnId(threadId, null);
          pushThreadErrorMessage(
            threadId,
            i18n.t("errors.turnStartFailed", { ns: "messages" }),
          );
          safeMessageActivity();
          return { status: "blocked" };
        }
        setActiveTurnId(threadId, turnId);
        return { status: "sent" };
      } catch (error) {
        const errorMessage = error instanceof Error ? error.message : String(error);
        if (requestMode !== "steer") {
          markProcessing(threadId, false);
          setActiveTurnId(threadId, null);
        } else if (isStaleSteerTurnError(errorMessage)) {
          markProcessing(threadId, false);
          setActiveTurnId(threadId, null);
        }
        onDebug?.({
          id: `${Date.now()}-${requestMode === "steer" ? "client-turn-steer-error" : "client-turn-start-error"}`,
          timestamp: Date.now(),
          source: "error",
          label: requestMode === "steer" ? "turn/steer error" : "turn/start error",
          payload: errorMessage,
        });
        pushThreadErrorMessage(
          threadId,
          requestMode === "steer"
            ? i18n.t("errors.turnSteerFailedWithMessage", {
                ns: "messages",
                message: errorMessage,
              })
            : errorMessage,
        );
        safeMessageActivity();
        return { status: requestMode === "steer" ? "steer_failed" : "blocked" };
      }
    },
    [
      customPrompts,
      dispatch,
      effort,
      serviceTier,
      activeTurnIdByThread,
      getCustomName,
      markProcessing,
      model,
      onDebug,
      pushThreadErrorMessage,
      recordThreadActivity,
      safeMessageActivity,
      setActiveTurnId,
      steerEnabled,
      threadStatusById,
    ],
  );

  const sendUserMessage = useCallback(
    async (
      text: string,
      images: string[] = [],
      options?: { sendIntent?: ComposerSendIntent },
    ): Promise<SendMessageResult> => {
      if (!activeWorkspace) {
        return { status: "blocked" };
      }
      const messageText = text.trim();
      if (!messageText && images.length === 0) {
        return { status: "blocked" };
      }
      const promptExpansion = expandCustomPromptText(messageText, customPrompts);
      if (promptExpansion && "error" in promptExpansion) {
        if (activeThreadId) {
          pushThreadErrorMessage(activeThreadId, promptExpansion.error);
          safeMessageActivity();
        } else {
          onDebug?.({
            id: `${Date.now()}-client-prompt-expand-error`,
            timestamp: Date.now(),
            source: "error",
            label: "prompt/expand error",
            payload: promptExpansion.error,
          });
        }
        return { status: "blocked" };
      }
      const finalText = promptExpansion?.expanded ?? messageText;
      const threadId = await ensureThreadForActiveWorkspace();
      if (!threadId) {
        return { status: "blocked" };
      }
      return sendMessageToThread(activeWorkspace, threadId, finalText, images, {
        skipPromptExpansion: true,
        sendIntent: options?.sendIntent,
      });
    },
    [
      activeThreadId,
      activeWorkspace,
      customPrompts,
      ensureThreadForActiveWorkspace,
      onDebug,
      pushThreadErrorMessage,
      safeMessageActivity,
      sendMessageToThread,
    ],
  );

  const sendUserMessageToThread = useCallback(
    async (
      workspace: WorkspaceInfo,
      threadId: string,
      text: string,
      images: string[] = [],
      options?: SendMessageOptions,
    ): Promise<SendMessageResult> => {
      return sendMessageToThread(workspace, threadId, text, images, options);
    },
    [sendMessageToThread],
  );

  const interruptTurn = useCallback(async () => {
    if (!activeWorkspace || !activeThreadId) {
      return;
    }
    const activeTurnId = activeTurnIdByThread[activeThreadId] ?? null;
    const turnId = activeTurnId ?? "pending";
    markProcessing(activeThreadId, false);
    setActiveTurnId(activeThreadId, null);
    dispatch({
      type: "addAssistantMessage",
      threadId: activeThreadId,
      text: i18n.t("notices.sessionStopped", { ns: "messages" }),
    });
    if (!activeTurnId) {
      pendingInterruptsRef.current.add(activeThreadId);
    }
    onDebug?.({
      id: `${Date.now()}-client-turn-interrupt`,
      timestamp: Date.now(),
      source: "client",
      label: "turn/interrupt",
      payload: {
        workspaceId: activeWorkspace.id,
        threadId: activeThreadId,
        turnId,
        queued: !activeTurnId,
      },
    });
    try {
      const response = await interruptTurnService(
        activeWorkspace.id,
        activeThreadId,
        turnId,
      );
      onDebug?.({
        id: `${Date.now()}-server-turn-interrupt`,
        timestamp: Date.now(),
        source: "server",
        label: "turn/interrupt response",
        payload: response,
      });
    } catch (error) {
      onDebug?.({
        id: `${Date.now()}-client-turn-interrupt-error`,
        timestamp: Date.now(),
        source: "error",
        label: "turn/interrupt error",
        payload: error instanceof Error ? error.message : String(error),
      });
    }
  }, [
    activeThreadId,
    activeTurnIdByThread,
    activeWorkspace,
    dispatch,
    markProcessing,
    onDebug,
    pendingInterruptsRef,
    setActiveTurnId,
  ]);

  const startStatus = useCallback(
    async (_text: string) => {
      if (!activeWorkspace) {
        return;
      }
      const threadId = await ensureThreadForActiveWorkspace();
      if (!threadId) {
        return;
      }

      const lines = buildStatusLines({
        model,
        serviceTier,
        effort,
      });
      const timestamp = Date.now();
      recordThreadActivity(activeWorkspace.id, threadId, timestamp);
      dispatch({
        type: "addAssistantMessage",
        threadId,
        text: lines.join("\n"),
      });
      safeMessageActivity();
    },
    [
      activeWorkspace,
      dispatch,
      effort,
      ensureThreadForActiveWorkspace,
      model,
      serviceTier,
      recordThreadActivity,
      safeMessageActivity,
    ],
  );

  const startFast = useCallback(
    async (text: string) => {
      if (!activeWorkspace) {
        return;
      }
      const threadId = await ensureThreadForActiveWorkspace();
      if (!threadId) {
        return;
      }

      const action = parseFastCommand(text);
      const isEnabled = serviceTier === "fast";
      let nextTier = serviceTier ?? null;
      let message = "";

      if (action === "invalid") {
        message = i18n.t("commands.fast.usage", { ns: "messages" });
      } else if (action === "status") {
        message = i18n.t("commands.fast.status", {
          ns: "messages",
          state: i18n.t(
            isEnabled ? "commands.status.on" : "commands.status.off",
            { ns: "messages" },
          ),
        });
      } else {
        nextTier =
          action === "on"
            ? "fast"
            : action === "off"
              ? null
              : isEnabled
                ? null
                : "fast";
        onSelectServiceTier?.(nextTier);
        message = i18n.t(
          nextTier === "fast"
            ? "commands.fast.enabled"
            : "commands.fast.disabled",
          { ns: "messages" },
        );
      }

      const timestamp = Date.now();
      recordThreadActivity(activeWorkspace.id, threadId, timestamp);
      dispatch({
        type: "addAssistantMessage",
        threadId,
        text: message,
      });
      safeMessageActivity();
    },
    [
      activeWorkspace,
      dispatch,
      ensureThreadForActiveWorkspace,
      onSelectServiceTier,
      recordThreadActivity,
      safeMessageActivity,
      serviceTier,
    ],
  );

  const startFork = useCallback(
    async (text: string) => {
      if (!activeWorkspace || !activeThreadId) {
        return;
      }
      const trimmed = text.trim();
      const rest = trimmed.replace(/^\/fork\b/i, "").trim();
      const threadId = await forkThreadForWorkspace(activeWorkspace.id, activeThreadId);
      if (!threadId) {
        return;
      }
      updateThreadParent(activeThreadId, [threadId]);
      if (rest) {
        await sendMessageToThread(activeWorkspace, threadId, rest, []);
      }
    },
    [
      activeThreadId,
      activeWorkspace,
      forkThreadForWorkspace,
      sendMessageToThread,
      updateThreadParent,
    ],
  );

  const startResume = useCallback(
    async (_text: string) => {
      if (!activeWorkspace) {
        return;
      }
      if (activeThreadId && threadStatusById[activeThreadId]?.isProcessing) {
        return;
      }
      const threadId = activeThreadId ?? (await ensureThreadForActiveWorkspace());
      if (!threadId) {
        return;
      }
      await refreshThread(activeWorkspace.id, threadId);
      safeMessageActivity();
    },
    [
      activeThreadId,
      activeWorkspace,
      ensureThreadForActiveWorkspace,
      refreshThread,
      safeMessageActivity,
      threadStatusById,
    ],
  );

  const startCompact = useCallback(
    async (_text: string) => {
      if (!activeWorkspace) {
        return;
      }
      const threadId = activeThreadId ?? (await ensureThreadForActiveWorkspace());
      if (!threadId) {
        return;
      }
      try {
        await compactThreadService(activeWorkspace.id, threadId);
      } catch (error) {
        pushThreadErrorMessage(
          threadId,
          error instanceof Error
            ? error.message
            : i18n.t("errors.compactionFailed", { ns: "messages" }),
        );
      } finally {
        safeMessageActivity();
      }
    },
    [
      activeThreadId,
      activeWorkspace,
      ensureThreadForActiveWorkspace,
      pushThreadErrorMessage,
      safeMessageActivity,
    ],
  );

  return {
    interruptTurn,
    sendUserMessage,
    sendUserMessageToThread,
    startFork,
    startResume,
    startCompact,
    startFast,
    startStatus,
  };
}
