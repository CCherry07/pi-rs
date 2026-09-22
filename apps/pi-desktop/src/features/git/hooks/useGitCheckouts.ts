import i18n from "@/i18n";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { WorkspaceInfo } from "@/types";
import { listGitCheckouts } from "@/services/tauri";
import type { GitInventory, GitTarget, GitWorkspace } from "../gitContext";

export function useGitCheckouts(workspace: WorkspaceInfo | null, threadId: string | null) {
  const environment = JSON.stringify([workspace?.id, threadId, workspace?.project, workspace?.path]);
  const visit = useRef({ environment });
  if (visit.current.environment !== environment) visit.current = { environment };
  const token = visit.current;
  const requestId = useRef(0);
  const [state, setState] = useState<{ token: object; inventory: GitInventory | null; error: string | null; loading: boolean }>();
  const [selection, setSelection] = useState<{ environment: string; value: string }>();
  const [depth, setDepth] = useState(2);
  const [scanned, setScanned] = useState<object | null>(null);
  const load = useCallback(async (scanDepth?: number) => {
    if (!workspace || visit.current !== token) return;
    const id = ++requestId.current;
    setState((prev) => ({ token, inventory: prev?.token === token ? prev.inventory : null, error: null, loading: true }));
    try {
      const inventory = await listGitCheckouts(workspace.id, threadId, scanDepth);
      if (visit.current !== token || id !== requestId.current) return;
      setState({ token, inventory, error: inventory.errors.map((entry) => entry.message).join("\n") || null, loading: false });
    } catch (error) {
      if (visit.current !== token || id !== requestId.current) return;
      setState({ token, inventory: null, error: String(error), loading: false });
    }
  }, [workspace, threadId, token]);
  useEffect(() => { void load(); }, [load]);
  const inventory = state?.token === token ? state.inventory : null;
  const options = useMemo(() => {
    if (!inventory) return [];
    const checkouts = inventory.checkouts.map((checkout) => ({
      value: checkout.key, label: checkout.workdir,
      target: { kind: "checkout", key: checkout.key, workdir: checkout.workdir } as GitTarget,
    }));
    const directories = inventory.workspace.roots.filter((root) => inventory.directoryRootIds.includes(root.id)).map((root) => ({
      value: `directory:${root.id}`, label: root.path,
      target: { kind: "directory", rootId: root.id } as GitTarget,
    }));
    return [...checkouts, ...directories];
  }, [inventory]);
  const selected = useMemo(() => {
    if (selection?.environment === environment) {
      const existing = options.find((option) => option.value === selection.value);
      if (existing) return existing;
      // Successful initialization replaces a directory target with its containing checkout.
      if (selection.value.startsWith("directory:")) {
        const rootId = selection.value.slice("directory:".length);
        if (!inventory?.directoryRootIds.includes(rootId)) {
          const checkout = inventory?.checkouts.find((item) => item.rootIds.includes(rootId));
          return options.find((option) => option.value === checkout?.key);
        }
      }
      return undefined;
    }
    return options.find((option) => option.value === inventory?.defaultCheckoutKey)
      ?? options.find((option) => option.value === `directory:${inventory?.workspace.primaryRoot}`) ?? options[0];
  }, [environment, inventory, options, selection]);
  const gitWorkspace = useMemo<GitWorkspace | null>(() => workspace && inventory && selected ? ({
    ...workspace,
    gitWorkdir: selected.label,
    gitRequest: { workspaceId: workspace.id, threadId, target: selected.target },
    gitScope: JSON.stringify([workspace.id, threadId, inventory.workspace, selected.target]),
  }) : null, [workspace, inventory, selected, threadId]);
  const select = useCallback((value: string) => setSelection({ environment, value }), [environment]);
  const scan = useCallback(() => { setScanned(token); return load(depth); }, [depth, load, token]);
  const refresh = useCallback(() => load(scanned === token ? depth : undefined), [depth, load, scanned, token]);
  return {
    inventory,
    gitWorkspace, options, selected: selected?.value ?? "", select,
    workdir: selected?.label ?? null, refresh, scan,
    isLoading: state?.token === token && state.loading,
    error: state?.token === token ? state.error ?? (inventory && selection?.environment === environment && !selected ? i18n.t("repositories.unavailable", { ns: "git" }) : null) : null,
    depth, setDepth, hasScanned: scanned === token,
  };
}
