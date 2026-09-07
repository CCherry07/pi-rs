// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ThreadSummary } from "../../../types";
import { ThreadList } from "./ThreadList";

const nestedThread: ThreadSummary = {
  id: "thread-2",
  name: "Nested Agent",
  updatedAt: 900,
  isSubagent: true,
  subagentNickname: "Robie",
  subagentRole: "explorer",
};

const thread: ThreadSummary = {
  id: "thread-1",
  name: "Alpha",
  updatedAt: 1000,
};

const statusMap = {
  "thread-1": { isProcessing: false, hasUnread: true, isReviewing: false },
  "thread-2": { isProcessing: false, hasUnread: false, isReviewing: false },
};

const baseProps = {
  workspaceId: "ws-1",
  pinnedRows: [],
  unpinnedRows: [{ thread, depth: 0 }],
  totalThreadRoots: 1,
  isExpanded: false,
  nextCursor: null,
  isPaging: false,
  nested: false,
  activeWorkspaceId: "ws-1",
  activeThreadId: "thread-1",
  threadStatusById: statusMap,
  getThreadTime: () => "2m",
  isThreadPinned: () => false,
  onToggleExpanded: vi.fn(),
  onLoadOlderThreads: vi.fn(),
  onSelectThread: vi.fn(),
  onShowThreadMenu: vi.fn(),
};

describe("ThreadList", () => {
  afterEach(() => {
    cleanup();
  });

  it("renders active row and handles click/context menu", () => {
    const onSelectThread = vi.fn();
    const onShowThreadMenu = vi.fn();

    render(
      <ThreadList
        {...baseProps}
        onSelectThread={onSelectThread}
        onShowThreadMenu={onShowThreadMenu}
      />,
    );

    const row = screen.getByText("Alpha").closest(".thread-row");
    expect(row).toBeTruthy();
    if (!row) {
      throw new Error("Missing thread row");
    }
    expect(row.classList.contains("active")).toBe(true);
    expect(row.querySelector(".thread-status")?.className).toContain("unread");

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

  it("shows the more button and toggles expanded", () => {
    const onToggleExpanded = vi.fn();
    render(
      <ThreadList
        {...baseProps}
        totalThreadRoots={4}
        onToggleExpanded={onToggleExpanded}
      />,
    );

    const moreButton = screen.getByRole("button", { name: "More..." });
    fireEvent.click(moreButton);
    expect(onToggleExpanded).toHaveBeenCalledWith("ws-1");
  });

  it("loads older threads when a cursor is available", () => {
    const onLoadOlderThreads = vi.fn();
    render(
      <ThreadList
        {...baseProps}
        nextCursor="cursor"
        onLoadOlderThreads={onLoadOlderThreads}
      />,
    );

    const loadButton = screen.getByRole("button", { name: "Load older..." });
    fireEvent.click(loadButton);
    expect(onLoadOlderThreads).toHaveBeenCalledWith("ws-1");
  });

  it("renders nested rows with indentation and disables pinning", () => {
    const onShowThreadMenu = vi.fn();
    render(
      <ThreadList
        {...baseProps}
        nested
        unpinnedRows={[
          { thread, depth: 0 },
          { thread: nestedThread, depth: 1 },
        ]}
        onShowThreadMenu={onShowThreadMenu}
      />,
    );

    const nestedRow = screen.getByText("Nested Agent").closest(".thread-row");
    expect(nestedRow).toBeTruthy();
    if (!nestedRow) {
      throw new Error("Missing nested thread row");
    }
    expect(nestedRow.getAttribute("style")).toContain("--thread-indent");

    fireEvent.contextMenu(nestedRow);
    expect(onShowThreadMenu).toHaveBeenCalledWith(
      expect.anything(),
      "ws-1",
      "thread-2",
      false,
    );
  });

  it("shows the subagent nickname pill with role styling", () => {
    const { container } = render(
      <ThreadList
        {...baseProps}
        unpinnedRows={[{ thread: nestedThread, depth: 1 }]}
        activeThreadId="thread-2"
      />,
    );

    const pill = screen.getByText("Robie");
    const role = screen.getByText("Explorer");
    expect(pill.className).toContain("thread-subagent-pill");
    expect(role.className).toContain("thread-subagent-role");
    expect((pill as HTMLElement).style.getPropertyValue("--thread-subagent-pill-hue")).toBeTruthy();
    expect(container.querySelector(".thread-workspace-label")).toBeNull();
  });

  it("shows blue unread-style status when a thread is waiting for user input", () => {
    const { container } = render(
      <ThreadList
        {...baseProps}
        threadStatusById={{
          "thread-1": { isProcessing: true, hasUnread: false },
          "thread-2": { isProcessing: false, hasUnread: false },
        }}
        pendingUserInputKeys={new Set(["ws-1:thread-1"])}
      />,
    );

    const row = container.querySelector(".thread-row");
    expect(row).toBeTruthy();
    expect(row?.querySelector(".thread-name")?.textContent).toBe("Alpha");
    expect(row?.querySelector(".thread-status")?.className).toContain("unread");
    expect(row?.querySelector(".thread-status")?.className).not.toContain("processing");
  });

  it("toggles sub-agent descendants for parent rows", () => {
    const { getByText, queryByText, getByRole } = render(
      <ThreadList
        {...baseProps}
        unpinnedRows={[
          { thread, depth: 0 },
          { thread: nestedThread, depth: 1 },
        ]}
      />,
    );

    expect(getByText("Nested Agent")).toBeTruthy();
    const hideButton = getByRole("button", { name: "Hide sub-agents" });
    fireEvent.click(hideButton);
    expect(queryByText("Nested Agent")).toBeNull();

    const showButton = getByRole("button", { name: "Show sub-agents" });
    fireEvent.click(showButton);
    expect(getByText("Nested Agent")).toBeTruthy();
  });

  it("does not show sub-agent toggle for rows without descendants", () => {
    const { queryByRole } = render(<ThreadList {...baseProps} />);

    expect(queryByRole("button", { name: "Hide sub-agents" })).toBeNull();
    expect(queryByRole("button", { name: "Show sub-agents" })).toBeNull();
  });
});
