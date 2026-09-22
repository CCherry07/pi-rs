// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import type { WorkspaceInfo } from "../../../types";
import { executeWorktreeDelivery, finishWorktreeDeliveryAttempt, getWorktreeDelivery, inspectWorktreeDeliveryAttempt, previewWorktreeDelivery, type DeliveryAttempt, type DeliveryOverview, type DeliveryPreview } from "../../../services/tauri";
import { useWorktreeDelivery } from "./useWorktreeDelivery";

vi.mock("../../../services/tauri", () => ({ getWorktreeDelivery: vi.fn(), previewWorktreeDelivery: vi.fn(), executeWorktreeDelivery: vi.fn(), inspectWorktreeDeliveryAttempt: vi.fn(), finishWorktreeDeliveryAttempt: vi.fn() }));
const workspace: WorkspaceInfo = { id: "managed", name: "Feature", path: "/worktrees/app", kind: "worktree", worktree: { branch: "feature", managed: true }, settings: { sidebarCollapsed: false } };
const initial = { workspace, threadId: "session-a", scope: "checkout-a" };
function overview(name = "Feature"): DeliveryOverview {
  return { workspaceId: workspace.id, name, sharedRoots: [], attempts: [], checkouts: ["app", "api"].map((key) => ({
    key, workdir: `/worktrees/${key}`, originWorkdir: `/source/${key}`, rootIds: [key], createdBranch: "feature", startOid: "start",
    head: { oid: "source", branch: "feature" }, changes: [], changesTruncated: false,
    targetBranches: [{ name: "main", oid: "target" }, { name: "release", oid: "release" }], defaultTargetBranch: "main", warnings: [], error: null,
  })) };
}
function preview(sourceOid = "source"): DeliveryPreview {
  return { workspaceId: workspace.id, checkoutKey: "app", sourceWorkdir: "/worktrees/app", targetWorkdir: "/source/app",
    source: { oid: sourceOid, branch: "feature" }, target: { oid: "target", branch: "main" },
    sourceChanges: [], targetChanges: [], sourceChangesTruncated: false, targetChangesTruncated: false, blockers: [], warnings: [],
    comparison: { sourceOid, targetOid: "target", mergeBaseOids: ["target"], ahead: 1, behind: 0, kind: "fastForward", commits: [], commitsTruncated: false, files: [], filesTruncated: false, conflicts: [], warnings: [] },
  };
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}
function attempt(status: DeliveryAttempt["status"] = "completed", checkoutKey = "app", id = "attempt-id"): DeliveryAttempt {
  return { id, checkoutKey, sourceOid: "source", targetOid: "target", targetBranch: "main", resultOid: status === "unchanged" ? null : "result",
    status, createdAt: "2026-09-22T00:00:00Z", updatedAt: "2026-09-22T00:00:00Z", error: null };
}
function setup(onRepositoryChanged = vi.fn()) {
  return { ...renderHook((props) => useWorktreeDelivery(props.workspace, props.threadId, props.scope, onRepositoryChanged), { initialProps: initial }), onRepositoryChanged };
}
async function openPreview(result: { current: ReturnType<typeof useWorktreeDelivery> }) {
  await act(async () => { result.current.open(); });
  await act(async () => { await result.current.preview(); });
}
beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(getWorktreeDelivery).mockResolvedValue(overview());
  vi.mocked(previewWorktreeDelivery).mockResolvedValue(preview());
});

it("captures the group and session while repository and target choices only affect comparison", async () => {
  const { result } = setup();
  await act(async () => { result.current.open(); });
  expect(getWorktreeDelivery).toHaveBeenCalledWith("managed", "session-a");
  act(() => { result.current.selectCheckout("api"); result.current.selectTarget("release"); });
  await act(async () => { await result.current.preview(); });
  expect(previewWorktreeDelivery).toHaveBeenCalledWith("managed", "session-a", "api", "release");
});

it.each(["project", "session", "checkout"])("discards overview responses after %s A → B → A", async (change) => {
  const old = deferred<DeliveryOverview>();
  vi.mocked(getWorktreeDelivery).mockReturnValueOnce(old.promise).mockResolvedValueOnce(overview("Latest"));
  const { result, rerender } = setup();
  act(() => { result.current.open(); });
  act(() => { rerender({
    workspace: change === "project" ? { ...workspace, id: "other" } : workspace,
    threadId: change === "session" ? "session-b" : initial.threadId,
    scope: change === "checkout" ? "checkout-b" : initial.scope,
  }); });
  expect(result.current.state).toBeNull();
  act(() => { rerender(initial); });
  await act(async () => { result.current.open(); });
  await act(async () => { old.resolve(overview("Stale")); });
  expect(result.current.state?.overview?.name).toBe("Latest");
  expect(result.current.state?.isLoading).toBe(false);
});

