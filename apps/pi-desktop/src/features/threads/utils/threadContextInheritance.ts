import type { ThreadContextInheritance } from "@/types";
import { buildItemsFromThread } from "@utils/threadItems";

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isIdentity(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0;
}

function isNullableIdentity(value: unknown): value is string | null {
  return value === null || isIdentity(value);
}

function inheritedItemsFromSnapshot(snapshot: unknown): ThreadContextInheritance["inheritedItems"] {
  if (!isRecord(snapshot) || !Array.isArray(snapshot.turns) ||
      !snapshot.turns.every((turn: unknown) => isRecord(turn) && Array.isArray(turn.items) && turn.items.every(isRecord))) {
    return null;
  }
  // Item variants and fields belong to the shared converter, not provenance parsing.
  try {
    return buildItemsFromThread(snapshot);
  } catch {
    return null;
  }
}

/** Only explicit backend provenance describes inheritance; parent links alone do not. */
export function contextInheritanceFromThread(thread: Record<string, unknown>): ThreadContextInheritance | null {
  const origin = thread.contextOrigin;
  if (!isRecord(origin) || (origin.mode !== "fresh" && origin.mode !== "fork") ||
      !isIdentity(origin.parentThreadId) || !isNullableIdentity(origin.parentEntryId) ||
      !isNullableIdentity(origin.snapshotEntryId)) {
    return null;
  }
  return {
    origin: {
      mode: origin.mode,
      parentThreadId: origin.parentThreadId,
      parentEntryId: origin.parentEntryId,
      snapshotEntryId: origin.snapshotEntryId,
    },
    inheritedItems: origin.mode === "fork" ? inheritedItemsFromSnapshot(thread.inheritedContext) : null,
  };
}
