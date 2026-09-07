import type { ThreadSummary, WorkspaceInfo } from "../../../types";

export type WorkspaceGroupSection = {
  id: string | null;
  name: string;
  workspaces: WorkspaceInfo[];
};

export type FlatThreadRow = {
  thread: ThreadSummary;
  depth: number;
  workspaceId: string;
  workspaceName: string;
};

export type FlatThreadRootGroup = {
  rootTimestamp: number;
  workspaceName: string;
  workspaceId: string;
  rootIndex: number;
  rows: FlatThreadRow[];
};

export type ThreadBucket = {
  id: "now" | "today" | "yesterday" | "week" | "older";
  label: string;
  rows: FlatThreadRow[];
};

export type ThreadRowsResult = {
  pinnedRows: Array<{ thread: ThreadSummary; depth: number }>;
  unpinnedRows: Array<{ thread: ThreadSummary; depth: number }>;
  totalRoots: number;
  hasMoreRoots: boolean;
};

export type SidebarWorkspaceAddMenuAnchor = {
  workspaceId: string;
  top: number;
  left: number;
  width: number;
};

export type SidebarOverlayMenuAnchor = {
  top: number;
  left: number;
  width: number;
};
