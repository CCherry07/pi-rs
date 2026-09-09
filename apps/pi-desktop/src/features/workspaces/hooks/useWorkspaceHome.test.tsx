// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { ModelOption, WorkspaceInfo } from "../../../types";
import { generateRunMetadata } from "../../../services/tauri";
import { useWorkspaceHome } from "./useWorkspaceHome";

vi.mock("../../../services/tauri", () => ({
  generateRunMetadata: vi.fn(),
}));

const workspace: WorkspaceInfo = {
  id: "ws-1",
  name: "Project",
  path: "/tmp/project",
  kind: "main",
  settings: { sidebarCollapsed: false },
};

const worktreeWorkspace: WorkspaceInfo = {
  id: "wt-1",
  name: "feat/test",
  path: "/tmp/project/worktrees/feat-test",
  kind: "worktree",
  parentId: "ws-1",
  worktree: { branch: "feat/test" },
  settings: { sidebarCollapsed: false },
};

const models: ModelOption[] = [
  {
    id: "openai/gpt-5.1-max",
    model: "gpt-5.1-max",
    displayName: "GPT-5.1 Max",
    description: "Test model",
    supportedReasoningEfforts: [
      { reasoningEffort: "low", description: "Low effort" },
      { reasoningEffort: "medium", description: "Medium effort" },
      { reasoningEffort: "high", description: "High effort" },
    ],
    defaultReasoningEffort: "medium",
    isDefault: false,
  },
];

