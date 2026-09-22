// @vitest-environment jsdom
import { useGitActions } from "./useGitActions";
import { useInitGitRepoPrompt } from "./useInitGitRepoPrompt";
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { GitInventory, GitWorkspace } from "../gitContext";
import { useGitStatus } from "./useGitStatus";
import { useGitBranches } from "./useGitBranches";
import { useGitCheckouts } from "./useGitCheckouts";
import { useGitCommitController } from "../../app/hooks/useGitCommitController";
import * as api from "@/services/tauri";

vi.mock("@/services/tauri", () => ({
  initGitRepo: vi.fn(), createGitHubRepo: vi.fn(),
  getGitStatus: vi.fn(), listGitBranches: vi.fn(), listGitCheckouts: vi.fn(),
  checkoutGitBranch: vi.fn(), checkoutGitHubPullRequest: vi.fn(), createGitBranch: vi.fn(),
  stageGitAll: vi.fn(), commitGit: vi.fn(), pushGit: vi.fn(), pullGit: vi.fn(), fetchGit: vi.fn(), syncGit: vi.fn(), generateCommitMessage: vi.fn(),
}));
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((complete) => { resolve = complete; });
  return { promise, resolve };
}
function workspace(name: string): GitWorkspace {
  return {
    id: "project", name: "Project", path: "/a", settings: { sidebarCollapsed: false },
    gitScope: `session:${name}`,
    gitRequest: { workspaceId: "project", threadId: "session", target: { kind: "checkout", key: `/${name}/.git`, workdir: `/${name}` } },
  };
}
const a = workspace("a");
const b = workspace("b");
const status = (branchName: string) => ({ branchName, files: [], stagedFiles: [], unstagedFiles: [], totalAdditions: 0, totalDeletions: 0 });
afterEach(() => { cleanup(); vi.clearAllMocks(); });
beforeEach(() => { vi.mocked(api.listGitBranches).mockResolvedValue({ branches: [] }); });

it("rejects old A responses after A → B → A within the same Project", async () => {
  const old = deferred<ReturnType<typeof status>>();
  vi.mocked(api.getGitStatus).mockReturnValueOnce(old.promise).mockResolvedValueOnce(status("b")).mockResolvedValueOnce(status("new-a"));
  const { result, rerender } = renderHook(({ selected }) => useGitStatus(selected), { initialProps: { selected: a } });
  await act(async () => { rerender({ selected: b }); });
  await act(async () => { rerender({ selected: a }); });
  await act(async () => { old.resolve(status("old-a")); });
  expect(result.current.status.branchName).toBe("new-a");
  expect(api.getGitStatus).toHaveBeenNthCalledWith(2, b.gitRequest);
});

it("displays branches from the selected checkout when an older request finishes late", async () => {
  const old = deferred<{ branches: { name: string; lastCommit: number }[] }>();
  vi.mocked(api.listGitBranches).mockReturnValueOnce(old.promise).mockResolvedValueOnce({ branches: [{ name: "b", lastCommit: 1 }] });
  const { result, rerender } = renderHook(({ selected }) => useGitBranches({ activeWorkspace: selected }), { initialProps: { selected: a } });
  await act(async () => { rerender({ selected: b }); });
  await act(async () => { old.resolve({ branches: [{ name: "a", lastCommit: 1 }] }); });
  expect(result.current.branches.map((branch) => branch.name)).toEqual(["b"]);
});

function useCommit(selected: GitWorkspace) {
  return useGitCommitController({
    activeWorkspace: selected,
    commitMessageModelId: null, refreshGitStatus: vi.fn(),
    gitStatus: { ...status("main"), error: null, unstagedFiles: [{ path: "same.txt", status: "M", additions: 1, deletions: 0 }] },
  });
}