it("ignores old failures and loading completions while the refreshed overview is pending", async () => {
  const old = deferred<DeliveryOverview>();
  const latest = deferred<DeliveryOverview>();
  vi.mocked(getWorktreeDelivery).mockReturnValueOnce(old.promise).mockReturnValueOnce(latest.promise);
  const { result } = setup();
  act(() => { result.current.open(); });
  let refreshing!: Promise<void>;
  act(() => { refreshing = result.current.refresh(); });
  await act(async () => { old.reject(new Error("old error")); });
  expect(result.current.state?.error).toBeNull();
  expect(result.current.state?.isLoading).toBe(true);
  await act(async () => { latest.resolve(overview()); await refreshing; });
  expect(result.current.state?.isLoading).toBe(false);
});

it.each(["target", "repository"])("discards previews after %s A → B → A", async (change) => {
  const old = deferred<DeliveryPreview>();
  const latest = deferred<DeliveryPreview>();
  vi.mocked(previewWorktreeDelivery).mockReturnValueOnce(old.promise).mockReturnValueOnce(latest.promise);
  const { result } = setup();
  await act(async () => { result.current.open(); });
  let first!: Promise<void>;
  act(() => { first = result.current.preview(); });
  act(() => {
    if (change === "target") { result.current.selectTarget("release"); result.current.selectTarget("main"); }
    else { result.current.selectCheckout("api"); result.current.selectCheckout("app"); }
  });
  let second!: Promise<void>;
  act(() => { second = result.current.preview(); });
  await act(async () => { old.reject(new Error("stale preview")); await first; });
  expect(result.current.state?.isPreviewing).toBe(true);
  expect(result.current.state?.error).toBeNull();
  await act(async () => { latest.resolve(preview("latest")); await second; });
  expect(result.current.state?.preview?.source.oid).toBe("latest");
});

it("cannot reopen a closed dialog with an old preview and requires a new overview visit", async () => {
  const old = deferred<DeliveryPreview>();
  vi.mocked(previewWorktreeDelivery).mockReturnValueOnce(old.promise);
  const { result } = setup();
  await act(async () => { result.current.open(); });
  let pending!: Promise<void>;
  act(() => { pending = result.current.preview(); });
  act(() => { result.current.close(); });
  await act(async () => { result.current.open(); });
  await act(async () => { old.resolve(preview()); await pending; });
  expect(result.current.state?.preview).toBeNull();
  expect(getWorktreeDelivery).toHaveBeenCalledTimes(2);
});

it("only offers group delivery for an owned managed workspace", () => {
  const { result, rerender } = setup();
  expect(result.current.canOpen).toBe(true);
  act(() => { rerender({ ...initial, workspace: { ...workspace, worktree: { branch: "feature", managed: false } } }); });
  act(() => { result.current.open(); });
  expect(result.current.canOpen).toBe(false);
  expect(getWorktreeDelivery).not.toHaveBeenCalled();
});

it("submits one pinned merge and refreshes the same scope without changing either repository selection", async () => {
  const pending = deferred<DeliveryAttempt>();
  vi.mocked(executeWorktreeDelivery).mockReturnValue(pending.promise);
  const { result, onRepositoryChanged } = setup();
  await act(async () => { result.current.open(); });
  await act(async () => { await result.current.preview(); });
  let executing!: Promise<void>;
  act(() => {
    executing = result.current.execute();
    void result.current.execute();
    result.current.selectCheckout("api");
    result.current.selectTarget("release");
    void result.current.refresh();
  });
  expect(executeWorktreeDelivery).toHaveBeenCalledTimes(1);
  const request = vi.mocked(executeWorktreeDelivery).mock.calls[0][2];
  expect(request).toEqual({ attemptId: expect.stringMatching(/^[0-9a-f-]{36}$/), checkoutKey: "app", targetBranch: "main", sourceOid: "source", targetOid: "target" });
  expect(executeWorktreeDelivery).toHaveBeenCalledWith("managed", "session-a", request);
  expect(result.current.state?.checkoutKey).toBe("app");
  expect(result.current.state?.targets.app).toBe("main");
  expect(result.current.state?.preview).toBeNull();
  expect(getWorktreeDelivery).toHaveBeenCalledTimes(1);
  const completed = { ...attempt(), id: request.attemptId };
  vi.mocked(getWorktreeDelivery).mockResolvedValue({ ...overview(), attempts: [completed] });
  await act(async () => { pending.resolve(completed); await executing; });
  expect(result.current.state?.operation).toBeNull();
  expect(result.current.state?.overview?.attempts).toEqual([completed]);
  expect(result.current.state?.checkoutKey).toBe("app");
  expect(onRepositoryChanged).toHaveBeenCalledTimes(1);
});

