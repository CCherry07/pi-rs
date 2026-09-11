import { memo, useContext, useEffect, useId, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import ChevronDown from "lucide-react/dist/esm/icons/chevron-down";
import Users from "lucide-react/dist/esm/icons/users";
import type { CollabAgentRef, CollabAgentStatus, ConversationItem } from "@/types";
import { ThreadConversationsContext } from "@threads/contexts/ThreadConversations";
import { formatTokens } from "@/utils/tokenUsage";
import { statusToneFromText } from "../utils/messageRenderUtils";

type ChildConversation = {
  threadId: string;
  items: ConversationItem[];
  isThinking: boolean;
  isLoadingMessages: boolean;
  processingStartedAt: number | null;
  lastDurationMs: number | null;
};

type SubagentChatPanelProps = {
  item: Extract<ConversationItem, { kind: "tool" }>;
  workspaceId: string | null;
  ancestorThreadIds: readonly string[];
  isExpanded: boolean;
  onToggle: (id: string) => void;
  renderConversation: (conversation: ChildConversation) => ReactNode;
};

const EMPTY_ITEMS: ConversationItem[] = [];

function ChildAgentActivity({ agent, fallback }: { agent?: CollabAgentRef; fallback?: CollabAgentStatus }) {
  const { t } = useTranslation("messages");
  const source = useContext(ThreadConversationsContext);
  const liveStatus = agent ? source?.threadStatusById[agent.threadId] : undefined;
  const fallbackTone = statusToneFromText(fallback?.status);
  // An idle session may have completed, failed or been interrupted. Do not infer
  // successful completion from the spawn receipt or the absence of an active turn.
  const tone = liveStatus?.isProcessing ? "processing" : liveStatus ? "idle" : fallbackTone;
  const totalTokens = (agent ? source?.tokenUsageByThread[agent.threadId]?.totalTokens : undefined) ?? fallback?.totalTokens;
  const statusLabel = tone === "idle" ? t("subagents.idle")
    : tone === "unknown" ? t("toolSummary.unknown") : t(`toolStatus.${tone}`);
  return (
    <span className={`subagent-chat-activity ${tone}`}>
      <span className={`subagent-execution-status ${tone}`} aria-hidden />
      {statusLabel}
      {totalTokens !== undefined && ` · ${t("subagents.tokens", { tokens: formatTokens(totalTokens) })}`}
    </span>
  );
}

function ChildAgentConversation({
  agent,
  workspaceId,
  ancestorThreadIds,
  renderConversation,
}: Pick<SubagentChatPanelProps, "workspaceId" | "ancestorThreadIds" | "renderConversation"> & {
  agent: CollabAgentRef;
}) {
  const { t } = useTranslation("messages");
  const source = useContext(ThreadConversationsContext);
  const loadThread = source?.loadThread;
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const isCycle = ancestorThreadIds.includes(agent.threadId);

  useEffect(() => {
    if (!workspaceId || !loadThread || isCycle) return;
    let disposed = false;
    setLoading(true);
    setError(null);
    void loadThread(workspaceId, agent.threadId)
      .catch((cause: unknown) => {
        if (!disposed) setError(cause instanceof Error ? cause.message : String(cause));
      })
      .finally(() => {
        if (!disposed) setLoading(false);
      });
    // Unmounting only stops display updates, never the underlying child Agent.
    return () => { disposed = true; };
  }, [agent.threadId, workspaceId, loadThread, isCycle, attempt]);

  if (isCycle || !source || !workspaceId) {
    return <div className="subagent-chat-notice" role="status">{t("subagents.unavailable")}</div>;
  }
  const items = source.itemsByThread[agent.threadId] ?? EMPTY_ITEMS;
  const status = source.threadStatusById[agent.threadId];
  return (
    <>
      {error && (
        <div className="subagent-chat-notice" role="alert">
          <span>{t("subagents.loadFailed")} {error}</span>
          <button type="button" className="ghost" onClick={() => setAttempt((value) => value + 1)}>
            {t("subagents.retry")}
          </button>
        </div>
      )}
      {(!error || items.length > 0) && renderConversation({
        threadId: agent.threadId,
        items,
        isThinking: status?.isProcessing ?? false,
        isLoadingMessages: loading,
        processingStartedAt: status?.processingStartedAt ?? null,
        lastDurationMs: status?.lastDurationMs ?? null,
      })}
    </>
  );
}

export const SubagentChatPanel = memo(function SubagentChatPanel({
  item,
  workspaceId,
  ancestorThreadIds,
  isExpanded,
  onToggle,
  renderConversation,
}: SubagentChatPanelProps) {
  const { t } = useTranslation("messages");
  const bodyId = useId();
  const agents = Array.from(new Map(
    (item.collabReceivers?.length ? item.collabReceivers : item.collabReceiver ? [item.collabReceiver] : [])
      .filter((agent) => agent.threadId.trim())
      .map((agent) => [agent.threadId, agent]),
  ).values());
  const labelFor = (agent: CollabAgentRef) => agent.nickname?.trim() || agent.role?.trim() || agent.threadId;
  const title = agents.length ? agents.map(labelFor).join(", ") : t("toolSummary.subagent");
  const fallbackFor = (agent: CollabAgentRef) => item.collabStatuses?.find((status) => status.threadId === agent.threadId)
    ?? { ...agent, status: item.status ?? "" };

  return (
    <section className={`subagent-execution-card${isExpanded ? " is-expanded" : ""}`} aria-label={t("subagents.chat", { agent: title })}>
      <button
        type="button"
        className="subagent-execution-card-toggle"
        onClick={() => onToggle(item.id)}
        aria-expanded={isExpanded}
        aria-controls={bodyId}
        aria-label={isExpanded ? t("subagents.collapseDetails") : t("subagents.expandDetails")}
      >
        <Users size={15} aria-hidden />
        <span className="subagent-execution-card-summary">
          <span className="subagent-execution-name">{title}</span>
          {item.collabTask && <span className="subagent-execution-task">{item.collabTask}</span>}
        </span>
        {agents.length <= 1 && <ChildAgentActivity agent={agents[0]} fallback={agents[0] ? fallbackFor(agents[0]) : { threadId: "", status: item.status ?? "" }} />}
        <ChevronDown className="subagent-execution-card-chevron" size={15} aria-hidden />
      </button>
      <div id={bodyId} hidden={!isExpanded}>
        {isExpanded && (
          <div className="subagent-execution-card-body">
            {item.collabTask && <div className="subagent-chat-task">{item.collabTask}</div>}
            {agents.length === 0 ? (
              <div className="subagent-chat-notice" role={item.status === "failed" ? "alert" : "status"}>
                {item.output || item.detail || t("subagents.waiting")}
              </div>
            ) : agents.map((agent) => (
              <section key={`${workspaceId}:${agent.threadId}`} className="subagent-chat-conversation" aria-label={t("subagents.conversation", { agent: labelFor(agent) })}>
                {agents.length > 1 && (
                  <div className="subagent-chat-heading">
                    <span>{labelFor(agent)}</span>
                    <ChildAgentActivity agent={agent} fallback={fallbackFor(agent)} />
                  </div>
                )}
                <ChildAgentConversation
                  agent={agent}
                  workspaceId={workspaceId}
                  ancestorThreadIds={ancestorThreadIds}
                  renderConversation={renderConversation}
                />
              </section>
            ))}
          </div>
        )}
      </div>
    </section>
  );
});
