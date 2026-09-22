import { expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { executeWorktreeDelivery, finishWorktreeDeliveryAttempt, getWorktreeDelivery, inspectWorktreeDeliveryAttempt, previewWorktreeDelivery } from "./tauri";

vi.mock("@tauri-apps/api/core", async (importOriginal) => ({ ...await importOriginal<typeof import("@tauri-apps/api/core")>(), invoke: vi.fn() }));
it("pins delivery and recovery commands to the captured group, session, checkout, commits, and attempt", async () => {
  const request = { attemptId: "attempt-id", checkoutKey: "owned-checkout", targetBranch: "release", sourceOid: "source", targetOid: "target" };
  await getWorktreeDelivery("managed", "saved-session");
  await previewWorktreeDelivery("managed", "saved-session", "owned-checkout", "release");
  await executeWorktreeDelivery("managed", "saved-session", request);
  await inspectWorktreeDeliveryAttempt("managed", "saved-session", "attempt-id");
  await finishWorktreeDeliveryAttempt("managed", "saved-session", "attempt-id");
  expect(vi.mocked(invoke).mock.calls).toEqual([
    ["get_worktree_delivery", { workspaceId: "managed", threadId: "saved-session" }],
    ["preview_worktree_delivery", { workspaceId: "managed", threadId: "saved-session", checkoutKey: "owned-checkout", targetBranch: "release" }],
    ["execute_worktree_delivery", { workspaceId: "managed", threadId: "saved-session", request }],
    ["inspect_worktree_delivery_attempt", { workspaceId: "managed", threadId: "saved-session", attemptId: "attempt-id" }],
    ["finish_worktree_delivery_attempt", { workspaceId: "managed", threadId: "saved-session", attemptId: "attempt-id" }],
  ]);
});
