// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
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

  it("keeps subagent details in the hover panel and supports keyboard access", async () => {
    render(
      <ThreadList
        {...baseProps}
        unpinnedRows={[{ thread: nestedThread, depth: 1 }]}
        activeThreadId="thread-2"
      />,
    );

    const row = screen.getByText("Nested Agent").closest(".thread-row");
    if (!row) {
      throw new Error("Missing nested thread row");
    }
    expect(screen.queryByText("Robie · Explorer")).toBeNull();
    expect(row.querySelector(".thread-details")).toBeNull();

    fireEvent.keyDown(row, { key: "ArrowRight" });
    const panel = await screen.findByRole("dialog");
    expect(within(panel).getByText("Robie · Explorer")).toBeTruthy();
    expect(row.getAttribute("aria-expanded")).toBe("true");

    fireEvent.keyDown(panel, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("shows model, context and pinned information on hover without expanding the row", async () => {
    render(
      <ThreadList
        {...baseProps}
        unpinnedRows={[{
          thread: { ...thread, modelId: "test-model", effort: "high" },
          depth: 0,
        }]}
        getThreadArgsBadge={() => "Custom context"}
        isThreadPinned={() => true}
      />,
    );

    const row = screen.getByText("Alpha").closest(".thread-row");
    if (!row) {
      throw new Error("Missing thread row");
    }
    expect(screen.queryByText("test-model · high")).toBeNull();
    expect(screen.queryByText("Custom context")).toBeNull();
    expect(screen.queryByText("Pinned")).toBeNull();

    fireEvent.mouseEnter(row);
    const panel = await screen.findByRole("dialog");
    expect(within(panel).getByText("test-model · high")).toBeTruthy();
    expect(within(panel).getByText("Custom context")).toBeTruthy();
    expect(within(panel).getByText("Pinned")).toBeTruthy();
    expect(row.querySelector(".thread-details")).toBeNull();
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

  it("toggles sub-agent descendants without selecting the parent row", () => {
    const onSelectThread = vi.fn();
    const { getByText, queryByText, getByRole } = render(
      <ThreadList
        {...baseProps}
        onSelectThread={onSelectThread}
        unpinnedRows={[
          { thread, depth: 0 },
          { thread: nestedThread, depth: 1 },
        ]}
      />,
    );

    expect(getByText("Nested Agent")).toBeTruthy();
    const hideButton = getByRole("button", { name: "Hide sub-agents" });
    fireEvent.keyDown(hideButton, { key: "Enter" });
    fireEvent.click(hideButton);
    expect(queryByText("Nested Agent")).toBeNull();

    const showButton = getByRole("button", { name: "Show sub-agents" });
    fireEvent.keyDown(showButton, { key: " " });
    fireEvent.click(showButton);
    expect(getByText("Nested Agent")).toBeTruthy();
    expect(onSelectThread).not.toHaveBeenCalled();
  });

  it("does not show sub-agent toggle for rows without descendants", () => {
    const { queryByRole } = render(<ThreadList {...baseProps} />);

    expect(queryByRole("button", { name: "Hide sub-agents" })).toBeNull();
    expect(queryByRole("button", { name: "Show sub-agents" })).toBeNull();
  });
});
