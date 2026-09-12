import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { DebugEntry, ModelOption } from "../../../types";
import { getModelList } from "../../../services/tauri";
import { usePiEvents } from "@/features/app/hooks/usePiEvents";
import {
  normalizeEffortValue,
  parseModelListResponse,
} from "../utils/modelListResponse";

type UseModelsOptions = {
  onDebug?: (entry: DebugEntry) => void;
  preferredModelId?: string | null;
  preferredEffort?: string | null;
  selectionKey?: string | null;
};

type CatalogContext = {
  workspaceId: string | null;
  threadId: string | null;
};

type LoadedCatalog = CatalogContext & { revision: number };

const findModelByIdOrModel = (
  models: ModelOption[],
  idOrModel: string | null,
): ModelOption | null => {
  if (!idOrModel) {
    return null;
  }
  return (
    models.find((model) => model.id === idOrModel) ??
    models.find((model) => model.model === idOrModel) ??
    null
  );
};

const pickDefaultModel = (models: ModelOption[]) =>
  models.find((model) => model.isDefault) ?? models[0] ?? null;

export function useModels({
  onDebug,
  preferredModelId = null,
  preferredEffort = null,
  selectionKey = null,
}: UseModelsOptions) {
  const [models, setModels] = useState<ModelOption[]>([]);
  const [selectedModelId, setSelectedModelIdState] = useState<string | null>(
    null,
  );
  const [selectedEffort, setSelectedEffortState] = useState<string | null>(
    null,
  );
  const [catalogContext, setCatalogContext] = useState<CatalogContext>({
    workspaceId: null,
    threadId: null,
  });
  const contextRef = useRef(catalogContext);
  contextRef.current = catalogContext;
  const request = useRef(0);
  const [loadedCatalog, setLoadedCatalog] = useState<LoadedCatalog | null>(
    null,
  );
  const modelSelectionRevision = useRef(0);
  const effortSelectionRevision = useRef(0);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const hasUserSelectedModel = useRef(false);
  const hasUserSelectedEffort = useRef(false);
  const lastWorkspaceId = useRef<string | null>(null);
  const lastSelectionKey = useRef<string | null>(null);

  const { workspaceId, threadId } = catalogContext;
  // The app resolves thread selection after model selection. Explicit context
  // keeps this query tied to that selected session, including a workspace draft.
  const selectCatalog = useCallback(
    (nextWorkspaceId: string | null, nextThreadId: string | null) => {
      const current = contextRef.current;
      if (
        current.workspaceId === nextWorkspaceId &&
        current.threadId === nextThreadId
      ) {
        return;
      }
      ++request.current;
      setLoadedCatalog(null);
      const next = { workspaceId: nextWorkspaceId, threadId: nextThreadId };
      contextRef.current = next;
      setCatalogContext(next);
      setModels([]);
    },
    [],
  );

  useEffect(() => {
    if (selectionKey === lastSelectionKey.current) {
      return;
    }
    lastSelectionKey.current = selectionKey;
    hasUserSelectedModel.current = false;
    hasUserSelectedEffort.current = false;
  }, [selectionKey]);

  useEffect(() => {
    if (workspaceId === lastWorkspaceId.current) {
      return;
    }
    hasUserSelectedModel.current = false;
    hasUserSelectedEffort.current = false;
    lastWorkspaceId.current = workspaceId;
  }, [workspaceId]);

  useEffect(() => {
    if (selectedEffort === null) {
      return;
    }
    if (selectedEffort.trim().length > 0) {
      return;
    }
    hasUserSelectedEffort.current = false;
    setSelectedEffortState(null);
  }, [selectedEffort]);

  const setSelectedModelId = useCallback((next: string | null) => {
    ++modelSelectionRevision.current;
    hasUserSelectedModel.current = true;
    setSelectedModelIdState(next);
  }, []);

  const setSelectedEffort = useCallback((next: string | null) => {
    ++effortSelectionRevision.current;
    hasUserSelectedEffort.current = true;
    setSelectedEffortState(next);
  }, []);

  const selectedModel = useMemo(
    () => models.find((model) => model.id === selectedModelId) ?? null,
    [models, selectedModelId],
  );

  const reasoningSupported = useMemo(() => {
    if (!selectedModel) {
      return false;
    }
    return (
      selectedModel.supportedReasoningEfforts.length > 0 ||
      selectedModel.defaultReasoningEffort !== null
    );
  }, [selectedModel]);

  const reasoningOptions = useMemo(() => {
    const supported = selectedModel?.supportedReasoningEfforts.map(
      (effort) => effort.reasoningEffort,
    );
    if (supported && supported.length > 0) {
      return supported;
    }
    const defaultEffort = normalizeEffortValue(
      selectedModel?.defaultReasoningEffort,
    );
    return defaultEffort ? [defaultEffort] : [];
  }, [selectedModel]);

  const resolveEffort = useCallback(
    (model: ModelOption, preferCurrent: boolean) => {
      const supportedEfforts = model.supportedReasoningEfforts.map(
        (effort) => effort.reasoningEffort,
      );
      const currentEffort = normalizeEffortValue(selectedEffort);
      if (
        preferCurrent &&
        currentEffort &&
        (supportedEfforts.length === 0 ||
          supportedEfforts.includes(currentEffort))
      ) {
        return currentEffort;
      }
      if (supportedEfforts.length === 0) {
        return normalizeEffortValue(preferredEffort);
      }
      const preferred = normalizeEffortValue(preferredEffort);
      if (preferred && supportedEfforts.includes(preferred)) {
        return preferred;
      }
      return normalizeEffortValue(model.defaultReasoningEffort);
    },
    [preferredEffort, selectedEffort],
  );

  const selection = useRef({
    selectedModelId,
    selectedEffort,
    preferredModelId,
    resolveEffort,
    onDebug,
  });
  selection.current = {
    selectedModelId,
    selectedEffort,
    preferredModelId,
    resolveEffort,
    onDebug,
  };
  const refreshModels = useCallback(
    async (expectedWorkspaceId?: string, expectedThreadId?: string | null) => {
      const context = contextRef.current;
      if (
        !context.workspaceId ||
        (expectedWorkspaceId !== undefined &&
          (context.workspaceId !== expectedWorkspaceId ||
            context.threadId !== expectedThreadId))
      ) {
        return;
      }
      const id = ++request.current;
      const initialModelRevision = modelSelectionRevision.current;
      const initialEffortRevision = effortSelectionRevision.current;
      setLoadedCatalog(null);
      const isCurrent = () =>
        mounted.current &&
        request.current === id &&
        contextRef.current === context;
      const debug = selection.current.onDebug;
      debug?.({
        id: `${Date.now()}-client-model-list`,
        timestamp: Date.now(),
        source: "client",
        label: "model/list",
        payload: context,
      });
      try {
        const response: unknown = await getModelList(
          context.workspaceId,
          context.threadId,
        );
        if (!isCurrent()) {
          return;
        }
        debug?.({
          id: `${Date.now()}-server-model-list`,
          timestamp: Date.now(),
          source: "server",
          label: "model/list response",
          payload: response,
        });
        const data: ModelOption[] = parseModelListResponse(response);
        setModels(data);
        const current = selection.current;
        const defaultModel = pickDefaultModel(data);
        const existingSelection = findModelByIdOrModel(
          data,
          current.selectedModelId,
        );
        if (current.selectedModelId && !existingSelection) {
          hasUserSelectedModel.current = false;
        }
        const preferredSelection = findModelByIdOrModel(
          data,
          current.preferredModelId,
        );
        const nativeSelection =
          context.threadId !== null &&
          initialModelRevision === modelSelectionRevision.current;
        const nativeEffort =
          context.threadId !== null &&
          initialEffortRevision === effortSelectionRevision.current;
        const nextSelection = nativeSelection
          ? (data.find((model) => model.isDefault) ?? null)
          : ((hasUserSelectedModel.current ? existingSelection : null) ??
            preferredSelection ??
            defaultModel ??
            existingSelection);
        if (nextSelection) {
          setSelectedModelIdState(nextSelection.id);
          setSelectedEffortState(
            nativeEffort
              ? normalizeEffortValue(nextSelection.defaultReasoningEffort)
              : current.resolveEffort(
                  nextSelection,
                  hasUserSelectedEffort.current,
                ),
          );
        } else {
          setSelectedModelIdState(null);
          setSelectedEffortState(null);
        }
        setLoadedCatalog({ ...context, revision: id });
      } catch (error) {
        if (!isCurrent()) {
          return;
        }
        debug?.({
          id: `${Date.now()}-client-model-list-error`,
          timestamp: Date.now(),
          source: "error",
          label: "model/list error",
          payload: error instanceof Error ? error.message : String(error),
        });
      }
    },
    [],
  );

  // Capture the catalog that produced this render's model selection. The live
  // revision also rejects a stale effect after a refresh begins in the same flush.
  const isCatalogCurrent = useCallback(
    (expectedWorkspaceId: string, expectedThreadId: string) =>
      loadedCatalog !== null &&
      loadedCatalog.workspaceId === expectedWorkspaceId &&
      loadedCatalog.threadId === expectedThreadId &&
      loadedCatalog.revision === request.current,
    [loadedCatalog],
  );

  useEffect(() => {
    void refreshModels();
  }, [workspaceId, threadId, refreshModels]);

  usePiEvents({
    onThreadReplaced: (eventWorkspaceId, previousThreadId, thread) => {
      const context = contextRef.current;
      if (
        context.workspaceId !== eventWorkspaceId ||
        context.threadId !== previousThreadId
      ) {
        return;
      }
      const nextId = String(thread.id);
      if (nextId === previousThreadId) {
        void refreshModels(eventWorkspaceId, nextId);
      } else {
        selectCatalog(eventWorkspaceId, nextId);
      }
    },
  });

  useEffect(() => {
    if (!selectedModel) {
      return;
    }
    const currentEffort = normalizeEffortValue(selectedEffort);
    const supportedEfforts = selectedModel.supportedReasoningEfforts.map(
      (effort) => effort.reasoningEffort,
    );
    if (
      currentEffort &&
      (supportedEfforts.length === 0 ||
        supportedEfforts.includes(currentEffort))
    ) {
      return;
    }
    const nextEffort = resolveEffort(selectedModel, false);
    if (nextEffort === selectedEffort) {
      return;
    }
    hasUserSelectedEffort.current = false;
    setSelectedEffortState(nextEffort);
  }, [resolveEffort, selectedEffort, selectedModel]);

  useEffect(() => {
    if (threadId !== null || !models.length) {
      return;
    }
    const preferredSelection = findModelByIdOrModel(models, preferredModelId);
    const defaultModel = pickDefaultModel(models);
    const existingSelection = findModelByIdOrModel(models, selectedModelId);
    if (selectedModelId && !existingSelection) {
      hasUserSelectedModel.current = false;
    }
    const shouldKeepUserSelection =
      hasUserSelectedModel.current && existingSelection !== null;
    if (shouldKeepUserSelection) {
      return;
    }
    const nextSelection =
      preferredSelection ?? defaultModel ?? existingSelection ?? null;
    if (!nextSelection) {
      return;
    }
    if (nextSelection.id !== selectedModelId) {
      setSelectedModelIdState(nextSelection.id);
    }
    const nextEffort = resolveEffort(
      nextSelection,
      hasUserSelectedEffort.current,
    );
    if (nextEffort !== selectedEffort) {
      setSelectedEffortState(nextEffort);
    }
  }, [
    models,
    threadId,
    preferredModelId,
    selectedEffort,
    selectedModelId,
    resolveEffort,
  ]);

  return {
    models,
    selectedModel,
    reasoningSupported,
    selectedModelId,
    setSelectedModelId,
    reasoningOptions,
    selectedEffort,
    setSelectedEffort,
    refreshModels,
    selectCatalog,
    isCatalogCurrent,
  };
}
