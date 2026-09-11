// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ConversationItem } from "../../../types";
import { MessageRow, ToolRow } from "./MessageRows";

afterEach(cleanup);

describe("ToolRow notices", () => {
  it("renders notices without assistant attribution or collapsed tool controls", () => {
    render(<ToolRow item={{ id: "notice", kind: "tool", toolType: "notice", title: "[warning]", detail: "Plugin warning", status: "completed" }} isExpanded={false} onToggle={vi.fn()} />);
    expect(screen.getByRole("status").textContent).toContain("[warning]");
    expect(screen.getByText("Plugin warning")).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
  });
});

describe("MessageRow actions", () => {
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
