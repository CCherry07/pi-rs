// @vitest-environment jsdom
import { StrictMode, useState } from "react";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  defineDesktopExtension,
  defineWidgetChannel,
  InlineCommandForm,
  Markdown,
  SessionView,
  StatusBadge,
  ToolCard,
  useDesktopContext,
  usePluginCommand,
  useWidget,
  type DesktopItemViewProps,
  type DesktopViewContext,
} from "@pi-rs/desktop-sdk";
import { DesktopExtensionsProvider, DesktopItemView, DesktopWorkspacePanels, useDesktopExtensions } from "./ExtensionHost";
import type { DesktopExtensionCatalog, DesktopExtensionSource, DesktopViewHost, LoadedDesktopExtension } from "./types";

const host: DesktopViewHost = {
  workspaceId: "workspace",
  threadId: "thread",
  locale: "en",
  readOnly: false,
  widgets: {},
  runCommand: vi.fn().mockResolvedValue(undefined),
  renderSession: (ref) => <span>Session {ref.sessionId}</span>,
  sessionStatus: () => ({ isProcessing: false, totalTokens: 5 }),
  renderMarkdown: (content) => <span>Markdown {content}</span>,
};
const item = { id: "call", toolName: "check", toolType: "tool", title: "Check", detail: "" };
const itemProps: DesktopItemViewProps = { item, expanded: true, onToggle: vi.fn() };
function builtin(component: React.ComponentType<DesktopItemViewProps>, id = "check"): LoadedDesktopExtension {
  return { definition: defineDesktopExtension({ id, views: [{ id: "result", slot: "tool.result", target: { tool: "check" }, component }] }), revision: "builtin", scope: "builtin" };
}
function source(scope: "global" | "project" = "global", revision = "first"): DesktopExtensionSource {
  return { id: "check", revision, javascript: "bundle", css: ".test {}", scope };
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}
function ReloadButton() {
  const { reload, error } = useDesktopExtensions();
  return <><button onClick={reload}>Reload</button>{error && <div role="alert">{error}</div>}</>;
}

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("desktop extension host", () => {
  it("lets plugins decode widget state and run typed commands through common interaction state", async () => {
    for (const key of ["checks", ".", "checks..progress", "checks:progress"]) {
      expect(() => defineWidgetChannel({ key, decode: (value: unknown) => value })).toThrow("namespaced key");
    }
    const channel = defineWidgetChannel({
      key: "checks.progress",
      decode(value: unknown) {
        if (!value || typeof value !== "object" || !("done" in value) || typeof value.done !== "number") {
          throw new Error("invalid checks state");
        }
        return value.done;
      },
    });
    const command = deferred<void>();
    const runCommand = vi.fn().mockReturnValueOnce(command.promise).mockRejectedValueOnce(new Error("retry failed"));
    function Renderer() {
      const progress = useWidget(channel);
      const retry = usePluginCommand<{ checkId: string }>("checks:retry");
      return <>
        <span>{progress.status === "ready" ? `Done ${progress.value}` : progress.status}</span>
        <InlineCommandForm ariaLabel="Retry instructions" submitLabel="Retry" pendingLabel="Sending"
          pending={retry.pending} error={retry.error}
          onSubmit={instructions => retry.run({ checkId: instructions })} />
      </>;
    }
    const builtins = [builtin(Renderer)];
    const tree = (widget: unknown) => (
      <DesktopExtensionsProvider workspaceKey="workspace" builtins={builtins}>
        <DesktopItemView {...itemProps} host={{ ...host, runCommand, widgets: { "checks.progress": widget } }} fallback="Fallback" />
      </DesktopExtensionsProvider>
    );
    const view = render(tree({ done: 2 }));
    expect(screen.getByText("Done 2")).toBeTruthy();
    const input = screen.getByRole("textbox", { name: "Retry instructions" }) as HTMLInputElement;
    fireEvent.change(input, { target: { value: "retry lint" } });
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(screen.getByRole("button", { name: "Sending" })).toBeTruthy();
    expect(input.value).toBe("retry lint");
    expect(runCommand).toHaveBeenCalledWith("checks:retry", JSON.stringify({ checkId: "retry lint" }));
    await act(async () => command.resolve());
    await waitFor(() => expect(input.value).toBe(""));

    fireEvent.change(input, { target: { value: "retry tests" } });
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("retry failed")).toBeTruthy();
    expect(input.value).toBe("retry tests");

    view.rerender(tree({ done: "bad" }));
    expect(screen.getByText("invalid")).toBeTruthy();
  });

  it("shares React hooks and common presentation with plugins without coupling their business data", async () => {
    function Renderer({ expanded, onToggle }: DesktopItemViewProps) {
      const [count, setCount] = useState(0);
      const context = useDesktopContext();
      return (
        <ToolCard title="Checks" expanded={expanded} onToggle={onToggle} status={<StatusBadge state="completed" label="Ready" />}
          actions={<button onClick={() => setCount((value) => value + 1)}>Count {count}</button>}>
          <Markdown>{String(context.widgets["check:progress"])}</Markdown>
          <SessionView reference={{ sessionId: "child" }} />
        </ToolCard>
      );
    }
    const builtins = [builtin(Renderer)];
    const toggle = vi.fn();
    const tree = (expanded: boolean, progress: string) => (
      <DesktopExtensionsProvider workspaceKey="workspace" builtins={builtins}>
        <DesktopItemView {...itemProps} host={{ ...host, widgets: { "check:progress": progress } }} expanded={expanded} onToggle={toggle} fallback="Fallback" />
      </DesktopExtensionsProvider>
    );
    const view = render(<StrictMode>{tree(true, "Partial")}</StrictMode>);
    expect(await screen.findByText("Markdown Partial")).toBeTruthy();
    expect(screen.getByText("Session child")).toBeTruthy();
    const button = screen.getByRole("button", { name: "Checks Ready" });
    expect(button.getAttribute("aria-expanded")).toBe("true");
    expect(document.getElementById(button.getAttribute("aria-controls") ?? "")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Count 0" }));
    expect(screen.getByRole("button", { name: "Count 1" })).toBeTruthy();
    expect(toggle).not.toHaveBeenCalled();
    view.rerender(<StrictMode>{tree(true, "Complete")}</StrictMode>);
    expect(screen.getByText("Markdown Complete")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Count 1" })).toBeTruthy();
    fireEvent.click(button);
    expect(toggle).toHaveBeenCalledOnce();
    view.rerender(<StrictMode>{tree(false, "Complete")}</StrictMode>);
    expect(screen.queryByText("Session child")).toBeNull();
  });

  it("aborts retired contexts on session changes and rejects stale command callbacks", async () => {
    const contexts: DesktopViewContext[] = [];
    function Renderer() {
      const context = useDesktopContext();
      contexts.push(context);
      return <span>{context.threadId}</span>;
    }
    const builtins = [builtin(Renderer)];
    const tree = (threadId: string) => (
      <DesktopExtensionsProvider workspaceKey="workspace" builtins={builtins}>
        <DesktopItemView {...itemProps} host={{ ...host, threadId }} fallback="Fallback" />
      </DesktopExtensionsProvider>
    );
    const view = render(<StrictMode>{tree("first")}</StrictMode>);
    const first = contexts[contexts.length - 1];
    expect(first.signal.aborted).toBe(false);
    await first.runCommand("check", "first");
    expect(host.runCommand).toHaveBeenCalledWith("check", "first");
    view.rerender(<StrictMode>{tree("second")}</StrictMode>);
    const second = contexts[contexts.length - 1];
    expect(first.signal.aborted).toBe(true);
    expect(second.signal.aborted).toBe(false);
    await expect(first.runCommand("check", "stale")).rejects.toThrow("no longer active");
    view.unmount();
    expect(second.signal.aborted).toBe(true);
  });

  it("keeps transcript fallbacks for missing, failing, and inherited snapshot renderers", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    const onError = vi.fn();
    function Broken(): never { throw new Error("view failed"); }
    render(
      <DesktopExtensionsProvider workspaceKey="workspace" builtins={[builtin(Broken)]}>
        <DesktopItemView {...itemProps} host={host} fallback={<span>Failed result</span>} onError={onError} />
        <DesktopItemView {...itemProps} item={{ ...item, toolName: "unknown" }} host={host} fallback={<span>Unknown result</span>} />
        <DesktopItemView {...itemProps} host={{ ...host, readOnly: true, frozen: true }} fallback={<span>Saved snapshot</span>} />
      </DesktopExtensionsProvider>,
    );
    expect(screen.getByText("Failed result")).toBeTruthy();
    expect(screen.getByText("Unknown result")).toBeTruthy();
    expect(screen.getByText("Saved snapshot")).toBeTruthy();
    expect(onError).toHaveBeenCalledWith("view failed");
  });

  it("renders live read-only plugin previews while rejecting commands", async () => {
    let context: DesktopViewContext | undefined;
    function Renderer() {
      context = useDesktopContext();
      return <><span>Live preview</span><SessionView reference={{ sessionId: "nested" }} /></>;
    }
    render(
      <DesktopExtensionsProvider workspaceKey="workspace" builtins={[builtin(Renderer)]}>
        <DesktopItemView {...itemProps} host={{ ...host, readOnly: true }} fallback="Saved fallback" />
      </DesktopExtensionsProvider>,
    );
    expect(screen.getByText("Live preview")).toBeTruthy();
    expect(screen.getByText("Session nested")).toBeTruthy();
    expect(screen.queryByText("Saved fallback")).toBeNull();
    expect(context?.readOnly).toBe(true);
    await expect(context!.runCommand("check", "preview")).rejects.toThrow("read-only conversation");
  });

  it("prepares a complete replacement before swapping and preserves it after a failed reload", async () => {
    const contexts: DesktopViewContext[] = [];
    function Renderer() { contexts.push(useDesktopContext()); return <span>Installed view</span>; }
    const next = deferred<DesktopExtensionCatalog>();
    const loadCatalog = vi.fn()
      .mockResolvedValueOnce({ extensions: [source()], projectTrusted: true })
      .mockImplementationOnce(() => next.promise)
      .mockResolvedValueOnce({ extensions: [source("global", "second")], projectTrusted: true });
    const importer = vi.fn()
      .mockResolvedValueOnce({ default: builtin(Renderer).definition })
      .mockRejectedValueOnce(new Error("broken module"))
      .mockResolvedValueOnce({ default: builtin(() => <span>Replacement view</span>).definition });
    render(
      <DesktopExtensionsProvider workspaceKey="workspace" loadCatalog={loadCatalog} importer={importer}>
        <ReloadButton />
        <DesktopItemView {...itemProps} host={host} fallback="Fallback" />
      </DesktopExtensionsProvider>,
    );
    await screen.findByText("Installed view");
    const first = contexts[contexts.length - 1];
    fireEvent.click(screen.getByText("Reload"));
    expect(screen.getByText("Installed view")).toBeTruthy();
    await act(async () => next.resolve({ extensions: [source("global", "second")], projectTrusted: true }));
    expect(await screen.findByText("broken module")).toBeTruthy();
    expect(screen.getByText("Installed view")).toBeTruthy();
    expect(first.signal.aborted).toBe(false);
    fireEvent.click(screen.getByText("Reload"));
    await screen.findByText("Replacement view");
    expect(first.signal.aborted).toBe(true);
  });

  it("retires a revoked project renderer even when another package is invalid", async () => {
    let context: DesktopViewContext | undefined;
    function Renderer() { context = useDesktopContext(); return <span>Project view</span>; }
    const loadCatalog = vi.fn()
      .mockResolvedValueOnce({ extensions: [source("project")], projectTrusted: true })
      .mockResolvedValueOnce({ extensions: [], projectTrusted: false, error: "invalid global package" });
    render(
      <DesktopExtensionsProvider workspaceKey="workspace" loadCatalog={loadCatalog} importer={async () => ({ default: builtin(Renderer).definition })}>
        <ReloadButton />
        <DesktopItemView {...itemProps} host={host} fallback="Fallback" />
      </DesktopExtensionsProvider>,
    );
    await screen.findByText("Project view");
    fireEvent.click(screen.getByText("Reload"));
    await screen.findByText("invalid global package");
    expect(screen.queryByText("Project view")).toBeNull();
    expect(screen.getByText("Fallback")).toBeTruthy();
    expect(context?.signal.aborted).toBe(true);
  });

  it("ignores a previous workspace's late catalog response", async () => {
    const pending = deferred<DesktopExtensionCatalog>();
    const loadCatalog = vi.fn().mockImplementationOnce(() => pending.promise).mockResolvedValue({ extensions: [], projectTrusted: true });
    const importer = vi.fn().mockResolvedValue({ default: builtin(() => <span>Old workspace view</span>).definition });
    const tree = (workspaceKey: string) => (
      <DesktopExtensionsProvider workspaceKey={workspaceKey} loadCatalog={loadCatalog} importer={importer}>
        <DesktopItemView {...itemProps} host={{ ...host, workspaceId: workspaceKey }} fallback="Fallback" />
      </DesktopExtensionsProvider>
    );
    const view = render(tree("first"));
    view.rerender(tree("second"));
    await act(async () => pending.resolve({ extensions: [source()], projectTrusted: true }));
    expect(screen.getByText("Fallback")).toBeTruthy();
    expect(screen.queryByText("Old workspace view")).toBeNull();
    expect(importer).not.toHaveBeenCalled();
  });

  it("mounts panels on demand and disposes their context on collapse", async () => {
    let context: DesktopViewContext | undefined;
    function Panel() { context = useDesktopContext(); return <span>Panel body</span>; }
    const definition = defineDesktopExtension({ id: "panel", views: [{ id: "panel", slot: "workspace.panel", title: "My panel", component: Panel }] });
    render(
      <DesktopExtensionsProvider workspaceKey="workspace" builtins={[{ definition, revision: "builtin" }]}>
        <DesktopWorkspacePanels host={host} />
      </DesktopExtensionsProvider>,
    );
    expect(screen.queryByText("Panel body")).toBeNull();
    fireEvent.click(screen.getByText("My panel"));
    await screen.findByText("Panel body");
    expect(context?.signal.aborted).toBe(false);
    fireEvent.click(screen.getByText("My panel"));
    await waitFor(() => expect(screen.queryByText("Panel body")).toBeNull());
    expect(context?.signal.aborted).toBe(true);
  });
});
