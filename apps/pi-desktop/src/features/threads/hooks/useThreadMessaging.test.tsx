/** @vitest-environment jsdom */
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { sendUserMessage as sendService, steerTurn } from "@services/tauri";
import { useThreadMessaging } from "./useThreadMessaging";
import { useThreadStatus } from "./useThreadStatus";

vi.mock("@services/tauri", () => ({ sendUserMessage: vi.fn(), steerTurn: vi.fn(), compactThread: vi.fn(), interruptTurn: vi.fn() }));
vi.mock("@sentry/react", () => ({ metrics: { count: vi.fn() } }));

function options(): Parameters<typeof useThreadMessaging>[0] {
  return {
    activeWorkspace: { id: "ws", name: "test", path: "/tmp", settings: { sidebarCollapsed: false } },
    activeThreadId: "thread", steerEnabled: false, customPrompts: [],
    threadStatusById: {}, activeTurnIdByThread: {}, pendingInterruptsRef: { current: new Set() },
    dispatch: vi.fn(), getCustomName: vi.fn(), markProcessing: vi.fn(), setActiveTurnId: vi.fn(), getStatusRevision: () => 0,
    recordThreadActivity: vi.fn(), safeMessageActivity: vi.fn(), pushThreadErrorMessage: vi.fn(),
    ensureThreadForActiveWorkspace: vi.fn().mockResolvedValue("thread"),
  };
}
beforeEach(() => vi.clearAllMocks());
describe("native submit receipts", () => {
  it.each(["handled", "completed", "queued"])("accepts %s without fabricating a turn", async (status) => {
    vi.mocked(sendService).mockResolvedValue({ status, isRunning: false });
    const opts = options();
    const { result } = renderHook(() => useThreadMessaging(opts));
    await act(async () => expect(await result.current.sendUserMessage("/native arg")).toEqual({ status }));
    expect(sendService).toHaveBeenCalledTimes(1);
    expect(opts.markProcessing).toHaveBeenLastCalledWith("thread", false);
    expect(opts.setActiveTurnId).toHaveBeenLastCalledWith("thread", null);
    expect(opts.pushThreadErrorMessage).not.toHaveBeenCalled();
  });
  it.each(["receipt", "error", "rpc error"])("does not let an old %s clear a newer submission or lifecycle", async (completion) => {
    let resolve!: (value: Record<string, unknown>) => void;
    let reject!: (reason: Error) => void;
    vi.mocked(sendService).mockImplementationOnce(() => new Promise((yes, no) => { resolve = yes; reject = no; }));
    vi.mocked(sendService).mockImplementationOnce(() => new Promise(() => {}));
    const opts = options();
    const { result } = renderHook(() => {
      const status = useThreadStatus({ dispatch: opts.dispatch });
      return { status, messaging: useThreadMessaging({ ...opts, ...status }) };
    });
    let pending!: Promise<unknown>;
    await act(async () => { pending = result.current.messaging.sendUserMessage("A"); });
    act(() => {
      result.current.status.markProcessing("thread", false); // A Settled
    });
    await act(async () => { void result.current.messaging.sendUserMessage("B"); });
    act(() => {
      result.current.status.markProcessing("thread", true); // B Started
      result.current.status.setActiveTurnId("thread", "same-pi-turn-id");
    });
    vi.mocked(opts.dispatch).mockClear();
    await act(async () => {
      if (completion === "error") reject(new Error("late failure"));
      else resolve(completion === "rpc error" ? { error: { message: "late failure" } } : { status: "completed", isRunning: false });
      await pending;
    });
    expect(opts.dispatch).not.toHaveBeenCalled();
  });
  it("does not clear a run started by a command without a new UI submission", async () => {
    let resolve!: (value: Record<string, unknown>) => void;
    vi.mocked(sendService).mockImplementationOnce(() => new Promise((yes) => { resolve = yes; }));
    const opts = options();
    const { result } = renderHook(() => {
      const status = useThreadStatus({ dispatch: opts.dispatch });
      return { status, messaging: useThreadMessaging({ ...opts, ...status }) };
    });
    let pending!: Promise<unknown>;
    await act(async () => { pending = result.current.messaging.sendUserMessage("/native"); });
    act(() => result.current.status.markProcessing("thread", true));
    vi.mocked(opts.dispatch).mockClear();
    await act(async () => { resolve({ status: "handled", isRunning: false }); await pending; });
    expect(opts.dispatch).not.toHaveBeenCalled();
  });
  it("shows a command error and releases optimistic processing", async () => {
    vi.mocked(sendService).mockRejectedValue(new Error("reload failed"));
    const opts = options();
    const { result } = renderHook(() => useThreadMessaging(opts));
    await act(async () => expect(await result.current.sendUserMessage("/native reload")).toEqual({ status: "blocked" }));
    expect(opts.markProcessing).toHaveBeenLastCalledWith("thread", false);
    expect(opts.pushThreadErrorMessage).toHaveBeenCalledWith("thread", "reload failed");
  });
  it("a handled steering command does not stop the active run", async () => {
    vi.mocked(steerTurn).mockResolvedValue({ status: "handled", isRunning: true });
    const opts = options();
    opts.steerEnabled = true;
    opts.threadStatusById = { thread: { isProcessing: true, hasUnread: false, lastDurationMs: null, processingStartedAt: null } };
    opts.activeTurnIdByThread = { thread: "turn" };
    const { result } = renderHook(() => useThreadMessaging(opts));
    await act(async () => expect(await result.current.sendUserMessage("/native notice", [], { sendIntent: "steer" })).toEqual({ status: "handled" }));
    expect(opts.markProcessing).not.toHaveBeenCalledWith("thread", false);
    expect(sendService).not.toHaveBeenCalled();
  });
});
