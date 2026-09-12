// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { subscribePiEvents } from "@services/events";
import { getDesktopWidgets, type DesktopWidgetSnapshot } from "@services/tauri";
import type { PiEvent } from "@/types";
import { applyWidget, useDesktopWidgets } from "./useDesktopWidgets";

vi.mock("@services/events", () => ({ subscribePiEvents: vi.fn() }));
vi.mock("@services/tauri", () => ({ getDesktopWidgets: vi.fn() }));

const listeners = new Set<(event: PiEvent) => void>();
const unsubscribe = vi.fn();

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

function emit(method: string, params: Record<string, unknown>, workspaceId = "workspace") {
  for (const listener of listeners) listener({ workspace_id: workspaceId, message: { method, params } });
}

function update(key: string, value: unknown, version: number, threadId = "thread", workspaceId = "workspace") {
  emit("thread/widgetUpdated", { threadId, key, value, version }, workspaceId);
}

beforeEach(() => {
  vi.resetAllMocks();
  listeners.clear();
  vi.mocked(subscribePiEvents).mockImplementation((listener, options) => {
    listeners.add(listener);
    options?.onReady?.();
    return () => { unsubscribe(); listeners.delete(listener); };
  });
});
afterEach(cleanup);

describe("desktop widget snapshots", () => {
  it("waits for the native event listener before reading its initial snapshot", async () => {
    const history = deferred<DesktopWidgetSnapshot>();
    let ready: (() => void) | undefined;
    vi.mocked(getDesktopWidgets).mockReturnValue(history.promise);
    vi.mocked(subscribePiEvents).mockImplementationOnce((listener, options) => {
      listeners.add(listener);
      ready = options?.onReady;
      return () => { listeners.delete(listener); };
    });
    const { result } = renderHook(() => useDesktopWidgets("workspace", "thread"));
    expect(getDesktopWidgets).not.toHaveBeenCalled();
    act(() => ready?.());
    expect(getDesktopWidgets).toHaveBeenCalledExactlyOnceWith("workspace", "thread");
    act(() => update("check:progress", "live", 3));
    await act(async () => history.resolve({ widgets: { "check:progress": "saved" }, versions: { "check:progress": 2 } }));
    expect(result.current.widgets).toEqual({ "check:progress": "live" });
  });

  it("keeps newer live updates over a concurrent older history response", async () => {
    const history = deferred<DesktopWidgetSnapshot>();
    vi.mocked(getDesktopWidgets).mockReturnValue(history.promise);
    const { result } = renderHook(() => useDesktopWidgets("workspace", "thread"));
    act(() => {
      update("check:progress", { done: 2 }, 20);
      update("unrelated", "wrong thread", 99, "other");
      update("unrelated", "wrong workspace", 99, "thread", "other");
    });
    expect(result.current.widgets).toEqual({ "check:progress": { done: 2 } });
    await act(async () => history.resolve({ widgets: { "check:progress": { done: 1 }, "check:title": "Review" }, versions: { "check:progress": 10, "check:title": 2 } }));
    expect(result.current.widgets).toEqual({ "check:progress": { done: 2 }, "check:title": "Review" });
    act(() => {
      update("check:progress", { done: 0 }, 19);
      update("check:progress", { done: 3 }, 21);
    });
    expect(result.current.widgets).toEqual({ "check:progress": { done: 3 }, "check:title": "Review" });
  });

  it("preserves tombstones through hydration so stale values cannot reappear", async () => {
    const history = deferred<DesktopWidgetSnapshot>();
    vi.mocked(getDesktopWidgets).mockReturnValue(history.promise);
    const { result } = renderHook(() => useDesktopWidgets("workspace", "thread"));
    act(() => update("check:progress", null, 20));
    await act(async () => history.resolve({ widgets: { "check:progress": "stale" }, versions: { "check:progress": 19 } }));
    expect(result.current.widgets).toEqual({});
    act(() => update("check:progress", "late duplicate", 20));
    expect(result.current.widgets).toEqual({});
    act(() => update("check:progress", "new", 21));
    expect(result.current.widgets).toEqual({ "check:progress": "new" });
  });

  it("atomically replaces branch state and rejects previous hydration epochs", async () => {
    const previous = deferred<DesktopWidgetSnapshot>();
    const replacement = deferred<DesktopWidgetSnapshot>();
    vi.mocked(getDesktopWidgets).mockReturnValueOnce(previous.promise).mockReturnValueOnce(replacement.promise);
    const { result } = renderHook(() => useDesktopWidgets("workspace", "thread"));
    act(() => update("check:progress", "old branch", 99));
    act(() => emit("thread/replaced", { thread: { id: "thread" } }));
    expect(result.current.widgets).toEqual({ "check:progress": "old branch" });
    expect(getDesktopWidgets).toHaveBeenCalledTimes(2);
    act(() => update("check:progress", "new live", 5));
    await act(async () => previous.resolve({ widgets: { "check:progress": "old hydration" }, versions: { "check:progress": 100 } }));
    expect(result.current.widgets).toEqual({ "check:progress": "old branch" });
    await act(async () => replacement.resolve({ widgets: { "check:progress": "new hydration" }, versions: { "check:progress": 3 } }));
    expect(result.current.widgets).toEqual({ "check:progress": "new live" });
  });

  it("does not let a stale failed hydration discard current pending live updates", async () => {
    const previous = deferred<DesktopWidgetSnapshot>();
    const replacement = deferred<DesktopWidgetSnapshot>();
    vi.mocked(getDesktopWidgets).mockReturnValueOnce(previous.promise).mockReturnValueOnce(replacement.promise);
    const { result } = renderHook(() => useDesktopWidgets("workspace", "thread"));
    act(() => emit("thread/replaced", { threadId: "thread" }));
    await act(async () => previous.reject(new Error("previous session retired")));
    act(() => update("check:progress", "current update", 6));
    await act(async () => replacement.resolve({ widgets: { "check:progress": "current history" }, versions: { "check:progress": 2 } }));
    expect(result.current.widgets).toEqual({ "check:progress": "current update" });
  });

  it("cleans up subscriptions and ignores pending reads when the workspace changes", async () => {
    const previous = deferred<DesktopWidgetSnapshot>();
    const replacement = deferred<DesktopWidgetSnapshot>();
    vi.mocked(getDesktopWidgets).mockReturnValueOnce(previous.promise).mockReturnValueOnce(replacement.promise);
    const initialProps: { workspaceId: string | null } = { workspaceId: "workspace" };
    const { result, rerender, unmount } = renderHook(({ workspaceId }) => useDesktopWidgets(workspaceId, "thread"), { initialProps });
    act(() => update("check:progress", "old workspace", 50));
    rerender({ workspaceId: "replacement" });
    expect(result.current.widgets).toEqual({});
    expect(unsubscribe).toHaveBeenCalledOnce();
    expect(listeners.size).toBe(1);
    await act(async () => previous.resolve({ widgets: { "check:progress": "late old workspace" }, versions: { "check:progress": 100 } }));
    await act(async () => replacement.resolve({ widgets: { "check:progress": "new workspace" }, versions: { "check:progress": 2 } }));
    act(() => update("check:progress", "wrong workspace event", 100));
    expect(result.current.widgets).toEqual({ "check:progress": "new workspace" });
    rerender({ workspaceId: null });
    expect(result.current.widgets).toEqual({});
    expect(listeners.size).toBe(0);
    expect(getDesktopWidgets).toHaveBeenCalledTimes(2);
    unmount();
  });

  it("keeps receiving live state when history is unavailable", async () => {
    const history = deferred<DesktopWidgetSnapshot>();
    vi.mocked(getDesktopWidgets).mockReturnValue(history.promise);
    const { result } = renderHook(() => useDesktopWidgets("workspace", "thread"));
    act(() => update("check:progress", "live while loading", 1));
    await act(async () => history.reject(new Error("missing history")));
    expect(result.current.widgets).toEqual({ "check:progress": "live while loading" });
    act(() => update("check:progress", "still live", 2));
    expect(result.current.widgets).toEqual({ "check:progress": "still live" });
  });

  it("orders opaque values by per-key sequence and rejects invalid revisions", () => {
    const snapshot = { widgets: { "check:progress": "current" }, versions: { "check:progress": 8 } };
    for (const version of [-1, 2.5, Infinity, Number.NaN, Number.MAX_SAFE_INTEGER + 1, 7, 8]) {
      expect(applyWidget(snapshot, "check:progress", "invalid", version)).toBe(snapshot);
    }
    const next = applyWidget(snapshot, "other:progress", [true, { finished: false }], 1);
    expect(next.widgets).toEqual({ "check:progress": "current", "other:progress": [true, { finished: false }] });
    expect(snapshot.widgets).toEqual({ "check:progress": "current" });
  });

  it("preserves the command scope during updates and clears it until replacement hydration", async () => {
    const initial = deferred<DesktopWidgetSnapshot>();
    const replacement = deferred<DesktopWidgetSnapshot>();
    const workspace = deferred<DesktopWidgetSnapshot>();
    vi.mocked(getDesktopWidgets).mockReturnValueOnce(initial.promise)
      .mockReturnValueOnce(replacement.promise).mockReturnValueOnce(workspace.promise);
    const { result, rerender } = renderHook(({ workspaceId }) => useDesktopWidgets(workspaceId, "thread"), { initialProps: { workspaceId: "workspace" } });
    expect(result.current.scopeToken).toBeUndefined();
    await act(async () => initial.resolve({ widgets: {}, versions: {}, scopeToken: "initial-scope" }));
    act(() => update("check:progress", "active", 1));
    expect(result.current.scopeToken).toBe("initial-scope");
    act(() => emit("thread/replaced", { threadId: "thread" }));
    expect(result.current.scopeToken).toBeUndefined();
    act(() => update("check:progress", "waiting for hydration", 3));
    expect(result.current.scopeToken).toBeUndefined();
    await act(async () => replacement.resolve({ widgets: {}, versions: {}, scopeToken: "replacement-scope" }));
    expect(result.current.scopeToken).toBe("replacement-scope");
    expect(result.current.widgets).toEqual({ "check:progress": "waiting for hydration" });
    rerender({ workspaceId: "other" });
    expect(result.current.scopeToken).toBeUndefined();
    await act(async () => workspace.resolve({ widgets: {}, versions: {}, scopeToken: "other-scope" }));
    expect(result.current.scopeToken).toBe("other-scope");
  });

  it("applies branch removals and queued tombstones without reusing old versions", async () => {
    const refresh = deferred<DesktopWidgetSnapshot>();
    vi.mocked(getDesktopWidgets)
      .mockResolvedValueOnce({ widgets: { "check:removed": "old", "check:kept": "old branch" }, versions: { "check:removed": 99, "check:kept": 99 }, scopeToken: "old-scope" })
      .mockReturnValueOnce(refresh.promise);
    const { result } = renderHook(() => useDesktopWidgets("workspace", "thread"));
    await act(async () => {});
    act(() => emit("thread/replaced", { threadId: "thread" }));
    expect(result.current.widgets).toEqual({ "check:removed": "old", "check:kept": "old branch" });
    expect(result.current.scopeToken).toBeUndefined();
    act(() => update("check:removed", null, 4));
    await act(async () => refresh.resolve({ widgets: { "check:removed": "saved", "check:new": "new branch" }, versions: { "check:removed": 3, "check:new": 1 }, scopeToken: "new-scope" }));
    expect(result.current.widgets).toEqual({ "check:new": "new branch" });
    expect(result.current.versions).toEqual({ "check:removed": 4, "check:new": 1 });
    expect(result.current.scopeToken).toBe("new-scope");
    act(() => update("check:removed", "late duplicate", 4));
    expect(result.current.widgets).toEqual({ "check:new": "new branch" });
  });

  it("immediately retires widgets and scope on a generation replacement even if refresh fails", async () => {
    const refresh = deferred<DesktopWidgetSnapshot>();
    vi.mocked(getDesktopWidgets).mockResolvedValueOnce({ widgets: { "check:progress": "old generation" }, versions: { "check:progress": 3 }, scopeToken: "old-scope" })
      .mockReturnValueOnce(refresh.promise);
    const { result } = renderHook(() => useDesktopWidgets("workspace", "thread"));
    await act(async () => {});
    act(() => emit("thread/replaced", { threadId: "thread", generationChanged: true }));
    expect(result.current.widgets).toEqual({});
    expect(result.current.scopeToken).toBeUndefined();
    await act(async () => refresh.reject(new Error("replacement unavailable")));
    expect(result.current.widgets).toEqual({});
    expect(result.current.scopeToken).toBeUndefined();
  });
});
