// @vitest-environment jsdom
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ConversationItem, ThreadContextInheritance } from "@/types";
import {
  buildConversationItem,
  buildConversationItemFromThreadItem,
  prepareThreadItems,
} from "@utils/threadItems";
import { Messages } from "./Messages";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../hooks/useFileLinkOpener", () => ({
  useFileLinkOpener: () => ({
    openFileLink: vi.fn(),
    showFileLinkMenu: vi.fn(),
  }),
}));

const spawn: Extract<ConversationItem, { kind: "tool" }> = {
  id: "spawn-call",
  kind: "tool",
  toolType: "mcpToolCall",
  toolName: "spawn_agent",
  title: "Tool: pi / spawn_agent",
  detail: '{"task":"Review the parser","agent":"reviewer"}',
  status: "completed",
  output: "Agent reviewer started in session child.",
  data: { details: { agentId: "agent-1", sessionId: "child" } },
};

const childItems: ConversationItem[] = [
  {
    id: "request",
    kind: "message",
    role: "user",
    text: "Check parser edge cases",
  },
  {
    id: "answer",
    entryId: "saved-answer",
    kind: "message",
    role: "assistant",
    text: "**Parser review complete**",
  },
];

function childChat(
  inheritance: ThreadContextInheritance,
  onForkMessage = vi.fn(),
) {
  return (
    <Messages
      items={childItems}
      contextInheritance={inheritance}
      threadId="child"
      workspaceId="workspace"
      isThinking={false}
      openTargets={[]}
      selectedOpenAppId=""
      onForkMessage={onForkMessage}
    />
  );
}

beforeEach(() => vi.clearAllMocks());
afterEach(cleanup);

