// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { useQueuedSend } from "./useQueuedSend";

const makeOptions = (
  overrides: Partial<Parameters<typeof useQueuedSend>[0]> = {},
): Parameters<typeof useQueuedSend>[0] => ({
  activeThreadId: "thread-1",
  activeTurnId: "turn-1",
  isProcessing: false,
  steerEnabled: false,
  followUpMessageBehavior: "queue" as const,
  sendUserMessage: vi.fn().mockResolvedValue({ status: "sent" }),
  startCompact: vi.fn().mockResolvedValue(undefined),
  startReload: vi.fn().mockResolvedValue(undefined),
  clearActiveImages: vi.fn(),
  ...overrides,
});

describe("useQueuedSend", () => {
  it.each(["message", "command"])("consumes %s attachments before waiting and preserves next draft images", async (kind) => {
    let resolve!: () => void;
    const pending = new Promise<void>((done) => { resolve = done; });
    let draft = ["submitted.png"];
    const options = makeOptions({
      sendUserMessage: vi.fn(async () => { await pending; return { status: "completed" as const }; }),
      startCompact: vi.fn(() => pending),
      clearActiveImages: vi.fn(() => { draft = []; }),
    });
    const { result } = renderHook(() => useQueuedSend(options));
    let sending!: Promise<void>;
    act(() => { sending = result.current.handleSend(kind === "command" ? "/compact" : "first", draft); });
    expect(draft).toEqual([]);
    draft = ["next-draft.png"];
    await act(async () => { resolve(); await sending; });
    expect(draft).toEqual(["next-draft.png"]);
    expect(options.clearActiveImages).toHaveBeenCalledTimes(1);
    if (kind === "message") {
      expect(options.sendUserMessage).toHaveBeenCalledWith("first", ["submitted.png"], { sendIntent: "default" });
    }
  });

  it("an old queued receipt cannot release a newer in-flight queue item", async () => {
    let resolve!: (value: { status: "completed" }) => void;
    const send = vi.fn().mockImplementationOnce(() => new Promise((done) => { resolve = done; }))
      .mockImplementation(() => new Promise(() => {}));
    const options = makeOptions({ sendUserMessage: send });
    const { result, rerender } = renderHook((props) => useQueuedSend(props), { initialProps: options });
    await act(async () => {
      await result.current.queueMessage("A");
      await result.current.queueMessage("B");
      await result.current.queueMessage("C");
    });
    act(() => rerender({ ...options, isProcessing: true }));
    act(() => rerender({ ...options, isProcessing: false }));
    expect(send).toHaveBeenCalledTimes(2);
    await act(async () => { resolve({ status: "completed" }); });
    expect(send).toHaveBeenCalledTimes(2);
    expect(result.current.activeQueue.map((item) => item.text)).toEqual(["C"]);
  });
  it("does not intercept native commands whose names begin with a Desktop action", async () => {
    const options = makeOptions();
    const { result } = renderHook(() => useQueuedSend(options));
    await act(async () => { await result.current.handleSend("/compact:detail arg"); });
    expect(options.startCompact).not.toHaveBeenCalled();
    expect(options.sendUserMessage).toHaveBeenCalledExactlyOnceWith("/compact:detail arg", [], { sendIntent: "default" });
    await act(async () => { await result.current.handleSend("/COMPACT"); });
    expect(options.startCompact).toHaveBeenCalledExactlyOnceWith("/COMPACT");
  });

  it("drains handled commands and completed submissions without waiting for AgentStart", async () => {
    const options = makeOptions({ sendUserMessage: vi.fn().mockResolvedValue({ status: "handled" }) });
    const { result } = renderHook(() => useQueuedSend(options));
    await act(async () => {
      await result.current.queueMessage("/native first");
      await result.current.queueMessage("/native second");
      await result.current.queueMessage("/compact");
    });
    expect(options.sendUserMessage).toHaveBeenCalledTimes(2);
    expect(options.startCompact).toHaveBeenCalledTimes(1);
    expect(result.current.activeQueue).toEqual([]);
  });

  it("sends queued messages one at a time after processing completes", async () => {
    const options = makeOptions();
    const { result, rerender } = renderHook(
      (props) => useQueuedSend(props),
      { initialProps: options },
    );

    await act(async () => {
      await result.current.queueMessage("First");
      await result.current.queueMessage("Second");
    });

    await act(async () => {
      await Promise.resolve();
    });

    expect(options.sendUserMessage).toHaveBeenCalledTimes(1);
    expect(options.sendUserMessage).toHaveBeenCalledWith("First", []);

    await act(async () => {
      rerender({ ...options, isProcessing: true });
    });
    await act(async () => {
      await Promise.resolve();
    });
    await act(async () => {
      rerender({ ...options, isProcessing: false });
    });
    await act(async () => {
      await Promise.resolve();
    });

    expect(options.sendUserMessage).toHaveBeenCalledTimes(2);
    expect(options.sendUserMessage).toHaveBeenLastCalledWith("Second", []);
  });

  it("waits for processing to start before sending the next queued message", async () => {
    const options = makeOptions();
    const { result } = renderHook((props) => useQueuedSend(props), {
      initialProps: options,
    });

    await act(async () => {
      await result.current.queueMessage("Alpha");
      await result.current.queueMessage("Beta");
    });

    await act(async () => {
      await Promise.resolve();
    });

    expect(options.sendUserMessage).toHaveBeenCalledTimes(1);
    expect(options.sendUserMessage).toHaveBeenCalledWith("Alpha", []);
  });

  it("steers a selected queued message and removes it from the queue", async () => {
    const options = makeOptions({ isProcessing: true, steerEnabled: true });
    const { result } = renderHook((props) => useQueuedSend(props), {
      initialProps: options,
    });

    await act(async () => {
      await result.current.queueMessage("Guide now", ["image.png"]);
    });
    const queued = result.current.activeQueue[0];
    expect(queued).toBeTruthy();

    await act(async () => {
      await result.current.steerQueuedMessage("thread-1", queued!);
    });

    expect(options.sendUserMessage).toHaveBeenCalledWith(
      "Guide now",
      ["image.png"],
      { sendIntent: "steer" },
    );
    expect(result.current.activeQueue).toHaveLength(0);
  });

  it("queues send while processing when steer is disabled", async () => {
    const options = makeOptions({ isProcessing: true, steerEnabled: false });
    const { result } = renderHook((props) => useQueuedSend(props), {
      initialProps: options,
    });

    await act(async () => {
      await result.current.handleSend("Queued");
    });

    expect(options.sendUserMessage).not.toHaveBeenCalled();
    expect(result.current.activeQueue).toHaveLength(1);
    expect(result.current.activeQueue[0]?.text).toBe("Queued");
  });

  it("sends immediately while processing when steer is enabled", async () => {
    const options = makeOptions({
      isProcessing: true,
      steerEnabled: true,
      followUpMessageBehavior: "steer",
    });
    const { result } = renderHook((props) => useQueuedSend(props), {
      initialProps: options,
    });

    await act(async () => {
      await result.current.handleSend("Steer");
    });

    expect(options.sendUserMessage).toHaveBeenCalledTimes(1);
    expect(options.sendUserMessage).toHaveBeenCalledWith(
      "Steer",
      [],
      { sendIntent: "steer" },
    );
    expect(result.current.activeQueue).toHaveLength(0);
  });

  it("queues send while processing when steer is enabled but turn id is unavailable", async () => {
    const options = makeOptions({
      isProcessing: true,
      steerEnabled: true,
      followUpMessageBehavior: "steer",
      activeTurnId: null,
    });
    const { result } = renderHook((props) => useQueuedSend(props), {
      initialProps: options,
    });

    await act(async () => {
      await result.current.handleSend("Wait for turn");
    });

    expect(options.sendUserMessage).not.toHaveBeenCalled();
    expect(result.current.activeQueue).toHaveLength(1);
    expect(result.current.activeQueue[0]?.text).toBe("Wait for turn");
  });

  it("queues the message when a forced steer attempt fails", async () => {
    const sendUserMessage = vi
      .fn()
      .mockResolvedValueOnce({ status: "steer_failed" })
      .mockResolvedValueOnce({ status: "sent" });
    const options = makeOptions({
      isProcessing: true,
      steerEnabled: true,
      followUpMessageBehavior: "steer",
      sendUserMessage,
    });
    const { result, rerender } = renderHook((props) => useQueuedSend(props), {
      initialProps: options,
    });

    await act(async () => {
      await result.current.handleSend("Fallback to queue");
    });

    expect(options.sendUserMessage).toHaveBeenCalledWith(
      "Fallback to queue",
      [],
      { sendIntent: "steer" },
    );
    expect(result.current.activeQueue).toHaveLength(1);
    expect(result.current.activeQueue[0]?.text).toBe("Fallback to queue");

    await act(async () => {
      await Promise.resolve();
    });
    expect(options.sendUserMessage).toHaveBeenCalledTimes(1);

    await act(async () => {
      rerender({ ...options, isProcessing: false, sendUserMessage });
    });
    await act(async () => {
      await Promise.resolve();
    });

    expect(options.sendUserMessage).toHaveBeenCalledTimes(2);
    expect(options.sendUserMessage).toHaveBeenLastCalledWith(
      "Fallback to queue",
      [],
    );
  });

  it("retries queued send after failure", async () => {
    const options = makeOptions({
      sendUserMessage: vi
        .fn()
        .mockRejectedValueOnce(new Error("boom"))
        .mockResolvedValueOnce({ status: "sent" }),
    });
    const { result } = renderHook((props) => useQueuedSend(props), {
      initialProps: options,
    });

    await act(async () => {
      await result.current.queueMessage("Retry");
    });

    await act(async () => {
      await Promise.resolve();
    });
    await act(async () => {
      await Promise.resolve();
    });

    expect(options.sendUserMessage).toHaveBeenCalledTimes(2);
    expect(options.sendUserMessage).toHaveBeenLastCalledWith("Retry", []);
  });

  it("queues messages per thread and only flushes the active thread", async () => {
    const options = makeOptions({ isProcessing: true });
    const { result, rerender } = renderHook(
      (props) => useQueuedSend(props),
      { initialProps: options },
    );

    await act(async () => {
      await result.current.queueMessage("Thread-1");
    });

    await act(async () => {
      rerender({ ...options, activeThreadId: "thread-2", isProcessing: false });
    });
    await act(async () => {
      await Promise.resolve();
    });

    expect(options.sendUserMessage).not.toHaveBeenCalled();

    await act(async () => {
      rerender({ ...options, activeThreadId: "thread-1", isProcessing: false });
    });
    await act(async () => {
      await Promise.resolve();
    });

    expect(options.sendUserMessage).toHaveBeenCalledTimes(1);
    expect(options.sendUserMessage).toHaveBeenCalledWith("Thread-1", []);
  });

  it("routes /compact to the compact handler", async () => {
    const startCompact = vi.fn().mockResolvedValue(undefined);
    const options = makeOptions({ startCompact });
    const { result } = renderHook((props) => useQueuedSend(props), {
      initialProps: options,
    });

    await act(async () => {
      await result.current.handleSend("/compact now", ["img-1"]);
    });

    expect(startCompact).toHaveBeenCalledWith("/compact now");
    expect(options.sendUserMessage).not.toHaveBeenCalled();
  });

  it("routes /reload to the reload handler", async () => {
    const startReload = vi.fn().mockResolvedValue(undefined);
    const options = makeOptions({ startReload });
    const { result } = renderHook((props) => useQueuedSend(props), {
      initialProps: options,
    });

    await act(async () => {
      await result.current.handleSend("/reload", ["img-1"]);
    });

    expect(startReload).toHaveBeenCalledWith("/reload");
    expect(options.sendUserMessage).not.toHaveBeenCalled();
  });

  it("preserves images for queued messages", async () => {
    const options = makeOptions();
    const { result } = renderHook((props) => useQueuedSend(props), {
      initialProps: options,
    });

    await act(async () => {
      await result.current.queueMessage("Images", ["img-1", "img-2"]);
    });

    await act(async () => {
      await Promise.resolve();
    });

    expect(options.sendUserMessage).toHaveBeenCalledTimes(1);
    expect(options.sendUserMessage).toHaveBeenCalledWith("Images", [
      "img-1",
      "img-2",
    ]);
  });

  it("does not flush queued messages while response is required", async () => {
    const options = makeOptions({ queueFlushPaused: true });
    const { result, rerender } = renderHook((props) => useQueuedSend(props), {
      initialProps: options,
    });

    await act(async () => {
      await result.current.queueMessage("Held");
    });

    await act(async () => {
      await Promise.resolve();
    });

    expect(options.sendUserMessage).not.toHaveBeenCalled();

    await act(async () => {
      rerender({ ...options, queueFlushPaused: false });
    });

    await act(async () => {
      await Promise.resolve();
    });

    expect(options.sendUserMessage).toHaveBeenCalledTimes(1);
    expect(options.sendUserMessage).toHaveBeenCalledWith("Held", []);
  });

});
