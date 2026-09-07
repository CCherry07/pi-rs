import { memo, useMemo } from "react";
import { useTranslation } from "react-i18next";
import type { GetHoveredLineResult } from "@pierre/diffs";
import { FileDiff } from "@pierre/diffs/react";
import RotateCcw from "lucide-react/dist/esm/icons/rotate-ccw";
import { parseDiff, type ParsedDiffLine } from "../../../utils/diff";
import { highlightLine, languageFromPath } from "../../../utils/syntax";
import {
  DIFF_VIEWER_SCROLL_CSS,
} from "../../design-system/diff/diffViewerTheme";
import { splitPath } from "./GitDiffPanel.utils";
import type { GitDiffViewerItem } from "./GitDiffViewer.types";
import {
  buildPierreFileDiff,
  isFallbackRawDiffLineHighlightable,
  parseRawDiffLines,
} from "./GitDiffViewer.utils";

type HoveredDiffLine = GetHoveredLineResult<"diff"> | undefined;

function isSelectableLine(
  line: ParsedDiffLine,
): line is ParsedDiffLine & { type: "add" | "del" | "context" } {
  return line.type === "add" || line.type === "del" || line.type === "context";
}

function resolveParsedLineForHover(
  parsedLines: ParsedDiffLine[],
  hovered: HoveredDiffLine,
): { line: ParsedDiffLine; index: number } | null {
  if (!hovered) {
    return null;
  }
  const side = hovered.side;
  const lineNumber = hovered.lineNumber;

  const matchForSide = (line: ParsedDiffLine) => {
    if (!isSelectableLine(line)) {
      return false;
    }
    if (side === "deletions") {
      return line.oldLine === lineNumber;
    }
    return line.newLine === lineNumber;
  };

  let index = parsedLines.findIndex(matchForSide);
  if (index >= 0) {
    return { line: parsedLines[index], index };
  }

  index = parsedLines.findIndex(
    (line) =>
      isSelectableLine(line) &&
      (line.newLine === lineNumber || line.oldLine === lineNumber),
  );
  if (index >= 0) {
    return { line: parsedLines[index], index };
  }

  return null;
}

export type DiffCardProps = {
  entry: GitDiffViewerItem;
  isSelected: boolean;
  diffStyle: "split" | "unified";
  isLoading: boolean;
  ignoreWhitespaceChanges: boolean;
  showRevert: boolean;
  onRequestRevert?: (path: string) => void;
  onLineAction?: (line: ParsedDiffLine, index: number) => void;
};

export const DiffCard = memo(function DiffCard({
  entry,
  isSelected,
  diffStyle,
  isLoading,
  ignoreWhitespaceChanges,
  showRevert,
  onRequestRevert,
  onLineAction,
}: DiffCardProps) {
  const { t } = useTranslation("git");
  const displayPath = entry.displayPath ?? entry.path;
  const { name: fileName, dir } = useMemo(
    () => splitPath(displayPath),
    [displayPath],
  );
  const displayDir = dir ? `${dir}/` : "";
  const fallbackLanguage = useMemo(
    () => languageFromPath(displayPath),
    [displayPath],
  );

  const fileDiff = useMemo(() => {
    return buildPierreFileDiff({
      diff: entry.diff,
      displayPath,
      oldLines: entry.oldLines,
      newLines: entry.newLines,
      status: entry.status,
    });
  }, [displayPath, entry.diff, entry.newLines, entry.oldLines, entry.status]);

  const placeholder = useMemo(() => {
    if (isLoading) {
      return t("viewer.loading");
    }
    if (ignoreWhitespaceChanges && !entry.diff.trim()) {
      return t("viewer.noWhitespaceChanges");
    }
    return t("viewer.unavailable");
  }, [entry.diff, ignoreWhitespaceChanges, isLoading, t]);

  const parsedLines = useMemo(() => {
    const parsed = parseDiff(entry.diff);
    if (parsed.length > 0) {
      return parsed;
    }
    return parseRawDiffLines(entry.diff);
  }, [entry.diff]);

  const hasSelectableLines = useMemo(
    () => parsedLines.some(isSelectableLine),
    [parsedLines],
  );
  const lineActionEnabled =
    diffStyle === "unified" && Boolean(onLineAction) && hasSelectableLines;

  const diffOptions = useMemo(
    () => ({
      diffStyle,
      hunkSeparators: "line-info" as const,
      overflow: "scroll" as const,
      unsafeCSS: DIFF_VIEWER_SCROLL_CSS,
      disableFileHeader: true,
      enableGutterUtility: lineActionEnabled,
    }),
    [
      diffStyle,
      lineActionEnabled,
    ],
  );

  return (
    <div
      data-diff-path={entry.path}
      className={`diff-viewer-item ${isSelected ? "active" : ""}`}
    >
      <div className="diff-viewer-header">
        <span className="diff-viewer-status" data-status={entry.status}>
          {entry.status}
        </span>
        <span className="diff-viewer-path" title={displayPath}>
          <span className="diff-viewer-name">{fileName}</span>
          {displayDir && <span className="diff-viewer-dir">{displayDir}</span>}
        </span>
        {showRevert && (
          <button
            type="button"
            className="diff-viewer-header-action diff-viewer-header-action--discard"
            title={t("viewer.discardFile")}
            aria-label={t("viewer.discardFile")}
            onClick={(event) => {
              event.preventDefault();
              event.stopPropagation();
              onRequestRevert?.(displayPath);
            }}
          >
            <RotateCcw size={14} aria-hidden />
          </button>
        )}
      </div>
      {entry.diff.trim().length > 0 && fileDiff ? (
        <div className="diff-viewer-output diff-viewer-output-flat">
          <FileDiff
            fileDiff={fileDiff}
            options={diffOptions}
            renderGutterUtility={
              lineActionEnabled
                ? (getHoveredLine) => (
                    <button
                      type="button"
                      className="diff-viewer-line-action-button"
                      aria-label={t("viewer.askHoveredLine")}
                      title={t("viewer.askThisLine")}
                      onMouseDown={(event) => {
                        event.preventDefault();
                        event.stopPropagation();
                      }}
                      onClick={(event) => {
                        event.preventDefault();
                        event.stopPropagation();
                        const resolved = resolveParsedLineForHover(
                          parsedLines,
                          getHoveredLine() as HoveredDiffLine,
                        );
                        if (!resolved) {
                          return;
                        }
                        onLineAction?.(resolved.line, resolved.index);
                      }}
                    >
                      +
                    </button>
                  )
                : undefined
            }
            style={{ width: "100%", maxWidth: "100%", minWidth: 0 }}
          />
        </div>
      ) : entry.diff.trim().length > 0 && parsedLines.length > 0 ? (
        <div className="diff-viewer-output diff-viewer-output-flat diff-viewer-output-raw">
          {parsedLines.map((line, index) => {
            const highlighted = highlightLine(
              line.text,
              isFallbackRawDiffLineHighlightable(line.type)
                ? fallbackLanguage
                : null,
            );

            return (
              <div
                key={index}
                className={`diff-viewer-raw-line diff-viewer-raw-line-${line.type}`}
              >
                <span
                  className="diff-line-content"
                  dangerouslySetInnerHTML={{ __html: highlighted }}
                />
              </div>
            );
          })}
        </div>
      ) : (
        <div className="diff-viewer-placeholder">{placeholder}</div>
      )}
    </div>
  );
});
