type DepthRow = {
  depth: number;
};

type ThreadRowVisibilityResult<T extends DepthRow> = {
  visibleRows: T[];
  rowsWithChildren: Set<T>;
};

export function buildThreadRowVisibility<T extends DepthRow>(
  rows: T[],
  isCollapsed: (row: T) => boolean,
): ThreadRowVisibilityResult<T> {
  const rowsWithChildren = new Set<T>();
  for (let index = 0; index < rows.length - 1; index += 1) {
    if (rows[index + 1].depth > rows[index].depth) {
      rowsWithChildren.add(rows[index]);
    }
  }

  const visibleRows: T[] = [];
  const collapsedAncestorDepths: number[] = [];
  rows.forEach((row) => {
    while (
      collapsedAncestorDepths.length > 0 &&
      row.depth <= collapsedAncestorDepths[collapsedAncestorDepths.length - 1]
    ) {
      collapsedAncestorDepths.pop();
    }
    if (
      collapsedAncestorDepths.length > 0 &&
      row.depth > collapsedAncestorDepths[collapsedAncestorDepths.length - 1]
    ) {
      return;
    }
    visibleRows.push(row);
    if (rowsWithChildren.has(row) && isCollapsed(row)) {
      collapsedAncestorDepths.push(row.depth);
    }
  });

  return { visibleRows, rowsWithChildren };
}
