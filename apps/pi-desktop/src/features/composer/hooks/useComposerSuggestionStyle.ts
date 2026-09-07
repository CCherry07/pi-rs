import { useLayoutEffect, useState, type CSSProperties, type RefObject } from "react";
import { getCaretPosition } from "../../../utils/caretPosition";

const CARET_ANCHOR_GAP = 8;

type UseComposerSuggestionStyleArgs = {
  isAutocompleteOpen: boolean;
  autocompleteAnchorIndex: number | null;
  selectionStart: number | null;
  text: string;
  textareaRef: RefObject<HTMLTextAreaElement | null>;
};

export function useComposerSuggestionStyle({
  isAutocompleteOpen,
  autocompleteAnchorIndex,
  selectionStart,
  text,
  textareaRef,
}: UseComposerSuggestionStyleArgs) {
  const [suggestionsStyle, setSuggestionsStyle] = useState<CSSProperties | undefined>(
    undefined,
  );

  useLayoutEffect(() => {
    if (!isAutocompleteOpen) {
      setSuggestionsStyle(undefined);
      return;
    }
    const textarea = textareaRef.current;
    if (!textarea) {
      return;
    }
    const cursor = autocompleteAnchorIndex ?? textarea.selectionStart ?? selectionStart ?? text.length;
    const caret = getCaretPosition(textarea, cursor);
    if (!caret) {
      return;
    }
    const textareaRect = textarea.getBoundingClientRect();
    const container = textarea.closest(".composer-input");
    const containerRect = container?.getBoundingClientRect();
    const offsetLeft = textareaRect.left - (containerRect?.left ?? 0);
    const containerWidth = container?.clientWidth ?? textarea.clientWidth ?? 0;
    const popoverWidth = Math.min(containerWidth, 420);
    const rawLeft = offsetLeft + caret.left;
    const maxLeft = Math.max(0, containerWidth - popoverWidth);
    const left = Math.min(Math.max(0, rawLeft), maxLeft);
    setSuggestionsStyle({
      left,
      right: "auto",
      bottom: `calc(100% + ${CARET_ANCHOR_GAP}px)`,
      top: "auto",
    });
  }, [autocompleteAnchorIndex, isAutocompleteOpen, selectionStart, text, textareaRef]);

  return suggestionsStyle;
}
