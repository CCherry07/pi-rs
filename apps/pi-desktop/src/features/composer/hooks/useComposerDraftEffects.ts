import { useEffect, type RefObject } from "react";
import type { DictationTranscript, QueuedMessage } from "../../../types";
import { computeDictationInsertion } from "../../../utils/dictation";

type UseComposerDraftEffectsArgs = {
  draftText: string;
  prefillDraft: QueuedMessage | null;
  onPrefillHandled?: (id: string) => void;
  insertText: QueuedMessage | null;
  onInsertHandled?: (id: string) => void;
  dictationTranscript: DictationTranscript | null;
  onDictationTranscriptHandled?: (id: string) => void;
  textareaRef: RefObject<HTMLTextAreaElement | null>;
  selectionStart: number | null;
  syncDraftText: (next: string) => void;
  text: string;
  setComposerText: (next: string) => void;
  resetHistoryNavigation: () => void;
  handleSelectionChange: (cursor: number | null) => void;
};

function applyQueuedMessage({
  message,
  handled,
  setComposerText,
  resetHistoryNavigation,
}: {
  message: QueuedMessage;
  handled?: (id: string) => void;
  setComposerText: (next: string) => void;
  resetHistoryNavigation: () => void;
}) {
  setComposerText(message.text);
  resetHistoryNavigation();
  handled?.(message.id);
}

export function useComposerDraftEffects({
  draftText,
  prefillDraft,
  onPrefillHandled,
  insertText,
  onInsertHandled,
  dictationTranscript,
  onDictationTranscriptHandled,
  textareaRef,
  selectionStart,
  syncDraftText,
  text,
  setComposerText,
  resetHistoryNavigation,
  handleSelectionChange,
}: UseComposerDraftEffectsArgs) {
  useEffect(() => {
    syncDraftText(draftText);
  }, [draftText, syncDraftText]);

  useEffect(() => {
    if (!prefillDraft) {
      return;
    }
    applyQueuedMessage({
      message: prefillDraft,
      handled: onPrefillHandled,
      setComposerText,
      resetHistoryNavigation,
    });
  }, [
    onPrefillHandled,
    prefillDraft,
    resetHistoryNavigation,
    setComposerText,
  ]);

  useEffect(() => {
    if (!insertText) {
      return;
    }
    applyQueuedMessage({
      message: insertText,
      handled: onInsertHandled,
      setComposerText,
      resetHistoryNavigation,
    });
  }, [
    insertText,
    onInsertHandled,
    resetHistoryNavigation,
    setComposerText,
  ]);

  useEffect(() => {
    if (!dictationTranscript) {
      return;
    }
    const textToInsert = dictationTranscript.text.trim();
    if (!textToInsert) {
      onDictationTranscriptHandled?.(dictationTranscript.id);
      return;
    }
    const textarea = textareaRef.current;
    const start = textarea?.selectionStart ?? selectionStart ?? text.length;
    const end = textarea?.selectionEnd ?? start;
    const { nextText, nextCursor } = computeDictationInsertion(
      text,
      textToInsert,
      start,
      end,
    );
    setComposerText(nextText);
    resetHistoryNavigation();
    requestAnimationFrame(() => {
      if (!textareaRef.current) {
        return;
      }
      textareaRef.current.focus();
      textareaRef.current.setSelectionRange(nextCursor, nextCursor);
      handleSelectionChange(nextCursor);
    });
    onDictationTranscriptHandled?.(dictationTranscript.id);
  }, [
    dictationTranscript,
    handleSelectionChange,
    onDictationTranscriptHandled,
    resetHistoryNavigation,
    selectionStart,
    setComposerText,
    text,
    textareaRef,
  ]);
}