it.each(["project", "session", "checkout"])("ignores mutation results and old execute callbacks after %s A → B → A", async (change) => {
  const pending = deferred<DeliveryAttempt>();
  vi.mocked(executeWorktreeDelivery).mockReturnValue(pending.promise);
  const { result, rerender, onRepositoryChanged } = setup();
  await openPreview(result);
  const oldExecute = result.current.execute;
  let executing!: Promise<void>;
  act(() => { executing = result.current.execute(); });
  act(() => { rerender({ workspace: change === "project" ? { ...workspace, id: "other" } : workspace,
    threadId: change === "session" ? "session-b" : initial.threadId, scope: change === "checkout" ? "checkout-b" : initial.scope }); });
  expect(result.current.state).toBeNull();
  act(() => { rerender(initial); });
  vi.mocked(getWorktreeDelivery).mockResolvedValue(overview("Latest"));
  await openPreview(result);
  await act(async () => { pending.resolve(attempt()); await executing; await oldExecute(); });
  expect(result.current.state?.overview?.name).toBe("Latest");
  expect(result.current.state?.overview?.attempts).toEqual([]);
  expect(result.current.state?.operation).toBeNull();
  expect(result.current.state?.operationError).toBeNull();
  expect(result.current.state?.preview?.source.oid).toBe("source");
  expect(executeWorktreeDelivery).toHaveBeenCalledTimes(1);
  expect(getWorktreeDelivery).toHaveBeenCalledTimes(2);
  expect(onRepositoryChanged).not.toHaveBeenCalled();
});

it("reopens persisted attempts after a lost response without applying old failure or loading state", async () => {
  const pending = deferred<DeliveryAttempt>();
  vi.mocked(executeWorktreeDelivery).mockReturnValue(pending.promise);
  const { result, onRepositoryChanged } = setup();
  await openPreview(result);
  let executing!: Promise<void>;
  act(() => { executing = result.current.execute(); result.current.close(); });
  const inProgress = attempt("applying");
  vi.mocked(getWorktreeDelivery).mockResolvedValue({ ...overview(), attempts: [inProgress] });
  await act(async () => { result.current.open(); });
  await act(async () => { pending.reject(new Error("lost old response")); await executing; });
  expect(result.current.state?.overview?.attempts).toEqual([inProgress]);
  expect(result.current.state?.operationError).toBeNull();
  expect(result.current.state?.isLoading).toBe(false);
  expect(onRepositoryChanged).not.toHaveBeenCalled();
  const completed = attempt();
  vi.mocked(getWorktreeDelivery).mockResolvedValue({ ...overview(), attempts: [completed] });
  await act(async () => { await result.current.refresh(); });
  expect(result.current.state?.overview?.attempts).toEqual([completed]);
});

it("does not submit from a callback retained by a closed modal visit", async () => {
  const { result } = setup();
  await openPreview(result);
  const previousExecute = result.current.execute;
  act(() => { result.current.close(); });
  await openPreview(result);
  await act(async () => { await previousExecute(); });
  expect(executeWorktreeDelivery).not.toHaveBeenCalled();
  expect(result.current.state?.preview?.source.oid).toBe("source");
});

it.each(["target", "checkout", "refresh"])("requires the execute callback for the current preview after %s changes within one modal visit", async (change) => {
  // Reusing the same DTO proves that the preview generation, not only object identity, is checked.
  const value = preview();
  vi.mocked(previewWorktreeDelivery).mockResolvedValue(value);
  vi.mocked(executeWorktreeDelivery).mockResolvedValue(attempt());
  const { result, onRepositoryChanged } = setup();
  await openPreview(result);
  const previousExecute = result.current.execute;
  await act(async () => {
    if (change === "target") { result.current.selectTarget("release"); result.current.selectTarget("main"); }
    else if (change === "checkout") { result.current.selectCheckout("api"); result.current.selectCheckout("app"); }
    else await result.current.refresh();
  });
  await act(async () => { await result.current.preview(); });
  await act(async () => { await previousExecute(); });
  expect(executeWorktreeDelivery).not.toHaveBeenCalled();
  expect(onRepositoryChanged).not.toHaveBeenCalled();
  expect(result.current.state?.preview).toBe(value);
  await act(async () => { await result.current.execute(); });
  expect(executeWorktreeDelivery).toHaveBeenCalledTimes(1);
  expect(executeWorktreeDelivery).toHaveBeenCalledWith("managed", "session-a", expect.objectContaining({
    checkoutKey: "app", targetBranch: "main", sourceOid: "source", targetOid: "target",
  }));
});

