import { useEffect, type CSSProperties, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import type { AutocompleteItem } from "../hooks/useComposerAutocomplete";
import FileText from "lucide-react/dist/esm/icons/file-text";
import GitFork from "lucide-react/dist/esm/icons/git-fork";
import Info from "lucide-react/dist/esm/icons/info";
import PlusCircle from "lucide-react/dist/esm/icons/plus-circle";
import Plug from "lucide-react/dist/esm/icons/plug";
import RotateCcw from "lucide-react/dist/esm/icons/rotate-ccw";
import ScrollText from "lucide-react/dist/esm/icons/scroll-text";
import Wrench from "lucide-react/dist/esm/icons/wrench";
import { PopoverSurface } from "../../design-system/components/popover/PopoverPrimitives";
import { getFileTypeIconUrl } from "../../../utils/fileTypeIcons";

type ComposerSuggestionsPopoverProps = {
  highlightIndex: number;
  onHighlightIndex: (index: number) => void;
  onSelectSuggestion: (item: AutocompleteItem) => void;
  suggestionListRef: RefObject<HTMLDivElement | null>;
  suggestionRefs: RefObject<Array<HTMLButtonElement | null>>;
  suggestions: AutocompleteItem[];
  suggestionsOpen: boolean;
  suggestionsStyle?: CSSProperties;
};

const isFileSuggestion = (item: AutocompleteItem) => item.group === "Files";

const suggestionIcon = (item: AutocompleteItem) => {
  if (isFileSuggestion(item)) {
    return FileText;
  }
  if (item.id.startsWith("skill:") || item.id.startsWith("slash-skill:")) {
    return Wrench;
  }
  if (item.id.startsWith("app:")) {
    return Plug;
  }
  if (item.id === "fork") {
    return GitFork;
  }
  if (item.id === "mcp" || item.id === "apps") {
    return Plug;
  }
  if (item.id === "new") {
    return PlusCircle;
  }
  if (item.id === "resume") {
    return RotateCcw;
  }
  if (item.id === "status") {
    return Info;
  }
  if (item.id.startsWith("prompt:")) {
    return ScrollText;
  }
  return Wrench;
};

const fileTitle = (path: string) => {
  const normalized = path.replace(/\\/g, "/");
  const parts = normalized.split("/").filter(Boolean);
  return parts.length ? parts[parts.length - 1] : path;
};

export function ComposerSuggestionsPopover({
  highlightIndex,
  onHighlightIndex,
  onSelectSuggestion,
  suggestionListRef,
  suggestionRefs,
  suggestions,
  suggestionsOpen,
  suggestionsStyle,
}: ComposerSuggestionsPopoverProps) {
  const { t } = useTranslation("messages");
  const suggestionsCount = suggestions.length;

  useEffect(() => {
    if (!suggestionsOpen || suggestionsCount === 0) {
      return;
    }
    const list = suggestionListRef.current;
    const item = suggestionRefs.current[highlightIndex];
    if (!list || !item) {
      return;
    }
    const listRect = list.getBoundingClientRect();
    const itemRect = item.getBoundingClientRect();
    if (itemRect.top < listRect.top) {
      item.scrollIntoView({ block: "nearest" });
      return;
    }
    if (itemRect.bottom > listRect.bottom) {
      item.scrollIntoView({ block: "nearest" });
    }
  }, [
    highlightIndex,
    suggestionListRef,
    suggestionRefs,
    suggestionsCount,
    suggestionsOpen,
  ]);

  if (!suggestionsOpen) {
    return null;
  }

  return (
    <PopoverSurface
      className="composer-suggestions"
      role="listbox"
      ref={suggestionListRef}
      style={suggestionsStyle}
    >
      {suggestions.map((item, index) => {
          const prevGroup = suggestions[index - 1]?.group;
          const showGroup = Boolean(item.group && item.group !== prevGroup);
          const Icon = suggestionIcon(item);
          const fileSuggestion = isFileSuggestion(item);
          const title = fileSuggestion ? fileTitle(item.label) : item.label;
          const description = fileSuggestion ? item.label : item.description;
          const fileTypeIconUrl = fileSuggestion ? getFileTypeIconUrl(item.label) : null;

          return (
            <div key={item.id}>
              {showGroup && item.group && (
                <div className="composer-suggestion-section">
                  {t(`composer.groups.${item.group}` as "composer.groups.Files")}
                </div>
              )}
              <button
                type="button"
                className={`composer-suggestion${index === highlightIndex ? " is-active" : ""}`}
                role="option"
                aria-selected={index === highlightIndex}
                ref={(node) => {
                  suggestionRefs.current[index] = node;
                }}
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => onSelectSuggestion(item)}
                onMouseEnter={() => onHighlightIndex(index)}
              >
                <span className="composer-suggestion-row">
                  <span className="composer-suggestion-icon" aria-hidden>
                    {fileTypeIconUrl ? (
                      <img
                        className="composer-suggestion-icon-image"
                        src={fileTypeIconUrl}
                        alt=""
                        loading="lazy"
                        decoding="async"
                      />
                    ) : (
                      <Icon size={14} />
                    )}
                  </span>
                  <span className="composer-suggestion-content">
                    <span className="composer-suggestion-title">{title}</span>
                    {description && (
                      <span
                        className="composer-suggestion-description"
                        title={description}
                      >
                        {description}
                      </span>
                    )}
                    {!fileSuggestion && item.hint && (
                      <span className="composer-suggestion-description" title={item.hint}>{item.hint}</span>
                    )}
                  </span>
                </span>
              </button>
            </div>
          );
        })}
    </PopoverSurface>
  );
}
