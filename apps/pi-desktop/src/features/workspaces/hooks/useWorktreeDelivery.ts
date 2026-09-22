import { useCallback, useEffect, useRef, useState } from "react";
import type { WorkspaceInfo } from "../../../types";
import { executeWorktreeDelivery, finishWorktreeDeliveryAttempt, getWorktreeDelivery, inspectWorktreeDeliveryAttempt, previewWorktreeDelivery, type DeliveryAttempt, type DeliveryOverview, type DeliveryPreview } from "../../../services/tauri";

type DeliveryOperation = { kind: "execute" | "inspect" | "finish"; checkoutKey: string; attemptId: string };

export type WorktreeDeliveryState = {
  workspaceId: string;
  threadId: string | null;
  name: string;
  overview: DeliveryOverview | null;
  checkoutKey: string | null;
  targets: Record<string, string>;
  preview: DeliveryPreview | null;
  isLoading: boolean;
  isPreviewing: boolean;
  error: string | null;
  operation: DeliveryOperation | null;
  operationError: string | null;
};

type Visit = { environment: object; id: object; state: WorktreeDeliveryState; overviewVersion: number; previewVersion: number };

export function isDeliveryAttemptPending(attempt: DeliveryAttempt) {
  return attempt.status !== "completed" && attempt.status !== "unchanged";
}

export function canExecuteDelivery(state: WorktreeDeliveryState) {
  const { preview, checkoutKey, overview } = state;
  return Boolean(preview && overview && !state.operation && !state.isLoading && !state.isPreviewing
    && preview.workspaceId === state.workspaceId && preview.checkoutKey === checkoutKey
    && preview.target.branch === state.targets[preview.checkoutKey]
    && !preview.blockers.length
    && (preview.comparison.kind === "fastForward" || preview.comparison.kind === "mergeable")
    && !overview.attempts.some((attempt) => attempt.checkoutKey === checkoutKey && isDeliveryAttemptPending(attempt)));
}

