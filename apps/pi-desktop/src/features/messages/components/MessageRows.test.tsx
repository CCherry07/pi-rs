// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ConversationItem } from "../../../types";
import { MessageRow, ToolRow } from "./MessageRows";

const item: Extract<ConversationItem, { kind: "tool" }> = {
  id: "subagent-call-1",
  kind: "tool",
  toolType: "collabToolCall",
  title: "Collab: spawn",
  detail: "→ child-thread",
  status: "inProgress",
  output: "Review the parser\n\nreviewer: inProgress",
  collabTask: "Review the parser",
  collabReceiver: {
    threadId: "child-thread",
    role: "reviewer",
  },
  collabReceivers: [
    {
      threadId: "child-thread",
      role: "reviewer",
    },
  ],
  collabStatuses: [
    {
      threadId: "child-thread",
      role: "reviewer",
      status: "inProgress",
    },
  ],
};

describe("ToolRow sub-agent execution tree", () => {
  afterEach(() => {
    cleanup();
  });

  it("renders notices without assistant attribution or collapsed tool controls", () => {
    render(<ToolRow item={{ id: "notice", kind: "tool", toolType: "notice", title: "[warning]", detail: "Plugin warning", status: "completed" }} isExpanded={false} onToggle={vi.fn()} />);
    expect(screen.getByRole("status").textContent).toContain("[warning]");
    expect(screen.getByText("Plugin warning")).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("shows live child status and opens the selected child thread", () => {
    const onOpenThreadLink = vi.fn();
    render(
      <ToolRow
        item={item}
        isExpanded={false}
        onToggle={vi.fn()}
        onOpenThreadLink={onOpenThreadLink}
      />,
    );

    expect(screen.getByLabelText("Sub-agent execution tree")).toBeTruthy();
    expect(screen.getByText("reviewer")).toBeTruthy();
    expect(screen.getByText("Review the parser")).toBeTruthy();
    expect(screen.getByText("processing")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Open reviewer progress" }));
    expect(onOpenThreadLink).toHaveBeenCalledWith("child-thread");
  });
});

describe("MessageRow actions", () => {
  afterEach(() => {
    cleanup();
  });

  it("forks from the selected persisted message", () => {
    const message: Extract<ConversationItem, { kind: "message" }> = {
      id: "message-1",
      entryId: "entry-1",
      kind: "message",
      role: "assistant",
      text: "A response",
    };
    const onFork = vi.fn();

    render(
      <MessageRow
        item={message}
        isCopied={false}
        onCopy={vi.fn()}
        onFork={onFork}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Fork from this message" }));
    expect(onFork).toHaveBeenCalledWith(message);
  });

  it("does not offer fork for a message without a persisted entry", () => {
    render(
      <MessageRow
        item={{
          id: "pending-message",
          kind: "message",
          role: "assistant",
          text: "Still streaming",
        }}
        isCopied={false}
        onCopy={vi.fn()}
      />,
    );

    expect(screen.queryByRole("button", { name: "Fork from this message" })).toBeNull();
  });
});
