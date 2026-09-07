export type ThreadStatusFlags = {
  isProcessing?: boolean;
  hasUnread?: boolean;
};

export type ThreadStatusById = Record<string, ThreadStatusFlags>;

export type ThreadStatusClass = "processing" | "unread" | "ready";

export function getThreadStatusClass(
  status: ThreadStatusFlags | undefined,
  hasPendingUserInput: boolean,
): ThreadStatusClass {
  if (hasPendingUserInput) {
    return "unread";
  }
  if (status?.isProcessing) {
    return "processing";
  }
  if (status?.hasUnread) {
    return "unread";
  }
  return "ready";
}

type WorkspaceHomeThreadState = {
  status: "running" | "idle";
  stateClass: "is-running" | "is-idle";
  isRunning: boolean;
};

export function getWorkspaceHomeThreadState(
  status: ThreadStatusFlags | undefined,
): WorkspaceHomeThreadState {
  if (status?.isProcessing) {
    return { status: "running", stateClass: "is-running", isRunning: true };
  }
  return { status: "idle", stateClass: "is-idle", isRunning: false };
}
