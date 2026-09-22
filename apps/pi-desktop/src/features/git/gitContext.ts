import type { WorkspaceInfo, WorkspaceSpec } from "@/types";

export type GitCheckout = {
  key: string;
  workdir: string;
  gitDir: string;
  commonDir: string;
  rootIds: string[];
};
export type GitTarget =
  | { kind: "checkout"; key: string; workdir: string }
  | { kind: "directory"; rootId: string };
export type GitRequest = string | {
  workspaceId: string;
  threadId: string | null;
  target: GitTarget;
};
export type GitInventory = {
  workspace: WorkspaceSpec;
  checkouts: GitCheckout[];
  directoryRootIds: string[];
  defaultCheckoutKey: string | null;
  errors: { rootId: string; message: string }[];
};
/** View context only; the Project identity and execution directory are unchanged. */
export type GitWorkspace = WorkspaceInfo & {
  gitRequest?: Exclude<GitRequest, string>;
  gitScope?: string;
  gitWorkdir?: string;
};
export function gitRequestFor(workspace: GitWorkspace | null): GitRequest {
  return workspace?.gitRequest ?? workspace?.id ?? "";
}
export function gitScopeKey(workspace: GitWorkspace | null): string | null {
  return workspace?.gitScope ?? workspace?.id ?? null;
}
export function gitRequestArgs(request: GitRequest) {
  return typeof request === "string" ? { workspaceId: request } : request;
}

export function gitLocationKey(workspace: GitWorkspace | null): string | null {
  return workspace?.gitRequest
    ? JSON.stringify([workspace.id, workspace.gitRequest.threadId, workspace.gitWorkdir])
    : workspace?.id ?? null;
}

export function isManagedGitCheckout(workspace: GitWorkspace | null): boolean {
  const request = gitRequestFor(workspace);
  return workspace?.kind === "worktree" && !workspace.worktree?.managed && (typeof request === "string" ||
    (request.target.kind === "checkout" && request.target.workdir === workspace.path));
}
