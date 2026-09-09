import {
  memo,
  useCallback,
  useEffect,
  useRef,
  useState,
  type ClipboardEvent,
} from "react";
import { useTranslation } from "react-i18next";
import type {
  ComposerSendIntent,
  ComposerEditorSettings,
  CustomPromptOption,
  DictationTranscript,
  FollowUpMessageBehavior,
  QueuedMessage,
  ServiceTier,
  ThreadTokenUsage,
} from "../../../types";
import {
  getFenceTriggerLine,
  getLineIndent,
  isCodeLikeSingleLine,
  isCursorInsideFence,
  normalizePastedText,
} from "../../../utils/composerText";
import { useComposerAutocompleteState } from "../hooks/useComposerAutocompleteState";
import { useComposerDraftEffects } from "../hooks/useComposerDraftEffects";
import { useComposerKeyDown } from "../hooks/useComposerKeyDown";
import { useComposerSuggestionStyle } from "../hooks/useComposerSuggestionStyle";
import { usePromptHistory } from "../hooks/usePromptHistory";
import { ComposerInput } from "./ComposerInput";
import { ComposerMetaBar } from "./ComposerMetaBar";
import { ComposerQueue } from "./ComposerQueue";
import { isMacPlatform } from "../../../utils/platformPaths";
import { isDesktopCommandName } from "../../../utils/desktopCommands";

type ComposerProps = {
  onSend: (
    text: string,
    images: string[],
    submitIntent?: ComposerSendIntent,
  ) => void;
  onStop: () => void;
  canStop: boolean;
  disabled?: boolean;
  isProcessing: boolean;
  steerAvailable: boolean;
  followUpMessageBehavior: FollowUpMessageBehavior;
  models: { id: string; displayName: string; model: string }[];
  selectedModelId: string | null;
  onSelectModel: (id: string) => void;
  reasoningOptions: string[];
  selectedEffort: string | null;
  onSelectEffort: (effort: string) => void;
  selectedServiceTier: ServiceTier | null;
  onSelectServiceTier: (tier: ServiceTier | null) => void;
  reasoningSupported: boolean;
  skills: { name: string; description?: string }[];
  runtimeCommands?: import("../../../utils/desktopCommands").RuntimeCommand[];
  prompts: CustomPromptOption[];
  files: string[];
  tokenUsage?: ThreadTokenUsage | null;
  queuedMessages?: QueuedMessage[];
  queuePausedReason?: string | null;
  onSteerQueued?: (item: QueuedMessage) => void;
  onEditQueued?: (item: QueuedMessage) => void;
  onDeleteQueued?: (id: string) => void;
  sendLabel?: string;
  draftText?: string;
  onDraftChange?: (text: string) => void;
  historyKey?: string | null;
  attachedImages?: string[];
  onPickImages?: () => void;
  onAttachImages?: (paths: string[]) => void;
  onRemoveImage?: (path: string) => void;
  prefillDraft?: QueuedMessage | null;
  onPrefillHandled?: (id: string) => void;
  insertText?: QueuedMessage | null;
  onInsertHandled?: (id: string) => void;
  textareaRef?: React.RefObject<HTMLTextAreaElement | null>;
  editorSettings?: ComposerEditorSettings;
  editorExpanded?: boolean;
  onToggleEditorExpanded?: () => void;
  dictationEnabled?: boolean;
  dictationState?: "idle" | "listening" | "processing";
  dictationLevel?: number;
  onToggleDictation?: () => void;
  onCancelDictation?: () => void;
  onOpenDictationSettings?: () => void;
  dictationTranscript?: DictationTranscript | null;
  onDictationTranscriptHandled?: (id: string) => void;
  dictationError?: string | null;
  onDismissDictationError?: () => void;
  dictationHint?: string | null;
  onDismissDictationHint?: () => void;
  onFileAutocompleteActiveChange?: (active: boolean) => void;
};

const DEFAULT_EDITOR_SETTINGS: ComposerEditorSettings = {
  preset: "default",
  expandFenceOnSpace: false,
  expandFenceOnEnter: false,
  fenceLanguageTags: false,
  fenceWrapSelection: false,
  autoWrapPasteMultiline: false,
  autoWrapPasteCodeLike: false,
  continueListOnShiftEnter: false,
};

