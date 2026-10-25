import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { ExtensionFooter } from "../src/extension-footer.js";

test("footer observes branch changes and stops its subscriptions on disposal", async t => {
  const root = await mkdtemp(join(tmpdir(), "pi-footer-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const git = (...args: string[]) => execFileSync("git", args, { cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
  git("init", "--quiet", "--initial-branch=first");
  const errors: unknown[] = [];
  const statuses = new Map([["tasks", "1 open"]]);
  const footer = new ExtensionFooter({ cwd: () => root, statuses: () => statuses, providerCount: () => 2, run: callback => callback(), report: error => errors.push(error) });
  t.after(() => footer.dispose());
  assert.equal(footer.getGitBranch(), "first");
  assert.equal(footer.getAvailableProviderCount(), 2);
  const oldStatuses = footer.getExtensionStatuses();
  statuses.set("tasks", "2 open");
  assert.equal(oldStatuses.get("tasks"), "1 open");
  assert.equal(footer.getExtensionStatuses().get("tasks"), "2 open");
  let callbacks = 0;
  const changed = new Promise<string | null>((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("branch notification timed out")), 5_000);
    t.after(() => clearTimeout(timeout));
    footer.onBranchChange(() => { callbacks++; clearTimeout(timeout); resolve(footer.getGitBranch()); });
  });
  git("symbolic-ref", "HEAD", "refs/heads/second");
  assert.equal(await changed, "second");
  assert.equal(callbacks, 1);
  footer.dispose();
  git("symbolic-ref", "HEAD", "refs/heads/third");
  assert.equal(footer.getGitBranch(), "second");
  assert.deepEqual(errors, []);
});

test("footer resolves detached worktrees and non-repository directories", async t => {
  const root = await mkdtemp(join(tmpdir(), "pi-footer-worktree-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  let cwd = root;
  const footer = new ExtensionFooter({ cwd: () => cwd, statuses: () => new Map(), providerCount: () => 0, run: callback => callback(), report: () => {} });
  t.after(() => footer.dispose());
  assert.equal(footer.getGitBranch(), null);
  const git = (...args: string[]) => execFileSync("git", args, { cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
  git("init", "--quiet", "--initial-branch=main");
  await writeFile(join(root, "file"), "fixture\n");
  git("add", "file");
  git("-c", "user.name=Test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "fixture");
  cwd = join(root, "worktree");
  git("worktree", "add", "--quiet", "--detach", cwd);
  assert.equal(footer.getGitBranch(), "detached");
  git("-C", cwd, "switch", "--quiet", "-c", "worktree-branch");
  // A new footer observes the branch immediately; the existing footer is covered by the polling test.
  const next = new ExtensionFooter({ cwd: () => cwd, statuses: () => new Map(), providerCount: () => 0, run: callback => callback(), report: () => {} });
  t.after(() => next.dispose());
  assert.equal(next.getGitBranch(), "worktree-branch");
});
