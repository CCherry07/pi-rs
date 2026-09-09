/** @vitest-environment jsdom */
import { act, renderHook } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { subscribePiEvents } from "../../../services/events";
import { usePiEvents } from "./usePiEvents";
vi.mock("../../../services/events", () => ({ subscribePiEvents: vi.fn(() => vi.fn()) }));
it("routes plugin notices and replacements without pretending they are agent turns", () => {
  const handlers = { onThreadNotice: vi.fn(), onThreadReplaced: vi.fn(), onTurnStarted: vi.fn(), onTurnError: vi.fn() };
  renderHook(() => usePiEvents(handlers));
  const listener = vi.mocked(subscribePiEvents).mock.calls.slice(-1)[0][0];
  act(() => {
    listener({ workspace_id: "ws", message: { method: "thread/notice", params: { threadId: "old", message: "ready", level: "warning" } } });
    listener({ workspace_id: "ws", message: { method: "thread/replaced", params: { previousThreadId: "old", thread: { id: "new" } } } });
  });
  expect(handlers.onThreadNotice).toHaveBeenCalledWith("ws", "old", "ready", "warning");
  expect(handlers.onThreadReplaced).toHaveBeenCalledWith("ws", "old", { id: "new" });
  expect(handlers.onTurnStarted).not.toHaveBeenCalled();
  expect(handlers.onTurnError).not.toHaveBeenCalled();
});