export const Composer = memo(function Composer({
  onSend,
  onStop,
  canStop,
  disabled = false,
  isProcessing,
  steerAvailable,
  followUpMessageBehavior,
  models,
  selectedModelId,
  onSelectModel,
  reasoningOptions,
  selectedEffort,
  onSelectEffort,
  selectedServiceTier,
  onSelectServiceTier,
  reasoningSupported,
  skills,
  runtimeCommands,
  prompts,
  files,
  tokenUsage = null,
  queuedMessages = [],
  queuePausedReason = null,
  onSteerQueued,
  onEditQueued,
  onDeleteQueued,
  sendLabel,
  draftText = "",
  onDraftChange,
  historyKey = null,
  attachedImages = [],
  onPickImages,
  onAttachImages,
  onRemoveImage,
  prefillDraft = null,
  onPrefillHandled,
  insertText = null,
  onInsertHandled,
  textareaRef: externalTextareaRef,
  editorSettings: editorSettingsProp,
  editorExpanded = false,
  onToggleEditorExpanded,
  dictationEnabled = false,
  dictationState = "idle",
  dictationLevel = 0,
  onToggleDictation,
  onCancelDictation,
  onOpenDictationSettings,
  dictationTranscript = null,
  onDictationTranscriptHandled,
  dictationError = null,
  onDismissDictationError,
  dictationHint = null,
  onDismissDictationHint,
  onFileAutocompleteActiveChange,
}: ComposerProps) {
  const { t } = useTranslation("messages");
  const [text, setText] = useState(draftText);
  const [selectionStart, setSelectionStart] = useState<number | null>(null);
  const internalRef = useRef<HTMLTextAreaElement | null>(null);
  const textareaRef = externalTextareaRef ?? internalRef;
  const editorSettings = editorSettingsProp ?? DEFAULT_EDITOR_SETTINGS;
  const isDictationBusy = dictationState !== "idle";
  const canSend = text.trim().length > 0 || attachedImages.length > 0;
  const isMac = isMacPlatform();
  const effectiveFollowUpBehavior: FollowUpMessageBehavior =
    followUpMessageBehavior === "steer" && steerAvailable ? "steer" : "queue";
  const oppositeFollowUpIntent: ComposerSendIntent =
    effectiveFollowUpBehavior === "queue" ? "steer" : "queue";
  const defaultSubmitIntent: ComposerSendIntent = isProcessing
    ? effectiveFollowUpBehavior
    : "default";
  const oppositeSubmitIntent: ComposerSendIntent = isProcessing
    ? oppositeFollowUpIntent
    : "default";
  const effectiveSendLabel = isProcessing
    ? effectiveFollowUpBehavior === "steer"
      ? t("composer.steer")
      : t("composer.queue")
    : sendLabel ?? t("composer.send");
  const {
    expandFenceOnSpace,
    expandFenceOnEnter,
    fenceLanguageTags,
    fenceWrapSelection,
    autoWrapPasteMultiline,
    autoWrapPasteCodeLike,
    continueListOnShiftEnter,
  } = editorSettings;

  const setComposerText = useCallback(
    (next: string) => {
      setText(next);
      onDraftChange?.(next);
    },
    [onDraftChange],
  );
  const syncDraftText = useCallback((next: string) => {
    setText((prev) => (prev === next ? prev : next));
  }, []);

  const {
    isAutocompleteOpen,
    autocompleteMatches,
    autocompleteAnchorIndex,
    highlightIndex,
    setHighlightIndex,
    applyAutocomplete,
    handleInputKeyDown,
    handleTextChange,
    handleSelectionChange,
    fileTriggerActive,
  } = useComposerAutocompleteState({
    text,
    selectionStart,
    disabled,
    skills,
    runtimeCommands,
    prompts,
    files,
    textareaRef,
    setText: setComposerText,
    setSelectionStart,
  });
  useEffect(() => {
    onFileAutocompleteActiveChange?.(fileTriggerActive);
  }, [fileTriggerActive, onFileAutocompleteActiveChange]);
  const suggestionsOpen = isAutocompleteOpen;
  const suggestions = autocompleteMatches;
  const suggestionsStyle = useComposerSuggestionStyle({
    isAutocompleteOpen,
    autocompleteAnchorIndex,
    selectionStart,
    text,
    textareaRef,
  });

  const {
    handleHistoryKeyDown,
    handleHistoryTextChange,
    recordHistory,
    resetHistoryNavigation,
  } = usePromptHistory({
    historyKey,
    text,
    hasAttachments: attachedImages.length > 0,
    disabled,
    isAutocompleteOpen: suggestionsOpen,
    textareaRef,
    setText: setComposerText,
    setSelectionStart,
  });

  const handleTextChangeWithHistory = useCallback(
    (next: string, cursor: number | null) => {
      handleHistoryTextChange(next);
      handleTextChange(next, cursor);
    },
    [handleHistoryTextChange, handleTextChange],
  );

  const handleSend = useCallback((submitIntent: ComposerSendIntent = "default") => {
    if (disabled) {
      return;
    }
    const trimmed = text.trim();
    if (!trimmed && attachedImages.length === 0) {
      return;
    }
    if (trimmed) {
      recordHistory(trimmed);
    }
    onSend(trimmed, attachedImages, submitIntent);
    resetHistoryNavigation();
    setComposerText("");
  }, [
    attachedImages,
    disabled,
    onSend,
    recordHistory,
    resetHistoryNavigation,
    setComposerText,
    text,
  ]);

  useComposerDraftEffects({
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
  });

  const applyTextInsertion = useCallback(
    (nextText: string, nextCursor: number) => {
      setComposerText(nextText);
      requestAnimationFrame(() => {
        const textarea = textareaRef.current;
        if (!textarea) {
          return;
        }
        textarea.focus();
        textarea.setSelectionRange(nextCursor, nextCursor);
        handleSelectionChange(nextCursor);
      });
    },
    [handleSelectionChange, setComposerText, textareaRef],
  );

  const handleTextPaste = useCallback(
    (event: ClipboardEvent<HTMLTextAreaElement>) => {
      if (disabled) {
        return;
      }
      if (!autoWrapPasteMultiline && !autoWrapPasteCodeLike) {
        return;
      }
      const pasted = event.clipboardData?.getData("text/plain") ?? "";
      if (!pasted) {
        return;
      }
      const textarea = textareaRef.current;
      if (!textarea) {
        return;
      }
      const start = textarea.selectionStart ?? text.length;
      const end = textarea.selectionEnd ?? start;
      if (isCursorInsideFence(text, start)) {
        return;
      }
      const normalized = normalizePastedText(pasted);
      if (!normalized) {
        return;
      }
      const isMultiline = normalized.includes("\n");
      if (isMultiline && !autoWrapPasteMultiline) {
        return;
      }
      if (
        !isMultiline &&
        !(autoWrapPasteCodeLike && isCodeLikeSingleLine(normalized))
      ) {
        return;
      }
      event.preventDefault();
      const indent = getLineIndent(text, start);
      const content = indent
        ? normalized
            .split("\n")
            .map((line) => `${indent}${line}`)
            .join("\n")
        : normalized;
      const before = text.slice(0, start);
      const after = text.slice(end);
      const block = `${indent}\`\`\`\n${content}\n${indent}\`\`\``;
      const nextText = `${before}${block}${after}`;
      const nextCursor = before.length + block.length;
      applyTextInsertion(nextText, nextCursor);
    },
    [
      applyTextInsertion,
      autoWrapPasteCodeLike,
      autoWrapPasteMultiline,
      disabled,
      text,
      textareaRef,
    ],
  );

  const tryExpandFence = useCallback(
    (start: number, end: number) => {
      if (start !== end && !fenceWrapSelection) {
        return false;
      }
      const fence = getFenceTriggerLine(text, start, fenceLanguageTags);
      if (!fence) {
        return false;
      }
      const before = text.slice(0, fence.lineStart);
      const after = text.slice(fence.lineEnd);
      const openFence = `${fence.indent}\`\`\`${fence.tag}`;
      const closeFence = `${fence.indent}\`\`\``;
      if (fenceWrapSelection && start !== end) {
        const selection = normalizePastedText(text.slice(start, end));
        const content = fence.indent
          ? selection
              .split("\n")
              .map((line) => `${fence.indent}${line}`)
              .join("\n")
          : selection;
        const block = `${openFence}\n${content}\n${closeFence}`;
        const nextText = `${before}${block}${after}`;
        const nextCursor = before.length + block.length;
        applyTextInsertion(nextText, nextCursor);
        return true;
      }
      const block = `${openFence}\n${fence.indent}\n${closeFence}`;
      const nextText = `${before}${block}${after}`;
      const nextCursor =
        before.length + openFence.length + 1 + fence.indent.length;
      applyTextInsertion(nextText, nextCursor);
      return true;
    },
    [applyTextInsertion, fenceLanguageTags, fenceWrapSelection, text],
  );
  const handleKeyDown = useComposerKeyDown({
    applyTextInsertion,
    canSend,
    continueListOnShiftEnter,
    defaultSubmitIntent,
    expandFenceOnEnter,
    expandFenceOnSpace,
    handleHistoryKeyDown,
    handleInputKeyDown,
    handleSend,
    isDictationBusy,
    isMac,
    oppositeSubmitIntent,
    suggestionsOpen,
    text,
    textareaRef,
    tryExpandFence,
  });


  const commandCollisions = (runtimeCommands ?? []).filter((command) => isDesktopCommandName(command.name));

  return (
    <footer className={`composer${disabled ? " is-disabled" : ""}`}>
      {commandCollisions.length > 0 && (
        <div role="status" className="composer-command-warning">
          桌面命令优先；以下同名运行时命令不可用：{commandCollisions.map((command) => `/${command.name}`).join("、")}
        </div>
      )}
      <ComposerQueue
        queuedMessages={queuedMessages}
        pausedReason={queuePausedReason}
        steerAvailable={steerAvailable}
        onSteerQueued={onSteerQueued}
        onEditQueued={onEditQueued}
        onDeleteQueued={onDeleteQueued}
      />
      <ComposerInput
        text={text}
        disabled={disabled}
        sendLabel={effectiveSendLabel}
        canStop={canStop}
        canSend={canSend}
        isProcessing={isProcessing}
        onStop={onStop}
        onSend={() => handleSend(defaultSubmitIntent)}
        dictationEnabled={dictationEnabled}
        dictationState={dictationState}
        dictationLevel={dictationLevel}
        onToggleDictation={onToggleDictation}
        onCancelDictation={onCancelDictation}
        onOpenDictationSettings={onOpenDictationSettings}
        dictationError={dictationError}
        onDismissDictationError={onDismissDictationError}
        dictationHint={dictationHint}
        onDismissDictationHint={onDismissDictationHint}
        attachments={attachedImages}
        onAddAttachment={onPickImages}
        onAttachImages={onAttachImages}
        onRemoveAttachment={onRemoveImage}
        onTextChange={handleTextChangeWithHistory}
        onSelectionChange={handleSelectionChange}
        onTextPaste={handleTextPaste}
        isExpanded={editorExpanded}
        onToggleExpand={onToggleEditorExpanded}
        onKeyDown={handleKeyDown}
        textareaRef={textareaRef}
        suggestionsOpen={suggestionsOpen}
        suggestions={suggestions}
        highlightIndex={highlightIndex}
        onHighlightIndex={setHighlightIndex}
        onSelectSuggestion={applyAutocomplete}
        suggestionsStyle={suggestionsStyle}
      />
      <ComposerMetaBar
        disabled={disabled}
        models={models}
        selectedModelId={selectedModelId}
        onSelectModel={onSelectModel}
        reasoningOptions={reasoningOptions}
        selectedEffort={selectedEffort}
        onSelectEffort={onSelectEffort}
        selectedServiceTier={selectedServiceTier}
        onSelectServiceTier={onSelectServiceTier}
        reasoningSupported={reasoningSupported}
        tokenUsage={tokenUsage}
      />
    </footer>
  );
});

Composer.displayName = "Composer";
