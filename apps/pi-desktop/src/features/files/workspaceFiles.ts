import type { FileMention, WorkspaceFileListing, WorkspaceRoot, WorkspaceSpec } from "../../types";

/** Paths from the backend use / between segments; preserve spaces in directory names. */
export function workspaceFilePath(root: Pick<WorkspaceRoot, "path">, path: string): string {
  const windows = /^[A-Za-z]:[\\/]/.test(root.path) || root.path.startsWith("\\\\");
  const separator = windows ? "\\" : "/";
  const base = root.path.endsWith(separator) ? root.path : root.path + separator;
  return base + (windows ? path.replace(/\//g, "\\") : path);
}

export function fileMentionText(workspace: WorkspaceSpec, root: WorkspaceRoot, path: string): string {
  const value = workspace.roots.length === 1 && root.path === workspace.executionDir
    ? path
    : workspaceFilePath(root, path);
  return /[\s"`]/.test(value) ? JSON.stringify(value) : value;
}

export function workspaceFileMentions(listing: WorkspaceFileListing): FileMention[] {
  const roots = new Map(listing.workspace.roots.map((root) => [root.id, root]));
  const multiRoot = roots.size > 1;
  return listing.files.flatMap((file) => {
    const root = roots.get(file.rootId);
    return root ? [{
      id: JSON.stringify([file.rootId, file.path]),
      label: multiRoot ? `${root.name}/${file.path}` : file.path,
      description: multiRoot ? root.path : undefined,
      insertText: fileMentionText(listing.workspace, root, file.path),
    }] : [];
  });
}
