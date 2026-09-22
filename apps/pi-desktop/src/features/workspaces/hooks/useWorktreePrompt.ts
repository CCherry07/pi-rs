import { useCallback, useEffect, useRef, useState } from "react";
import i18n from "@/i18n";
import type { BranchInfo, WorkspaceInfo, WorkspaceSettings } from "../../../types";
import type { GitCheckout, GitInventory } from "../../git/gitContext";
import {
  cancelWorktreePlan,
  listGitBranches,
  listGitCheckouts,
  prepareWorktreePlan,
  type WorktreePlanPreview,
} from "../../../services/tauri";

export type WorktreeCheckoutChoice = {
  checkout: GitCheckout;
  selected: boolean;
  branch: string;
  branchWasEdited: boolean;
  startPoint: string;
  branches: BranchInfo[];
};

type WorktreePromptState = {
  workspace: WorkspaceInfo;
  threadId: string | null;
  name: string;
  branch: string;
  branchWasEdited: boolean;
  copyAgentsMd: boolean;
  setupScript: string;
  savedSetupScript: string | null;
  inventory: GitInventory | null;
  checkouts: WorktreeCheckoutChoice[];
  executionRootId: string | null;
  plan: WorktreePlanPreview | null;
  isLoading: boolean;
  isPreparing: boolean;
  isSubmitting: boolean;
  isSavingScript: boolean;
  error: string | null;
  scriptError: string | null;
} | null;

type UseWorktreePromptOptions = {
  addWorktreeAgent: (
    workspace: WorkspaceInfo,
    branch: string,
    options: { displayName?: string | null; copyAgentsMd?: boolean; activate?: boolean; planId: string },
  ) => Promise<WorkspaceInfo | null>;
  updateWorkspaceSettings: (
    id: string,
    settings: Partial<WorkspaceSettings>,
  ) => Promise<WorkspaceInfo>;
  onSelectWorkspace: (workspaceId: string) => void;
  onWorktreeCreated?: (worktree: WorkspaceInfo, parent: WorkspaceInfo) => Promise<void> | void;
  onCompactActivate?: () => void;
  onError?: (message: string) => void;
};

function normalizeSetupScript(value: string | null | undefined): string | null {
  return value?.trim() ? value : null;
}

function toBranchFromName(value: string): string | null {
  const slug = value.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-")
    .replace(/-+/g, "-").replace(/(^-|-$)/g, "");
  return slug ? `pi/${slug}` : null;
}

function discardPlan(plan: WorktreePlanPreview | null) {
  if (plan) void cancelWorktreePlan(plan.id).catch(() => undefined);
}

