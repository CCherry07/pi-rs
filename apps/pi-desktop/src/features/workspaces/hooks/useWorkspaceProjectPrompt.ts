import { useCallback, useEffect, useRef, useState } from "react";
import { getWorkspaceProject, pickWorkspacePaths, type CreateWorkspaceProjectInput } from "@/services/tauri";
import type { ProjectDefinition, WorkspaceInfo, WorkspaceRoot } from "@/types";
import { normalizeRootPath } from "../../threads/utils/threadNormalize";

type PromptState = {
  mode: "create" | "edit";
  project: ProjectDefinition | null;
  isLoading: boolean;
  name: string;
  nameEdited: boolean;
  roots: WorkspaceRoot[];
  primaryRootId: string | null;
  error: string | null;
  isBusy: boolean;
  isChoosing: boolean;
};

type PendingRequest = {
  workspaceId: string | null;
  loadAttempt: number;
  loaded: boolean;
  resolve: (workspace: WorkspaceInfo | null) => void;
  submitting: boolean;
  choosing: boolean;
};

function directoryName(path: string | undefined) {
  return path?.split(/[\\/]/).filter(Boolean).slice(-1)[0] ?? path ?? "";
}

export function useWorkspaceProjectPrompt({
  onSubmit,
  onUpdate,
}: {
  onSubmit: (input: CreateWorkspaceProjectInput) => Promise<WorkspaceInfo>;
  onUpdate: (project: ProjectDefinition) => Promise<void>;
}) {
  const [prompt, setPrompt] = useState<PromptState | null>(null);
  const pending = useRef<PendingRequest | null>(null);

  useEffect(() => () => {
    pending.current?.resolve(null);
    pending.current = null;
  }, []);

  const request = useCallback((): Promise<WorkspaceInfo | null> => {
    if (pending.current) return Promise.resolve(null);
    return new Promise((resolve) => {
      pending.current = { resolve, submitting: false, choosing: false, workspaceId: null, loadAttempt: 0, loaded: true };
      setPrompt({
        mode: "create", project: null, isLoading: false,
        name: "", nameEdited: false, roots: [], primaryRootId: null,
        error: null, isBusy: false, isChoosing: false,
      });
    });
  }, []);

  const loadProject = useCallback(async (current: PendingRequest) => {
    if (!current.workspaceId || current.submitting || current.choosing) return;
    const attempt = ++current.loadAttempt;
    current.loaded = false;
    setPrompt((prev) => prev && { ...prev, isLoading: true, error: null });
    try {
      const project = await getWorkspaceProject(current.workspaceId);
      if (pending.current !== current || current.loadAttempt !== attempt) return;
      current.loaded = true;
      setPrompt({
        mode: "edit", project, isLoading: false, name: project.name, nameEdited: true,
        roots: project.roots,
        primaryRootId: project.primaryRoot,
        error: null, isBusy: false, isChoosing: false,
      });
    } catch (error) {
      if (pending.current === current && current.loadAttempt === attempt) {
        setPrompt((prev) => prev && {
          ...prev, isLoading: false,
          error: error instanceof Error ? error.message : String(error),
        });
      }
    }
  }, []);

  const edit = useCallback((workspaceId: string) => {
    if (pending.current) return;
    const current: PendingRequest = {
      workspaceId, loadAttempt: 0, loaded: false,
      resolve: () => {}, submitting: false, choosing: false,
    };
    pending.current = current;
    setPrompt({
      mode: "edit", project: null, isLoading: true, name: "", nameEdited: true,
      roots: [], primaryRootId: null, error: null, isBusy: false, isChoosing: false,
    });
    void loadProject(current);
  }, [loadProject]);

  const retryLoad = useCallback(() => {
    if (pending.current) void loadProject(pending.current);
  }, [loadProject]);

  const cancel = useCallback(() => {
    if (pending.current?.submitting) return;
    pending.current?.resolve(null);
    pending.current = null;
    setPrompt(null);
  }, []);

  const chooseDirectories = useCallback(async () => {
    const current = pending.current;
    if (!current || !current.loaded || current.submitting || current.choosing) return;
    current.choosing = true;
    setPrompt((prev) => prev && { ...prev, isChoosing: true, error: null });
    try {
      const selected = await pickWorkspacePaths();
      if (pending.current !== current) return;
      setPrompt((prev) => {
        if (!prev) return prev;
        const roots = [...prev.roots];
        const seen = new Set(roots.map((root) => normalizeRootPath(root.path)));
        for (const path of selected) {
          if (!path.trim()) continue;
          const key = normalizeRootPath(path);
          if (!seen.has(key)) {
            seen.add(key);
            roots.push({ id: crypto.randomUUID(), name: directoryName(path), path, ownership: { kind: "external" } });
          }
        }
        return {
          ...prev, roots, primaryRootId: prev.primaryRootId ?? roots[0]?.id ?? null,
          name: prev.nameEdited ? prev.name : roots[0]?.name ?? "",
          isChoosing: false,
        };
      });
    } catch (error) {
      if (pending.current === current) {
        setPrompt((prev) => prev && {
          ...prev, isChoosing: false,
          error: error instanceof Error ? error.message : String(error),
        });
      }
    } finally {
      current.choosing = false;
    }
  }, []);

  const updateName = useCallback((name: string) => {
    if (pending.current?.submitting) return;
    setPrompt((prev) => prev && { ...prev, name, nameEdited: true, error: null });
  }, []);

  const removeDirectory = useCallback((rootId: string) => {
    if (pending.current?.submitting) return;
    setPrompt((prev) => {
      if (!prev) return prev;
      const roots = prev.roots.filter((root) => root.id !== rootId);
      return {
        ...prev, roots,
        primaryRootId: prev.primaryRootId === rootId ? roots[0]?.id ?? null : prev.primaryRootId,
        name: prev.nameEdited ? prev.name : roots[0]?.name ?? "",
        error: null,
      };
    });
  }, []);

  const updatePrimaryRoot = useCallback((primaryRootId: string) => {
    if (pending.current?.submitting) return;
    setPrompt((prev) => prev?.roots.some((root) => root.id === primaryRootId)
      ? { ...prev, primaryRootId, error: null } : prev);
  }, []);

  const confirm = useCallback(async () => {
    const current = pending.current;
    const primary = prompt?.roots.find((root) => root.id === prompt.primaryRootId);
    if (!current || !current.loaded || current.submitting || current.choosing || !prompt?.name.trim()
      || !primary) return;
    current.submitting = true;
    setPrompt((prev) => prev && { ...prev, isBusy: true, error: null });
    try {
      let workspace: WorkspaceInfo | null = null;
      if (prompt.mode === "edit" && prompt.project) {
        const original = prompt.project;
        await onUpdate({
          ...original, name: prompt.name.trim(), roots: prompt.roots, primaryRoot: primary.id,
          executionDir: primary.id === original.primaryRoot ? original.executionDir : null,
        });
      } else {
        workspace = await onSubmit({
          name: prompt.name.trim(), paths: prompt.roots.map((root) => root.path), primaryPath: primary.path,
        });
      }
      if (pending.current !== current) return;
      pending.current = null;
      setPrompt(null);
      current.resolve(workspace);
    } catch (error) {
      if (pending.current === current) {
        current.submitting = false;
        setPrompt((prev) => prev && {
          ...prev, isBusy: false,
          error: error instanceof Error ? error.message : String(error),
        });
      }
    }
  }, [onSubmit, onUpdate, prompt]);

  return { prompt, request, edit, retryLoad, cancel, chooseDirectories, updateName, removeDirectory, updatePrimaryRoot, confirm };
}
