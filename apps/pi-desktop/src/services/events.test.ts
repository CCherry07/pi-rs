import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Event, EventCallback, UnlistenFn } from "@tauri-apps/api/event";
import { listen } from "@tauri-apps/api/event";
import type { PiEvent } from "../types";
import {
  subscribePiEvents,
  subscribeMenuCycleModel,
  subscribeMenuNewAgent,
  subscribeTerminalOutput,
} from "./events";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

describe("events subscriptions", () => {
  beforeEach(() => {
    vi.resetAllMocks();
  });

  it("delivers payloads and unsubscribes on cleanup", async () => {
    let listener: EventCallback<PiEvent> = () => {};
    const unlisten = vi.fn();

    vi.mocked(listen).mockImplementation((_event, handler) => {
      listener = handler as EventCallback<PiEvent>;
      return Promise.resolve(unlisten);
    });

    const onEvent = vi.fn();
    const cleanup = subscribePiEvents(onEvent);
    const payload: PiEvent = {
      workspace_id: "ws-1",
      message: { method: "ping" },
    };

    const event: Event<PiEvent> = {
      event: "pi-event",
      id: 1,
      payload,
    };
    listener(event);
    expect(onEvent).toHaveBeenCalledWith(payload);

    cleanup();
    await Promise.resolve();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it("cleans up listeners that resolve after unsubscribe", async () => {
    let resolveListener: (handler: UnlistenFn) => void = () => {};
    const unlisten = vi.fn();

    vi.mocked(listen).mockImplementation(
      () =>
        new Promise<UnlistenFn>((resolve) => {
          resolveListener = resolve;
        }),
    );

    const cleanup = subscribeMenuNewAgent(() => {});
    cleanup();

    resolveListener(unlisten);
    await Promise.resolve();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it("delivers menu events to subscribers", async () => {
    let listener: EventCallback<void> = () => {};
    const unlisten = vi.fn();

    vi.mocked(listen).mockImplementation((_event, handler) => {
      listener = handler as EventCallback<void>;
      return Promise.resolve(unlisten);
    });

    const onEvent = vi.fn();
    const cleanup = subscribeMenuCycleModel(onEvent);

    const event: Event<void> = {
      event: "menu-composer-cycle-model",
      id: 1,
      payload: undefined,
    };
    listener(event);
    expect(onEvent).toHaveBeenCalledTimes(1);

    cleanup();
  });

  it("reports listen errors through options", async () => {
    const error = new Error("nope");
    vi.mocked(listen).mockRejectedValueOnce(error);

    const onError = vi.fn();
    const cleanup = subscribeTerminalOutput(() => {}, { onError });

    await Promise.resolve();
    await Promise.resolve();
    expect(onError).toHaveBeenCalledWith(error);

    cleanup();
  });

  it("notifies every active subscriber only after the shared native listener is ready", async () => {
    let resolveListener!: (handler: UnlistenFn) => void;
    const unlisten = vi.fn();
    vi.mocked(listen).mockImplementationOnce(() => new Promise<UnlistenFn>((resolve) => { resolveListener = resolve; }));
    const removedReady = vi.fn();
    const firstReady = vi.fn();
    const secondReady = vi.fn();
    const removed = subscribePiEvents(() => {}, { onReady: removedReady });
    const first = subscribePiEvents(() => {}, { onReady: firstReady });
    const second = subscribePiEvents(() => {}, { onReady: secondReady });
    expect(listen).toHaveBeenCalledOnce();
    expect(firstReady).not.toHaveBeenCalled();
    expect(secondReady).not.toHaveBeenCalled();
    removed();
    resolveListener(unlisten);
    await Promise.resolve();
    expect(removedReady).not.toHaveBeenCalled();
    expect(firstReady).toHaveBeenCalledOnce();
    expect(secondReady).toHaveBeenCalledOnce();
    const alreadyReady = vi.fn();
    const third = subscribePiEvents(() => {}, { onReady: alreadyReady });
    expect(alreadyReady).toHaveBeenCalledOnce();
    expect(listen).toHaveBeenCalledOnce();
    first(); second(); third();
    expect(unlisten).toHaveBeenCalledOnce();
  });

  it("does not report readiness for a failed listener", async () => {
    vi.mocked(listen).mockRejectedValueOnce(new Error("listener unavailable"));
    const onReady = vi.fn();
    const cleanup = subscribePiEvents(() => {}, { onReady });
    await Promise.resolve();
    await Promise.resolve();
    expect(onReady).not.toHaveBeenCalled();
    cleanup();
  });
});
