import {
  parsePatchFiles,
  processFile,
  type FileDiffMetadata,
} from "@pierre/diffs";
import type { ParsedDiffLine } from "../../../utils/diff";
import type { DiffStats, GitDiffViewerItem } from "./GitDiffViewer.types";

const DIFF_METADATA_PREFIXES = [
  "+++",
  "---",
  "diff --git",
  "@@",
  "index ",
  "\\ No newline",
] as const;

export function normalizePatchName(name: string) {
  if (!name) {
    return name;
  }
  return name.replace(/^(?:a|b)\//, "");
}

export function buildPierreFileDiff({
  diff,
  displayPath,
  oldLines,
  newLines,
  status,
}: {
  diff: string;
  displayPath: string;
  oldLines?: string[];
  newLines?: string[];
  status?: string;
}): FileDiffMetadata | null {
  if (!diff.trim()) {
    return null;
  }
  const parsedPatch = parsePatchFiles(diff)[0]?.files[0];
  if (!parsedPatch) {
    return null;
  }

  const hasCompleteOldFile = oldLines !== undefined || status === "A";
  const hasCompleteNewFile = newLines !== undefined || status === "D";
  const parsed =
    hasCompleteOldFile && hasCompleteNewFile
      ? (processFile(diff, {
          oldFile: {
            name: parsedPatch.prevName ?? displayPath,
            contents: (oldLines ?? []).join(""),
          },
          newFile: {
            name: parsedPatch.name || displayPath,
            contents: (newLines ?? []).join(""),
          },
        }) ?? parsedPatch)
      : parsedPatch;

  return {
    ...parsed,
    name: normalizePatchName(parsed.name || displayPath),
    prevName: parsed.prevName
      ? normalizePatchName(parsed.prevName)
      : undefined,
  };
}

export function parseRawDiffLines(diff: string): ParsedDiffLine[] {
  return diff
    .split("\n")
    .filter((line) => line.trim().length > 0)
    .map((line) => {
      if (line.startsWith("+") && !line.startsWith("+++")) {
        return {
          type: "add",
          oldLine: null,
          newLine: null,
          text: line.slice(1),
        } satisfies ParsedDiffLine;
      }
      if (line.startsWith("-") && !line.startsWith("---")) {
        return {
          type: "del",
          oldLine: null,
          newLine: null,
          text: line.slice(1),
        } satisfies ParsedDiffLine;
      }
      if (line.startsWith(" ")) {
        return {
          type: "context",
          oldLine: null,
          newLine: null,
          text: line.slice(1),
        } satisfies ParsedDiffLine;
      }
      return {
        type: "meta",
        oldLine: null,
        newLine: null,
        text: line,
      } satisfies ParsedDiffLine;
    });
}

export function isFallbackRawDiffLineHighlightable(
  type: ParsedDiffLine["type"],
) {
  return type === "add" || type === "del" || type === "context";
}

export function calculateDiffStats(diffs: GitDiffViewerItem[]): DiffStats {
  let additions = 0;
  let deletions = 0;

  for (const entry of diffs) {
    const lines = entry.diff.split("\n");
    for (const line of lines) {
      if (!line) {
        continue;
      }
      if (DIFF_METADATA_PREFIXES.some((prefix) => line.startsWith(prefix))) {
        continue;
      }
      if (line.startsWith("+")) {
        additions += 1;
      } else if (line.startsWith("-")) {
        deletions += 1;
      }
    }
  }

  return { additions, deletions };
}
