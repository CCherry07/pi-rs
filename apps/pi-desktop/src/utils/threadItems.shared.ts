import type { ConversationItem } from "../types";
import { CHAT_SCROLLBACK_DEFAULT } from "./chatScrollback";

export type PrepareThreadItemsOptions = {
  maxItemsPerThread?: number | null;
};

export type ExploreEntry =
  Extract<ConversationItem, { kind: "explore" }>["entries"][number];
export type ExploreItem = Extract<ConversationItem, { kind: "explore" }>;

const MAX_ITEM_TEXT = 20000;
const MAX_LARGE_TOOL_TEXT = 200000;
const LARGE_TOOL_TYPES = new Set(["fileChange", "commandExecution"]);

export const DEFAULT_MAX_ITEMS_PER_THREAD = CHAT_SCROLLBACK_DEFAULT;
export const TOOL_OUTPUT_RECENT_ITEMS = 40;

export function asString(value: unknown) {
  return typeof value === "string" ? value : value ? String(value) : "";
}

export function asNumber(value: unknown) {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }
  if (typeof value === "string") {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}

export function truncateText(text: string, maxLength = MAX_ITEM_TEXT) {
  if (text.length <= maxLength) {
    return text;
  }
  const sliceLength = Math.max(0, maxLength - 3);
  return `${text.slice(0, sliceLength)}...`;
}

export function truncateToolText(toolType: string, text: string) {
  const maxLength = LARGE_TOOL_TYPES.has(toolType)
    ? MAX_LARGE_TOOL_TEXT
    : MAX_ITEM_TEXT;
  return truncateText(text, maxLength);
}

export function normalizeStringList(value: unknown) {
  if (Array.isArray(value)) {
    return value.map((entry) => asString(entry)).filter(Boolean);
  }
  const single = asString(value);
  return single ? [single] : [];
}

export function normalizeThreadTimestamp(raw: unknown) {
  let numeric: number;
  if (typeof raw === "string") {
    const parsedNumber = Number(raw);
    if (Number.isFinite(parsedNumber)) {
      numeric = parsedNumber;
    } else {
      const parsedDate = Date.parse(raw);
      if (!Number.isFinite(parsedDate)) {
        return 0;
      }
      numeric = parsedDate;
    }
  } else {
    numeric = Number(raw);
  }
  if (!Number.isFinite(numeric) || numeric <= 0) {
    return 0;
  }
  return numeric < 1_000_000_000_000 ? numeric * 1000 : numeric;
}
