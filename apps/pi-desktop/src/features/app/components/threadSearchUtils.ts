import type { ThreadSummary } from "../../../types";

type RootRowGroup<Row extends { depth: number }> = {
  root: Row;
  rootIndex: number;
  rows: Row[];
};

function includesNormalizedText(value: string | null | undefined, query: string) {
  if (!value || !query) {
    return false;
  }
  return value.toLowerCase().includes(query);
}

export function workspaceMatchesQuery(workspaceName: string, query: string) {
  if (!query) {
    return true;
  }
  return includesNormalizedText(workspaceName, query);
}

export function threadMatchesQuery(
  thread: ThreadSummary,
  workspaceName: string,
  query: string,
) {
  if (!query) {
    return true;
  }
  return (
    includesNormalizedText(thread.name, query) ||
    includesNormalizedText(workspaceName, query) ||
    includesNormalizedText(thread.modelId ?? null, query) ||
    includesNormalizedText(thread.effort ?? null, query)
  );
}

export function splitRowsByRoot<Row extends { depth: number }>(
  rows: Row[],
): RootRowGroup<Row>[] {
  const groups: RootRowGroup<Row>[] = [];
  let current: Row[] = [];
  let currentRootIndex = 0;

  rows.forEach((row, rowIndex) => {
    if (row.depth === 0 && current.length > 0) {
      groups.push({
        root: current[0],
        rootIndex: currentRootIndex,
        rows: current,
      });
      current = [row];
      currentRootIndex = rowIndex;
      return;
    }
    if (current.length === 0) {
      currentRootIndex = rowIndex;
    }
    current.push(row);
  });

  if (current.length > 0) {
    groups.push({
      root: current[0],
      rootIndex: currentRootIndex,
      rows: current,
    });
  }

  return groups;
}

export function filterRowsByQuery<Row extends { depth: number; thread: ThreadSummary }>(
  rows: Row[],
  query: string,
  resolveWorkspaceName: (row: Row) => string,
) {
  if (!query) {
    return rows;
  }
  return splitRowsByRoot(rows)
    .filter((group) =>
      group.rows.some((row) => threadMatchesQuery(row.thread, resolveWorkspaceName(row), query)),
    )
    .flatMap((group) => group.rows);
}

export function countRootRows<Row extends { depth: number }>(rows: Row[]) {
  return splitRowsByRoot(rows).length;
}

export function getVisibleThreadListState<Row extends { depth: number; thread: ThreadSummary }>({
  rows,
  totalRoots,
  workspaceName,
  query,
  isSearchActive,
}: {
  rows: Row[];
  totalRoots: number;
  workspaceName: string;
  query: string;
  isSearchActive: boolean;
}) {
  const matchesWorkspace = workspaceMatchesQuery(workspaceName, query);
  const visibleRows =
    isSearchActive && !matchesWorkspace
      ? filterRowsByQuery(rows, query, () => workspaceName)
      : rows;

  return {
    visibleRows,
    displayRootCount: isSearchActive && !matchesWorkspace ? countRootRows(visibleRows) : totalRoots,
  };
}
