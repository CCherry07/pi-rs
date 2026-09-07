import { useMemo, useState, type MouseEvent } from "react";

import type { ThreadSummary } from "../../../types";
import type { ThreadStatusById } from "../../../utils/threadStatus";
import { ThreadRow } from "./ThreadRow";
import { buildThreadRowVisibility } from "./threadRowVisibility";

type PinnedThreadRow = {
  thread: ThreadSummary;
  depth: number;
  workspaceId: string;
};

type PinnedThreadListProps = {
  rows: PinnedThreadRow[];
  activeWorkspaceId: string | null;
  activeThreadId: string | null;
  threadStatusById: ThreadStatusById;
  pendingUserInputKeys?: Set<string>;
  getWorkspaceLabel?: (workspaceId: string) => string | null;
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
};

export function PinnedThreadList({
  rows,
  activeWorkspaceId,
  activeThreadId,
  threadStatusById,
  pendingUserInputKeys,
  getWorkspaceLabel,
  getThreadTime,
  getThreadArgsBadge,
  isThreadPinned,
  onSelectThread,
  onShowThreadMenu,
}: PinnedThreadListProps) {
  const [collapsedThreadKeys, setCollapsedThreadKeys] = useState<Set<string>>(new Set());
  const visibility = useMemo(
    () =>
      buildThreadRowVisibility(
        rows,
        (row) => collapsedThreadKeys.has(`${row.workspaceId}:${row.thread.id}`),
      ),
    [collapsedThreadKeys, rows],
  );

  const toggleThreadSubagents = (workspaceId: string, threadId: string) => {
    const threadKey = `${workspaceId}:${threadId}`;
    setCollapsedThreadKeys((prev) => {
      const next = new Set(prev);
      if (next.has(threadKey)) {
        next.delete(threadKey);
      } else {
        next.add(threadKey);
      }
      return next;
    });
  };

  return (
    <div className="thread-list pinned-thread-list">
      {visibility.visibleRows.map((row) => {
        const { thread, depth, workspaceId } = row;
        const threadKey = `${workspaceId}:${thread.id}`;
        return (
          <ThreadRow
            key={`${workspaceId}:${thread.id}`}
            thread={thread}
            depth={depth}
            workspaceId={workspaceId}
            indentUnit={14}
            activeWorkspaceId={activeWorkspaceId}
            activeThreadId={activeThreadId}
            threadStatusById={threadStatusById}
            pendingUserInputKeys={pendingUserInputKeys}
            workspaceLabel={getWorkspaceLabel?.(workspaceId) ?? null}
            getThreadTime={getThreadTime}
            getThreadArgsBadge={getThreadArgsBadge}
            isThreadPinned={isThreadPinned}
            onSelectThread={onSelectThread}
            onShowThreadMenu={onShowThreadMenu}
            hasSubagentChildren={visibility.rowsWithChildren.has(row)}
            subagentsExpanded={!collapsedThreadKeys.has(threadKey)}
            onToggleSubagents={toggleThreadSubagents}
            showPinnedLabel={false}
          />
        );
      })}
    </div>
  );
}
