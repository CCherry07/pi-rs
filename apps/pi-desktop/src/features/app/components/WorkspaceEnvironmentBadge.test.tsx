// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { WorkspaceInfo } from "@/types";
import { listGitCheckouts } from "@/services/tauri";
import type { GitInventory } from "../../git/gitContext";
import { useGitCheckouts } from "../../git/hooks/useGitCheckouts";
import { MainHeader } from "./MainHeader";

vi.mock("@/services/tauri", () => ({ listGitCheckouts: vi.fn() }));
const project: WorkspaceInfo = { id: "project", name: "Project", path: "/project/new-path", settings: { sidebarCollapsed: false } };
const writeText = vi.fn().mockResolvedValue(undefined);
function inventory(worktree = false): GitInventory {
  return {
    workspace: { primaryRoot: "app", executionDir: "/saved/app/src", roots: [
      { id: "app", name: "App", path: "/saved/app", ownership: worktree ? { kind: "managedWorktree", sourceRoot: "app" } : { kind: "external" } },
      { id: "docs", name: "Docs", path: "/saved/docs", ownership: { kind: "external" } },
    ] },
    checkouts: ["app", "docs"].map((id) => ({ key: id, workdir: `/saved/${id}`, gitDir: `/saved/${id}/.git`, commonDir: `/saved/${id}/.git`, rootIds: [id] })),
    directoryRootIds: [], defaultCheckoutKey: "docs", errors: [],
  };
}
function Header({ session = "a" }: { session?: string }) {
  const repositories = useGitCheckouts(project, session);
  return <>
    <button onClick={() => repositories.select("docs")}>Select Docs repository</button>
    <MainHeader workspace={project} workspaceInventory={repositories.inventory} workspaceError={repositories.error}
      onRefreshWorkspace={repositories.refresh} branchName="main" branches={[]} onCheckoutBranch={() => {}}
      onCreateBranch={() => {}} openTargets={[]} openAppIconById={{}} selectedOpenAppId="" onSelectOpenAppId={() => {}}
      onToggleTerminal={() => {}} isTerminalOpen={false} showWorkspaceTools={false} showTerminalButton={false} />
  </>;
}
beforeEach(() => {
  vi.resetAllMocks();
  writeText.mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
});
afterEach(cleanup);

it.each([false, true])("shows the saved execution directory independently of Project and selected Git paths (worktree=%s)", async (worktree) => {
  const data = inventory(worktree);
  data.errors = [{ rootId: "docs", message: "Supplemental repository scan failed" }];
  vi.mocked(listGitCheckouts).mockResolvedValue(data);
  render(<Header />);
  const badge = await screen.findByRole("button", { name: worktree ? "Worktree" : "Local" });
  fireEvent.click(badge);
  expect(screen.getByRole("dialog", { name: "Current environment" }).textContent).toContain("/saved/app/src");
  expect(screen.queryByText("/project/new-path")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Select Docs repository" }));
  expect(screen.getByRole("button", { name: worktree ? "Worktree" : "Local" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Copy path" }));
  await waitFor(() => expect(writeText).toHaveBeenCalledWith("/saved/app/src"));
  fireEvent.keyDown(window, { key: "Escape" });
  expect(screen.queryByRole("dialog", { name: "Current environment" })).toBeNull();
});

it("does not label a Local primary root as Worktree because of supplemental or unrelated nested checkouts", async () => {
  const data = inventory();
  data.workspace.roots[1].ownership = { kind: "managedWorktree", sourceRoot: "docs" };
  data.checkouts[1].commonDir = "/original/docs/.git";
  data.checkouts.push({ key: "nested", workdir: "/saved/app/other", gitDir: "/original/.git/worktrees/nested", commonDir: "/original/.git", rootIds: ["app"] });
  vi.mocked(listGitCheckouts).mockResolvedValue(data);
  render(<Header />);
  expect(await screen.findByRole("button", { name: "Local" })).toBeTruthy();
});

it("recognizes an external linked worktree containing the execution directory", async () => {
  const data = inventory();
  data.checkouts[0].commonDir = "/original/.git";
  vi.mocked(listGitCheckouts).mockResolvedValue(data);
  render(<Header />);
  expect(await screen.findByRole("button", { name: "Worktree" })).toBeTruthy();
});

it("uses the closest containing checkout when a root also contains nested repositories", async () => {
  const data = inventory();
  data.checkouts[0].commonDir = "/original/.git";
  data.checkouts.push({ key: "nested", workdir: "/saved/app/src", gitDir: "/saved/app/src/.git", commonDir: "/saved/app/src/.git", rootIds: ["app"] });
  vi.mocked(listGitCheckouts).mockResolvedValue(data);
  render(<Header />);
  expect(await screen.findByRole("button", { name: "Local" })).toBeTruthy();
});

it("discards late A responses after switching A → B → A and never falls back to the Project path", async () => {
  let resolveOld!: (value: GitInventory) => void;
  const old = new Promise<GitInventory>((resolve) => { resolveOld = resolve; });
  const latest = inventory(true);
  latest.workspace.executionDir = "/saved/app/other";
  vi.mocked(listGitCheckouts).mockReturnValueOnce(old).mockResolvedValueOnce(inventory()).mockResolvedValueOnce(latest);
  const { rerender } = render(<Header />);
  expect(screen.queryByRole("button", { name: "Local" })).toBeNull();
  expect(screen.getByRole("button", { name: "Loading…" })).toBeTruthy();
  await act(async () => { rerender(<Header session="b" />); });
  await act(async () => { rerender(<Header session="a" />); });
  await act(async () => { resolveOld(inventory()); });
  fireEvent.click(screen.getByRole("button", { name: "Worktree" }));
  expect(screen.getByText("/saved/app/other")).toBeTruthy();
  expect(screen.queryByText("/saved/app/src")).toBeNull();
});

it("shows failed environment reads and retries without presenting a guessed directory", async () => {
  vi.mocked(listGitCheckouts).mockRejectedValueOnce(new Error("Session unavailable")).mockResolvedValueOnce(inventory());
  render(<Header />);
  fireEvent.click(await screen.findByRole("button", { name: "Unavailable" }));
  expect(screen.getByRole("alert").textContent).toContain("Session unavailable");
  expect(screen.queryByText("/project/new-path")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Retry" }));
  fireEvent.click(await screen.findByRole("button", { name: "Local" }));
  expect(screen.getByText("/saved/app/src")).toBeTruthy();
});