export function useWorktreeDelivery(workspace: WorkspaceInfo | null, threadId: string | null, repositoryScope: string | null, onRepositoryChanged?: () => void) {
  const environmentKey = JSON.stringify([workspace?.id, workspace?.path, workspace?.project, workspace?.worktree?.managed, threadId, repositoryScope]);
  const environment = useRef({ key: environmentKey, token: {} });
  if (environment.current.key !== environmentKey) environment.current = { key: environmentKey, token: {} };
  const token = environment.current.token;
  const active = useRef<Visit | null>(null);
  // Native operations outlive a modal visit. Never submit twice while an earlier visit is running.
  const pending = useRef(new Map<string, DeliveryOperation>());
  const [rendered, setRendered] = useState<Visit | null>(null);
  const renderedVisitId = rendered?.id;
  const renderedPreview = rendered?.state.preview;
  const renderedPreviewVersion = rendered?.previewVersion;
  const canOpen = Boolean(workspace?.kind === "worktree" && workspace.worktree?.managed);
  const current = useCallback((visit: Visit) => active.current?.id === visit.id && environment.current.token === visit.environment, []);
  const publish = useCallback((visit: Visit, patch: Partial<WorktreeDeliveryState>) => {
    if (!current(visit)) return;
    visit.state = { ...visit.state, ...patch };
    setRendered({ ...visit });
  }, [current]);

  useEffect(() => () => { active.current = null; }, []);

  const load = useCallback(async (visit: Visit) => {
    const version = ++visit.overviewVersion;
    ++visit.previewVersion;
    publish(visit, { isLoading: true, isPreviewing: false, preview: null, error: null });
    try {
      const overview = await getWorktreeDelivery(visit.state.workspaceId, visit.state.threadId);
      if (!current(visit) || version !== visit.overviewVersion) return;
      const checkoutKey = overview.checkouts.some((entry) => entry.key === visit.state.checkoutKey)
        ? visit.state.checkoutKey : overview.checkouts[0]?.key ?? null;
      const targets = Object.fromEntries(overview.checkouts.map((checkout) => {
        const previous = visit.state.targets[checkout.key];
        const selected = checkout.targetBranches.some((branch) => branch.name === previous)
          ? previous : checkout.defaultTargetBranch ?? "";
        return [checkout.key, selected];
      }));
      publish(visit, { overview, name: overview.name, checkoutKey, targets, isLoading: false });
    } catch (error) {
      if (current(visit) && version === visit.overviewVersion) publish(visit, { overview: null, isLoading: false, error: String(error) });
    }
  }, [current, publish]);

  const open = useCallback(() => {
    if (!canOpen || !workspace || environment.current.token !== token) return;
    const visit: Visit = {
      environment: token, id: {}, overviewVersion: 0, previewVersion: 0,
      state: { workspaceId: workspace.id, threadId, name: workspace.name, overview: null, checkoutKey: null, targets: {}, preview: null, isLoading: true, isPreviewing: false, error: null, operation: null, operationError: null },
    };
    active.current = visit;
    setRendered({ ...visit });
    void load(visit);
  }, [canOpen, load, threadId, token, workspace]);
  const close = useCallback(() => { active.current = null; setRendered(null); }, []);
  const refresh = useCallback(() => {
    const visit = active.current;
    if (visit && current(visit) && !visit.state.operation) return load(visit);
    return Promise.resolve();
  }, [current, load]);
  const selectCheckout = useCallback((checkoutKey: string) => {
    const visit = active.current;
    if (!visit || !current(visit) || visit.state.operation || !visit.state.overview?.checkouts.some((entry) => entry.key === checkoutKey)) return;
    ++visit.previewVersion;
    publish(visit, { checkoutKey, preview: null, isPreviewing: false, error: null });
  }, [current, publish]);
  const selectTarget = useCallback((targetBranch: string) => {
    const visit = active.current;
    const checkoutKey = visit?.state.checkoutKey;
    if (!visit || !current(visit) || visit.state.operation || !checkoutKey) return;
    ++visit.previewVersion;
    publish(visit, { targets: { ...visit.state.targets, [checkoutKey]: targetBranch }, preview: null, isPreviewing: false, error: null });
  }, [current, publish]);
  const preview = useCallback(async () => {
    const visit = active.current;
    const checkoutKey = visit?.state.checkoutKey;
    const target = checkoutKey ? visit?.state.targets[checkoutKey] : null;
    if (!visit || !current(visit) || !visit.state.overview || !checkoutKey || !target || visit.state.operation || visit.state.isLoading || visit.state.isPreviewing) return;
    const version = ++visit.previewVersion;
    const { workspaceId, threadId: capturedThreadId } = visit.state;
    publish(visit, { isPreviewing: true, preview: null, error: null });
    try {
      const result = await previewWorktreeDelivery(workspaceId, capturedThreadId, checkoutKey, target);
      if (current(visit) && version === visit.previewVersion) publish(visit, { preview: result, isPreviewing: false });
    } catch (error) {
      if (current(visit) && version === visit.previewVersion) publish(visit, { error: String(error), isPreviewing: false });
    }
  }, [current, publish]);

  const runOperation = useCallback(async (visit: Visit, operation: DeliveryOperation, invoke: () => Promise<DeliveryAttempt>) => {
    const workspaceId = visit.state.workspaceId;
    if (!current(visit) || visit.state.operation || visit.state.isLoading || visit.state.isPreviewing || pending.current.has(workspaceId)) return;
    pending.current.set(workspaceId, operation);
    ++visit.overviewVersion;
    ++visit.previewVersion;
    publish(visit, { operation, operationError: null, preview: null, error: null });
    try {
      const attempt = await invoke();
      if (current(visit)) {
        const overview = visit.state.overview;
        publish(visit, { operation: null, overview: overview ? { ...overview,
          attempts: overview.attempts.some((entry) => entry.id === attempt.id)
            ? overview.attempts.map((entry) => entry.id === attempt.id ? attempt : entry)
            : [...overview.attempts, attempt],
        } : null });
      }
    } catch (error) {
      if (current(visit)) publish(visit, { operation: null, operationError: String(error) });
    } finally {
      if (pending.current.get(workspaceId) === operation) pending.current.delete(workspaceId);
    }
    if (!current(visit)) return;
    // Even a rejected IPC may have changed Git before its response was lost. Read the journal.
    onRepositoryChanged?.();
    await load(visit);
  }, [current, load, onRepositoryChanged, publish]);

  const execute = useCallback(async () => {
    const visit = active.current;
    if (environment.current.token !== token || !visit || visit.id !== renderedVisitId || !current(visit)
      || visit.previewVersion !== renderedPreviewVersion || !renderedPreview || visit.state.preview !== renderedPreview
      || !canExecuteDelivery(visit.state)) return;
    const { workspaceId, threadId: capturedThreadId } = visit.state;
    const request = { attemptId: crypto.randomUUID(), checkoutKey: renderedPreview.checkoutKey,
      targetBranch: renderedPreview.target.branch, sourceOid: renderedPreview.source.oid, targetOid: renderedPreview.target.oid };
    await runOperation(visit, { kind: "execute", checkoutKey: request.checkoutKey, attemptId: request.attemptId },
      () => executeWorktreeDelivery(workspaceId, capturedThreadId, request));
  }, [current, renderedPreview, renderedPreviewVersion, renderedVisitId, runOperation, token]);

  const recover = useCallback(async (attemptId: string, kind: "inspect" | "finish") => {
    const visit = active.current;
    const attempt = visit?.state.overview?.attempts.find((entry) => entry.id === attemptId);
    if (environment.current.token !== token || !visit || visit.id !== renderedVisitId || !current(visit) || !attempt || !isDeliveryAttemptPending(attempt)
      || (kind === "finish" && attempt.status !== "readyToFinish")) return;
    const { workspaceId, threadId: capturedThreadId } = visit.state;
    const invoke = kind === "inspect" ? inspectWorktreeDeliveryAttempt : finishWorktreeDeliveryAttempt;
    await runOperation(visit, { kind, attemptId, checkoutKey: attempt.checkoutKey },
      () => invoke(workspaceId, capturedThreadId, attemptId));
  }, [current, renderedVisitId, runOperation, token]);
  const inspect = useCallback((attemptId: string) => recover(attemptId, "inspect"), [recover]);
  const finish = useCallback((attemptId: string) => recover(attemptId, "finish"), [recover]);

  return { state: rendered?.environment === token ? rendered.state : null, canOpen, open, close, refresh, selectCheckout, selectTarget, preview, execute, inspect, finish };
}