describe("useWorkspaceHome", () => {
  it("reloads the prepared workspace without starting a thread", async () => {
    const reloadWorkspace = vi.fn().mockResolvedValue(undefined);
    const startThreadForWorkspace = vi.fn();
    const sendUserMessageToThread = vi.fn();
    const { result } = renderHook(() =>
      useWorkspaceHome({
        activeWorkspace: workspace,
        models,
        selectedModelId: "openai/gpt-5.1-max",
        addWorktreeAgent: vi.fn(),
        startThreadForWorkspace,
        sendUserMessageToThread,
        reloadWorkspace,
      }),
    );

    act(() => result.current.setDraft("/reload"));
    await act(async () => {
      expect(await result.current.startRun()).toBe(true);
    });

    expect(reloadWorkspace).toHaveBeenCalledOnce();
    expect(startThreadForWorkspace).not.toHaveBeenCalled();
    expect(sendUserMessageToThread).not.toHaveBeenCalled();
    expect(result.current.draft).toBe("");
    expect(result.current.runs).toEqual([]);
  });

  it("uses provider-qualified model identity for worktree runs", async () => {
    const addWorktreeAgent = vi.fn().mockResolvedValue(worktreeWorkspace);
    const startThreadForWorkspace = vi.fn().mockResolvedValue("thread-1");
    const sendUserMessageToThread = vi.fn().mockResolvedValue(undefined);
    const seedThreadRunParams = vi.fn();
    vi.mocked(generateRunMetadata).mockResolvedValue({
      title: "Test run",
      worktreeName: "feat/test",
    });

    const { result } = renderHook(() =>
      useWorkspaceHome({
        activeWorkspace: workspace,
        models,
        selectedModelId: null,
        seedThreadRunParams,
        addWorktreeAgent,
        startThreadForWorkspace,
        sendUserMessageToThread,
      }),
    );

    act(() => {
      result.current.setRunMode("worktree");
      result.current.toggleModelSelection("openai/gpt-5.1-max");
      result.current.setDraft("Hello worktree");
    });

    await act(async () => {
      await result.current.startRun();
    });

    expect(sendUserMessageToThread).toHaveBeenCalledWith(
      worktreeWorkspace,
      "thread-1",
      "Hello worktree",
      [],
      expect.objectContaining({ model: "openai/gpt-5.1-max" }),
    );
    expect(seedThreadRunParams).toHaveBeenCalledWith("wt-1", "thread-1", {
      modelId: "openai/gpt-5.1-max",
      effort: null,
      serviceTier: undefined,
    });
  });

  it("allows image-only local runs", async () => {
    const addWorktreeAgent = vi.fn();
    const startThreadForWorkspace = vi.fn().mockResolvedValue("thread-1");
    const sendUserMessageToThread = vi.fn().mockResolvedValue(undefined);
    const seedThreadRunParams = vi.fn();
    vi.mocked(generateRunMetadata).mockResolvedValue({
      title: "Image run",
      worktreeName: "feat/image",
    });

    const { result } = renderHook(() =>
      useWorkspaceHome({
        activeWorkspace: workspace,
        models,
        selectedModelId: "openai/gpt-5.1-max",
        seedThreadRunParams,
        addWorktreeAgent,
        startThreadForWorkspace,
        sendUserMessageToThread,
      }),
    );

    await act(async () => {
      const started = await result.current.startRun(["img-1"]);
      expect(started).toBe(true);
    });

    expect(sendUserMessageToThread).toHaveBeenCalledWith(
      workspace,
      "thread-1",
      "",
      ["img-1"],
      expect.objectContaining({ model: "openai/gpt-5.1-max" }),
    );
    expect(seedThreadRunParams).toHaveBeenCalledWith("ws-1", "thread-1", {
      modelId: "openai/gpt-5.1-max",
      effort: null,
      serviceTier: undefined,
    });
  });

  it("blocks worktree runs without model selections", async () => {
    const addWorktreeAgent = vi.fn();
    const startThreadForWorkspace = vi.fn();
    const sendUserMessageToThread = vi.fn();
    vi.mocked(generateRunMetadata).mockResolvedValue({
      title: "Blocked",
      worktreeName: "feat/blocked",
    });

    const { result } = renderHook(() =>
      useWorkspaceHome({
        activeWorkspace: workspace,
        models,
        selectedModelId: null,
        addWorktreeAgent,
        startThreadForWorkspace,
        sendUserMessageToThread,
      }),
    );

    act(() => {
      result.current.setRunMode("worktree");
      result.current.setDraft("Hello");
    });

    let started = true;
    await act(async () => {
      started = await result.current.startRun();
    });

    expect(started).toBe(false);
    expect(result.current.error).toBe(
      "Select at least one model to run in a worktree.",
    );
    expect(result.current.runs).toHaveLength(0);
  });

  it("captures partial failures for multi-instance worktree runs", async () => {
    const addWorktreeAgent = vi
      .fn()
      .mockResolvedValueOnce(worktreeWorkspace)
      .mockResolvedValueOnce(null);
    const startThreadForWorkspace = vi.fn().mockResolvedValue("thread-1");
    const sendUserMessageToThread = vi.fn().mockResolvedValue(undefined);
    vi.mocked(generateRunMetadata).mockResolvedValue({
      title: "Partial",
      worktreeName: "feat/partial",
    });

    const { result } = renderHook(() =>
      useWorkspaceHome({
        activeWorkspace: workspace,
        models,
        selectedModelId: null,
        addWorktreeAgent,
        startThreadForWorkspace,
        sendUserMessageToThread,
      }),
    );

    act(() => {
      result.current.setRunMode("worktree");
      result.current.toggleModelSelection("openai/gpt-5.1-max");
      result.current.setModelCount("openai/gpt-5.1-max", 2);
      result.current.setDraft("Hello");
    });

    await act(async () => {
      await result.current.startRun();
    });

    expect(result.current.runs[0].status).toBe("partial");
    expect(result.current.runs[0].instanceErrors.length).toBeGreaterThan(0);
  });

  it("updates title after metadata resolves for local runs", async () => {
    const addWorktreeAgent = vi.fn();
    const startThreadForWorkspace = vi.fn().mockResolvedValue("thread-1");
    const sendUserMessageToThread = vi.fn().mockResolvedValue(undefined);
    let resolveMetadata: (value: { title: string; worktreeName: string }) => void =
      () => {};
    vi.mocked(generateRunMetadata).mockReturnValue(
      new Promise((resolve) => {
        resolveMetadata = resolve;
      }),
    );

    const { result } = renderHook(() =>
      useWorkspaceHome({
        activeWorkspace: workspace,
        models,
        selectedModelId: "openai/gpt-5.1-max",
        addWorktreeAgent,
        startThreadForWorkspace,
        sendUserMessageToThread,
      }),
    );

    act(() => {
      result.current.setDraft("Local prompt");
    });

    await act(async () => {
      await result.current.startRun();
    });

    expect(result.current.runs[0].title).toBe("Local prompt");

    await act(async () => {
      resolveMetadata({ title: "Meta title", worktreeName: "feat/meta" });
      await Promise.resolve();
    });

    expect(result.current.runs[0].title).toBe("Meta title");
  });

  it("keeps attachments when worktree selection is missing", async () => {
    const addWorktreeAgent = vi.fn();
    const startThreadForWorkspace = vi.fn();
    const sendUserMessageToThread = vi.fn();
    vi.mocked(generateRunMetadata).mockResolvedValue({
      title: "Blocked",
      worktreeName: "feat/blocked",
    });

    const { result } = renderHook(() =>
      useWorkspaceHome({
        activeWorkspace: workspace,
        models,
        selectedModelId: null,
        addWorktreeAgent,
        startThreadForWorkspace,
        sendUserMessageToThread,
      }),
    );

    act(() => {
      result.current.setRunMode("worktree");
    });

    let started = true;
    await act(async () => {
      started = await result.current.startRun(["img-1"]);
    });

    expect(started).toBe(false);
    expect(result.current.runs).toHaveLength(0);
    expect(result.current.error).toBe(
      "Select at least one model to run in a worktree.",
    );
  });
});
