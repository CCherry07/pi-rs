// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ConversationItem } from "../../../types";
import { ToolRow } from "./MessageRows";

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
