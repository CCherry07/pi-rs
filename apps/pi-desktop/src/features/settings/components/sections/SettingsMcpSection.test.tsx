// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ask } from "@tauri-apps/plugin-dialog";
import { readMcpConfig, saveMcpConfig, testMcpConnection, type McpDocument } from "@/services/tauri";
import { SettingsMcpSection } from "./SettingsMcpSection";

vi.mock("@/services/tauri", () => ({ readMcpConfig: vi.fn(), saveMcpConfig: vi.fn(), testMcpConnection: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ ask: vi.fn() }));
const content = JSON.stringify({ version: 1, custom: "keep", mcpServers: { remote: { type: "http", url: "https://example.com/mcp", headers: { Authorization: "Bearer ${TOKEN}" }, future: 42 } } }, null, 2);
const doc: McpDocument = { path: "/agent/mcp.json", content, revision: "rev-1", writable: true, projectTrusted: false, diagnostic: null, servers: [{ name: "remote", scope: "global", enabled: true, transport: "http" }] };
beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(readMcpConfig).mockResolvedValue(doc);
  vi.mocked(saveMcpConfig).mockImplementation(async (_workspace, _scope, _revision, next) => ({ ...doc, content: next, revision: "rev-2" }));
  vi.mocked(testMcpConnection).mockResolvedValue(["mcp__remote__echo"]);
  vi.mocked(ask).mockResolvedValue(true);
});
afterEach(cleanup);

function mount() { const dirty = vi.fn(); render(<SettingsMcpSection projects={[]} onDirtyChange={dirty} />); return dirty; }

describe("SettingsMcpSection", () => {
  it("reads without connecting, tests only on demand, and centers action icons", async () => {
    mount();
    await screen.findByText("remote");
    expect(readMcpConfig).toHaveBeenCalledWith(null, "global");
    expect(testMcpConnection).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Test" }));
    await screen.findByText(/mcp__remote__echo/);
    expect(testMcpConnection).toHaveBeenCalledExactlyOnceWith(null, "remote");
    const refresh = screen.getByRole("button", { name: "Reload file" });
    expect(refresh.classList.contains("settings-mcp-icon")).toBe(true);
    expect(refresh.querySelector("svg")).not.toBeNull();
  });

  it("edits a selected server, preserves unknown fields, and saves with the revision", async () => {
    const dirty = mount();
    fireEvent.click(await screen.findByRole("button", { name: /remote.*http/ }));
    fireEvent.change(screen.getByLabelText("URL"), { target: { value: "https://new.example/mcp" } });
    expect(dirty).toHaveBeenLastCalledWith(true);
    expect(screen.getByRole("button", { name: "Test" })).toHaveProperty("disabled", true);
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(saveMcpConfig).toHaveBeenCalledOnce());
    const [workspace, scope, revision, next] = vi.mocked(saveMcpConfig).mock.calls[0];
    expect([workspace, scope, revision]).toEqual([null, "global", "rev-1"]);
    expect(JSON.parse(next)).toEqual({ ...JSON.parse(content), mcpServers: { remote: { ...JSON.parse(content).mcpServers.remote, url: "https://new.example/mcp" } } });
    await waitFor(() => expect(dirty).toHaveBeenLastCalledWith(false));
  });

  it("lets invalid files be fixed directly and keeps the draft on a save conflict", async () => {
    vi.mocked(readMcpConfig).mockResolvedValue({ ...doc, content: "{broken", diagnostic: "Invalid mcp.json", servers: [] });
    vi.mocked(saveMcpConfig).mockRejectedValue("MCP configuration changed on disk; reload before saving");
    mount();
    const editor = await screen.findByRole("textbox", { name: "mcp.json" });
    fireEvent.change(editor, { target: { value: content } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await screen.findByText(/changed on disk/);
    expect(editor).toHaveProperty("value", content);
    expect(testMcpConnection).not.toHaveBeenCalled();
  });

  it("asks before reloading a dirty file and stages new servers disabled", async () => {
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "Add server" }));
    expect(screen.getByRole("button", { name: "Enable new-server" }).getAttribute("aria-pressed")).toBe("false");
    vi.mocked(ask).mockResolvedValue(false);
    fireEvent.click(screen.getByRole("button", { name: "Reload file" }));
    await waitFor(() => expect(ask).toHaveBeenCalled());
    expect(readMcpConfig).toHaveBeenCalledOnce();
    expect(screen.getByLabelText("Name")).toHaveProperty("value", "new-server");
  });
});