describe("native transcript presentation", () => {
  it.each([
    ["live read", buildConversationItem, "mcpToolCall"],
    ["restored read", buildConversationItemFromThreadItem, "mcpToolCall"],
    [
      "restored command",
      buildConversationItemFromThreadItem,
      "commandExecution",
    ],
  ] as const)(
    "shows %s images outside collapsed tool groups",
    (_label, convert, type) => {
      const source =
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aX1sAAAAASUVORK5CYII=";
      const item = convert({
        id: "image-call",
        type,
        toolName: type === "commandExecution" ? "bash" : "read",
        server: "pi",
        tool: "read",
        command: "cat /tmp/image.png",
        arguments: { path: "/tmp/image.png" },
        status: "completed",
        result: "Read image file [image/png]",
        aggregatedOutput: "Read image file [image/png]",
        images: [source],
      });
      if (!item) throw new Error("Expected image tool item");
      const prepared = prepareThreadItems([
        spawn,
        { ...spawn, id: "second-tool" },
        item,
      ]);
      render(
        <Messages
          items={prepared}
          threadId="parent"
          workspaceId="workspace"
          isThinking={false}
          openTargets={[]}
          selectedOpenAppId=""
        />,
      );

      fireEvent.click(
        screen.getByRole("button", { name: "Collapse tool calls" }),
      );
      const image = screen.getByRole("img");
      expect(image.getAttribute("src")).toBe(source);
      expect(image.closest(".tool-group")).toBeNull();
      fireEvent.click(screen.getByRole("button", { name: "Open image 1" }));
      expect(
        within(screen.getByRole("dialog")).getByRole("img").getAttribute("src"),
      ).toBe(source);
      fireEvent.click(
        screen.getByRole("button", { name: "Close image preview" }),
      );
      expect(screen.queryByRole("dialog")).toBeNull();
    },
  );

  it.each(["Tool: pi / spawn_agent", "工具：pi / spawn_agent"])(
    "shows %s results without loading an extension or a child view",
    (title) => {
      render(
        <Messages
          items={[{ ...spawn, title }]}
          threadId="parent"
          workspaceId="workspace"
          isThinking={false}
          openTargets={[]}
          selectedOpenAppId=""
        />,
      );

      expect(screen.getByText("pi / spawn_agent")).toBeTruthy();
      fireEvent.click(
        screen.getByRole("button", { name: "Toggle tool details" }),
      );
      expect(screen.getByText(spawn.detail)).toBeTruthy();
      expect(screen.getByText(spawn.output!)).toBeTruthy();
      expect(
        screen.queryByRole("button", { name: "Expand child-agent chat" }),
      ).toBeNull();
      expect(invoke).not.toHaveBeenCalled();
    },
  );

  it("keeps persisted plugin messages readable through generic Markdown and images", () => {
    render(
      <Messages
        items={[
          {
            id: "plugin-note",
            kind: "tool",
            toolType: "customMessage",
            title: "review.notes",
            detail: "**Parser findings**",
            images: ["data:image/png;base64,aGVsbG8="],
            status: "completed",
          },
        ]}
        threadId="parent"
        workspaceId="workspace"
        isThinking={false}
        openTargets={[]}
        selectedOpenAppId=""
      />,
    );

    expect(screen.getByText("review.notes")).toBeTruthy();
    expect(screen.getByText("Parser findings").tagName).toBe("STRONG");
    expect(screen.getByRole("img").getAttribute("src")).toBe(
      "data:image/png;base64,aGVsbG8=",
    );
    expect(invoke).not.toHaveBeenCalled();
  });

  it("keeps a directly opened child's inherited snapshot separate and read-only", () => {
    const fork = vi.fn();
    render(
      childChat(
        {
          origin: {
            mode: "fork",
            parentThreadId: "parent",
            parentEntryId: "entry-before-spawn",
            snapshotEntryId: "seed-entry",
          },
          inheritedItems: [
            {
              id: "seed-user",
              kind: "message",
              role: "user",
              text: "Parent context at creation",
            },
            {
              id: "seed-answer",
              entryId: "parent-answer",
              kind: "message",
              role: "assistant",
              text: "Earlier answer",
            },
            spawn,
          ],
        },
        fork,
      ),
    );

    expect(
      screen.getByText("Inherited the parent context at creation"),
    ).toBeTruthy();
    expect(screen.getByText("entry-before-spawn")).toBeTruthy();
    expect(screen.getByText("Parser review complete")).toBeTruthy();
    expect(screen.queryByText("Parent context at creation")).toBeNull();
    fireEvent.click(
      screen.getByRole("button", { name: "View inherited context" }),
    );
    const snapshot = screen.getByRole("region", {
      name: "Inherited context snapshot",
    });
    expect(
      within(snapshot).getByText("Parent context at creation"),
    ).toBeTruthy();
    expect(within(snapshot).getByText("Earlier answer")).toBeTruthy();
    expect(within(snapshot).queryByText("Parser review complete")).toBeNull();
    expect(
      within(snapshot).queryByRole("button", {
        name: "Fork from this message",
      }),
    ).toBeNull();
    expect(
      screen.getAllByRole("button", { name: "Hide inherited context" }),
    ).toHaveLength(1);
    fireEvent.click(
      within(snapshot).getByRole("button", { name: "Toggle tool details" }),
    );
    expect(within(snapshot).getByText(spawn.output!)).toBeTruthy();
    expect(invoke).not.toHaveBeenCalled();
    expect(fork).not.toHaveBeenCalled();

    fireEvent.click(
      screen.getByRole("button", { name: "Hide inherited context" }),
    );
    expect(
      screen.queryByRole("region", { name: "Inherited context snapshot" }),
    ).toBeNull();
    expect(screen.getByText("Parser review complete")).toBeTruthy();
  });

  it("distinguishes an unavailable inherited snapshot from empty history", () => {
    render(
      childChat({
        origin: {
          mode: "fork",
          parentThreadId: "parent",
          parentEntryId: null,
          snapshotEntryId: "legacy-seed",
        },
        inheritedItems: null,
      }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "View inherited context" }),
    );
    expect(
      screen.getByText("The inherited snapshot is unavailable."),
    ).toBeTruthy();
    expect(invoke).not.toHaveBeenCalled();
  });

  it("shows fresh child provenance without offering an inherited snapshot", () => {
    render(
      childChat({
        origin: {
          mode: "fresh",
          parentThreadId: "parent",
          parentEntryId: null,
          snapshotEntryId: null,
        },
        inheritedItems: null,
      }),
    );
    expect(
      screen.getByText("Task-only context; no parent history inherited"),
    ).toBeTruthy();
    expect(
      screen.queryByRole("button", { name: "View inherited context" }),
    ).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
  });
});
