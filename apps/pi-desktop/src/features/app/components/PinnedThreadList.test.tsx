// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ThreadSummary } from "../../../types";
import { PinnedThreadList } from "./PinnedThreadList";

const thread: ThreadSummary = {
  id: "thread-1",
  name: "Pinned Alpha",
  updatedAt: 1000,
};

const otherThread: ThreadSummary = {
  id: "thread-2",
  name: "Pinned Beta",
  updatedAt: 800,
};

const nestedThread: ThreadSummary = {
  id: "thread-3",
  name: "Pinned Nested",
  updatedAt: 700,
};

const statusMap = {
  "thread-1": { isProcessing: false, hasUnread: false },
  "thread-2": { isProcessing: true, hasUnread: false },
};

const baseProps = {
  rows: [{ thread, depth: 0, workspaceId: "ws-1" }],
  activeWorkspaceId: "ws-1",
  activeThreadId: "thread-1",
  threadStatusById: statusMap,
  getThreadTime: () => "1h",
  isThreadPinned: () => true,
  onSelectThread: vi.fn(),
  onShowThreadMenu: vi.fn(),
};

describe("PinnedThreadList", () => {
  afterEach(() => {
    cleanup();
  });

  it("renders pinned rows and handles click/context menu", () => {
    const onSelectThread = vi.fn();
    const onShowThreadMenu = vi.fn();

    render(
      <PinnedThreadList
        {...baseProps}
        onSelectThread={onSelectThread}
        onShowThreadMenu={onShowThreadMenu}
      />,
    );

    const row = screen.getByText("Pinned Alpha").closest(".thread-row");
    expect(row).toBeTruthy();
    if (!row) {
      throw new Error("Missing pinned row");
    }
    expect(row.classList.contains("active")).toBe(true);
    expect(row.querySelector(".thread-status")?.className).toContain("ready");
    expect(screen.queryByText("Pinned")).toBeNull();

    fireEvent.click(row);
    expect(onSelectThread).toHaveBeenCalledWith("ws-1", "thread-1");

    fireEvent.contextMenu(row);
    expect(onShowThreadMenu).toHaveBeenCalledWith(
      expect.anything(),
      "ws-1",
      "thread-1",
      true,
    );
  });

  it("routes callbacks for rows across workspaces", () => {
    const onSelectThread = vi.fn();
    const onShowThreadMenu = vi.fn();

    render(
      <PinnedThreadList
        {...baseProps}
        rows={[
          { thread, depth: 0, workspaceId: "ws-1" },
          { thread: otherThread, depth: 0, workspaceId: "ws-2" },
        ]}
        onSelectThread={onSelectThread}
        onShowThreadMenu={onShowThreadMenu}
      />,
    );

    const secondRow = screen.getByText("Pinned Beta").closest(".thread-row");
    expect(secondRow).toBeTruthy();
    if (!secondRow) {
      throw new Error("Missing second pinned row");
    }

    fireEvent.click(secondRow);
    expect(onSelectThread).toHaveBeenCalledWith("ws-2", "thread-2");

    fireEvent.contextMenu(secondRow);
    expect(onShowThreadMenu).toHaveBeenCalledWith(
      expect.anything(),
      "ws-2",
      "thread-2",
      true,
    );
  });

  it("shows blue unread-style status when a pinned thread is waiting for user input", () => {
    const { container } = render(
      <PinnedThreadList
        {...baseProps}
        rows={[{ thread: otherThread, depth: 0, workspaceId: "ws-2" }]}
        threadStatusById={{
          "thread-1": { isProcessing: false, hasUnread: false },
          "thread-2": { isProcessing: true, hasUnread: false },
        }}
        pendingUserInputKeys={new Set(["ws-2:thread-2"])}
      />,
    );

    const row = container.querySelector(".thread-row");
    expect(row).toBeTruthy();
    expect(row?.querySelector(".thread-name")?.textContent).toBe("Pinned Beta");
    expect(row?.querySelector(".thread-status")?.className).toContain("unread");
    expect(row?.querySelector(".thread-status")?.className).not.toContain("processing");
  });

  it("toggles descendant visibility for pinned rows with sub-agents", () => {
    render(
      <PinnedThreadList
        {...baseProps}
        rows={[
          { thread, depth: 0, workspaceId: "ws-1" },
          { thread: nestedThread, depth: 1, workspaceId: "ws-1" },
        ]}
      />,
    );

    expect(screen.getByText("Pinned Nested")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Hide sub-agents" }));
    expect(screen.queryByText("Pinned Nested")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Show sub-agents" }));
    expect(screen.getByText("Pinned Nested")).toBeTruthy();
  });
});
