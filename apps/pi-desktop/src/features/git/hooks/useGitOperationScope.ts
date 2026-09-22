import { useCallback, useMemo, useRef, useState, type Dispatch, type SetStateAction } from "react";
import { gitScopeKey, type GitWorkspace } from "../gitContext";

/** A new visit has a new token, including A → B → A. Captured callbacks stay on their visit. */
export function useGitOperationScope(workspace: GitWorkspace | null) {
  const key = gitScopeKey(workspace);
  const current = useRef({ key });
  if (current.current.key !== key) current.current = { key };
  const token = current.current;
  const isCurrent = useCallback(() => current.current === token, [token]);
  return useMemo(() => ({ token, isCurrent }), [token, isCurrent]);
}

export function useGitScopedState<T>(
  scope: ReturnType<typeof useGitOperationScope>, initial: T,
): [T, Dispatch<SetStateAction<T>>] {
  const { token, isCurrent } = scope;
  const initialRef = useRef(initial);
  const [state, setState] = useState({ token, value: initial });
  const set = useCallback<Dispatch<SetStateAction<T>>>((update) => {
    if (!isCurrent()) return;
    setState((previous) => ({
      token,
      value: typeof update === "function"
        ? (update as (value: T) => T)(previous.token === token ? previous.value : initialRef.current)
        : update,
    }));
  }, [isCurrent, token]);
  return [state.token === token ? state.value : initial, set];
}
