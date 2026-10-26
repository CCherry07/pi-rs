import { useEffect, useRef } from "react";
import type { PiEvent } from "../../../types";
import { subscribePiEvents } from "../../../services/events";
import {
  getPiEventParams,
  getPiEventRawMethod,
  isSupportedPiEventMethod,
} from "../../../utils/piEvents";
import type { SupportedPiEventMethod } from "../../../utils/piEvents";

type AgentDelta = {
  workspaceId: string;
  threadId: string;
  itemId: string;
  delta: string;
};

type AgentCompleted = {
  workspaceId: string;
  threadId: string;
  itemId: string;
  text: string;
};

type PiEventHandlers = {
  onThreadNotice?: (workspaceId: string, threadId: string, message: string, level: string) => void;
  onThreadReplaced?: (workspaceId: string, previousThreadId: string, thread: Record<string, unknown>) => void;
  onThreadStarted?: (workspaceId: string, thread: Record<string, unknown>) => void;
  onThreadNameUpdated?: (
    workspaceId: string,
    payload: { threadId: string; threadName: string | null },
  ) => void;
  onThreadStatusChanged?: (
    workspaceId: string,
    threadId: string,
    status: Record<string, unknown>,
  ) => void;
  onThreadArchived?: (workspaceId: string, threadId: string) => void;
  onThreadUnarchived?: (
    workspaceId: string,
    threadId: string,
    thread: Record<string, unknown> | null,
  ) => void;
  onAgentMessageDelta?: (event: AgentDelta) => void;
  onAgentMessageCompleted?: (event: AgentCompleted) => void;
  onPiEvent?: (event: PiEvent) => void;
  onTurnStarted?: (workspaceId: string, threadId: string, turnId: string) => void;
  onTurnCompleted?: (workspaceId: string, threadId: string, turnId: string) => void;
  onTurnError?: (
    workspaceId: string,
    threadId: string,
    turnId: string,
    payload: { message: string; willRetry: boolean },
  ) => void;
  onItemStarted?: (workspaceId: string, threadId: string, item: Record<string, unknown>) => void;
  onItemCompleted?: (workspaceId: string, threadId: string, item: Record<string, unknown>) => void;
  onReasoningTextDelta?: (workspaceId: string, threadId: string, itemId: string, delta: string) => void;
  onThreadTokenUsageUpdated?: (
    workspaceId: string,
    threadId: string,
    tokenUsage: Record<string, unknown> | null,
  ) => void;
};

export const METHODS_ROUTED_IN_USE_PI_EVENTS = [
  "error",
  "item/agentMessage/delta",
  "item/completed",
  "item/reasoning/textDelta",
  "item/started",
  "thread/archived",
  "thread/deleted",
  "thread/name/updated",
  "thread/notice",
  "thread/replaced",
  "thread/status/changed",
  "thread/started",
  "thread/tokenUsage/updated",
  "thread/unarchived",
  "turn/completed",
  "turn/started",
] as const satisfies readonly SupportedPiEventMethod[];

