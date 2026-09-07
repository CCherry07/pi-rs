import type { ServiceTier } from "@/types";

const STORAGE_KEY_THREAD_ACTIVITY = "pi-monitor.threadLastUserActivity";
export const STORAGE_KEY_PINNED_THREADS = "pi-monitor.pinnedThreads";
export const STORAGE_KEY_CUSTOM_NAMES = "pi-monitor.threadCustomNames";
export const STORAGE_KEY_THREAD_RUN_PARAMS = "pi-monitor.threadRunParams";
export const MAX_PINS_SOFT_LIMIT = 5;

export type ThreadActivityMap = Record<string, Record<string, number>>;
export type PinnedThreadsMap = Record<string, number>;
export type CustomNamesMap = Record<string, string>;

// Per-thread Run parameter overrides. Keyed by `${workspaceId}:${threadId}`.
// These are UI-level preferences (not server state) and are best-effort persisted.
export type ThreadRunParams = {
  modelId: string | null;
  effort: string | null;
  // string => explicit per-thread tier override
  // null => explicit "Default/off" override
  // undefined => legacy/unset thread value that should inherit no-thread scope
  serviceTier: ServiceTier | null | undefined;
  updatedAt: number;
};

export type ThreadRunParamsMap = Record<string, ThreadRunParams>;

export function makeThreadRunParamsKey(workspaceId: string, threadId: string): string {
  return `${workspaceId}:${threadId}`;
}

export function loadThreadRunParams(): ThreadRunParamsMap {
  if (typeof window === "undefined") {
    return {};
  }
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY_THREAD_RUN_PARAMS);
    if (!raw) {
      return {};
    }
    const parsed = JSON.parse(raw) as ThreadRunParamsMap;
    if (!parsed || typeof parsed !== "object") {
      return {};
    }
    return parsed;
  } catch {
    return {};
  }
}

export function saveThreadRunParams(next: ThreadRunParamsMap): void {
  if (typeof window === "undefined") {
    return;
  }
  try {
    window.localStorage.setItem(
      STORAGE_KEY_THREAD_RUN_PARAMS,
      JSON.stringify(next),
    );
  } catch {
    // Best-effort persistence.
  }
}

export function loadThreadActivity(): ThreadActivityMap {
  if (typeof window === "undefined") {
    return {};
  }
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY_THREAD_ACTIVITY);
    if (!raw) {
      return {};
    }
    const parsed = JSON.parse(raw) as ThreadActivityMap;
    if (!parsed || typeof parsed !== "object") {
      return {};
    }
    return parsed;
  } catch {
    return {};
  }
}

export function saveThreadActivity(activity: ThreadActivityMap) {
  if (typeof window === "undefined") {
    return;
  }
  try {
    window.localStorage.setItem(
      STORAGE_KEY_THREAD_ACTIVITY,
      JSON.stringify(activity),
    );
  } catch {
    // Best-effort persistence; ignore write failures.
  }
}

export function makeCustomNameKey(workspaceId: string, threadId: string): string {
  return `${workspaceId}:${threadId}`;
}

export function loadCustomNames(): CustomNamesMap {
  if (typeof window === "undefined") {
    return {};
  }
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY_CUSTOM_NAMES);
    if (!raw) {
      return {};
    }
    const parsed = JSON.parse(raw) as CustomNamesMap;
    if (!parsed || typeof parsed !== "object") {
      return {};
    }
    return parsed;
  } catch {
    return {};
  }
}

export function saveCustomName(workspaceId: string, threadId: string, name: string): void {
  if (typeof window === "undefined") {
    return;
  }
  try {
    const current = loadCustomNames();
    const key = makeCustomNameKey(workspaceId, threadId);
    current[key] = name;
    window.localStorage.setItem(
      STORAGE_KEY_CUSTOM_NAMES,
      JSON.stringify(current),
    );
  } catch {
    // Best-effort persistence.
  }
}

export function makePinKey(workspaceId: string, threadId: string): string {
  return `${workspaceId}:${threadId}`;
}

export function loadPinnedThreads(): PinnedThreadsMap {
  if (typeof window === "undefined") {
    return {};
  }
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY_PINNED_THREADS);
    if (!raw) {
      return {};
    }
    const parsed = JSON.parse(raw) as PinnedThreadsMap;
    if (!parsed || typeof parsed !== "object") {
      return {};
    }
    return parsed;
  } catch {
    return {};
  }
}

export function savePinnedThreads(pinned: PinnedThreadsMap) {
  if (typeof window === "undefined") {
    return;
  }
  try {
    window.localStorage.setItem(
      STORAGE_KEY_PINNED_THREADS,
      JSON.stringify(pinned),
    );
  } catch {
    // Best-effort persistence; ignore write failures.
  }
}
