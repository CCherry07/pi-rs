import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ServiceTier } from "@/types";
import {
  STORAGE_KEY_THREAD_RUN_PARAMS,
  type ThreadRunParams,
  type ThreadRunParamsMap,
  loadThreadRunParams,
  makeThreadRunParamsKey,
  saveThreadRunParams,
} from "@threads/utils/threadStorage";

type ThreadRunParamsPatch = Partial<
  Pick<ThreadRunParams, "modelId" | "effort" | "serviceTier">
>;

type UseThreadRunParamsResult = {
  version: number;
  getThreadRunParams: (workspaceId: string, threadId: string) => ThreadRunParams | null;
  patchThreadRunParams: (
    workspaceId: string,
    threadId: string,
    patch: ThreadRunParamsPatch,
  ) => void;
  deleteThreadRunParams: (workspaceId: string, threadId: string) => void;
};

const DEFAULT_ENTRY: ThreadRunParams = {
  modelId: null,
  effort: null,
  serviceTier: undefined,
  updatedAt: 0,
};

function coerceServiceTier(value: unknown): ServiceTier | null {
  if (value === "fast" || value === "flex") {
    return value;
  }
  return null;
}

function sanitizeEntry(value: unknown): ThreadRunParams | null {
  if (!value || typeof value !== "object") {
    return null;
  }
  const entry = value as Record<string, unknown>;
  const hasServiceTierField = Object.prototype.hasOwnProperty.call(entry, "serviceTier");
  const serviceTier = hasServiceTierField
    ? entry.serviceTier === undefined
      ? undefined
      : entry.serviceTier === null
        ? null
        : coerceServiceTier(entry.serviceTier)
    : undefined;
  return {
    modelId: typeof entry.modelId === "string" ? entry.modelId : null,
    effort: typeof entry.effort === "string" ? entry.effort : null,
    serviceTier,
    updatedAt: typeof entry.updatedAt === "number" ? entry.updatedAt : 0,
  };
}

export function useThreadRunParams(): UseThreadRunParamsResult {
  const paramsRef = useRef<ThreadRunParamsMap>(loadThreadRunParams());
  const [version, setVersion] = useState(0);

  useEffect(() => {
    if (typeof window === "undefined") {
      return undefined;
    }
    const handleStorage = (event: StorageEvent) => {
      if (event.key !== STORAGE_KEY_THREAD_RUN_PARAMS) {
        return;
      }
      paramsRef.current = loadThreadRunParams();
      setVersion((v) => v + 1);
    };
    window.addEventListener("storage", handleStorage);
    return () => window.removeEventListener("storage", handleStorage);
  }, []);

  const getThreadRunParams = useCallback(
    (workspaceId: string, threadId: string): ThreadRunParams | null => {
      const key = makeThreadRunParamsKey(workspaceId, threadId);
      const entry = paramsRef.current[key];
      return sanitizeEntry(entry) ?? null;
    },
    [],
  );

  const patchThreadRunParams = useCallback(
    (workspaceId: string, threadId: string, patch: ThreadRunParamsPatch) => {
      const key = makeThreadRunParamsKey(workspaceId, threadId);
      const current = sanitizeEntry(paramsRef.current[key]) ?? DEFAULT_ENTRY;
      const nextEntry: ThreadRunParams = {
        ...current,
        ...patch,
        updatedAt: Date.now(),
      };
      const next: ThreadRunParamsMap = { ...paramsRef.current, [key]: nextEntry };
      paramsRef.current = next;
      saveThreadRunParams(next);
      setVersion((v) => v + 1);
    },
    [],
  );

  const deleteThreadRunParams = useCallback((workspaceId: string, threadId: string) => {
    const key = makeThreadRunParamsKey(workspaceId, threadId);
    if (!(key in paramsRef.current)) {
      return;
    }
    const { [key]: _removed, ...rest } = paramsRef.current;
    paramsRef.current = rest;
    saveThreadRunParams(rest);
    setVersion((v) => v + 1);
  }, []);

  return useMemo(
    () => ({
      version,
      getThreadRunParams,
      patchThreadRunParams,
      deleteThreadRunParams,
    }),
    [deleteThreadRunParams, getThreadRunParams, patchThreadRunParams, version],
  );
}