export function usePiEvents(handlers: PiEventHandlers) {
  // Use ref to keep handlers current without triggering re-subscription
  const handlersRef = useRef(handlers);

  // Update ref on every render to always have latest handlers
  useEffect(() => {
    handlersRef.current = handlers;
  });

  useEffect(() => {
    const unlisten = subscribePiEvents((payload) => {
      const currentHandlers = handlersRef.current;
      currentHandlers.onPiEvent?.(payload);

      const { workspace_id } = payload;
      const method = getPiEventRawMethod(payload);
      if (!method) {
        return;
      }
      const params = getPiEventParams(payload);

      if (!isSupportedPiEventMethod(method)) {
        return;
      }

      if (method === "item/agentMessage/delta") {
        const threadId = String(params.threadId ?? params.thread_id ?? "");
        const itemId = String(params.itemId ?? params.item_id ?? "");
        const delta = String(params.delta ?? "");
        if (threadId && itemId && delta) {
          currentHandlers.onAgentMessageDelta?.({
            workspaceId: workspace_id,
            threadId,
            itemId,
            delta,
          });
        }
        return;
      }

      if (method === "turn/started") {
        const turn = params.turn as Record<string, unknown> | undefined;
        const threadId = String(
          params.threadId ?? params.thread_id ?? turn?.threadId ?? turn?.thread_id ?? "",
        );
        const turnId = String(turn?.id ?? params.turnId ?? params.turn_id ?? "");
        if (threadId) {
          currentHandlers.onTurnStarted?.(workspace_id, threadId, turnId);
        }
        return;
      }

      if (method === "thread/started") {
        const thread = (params.thread as Record<string, unknown> | undefined) ?? null;
        const threadId = String(thread?.id ?? "");
        if (thread && threadId) {
          currentHandlers.onThreadStarted?.(workspace_id, thread);
        }
        return;
      }

      if (method === "thread/replaced") {
        const thread = params.thread as Record<string, unknown> | undefined;
        const previousThreadId = String(params.previousThreadId ?? "");
        if (previousThreadId && thread && typeof thread.id === "string") {
          currentHandlers.onThreadReplaced?.(workspace_id, previousThreadId, thread);
        }
        return;
      }

      if (method === "thread/notice") {
        const threadId = String(params.threadId ?? "");
        if (threadId && typeof params.message === "string") {
          currentHandlers.onThreadNotice?.(workspace_id, threadId, params.message, String(params.level ?? "info"));
        }
        return;
      }

      if (method === "thread/name/updated") {
        const threadId = String(params.threadId ?? params.thread_id ?? "").trim();
        const threadNameRaw = params.threadName ?? params.thread_name ?? null;
        const threadName =
          typeof threadNameRaw === "string" && threadNameRaw.trim().length > 0
            ? threadNameRaw.trim()
            : null;
        if (threadId) {
          currentHandlers.onThreadNameUpdated?.(workspace_id, { threadId, threadName });
        }
        return;
      }

      if (method === "thread/status/changed") {
        const threadId = String(params.threadId ?? params.thread_id ?? "").trim();
        if (!threadId) {
          return;
        }
        const statusRaw = params.status;
        if (statusRaw && typeof statusRaw === "object" && !Array.isArray(statusRaw)) {
          currentHandlers.onThreadStatusChanged?.(
            workspace_id,
            threadId,
            statusRaw as Record<string, unknown>,
          );
          return;
        }
        if (typeof statusRaw === "string" && statusRaw.trim().length > 0) {
          currentHandlers.onThreadStatusChanged?.(workspace_id, threadId, {
            type: statusRaw.trim(),
          });
        }
        return;
      }

      if (method === "thread/archived" || method === "thread/deleted") {
        const threadId = String(params.threadId ?? params.thread_id ?? "").trim();
        if (threadId) {
          currentHandlers.onThreadArchived?.(workspace_id, threadId);
        }
        return;
      }

      if (method === "thread/unarchived") {
        const threadId = String(params.threadId ?? params.thread_id ?? "").trim();
        if (threadId) {
          const thread =
            params.thread && typeof params.thread === "object" && !Array.isArray(params.thread)
              ? (params.thread as Record<string, unknown>)
              : null;
          currentHandlers.onThreadUnarchived?.(workspace_id, threadId, thread);
        }
        return;
      }

      if (method === "error") {
        const threadId = String(params.threadId ?? params.thread_id ?? "");
        const turnId = String(params.turnId ?? params.turn_id ?? "");
        const error = (params.error as Record<string, unknown> | undefined) ?? {};
        const messageText = String(error.message ?? "");
        const willRetry = Boolean(params.willRetry ?? params.will_retry);
        if (threadId) {
          currentHandlers.onTurnError?.(workspace_id, threadId, turnId, {
            message: messageText,
            willRetry,
          });
        }
        return;
      }

      if (method === "turn/completed") {
        const turn = params.turn as Record<string, unknown> | undefined;
        const threadId = String(
          params.threadId ?? params.thread_id ?? turn?.threadId ?? turn?.thread_id ?? "",
        );
        const turnId = String(turn?.id ?? params.turnId ?? params.turn_id ?? "");
        if (threadId) {
          currentHandlers.onTurnCompleted?.(workspace_id, threadId, turnId);
        }
        return;
      }

      if (method === "thread/tokenUsage/updated") {
        const threadId = String(params.threadId ?? params.thread_id ?? "");
        const tokenUsage =
          (params.tokenUsage as Record<string, unknown> | null | undefined) ??
          (params.token_usage as Record<string, unknown> | null | undefined);
        if (threadId && tokenUsage !== undefined) {
          currentHandlers.onThreadTokenUsageUpdated?.(workspace_id, threadId, tokenUsage);
        }
        return;
      }

      if (method === "item/completed") {
        const threadId = String(params.threadId ?? params.thread_id ?? "");
        const item = params.item as Record<string, unknown> | undefined;
        if (threadId && item) {
          currentHandlers.onItemCompleted?.(workspace_id, threadId, item);
        }
        if (threadId && item?.type === "agentMessage") {
          const itemId = String(item.id ?? "");
          const text = String(item.text ?? "");
          if (itemId) {
            currentHandlers.onAgentMessageCompleted?.({
              workspaceId: workspace_id,
              threadId,
              itemId,
              text,
            });
          }
        }
        return;
      }

      if (method === "item/started") {
        const threadId = String(params.threadId ?? params.thread_id ?? "");
        const item = params.item as Record<string, unknown> | undefined;
        if (threadId && item) {
          currentHandlers.onItemStarted?.(workspace_id, threadId, item);
        }
        return;
      }

      if (method === "item/reasoning/textDelta") {
        const threadId = String(params.threadId ?? params.thread_id ?? "");
        const itemId = String(params.itemId ?? params.item_id ?? "");
        const delta = String(params.delta ?? "");
        if (threadId && itemId && delta) {
          currentHandlers.onReasoningTextDelta?.(workspace_id, threadId, itemId, delta);
        }
        return;
      }
    });

    return () => {
      unlisten();
    };
  }, []);
}
