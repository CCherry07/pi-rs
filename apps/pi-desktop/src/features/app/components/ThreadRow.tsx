import type { CSSProperties, MouseEvent } from "react";
import { useTranslation } from "react-i18next";

import type { ThreadSummary } from "../../../types";
import { getThreadStatusClass, type ThreadStatusById } from "../../../utils/threadStatus";
import { SidebarHoverCard } from "./SidebarHoverCard";

function formatSubagentRoleLabel(role: string | null | undefined) {
  const normalized = (role ?? "").trim();
  if (!normalized) {
    return null;
  }
  return normalized
    .replace(/[_-]+/g, " ")
    .replace(/\s+/g, " ")
    .replace(/\b\w/g, (match) => match.toUpperCase());
}

type ThreadRowProps = {
  thread: ThreadSummary;
  depth: number;
  workspaceId: string;
  indentUnit: number;
  activeWorkspaceId: string | null;
  activeThreadId: string | null;
  threadStatusById: ThreadStatusById;
  pendingUserInputKeys?: Set<string>;
  workspaceLabel?: string | null;
  getThreadTime: (thread: ThreadSummary) => string | null;
  getThreadArgsBadge?: (workspaceId: string, threadId: string) => string | null;
  isThreadPinned: (workspaceId: string, threadId: string) => boolean;
  onSelectThread: (workspaceId: string, threadId: string) => void;
  onShowThreadMenu: (
    event: MouseEvent,
    workspaceId: string,
    threadId: string,
    canPin: boolean,
  ) => void;
  hasSubagentChildren?: boolean;
  subagentsExpanded?: boolean;
  onToggleSubagents?: (workspaceId: string, threadId: string) => void;
  showPinnedLabel?: boolean;
};

export function ThreadRow({
  thread,
  depth,
  workspaceId,
  indentUnit,
  activeWorkspaceId,
  activeThreadId,
  threadStatusById,
  pendingUserInputKeys,
  workspaceLabel,
  getThreadTime,
  getThreadArgsBadge,
  isThreadPinned,
  onSelectThread,
  onShowThreadMenu,
  hasSubagentChildren = false,
  subagentsExpanded = true,
  onToggleSubagents,
  showPinnedLabel = true,
}: ThreadRowProps) {
  const { t } = useTranslation("app");
  const displayThreadName =
    thread.name === "New Agent" ? t("sidebar.workspace.newAgent") : thread.name;
  const relativeTime = getThreadTime(thread);
  const badge = getThreadArgsBadge?.(workspaceId, thread.id) ?? null;
  const modelBadge =
    thread.modelId && thread.modelId.trim().length > 0
      ? thread.effort && thread.effort.trim().length > 0
        ? `${thread.modelId} · ${thread.effort}`
        : thread.modelId
      : null;
  const indentStyle =
    depth > 0
      ? ({ "--thread-indent": `${depth * indentUnit}px` } as CSSProperties)
      : undefined;
  const hasPendingUserInput = Boolean(
    pendingUserInputKeys?.has(`${workspaceId}:${thread.id}`),
  );
  const statusClass = getThreadStatusClass(
    threadStatusById[thread.id],
    hasPendingUserInput,
  );
  const statusLabel =
    hasPendingUserInput
        ? t("sidebar.thread.waiting")
        : null;
  const subagentLabel =
    thread.isSubagent && (thread.subagentNickname || thread.subagentRole)
      ? thread.subagentNickname ?? thread.subagentRole ?? null
      : null;
  const subagentRoleLabel =
    thread.subagentNickname && thread.subagentRole
      ? formatSubagentRoleLabel(thread.subagentRole)
      : null;
  const subagentDetails = subagentLabel
    ? [subagentLabel, subagentRoleLabel].filter(Boolean).join(" · ")
    : null;
  const effectiveWorkspaceLabel = depth > 0 ? null : workspaceLabel;
  const canPin = depth === 0;
  const isPinned = canPin && isThreadPinned(workspaceId, thread.id);
  const canToggleSubagents = hasSubagentChildren && Boolean(onToggleSubagents);
  return (
    <SidebarHoverCard
      label={t("sidebar.details.thread")}
      content={(
        <>
          <div className="sidebar-hovercard-summary">{displayThreadName}</div>
          <dl className="sidebar-hovercard-details">
            {effectiveWorkspaceLabel && (
              <>
                <dt>{t("sidebar.details.workspace")}</dt>
                <dd>{effectiveWorkspaceLabel}</dd>
              </>
            )}
            {subagentDetails && (
              <>
                <dt>{t("sidebar.details.agent")}</dt>
                <dd>{subagentDetails}</dd>
              </>
            )}
            {modelBadge && (
              <>
                <dt>{t("sidebar.details.model")}</dt>
                <dd>{modelBadge}</dd>
              </>
            )}
            {badge && (
              <>
                <dt>{t("sidebar.details.context")}</dt>
                <dd>{badge}</dd>
              </>
            )}
          </dl>
          {showPinnedLabel && isPinned && (
            <span className="thread-pinned-label">{t("sidebar.thread.pinned")}</span>
          )}
        </>
      )}
    >
      {({ isOpen, panelId, close }) => (
        <div
          className={`thread-row ${
            workspaceId === activeWorkspaceId && thread.id === activeThreadId
              ? "active"
              : ""
          }${canToggleSubagents ? " has-subagent-children" : ""}${
            depth > 0 ? " is-nested" : ""
          }${isPinned ? " is-pinned" : ""}`}
          style={indentStyle}
          onClick={() => {
            close();
            onSelectThread(workspaceId, thread.id);
          }}
          onContextMenu={(event) => {
            close();
            onShowThreadMenu(event, workspaceId, thread.id, canPin);
          }}
          role="button"
          tabIndex={0}
          aria-haspopup="dialog"
          aria-expanded={isOpen}
          aria-controls={isOpen ? panelId : undefined}
          onKeyDown={(event) => {
            if (
              event.target === event.currentTarget &&
              (event.key === "Enter" || event.key === " ")
            ) {
              event.preventDefault();
              close();
              onSelectThread(workspaceId, thread.id);
            }
          }}
        >
          <span className={`thread-status ${statusClass}`} aria-hidden />
          <div className="thread-content">
            <div className="thread-headline">
              <span className="thread-name">{displayThreadName}</span>
              {statusLabel && (
                <span className={`thread-state-chip ${statusClass}`}>{statusLabel}</span>
              )}
            </div>
          </div>
          <div className="thread-meta">
            {canToggleSubagents ? (
              <button
                type="button"
                className={`thread-subagent-time-toggle ${subagentsExpanded ? "expanded" : ""}`}
                onClick={(event) => {
                  event.stopPropagation();
                  onToggleSubagents?.(workspaceId, thread.id);
                }}
                data-tauri-drag-region="false"
                aria-label={
                  subagentsExpanded
                    ? t("sidebar.thread.hideSubagents")
                    : t("sidebar.thread.showSubagents")
                }
                aria-expanded={subagentsExpanded}
              >
                <span className="thread-subagent-time-label">
                  {relativeTime ?? t("sidebar.thread.now")}
                </span>
                <span className="thread-subagent-toggle-icon" aria-hidden>
                  ›
                </span>
              </button>
            ) : (
              relativeTime && <span className="thread-time">{relativeTime}</span>
            )}
          </div>
        </div>
      )}
    </SidebarHoverCard>
  );
}