it("requires a successful journal refresh before another preview or merge after the journal becomes unavailable", async () => {
  const { result } = setup();
  await openPreview(result);
  vi.mocked(getWorktreeDelivery).mockRejectedValueOnce(new Error("Journal is locked"));
  await act(async () => { await result.current.refresh(); });
  expect(result.current.state?.error).toContain("Journal is locked");
  await act(async () => { await result.current.preview(); });
  await act(async () => { await result.current.execute(); });
  expect(result.current.state?.overview).toBeNull();
  expect(previewWorktreeDelivery).toHaveBeenCalledTimes(1);
  expect(executeWorktreeDelivery).not.toHaveBeenCalled();
  const unresolved = { ...overview(), attempts: [attempt("needsAttention")] };
  vi.mocked(getWorktreeDelivery).mockResolvedValue(unresolved);
  await act(async () => { await result.current.refresh(); });
  await act(async () => { await result.current.preview(); });
  await act(async () => { await result.current.execute(); });
  expect(result.current.state?.overview).toBe(unresolved);
  expect(executeWorktreeDelivery).not.toHaveBeenCalled();
});

it("reads the journal after a failed IPC and requires an explicit finish of the recorded attempt", async () => {
  vi.mocked(executeWorktreeDelivery).mockRejectedValue(new Error("response lost"));
  const { result, onRepositoryChanged } = setup();
  await openPreview(result);
  const interrupted = attempt("readyToFinish");
  vi.mocked(getWorktreeDelivery).mockResolvedValue({ ...overview(), attempts: [interrupted] });
  await act(async () => { await result.current.execute(); });
  expect(result.current.state?.operationError).toContain("response lost");
  expect(result.current.state?.overview?.attempts).toEqual([interrupted]);
  expect(finishWorktreeDeliveryAttempt).not.toHaveBeenCalled();
  const completed = attempt();
  vi.mocked(finishWorktreeDeliveryAttempt).mockResolvedValue(completed);
  vi.mocked(getWorktreeDelivery).mockResolvedValue({ ...overview(), attempts: [completed] });
  await act(async () => { await result.current.finish(interrupted.id); });
  expect(finishWorktreeDeliveryAttempt).toHaveBeenCalledWith("managed", "session-a", interrupted.id);
  expect(result.current.state?.overview?.attempts).toEqual([completed]);
  expect(result.current.state?.operationError).toBeNull();
  expect(onRepositoryChanged).toHaveBeenCalledTimes(2);
});

it("checks attempts across checkout history without switching the selected checkout or finishing an unsafe state", async () => {
  const other = attempt("needsAttention", "api", "api-attempt");
  vi.mocked(getWorktreeDelivery).mockResolvedValue({ ...overview(), attempts: [other] });
  const { result } = setup();
  await act(async () => { result.current.open(); });
  await act(async () => { await result.current.finish(other.id); await result.current.inspect("unknown"); });
  expect(finishWorktreeDeliveryAttempt).not.toHaveBeenCalled();
  expect(inspectWorktreeDeliveryAttempt).not.toHaveBeenCalled();
  const inspected = { ...other, status: "unchanged" as const, resultOid: null };
  vi.mocked(inspectWorktreeDeliveryAttempt).mockResolvedValue(inspected);
  vi.mocked(getWorktreeDelivery).mockResolvedValue({ ...overview(), attempts: [inspected] });
  await act(async () => { await result.current.inspect(other.id); });
  expect(inspectWorktreeDeliveryAttempt).toHaveBeenCalledWith("managed", "session-a", "api-attempt");
  expect(result.current.state?.checkoutKey).toBe("app");
  expect(result.current.state?.targets).toEqual({ app: "main", api: "main" });
  expect(result.current.state?.overview?.attempts).toEqual([inspected]);
});

it.each(["conflicts", "unrelated", "unsupported", "upToDate"] as const)("does not execute a %s preview", async (kind) => {
  const value = preview();
  vi.mocked(previewWorktreeDelivery).mockResolvedValue({ ...value, comparison: { ...value.comparison, kind } });
  const { result } = setup();
  await openPreview(result);
  await act(async () => { await result.current.execute(); });
  expect(executeWorktreeDelivery).not.toHaveBeenCalled();
});

it.each(["blocker", "unfinished"])("does not execute with a native %s", async (condition) => {
  if (condition === "blocker") vi.mocked(previewWorktreeDelivery).mockResolvedValue({ ...preview(), blockers: ["Target changed"] });
  else vi.mocked(getWorktreeDelivery).mockResolvedValue({ ...overview(), attempts: [attempt("needsAttention")] });
  const { result } = setup();
  await openPreview(result);
  await act(async () => { await result.current.execute(); });
  expect(executeWorktreeDelivery).not.toHaveBeenCalled();
});