export function useWorktreePrompt({
  addWorktreeAgent,
  updateWorkspaceSettings,
  onSelectWorkspace,
  onWorktreeCreated,
  onCompactActivate,
  onError,
}: UseWorktreePromptOptions) {
  const [worktreePrompt, setWorktreePrompt] = useState<WorktreePromptState>(null);
  const promptRef = useRef<WorktreePromptState>(null);
  const visit = useRef(0);
  const revision = useRef(0);
  const requestedWorktree = useRef<{
    visit: number;
    resolve: (workspace: WorkspaceInfo | null) => void;
  } | null>(null);
  const settleRequest = useCallback((workspace: WorkspaceInfo | null) => {
    const request = requestedWorktree.current;
    requestedWorktree.current = null;
    request?.resolve(workspace);
  }, []);
  const update = useCallback((fn: (prev: WorktreePromptState) => WorktreePromptState) => {
    promptRef.current = fn(promptRef.current);
    setWorktreePrompt(promptRef.current);
  }, []);
  const edit = useCallback((fn: (prev: NonNullable<WorktreePromptState>) => NonNullable<WorktreePromptState>) => {
    const current = promptRef.current;
    if (!current || current.isSubmitting) return;
    ++revision.current;
    discardPlan(current.plan);
    update(() => ({ ...fn(current), plan: null, isPreparing: false, error: null }));
  }, [update]);

  useEffect(() => () => {
    ++visit.current;
    discardPlan(promptRef.current?.isSubmitting ? null : promptRef.current?.plan ?? null);
    settleRequest(null);
  }, [settleRequest]);

  const openPrompt = useCallback((workspace: WorkspaceInfo, threadId: string | null = null) => {
    if (promptRef.current?.isSubmitting) return;
    settleRequest(null);
    discardPlan(promptRef.current?.plan ?? null);
    const token = ++visit.current;
    ++revision.current;
    const defaultBranch = `pi/${new Date().toISOString().slice(0, 10)}-${Math.random().toString(36).slice(2, 6)}`;
    const savedSetupScript = normalizeSetupScript(workspace.settings.worktreeSetupScript);
    update(() => ({
      workspace, threadId, name: "", branch: defaultBranch, branchWasEdited: false,
      copyAgentsMd: true, setupScript: savedSetupScript ?? "", savedSetupScript,
      inventory: null, checkouts: [], executionRootId: null, plan: null,
      isLoading: true, isPreparing: false, isSubmitting: false, isSavingScript: false,
      error: null, scriptError: null,
    }));
    void (async () => {
      try {
        const inventory = await listGitCheckouts(workspace.id, threadId);
        if (visit.current !== token) return;
        update((prev) => prev ? {
          ...prev, inventory, isLoading: false,
          checkouts: inventory.checkouts.map((checkout) => ({
            checkout, selected: true, branch: prev.branch, branchWasEdited: false,
            startPoint: "HEAD", branches: [],
          })),
          error: inventory.errors.map((entry) => entry.message).join("\n") || null,
        } : prev);
        await Promise.allSettled(inventory.checkouts.map(async (checkout) => {
          const response = await listGitBranches({
            workspaceId: workspace.id, threadId,
            target: { kind: "checkout", key: checkout.key, workdir: checkout.workdir },
          });
          if (visit.current !== token) return;
          const data: unknown = response?.branches ?? response?.result?.branches ?? response ?? [];
          const branches: BranchInfo[] = Array.isArray(data) ? data.flatMap((item: unknown) => {
            if (!item || typeof item !== "object" || !("name" in item)) return [];
            return [{ name: String(item.name), lastCommit: 0 }];
          }) : [];
          update((prev) => prev ? {
            ...prev, checkouts: prev.checkouts.map((choice) => choice.checkout.key === checkout.key ? { ...choice, branches } : choice),
          } : prev);
        }));
      } catch (error) {
        if (visit.current !== token) return;
        update((prev) => prev ? { ...prev, isLoading: false, error: String(error) } : prev);
      }
    })();
  }, [settleRequest, update]);

  const requestWorktree = useCallback((workspace: WorkspaceInfo, branch: string) => {
    if (promptRef.current) return Promise.resolve(null);
    openPrompt(workspace);
    update((prev) => prev ? { ...prev, branch } : prev);
    return new Promise<WorkspaceInfo | null>((resolve) => {
      requestedWorktree.current = { visit: visit.current, resolve };
    });
  }, [openPrompt, update]);

  const updateName = useCallback((name: string) => edit((prev) => {
    const branch = prev.branchWasEdited ? prev.branch : toBranchFromName(name) ?? prev.branch;
    return { ...prev, name, branch, checkouts: prev.checkouts.map((choice) => choice.branchWasEdited ? choice : { ...choice, branch }) };
  }), [edit]);
  const updateBranch = useCallback((branch: string) => edit((prev) => ({
    ...prev, branch, branchWasEdited: true,
    checkouts: prev.checkouts.map((choice) => ({ ...choice, branch, branchWasEdited: true })),
  })), [edit]);
  const updateCheckout = useCallback((key: string, patch: Partial<Pick<WorktreeCheckoutChoice, "selected" | "branch" | "startPoint">>) => edit((prev) => ({
    ...prev, checkouts: prev.checkouts.map((choice) => choice.checkout.key === key ? {
      ...choice, ...patch, branchWasEdited: patch.branch !== undefined || choice.branchWasEdited,
    } : choice),
  })), [edit]);
  const updateExecutionRoot = useCallback((executionRootId: string | null) => edit((prev) => ({ ...prev, executionRootId })), [edit]);
  const updateCopyAgentsMd = useCallback((copyAgentsMd: boolean) => edit((prev) => ({ ...prev, copyAgentsMd })), [edit]);
  const updateSetupScript = useCallback((setupScript: string) => edit((prev) => ({ ...prev, setupScript, scriptError: null })), [edit]);

  const cancelPrompt = useCallback(() => {
    if (promptRef.current?.isSubmitting) return;
    ++visit.current;
    ++revision.current;
    discardPlan(promptRef.current?.plan ?? null);
    update(() => null);
    settleRequest(null);
  }, [settleRequest, update]);

  const reviewPrompt = useCallback(async () => {
    const snapshot = promptRef.current;
    if (!snapshot || snapshot.isLoading || snapshot.isPreparing || snapshot.isSubmitting) return;
    const selected = snapshot.checkouts.filter((choice) => choice.selected);
    if (!selected.length || selected.some((choice) => !choice.branch.trim() || !choice.startPoint.trim())) {
      update((prev) => prev ? { ...prev, error: i18n.t("worktree.selectionRequired", { ns: "workspaces" }) } : prev);
      return;
    }
    discardPlan(snapshot.plan);
    const token = visit.current;
    const version = ++revision.current;
    update((prev) => prev ? { ...prev, isPreparing: true, plan: null, error: null } : prev);
    try {
      const nextScript = normalizeSetupScript(snapshot.setupScript);
      if (nextScript !== snapshot.savedSetupScript) {
        update((prev) => prev ? { ...prev, isSavingScript: true, scriptError: null } : prev);
        try {
          const workspace = await updateWorkspaceSettings(snapshot.workspace.id, {
            ...snapshot.workspace.settings, worktreeSetupScript: nextScript,
          });
          if (visit.current !== token) return;
          update((prev) => prev ? { ...prev, workspace, savedSetupScript: nextScript } : prev);
        } catch (error) {
          if (visit.current === token && revision.current === version) {
            update((prev) => prev ? { ...prev, scriptError: String(error) } : prev);
          }
          throw error;
        } finally {
          if (visit.current === token) update((prev) => prev ? { ...prev, isSavingScript: false } : prev);
        }
      }
      if (visit.current !== token || revision.current !== version) return;
      const plan = await prepareWorktreePlan({
        parentId: snapshot.workspace.id, threadId: snapshot.threadId,
        name: snapshot.name.trim() || selected[0].branch.trim(),
        copyAgentsMd: snapshot.copyAgentsMd, executionRootId: snapshot.executionRootId,
        checkouts: selected.map(({ checkout, branch, startPoint }) => ({
          target: { kind: "checkout", key: checkout.key, workdir: checkout.workdir },
          branch: branch.trim(), startPoint: startPoint.trim(),
        })),
      });
      if (visit.current !== token || revision.current !== version) {
        discardPlan(plan);
        return;
      }
      update((prev) => prev ? { ...prev, isPreparing: false, plan } : prev);
    } catch (error) {
      if (visit.current !== token || revision.current !== version) return;
      update((prev) => prev ? { ...prev, isPreparing: false, error: String(error) } : prev);
    }
  }, [update, updateWorkspaceSettings]);

  const confirmPrompt = useCallback(async () => {
    const snapshot = promptRef.current;
    if (!snapshot || snapshot.isSubmitting || !snapshot.plan) return;
    const token = visit.current;
    const background = requestedWorktree.current?.visit === token;
    update((prev) => prev ? { ...prev, isSubmitting: true, error: null, scriptError: null } : prev);
    const parentWorkspace = snapshot.workspace;
    try {
      const worktreeWorkspace = await addWorktreeAgent(parentWorkspace, snapshot.plan.checkouts[0].branch, {
        displayName: snapshot.name.trim() || null,
        copyAgentsMd: snapshot.copyAgentsMd,
        planId: snapshot.plan.id,
        ...(background ? { activate: false } : {}),
      });
      if (visit.current !== token) return;
      if (worktreeWorkspace) {
        if (!background) onSelectWorkspace(worktreeWorkspace.id);
        try {
          await onWorktreeCreated?.(worktreeWorkspace, parentWorkspace);
        } catch (error) {
          onError?.(error instanceof Error ? error.message : String(error));
        }
        if (!background) onCompactActivate?.();
      }
      if (visit.current === token) {
        update(() => null);
        settleRequest(worktreeWorkspace);
      }
    } catch (error) {
      if (visit.current !== token) return;
      const message = error instanceof Error ? error.message : String(error);
      discardPlan(snapshot.plan);
      // Execution failures may have consumed the prepared plan. Review again before retrying.
      update((prev) => prev ? { ...prev, isSubmitting: false, plan: null, error: message } : prev);
      onError?.(message);
    }
  }, [addWorktreeAgent, onCompactActivate, onError, onSelectWorkspace, onWorktreeCreated, settleRequest, update]);

  return {
    worktreePrompt, openPrompt, requestWorktree, confirmPrompt, reviewPrompt, cancelPrompt,
    updateName, updateBranch, updateCheckout, updateExecutionRoot, updateCopyAgentsMd, updateSetupScript,
  };
}
