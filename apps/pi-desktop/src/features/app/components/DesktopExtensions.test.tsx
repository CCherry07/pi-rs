// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { getDesktopExtensions } from "@services/tauri";
import { subscribePiEvents } from "@services/events";
import { DesktopItemView } from "../../extensions/ExtensionHost";
import type { DesktopExtensionCatalog, DesktopViewHost } from "../../extensions/types";
import { DesktopExtensions } from "./DesktopExtensions";

vi.mock("@services/tauri", () => ({ getDesktopExtensions: vi.fn() }));
vi.mock("@services/events", () => ({ subscribePiEvents: vi.fn(() => vi.fn()) }));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

const task = { agentId: "agent-1", agent: "reviewer", task: "Review", state: "running", session: { ownerSessionId: "parent" } };
const host: DesktopViewHost = {
  workspaceId: "workspace", threadId: "parent", locale: "en", readOnly: false,
  widgets: { "subagents.tasks": { version: 1, ownerSessionId: "parent", liveAgentIds: ["agent-1"], agents: { "agent-1": task } } },
  runCommand: vi.fn(), renderSession: () => null, sessionStatus: () => undefined, renderMarkdown: (content) => content,
};
const item = { id: "call", toolName: "spawn_agent", toolType: "mcpToolCall", title: "Subagent", detail: "", data: { details: { agentId: "agent-1" } } };

beforeEach(() => vi.clearAllMocks());
afterEach(cleanup);

it("preserves plugin drafts for history refreshes and remounts only on an actual generation reload", async () => {
  const initial = deferred<DesktopExtensionCatalog>();
  const replacement = deferred<DesktopExtensionCatalog>();
  vi.mocked(getDesktopExtensions).mockReturnValueOnce(initial.promise).mockReturnValueOnce(replacement.promise);
  render(
    <DesktopExtensions workspaceId="workspace">
      <DesktopItemView host={host} item={item} expanded onToggle={() => {}} fallback="Fallback" />
    </DesktopExtensions>,
  );
  await act(async () => initial.resolve({ extensions: [], projectTrusted: true }));
  const draft = screen.getByRole("textbox", { name: "Follow up" });
  fireEvent.change(draft, { target: { value: "Keep this draft" } });
  const event = vi.mocked(subscribePiEvents).mock.calls[0][0];
  act(() => event({ workspace_id: "workspace", message: { method: "thread/replaced", params: { thread: { id: "parent" } } } }));
  expect(getDesktopExtensions).toHaveBeenCalledOnce();
  expect(screen.getByRole("textbox", { name: "Follow up" })).toBe(draft);
  expect((draft as HTMLInputElement).value).toBe("Keep this draft");
  act(() => event({ workspace_id: "other", message: { method: "thread/replaced", params: { generationChanged: true } } }));
  expect(getDesktopExtensions).toHaveBeenCalledOnce();
  act(() => event({ workspace_id: "workspace", message: { method: "thread/replaced", params: { generationChanged: true } } }));
  expect(getDesktopExtensions).toHaveBeenCalledTimes(2);
  expect((draft as HTMLInputElement).value).toBe("Keep this draft");
  await act(async () => replacement.resolve({ extensions: [], projectTrusted: true }));
  const nextDraft = screen.getByRole("textbox", { name: "Follow up" });
  expect(nextDraft).not.toBe(draft);
  expect((nextDraft as HTMLInputElement).value).toBe("");
});