it("keeps commit-and-push on its captured checkout and preserves the new checkout's message", async () => {
  const committing = deferred<void>();
  vi.mocked(api.commitGit).mockReturnValueOnce(committing.promise);
  const { result, rerender } = renderHook(({ selected }) => useCommit(selected), { initialProps: { selected: a } });
  act(() => result.current.onCommitMessageChange("commit a"));
  let operation!: Promise<void>;
  await act(async () => { operation = result.current.onCommitAndPush(); });
  expect(api.stageGitAll).toHaveBeenCalledWith(a.gitRequest);
  expect(api.commitGit).toHaveBeenCalledWith(a.gitRequest, "commit a");
  act(() => rerender({ selected: b }));
  act(() => result.current.onCommitMessageChange("draft b"));
  await act(async () => { committing.resolve(); await operation; });
  expect(api.pushGit).toHaveBeenCalledWith(a.gitRequest);
  expect(result.current.commitMessage).toBe("draft b");
  expect(result.current.commitLoading).toBe(false);
});

it("does not overwrite typed text with a late generated commit message", async () => {
  const pending = deferred<string>();
  vi.mocked(api.generateCommitMessage).mockReturnValueOnce(pending.promise);
  const { result } = renderHook(() => useCommit(a));
  let operation!: Promise<void>;
  act(() => { operation = result.current.onGenerateCommitMessage(); });
  act(() => result.current.onCommitMessageChange("my text"));
  await act(async () => { pending.resolve("generated"); await operation; });
  expect(result.current.commitMessage).toBe("my text");
});

it("selects a repository without changing Project settings and resets when the session changes", async () => {
  const inventory: GitInventory = {
    directoryRootIds: [], defaultCheckoutKey: "/a/.git",
    workspace: { primaryRoot: "a", executionDir: "/a", roots: [
      { id: "a", name: "a", path: "/a", ownership: { kind: "external" } },
      { id: "b", name: "b", path: "/b", ownership: { kind: "external" } },
    ] }, errors: [], checkouts: ["a", "b"].map((name) => ({ key: `/${name}/.git`, workdir: `/${name}`, gitDir: `/${name}/.git`, commonDir: `/${name}/.git`, rootIds: [name] })),
  };
  vi.mocked(api.listGitCheckouts).mockResolvedValue(inventory);
  const { result, rerender } = renderHook(({ session }) => useGitCheckouts(a, session), { initialProps: { session: "s1" } });
  await act(async () => {});
  act(() => result.current.select("/b/.git"));
  expect(result.current.gitWorkspace?.gitRequest?.target).toEqual(b.gitRequest?.target);
  expect(a.settings.gitRoot).toBeUndefined();
  await act(async () => { rerender({ session: "s2" }); });
  expect(result.current.gitWorkspace?.gitRequest?.threadId).toBe("s2");
  expect(result.current.workdir).toBe("/a");
});


it("passes the initialized checkout identity to remote creation", async () => {
  const directory: GitWorkspace = { ...a, gitWorkdir: "/a", gitRequest: { workspaceId: "project", threadId: "session", target: { kind: "directory", rootId: "a" } } };
  vi.mocked(api.initGitRepo).mockResolvedValue({ status: "initialized", target: a.gitRequest!.target });
  vi.mocked(api.createGitHubRepo).mockResolvedValue({ status: "ok", repo: "a" });
  const { result } = renderHook(() => {
    const actions = useGitActions({ activeWorkspace: directory, onRefreshGitStatus: vi.fn(), onRefreshGitDiffs: vi.fn() });
    return useInitGitRepoPrompt({ activeWorkspace: directory, initGitRepo: actions.initGitRepo, createGitHubRepo: actions.createGitHubRepo, refreshGitRemote: vi.fn(), isBusy: false });
  });
  act(() => result.current.openInitGitRepoPrompt());
  await act(async () => { await result.current.handleInitGitRepoPromptConfirm(); });
  expect(api.initGitRepo).toHaveBeenCalledWith(directory.gitRequest, "main", false);
  expect(api.createGitHubRepo).toHaveBeenCalledWith(a.gitRequest, "a", "private", "main");
  expect(result.current.initGitRepoPrompt).toBeNull();
});
