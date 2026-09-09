import { useCallback, useMemo } from "react";
import { useTranslation } from "react-i18next";
import type { AutocompleteItem } from "./useComposerAutocomplete";
import { useComposerAutocomplete } from "./useComposerAutocomplete";
import type { CustomPromptOption } from "../../../types";
import {
  buildPromptInsertText,
  findNextPromptArgCursor,
  findPromptArgRangeAtCursor,
  getPromptArgumentHint,
} from "../../../utils/customPrompts";
import { DESKTOP_COMMANDS, isDesktopCommandName, type RuntimeCommand } from "../../../utils/desktopCommands";
import { isComposingEvent } from "../../../utils/keys";

type Skill = { name: string; description?: string };
type UseComposerAutocompleteStateArgs = {
  text: string;
  selectionStart: number | null;
  disabled: boolean;
  skills: Skill[];
  runtimeCommands?: RuntimeCommand[];
  prompts: CustomPromptOption[];
  files: string[];
  textareaRef: React.RefObject<HTMLTextAreaElement | null>;
  setText: (next: string) => void;
  setSelectionStart: (next: number | null) => void;
};

const MAX_FILE_SUGGESTIONS = 500;
const FILE_TRIGGER_PREFIX = new RegExp("^(?:\\s|[\"'`]|\\(|\\[|\\{)$");

function isFileTriggerActive(text: string, cursor: number | null) {
  if (!text || cursor === null) {
    return false;
  }
  const beforeCursor = text.slice(0, cursor);
  const atIndex = beforeCursor.lastIndexOf("@");
  if (atIndex < 0) {
    return false;
  }
  const prevChar = atIndex > 0 ? beforeCursor[atIndex - 1] : "";
  if (prevChar && !FILE_TRIGGER_PREFIX.test(prevChar)) {
    return false;
  }
  const afterAt = beforeCursor.slice(atIndex + 1);
  return afterAt.length === 0 || !/\s/.test(afterAt);
}

function getFileTriggerQuery(text: string, cursor: number | null) {
  if (!text || cursor === null) {
    return null;
  }
  const beforeCursor = text.slice(0, cursor);
  const atIndex = beforeCursor.lastIndexOf("@");
  if (atIndex < 0) {
    return null;
  }
  const prevChar = atIndex > 0 ? beforeCursor[atIndex - 1] : "";
  if (prevChar && !FILE_TRIGGER_PREFIX.test(prevChar)) {
    return null;
  }
  const afterAt = beforeCursor.slice(atIndex + 1);
  if (/\s/.test(afterAt)) {
    return null;
  }
  return afterAt;
}

export function useComposerAutocompleteState({
  text,
  selectionStart,
  disabled,
  skills,
  runtimeCommands = [],
  prompts,
  files,
  textareaRef,
  setText,
  setSelectionStart,
}: UseComposerAutocompleteStateArgs) {
  const { t } = useTranslation("messages");
  const skillItems = useMemo<AutocompleteItem[]>(
    () =>
      skills.map((skill) => ({
        id: `skill:${skill.name}`,
        label: skill.name,
        description: skill.description,
        insertText: skill.name,
        group: "Skills" as const,
      })),
    [skills],
  );

  const slashSkillItems = useMemo<AutocompleteItem[]>(
    () =>
      skills.map((skill) => ({
        id: `slash-skill:${skill.name}`,
        label: `skill:${skill.name}`,
        description: skill.description,
        insertText: `skill:${skill.name}`,
        group: "Skills" as const,
      })),
    [skills],
  );

  const fileTriggerActive = useMemo(
    () => isFileTriggerActive(text, selectionStart),
    [selectionStart, text],
  );
  const fileItems = useMemo<AutocompleteItem[]>(
    () =>
      fileTriggerActive
        ? (() => {
            const query = getFileTriggerQuery(text, selectionStart) ?? "";
            const limited = query ? files : files.slice(0, MAX_FILE_SUGGESTIONS);
            return limited.map((path) => ({
              id: path,
              label: path,
              insertText: path,
              group: "Files" as const,
            }));
          })()
        : [],
    [fileTriggerActive, files, selectionStart, text],
  );

  const promptItems = useMemo<AutocompleteItem[]>(
    () =>
      prompts
        .filter((prompt) => prompt.name)
        .map((prompt) => {
          const insert = buildPromptInsertText(prompt);
          return {
            id: `prompt:${prompt.name}`,
            label: `prompts:${prompt.name}`,
            description: prompt.description,
            hint: getPromptArgumentHint(prompt),
            insertText: insert.text,
            cursorOffset: insert.cursorOffset,
            group: "Prompts" as const,
          };
        }),
    [prompts],
  );

  const slashCommandItems = useMemo<AutocompleteItem[]>(() => {
    return DESKTOP_COMMANDS.map((name): AutocompleteItem => ({
      id: `desktop:${name}`,
      label: name,
      description: t(`composer.slash.${name}`),
      insertText: name,
      group: "Slash",
    }));
  }, [t]);

  const slashItems = useMemo<AutocompleteItem[]>(
    () => {
      const runtimeItems = runtimeCommands
        .filter((command) => !isDesktopCommandName(command.name))
        .map((command): AutocompleteItem => ({
          id: `runtime:${command.name}`,
          label: command.name,
          description: command.description,
          hint: command.argumentHint ?? undefined,
          insertText: command.name,
          group: "Slash",
        }));
      // Desktop prompt expansion owns /prompts:*; registered commands own all
      // remaining runtime names (including /skill:*).
      const items = [...slashCommandItems, ...promptItems, ...runtimeItems, ...slashSkillItems];
      return items.filter((item, index) => items.findIndex((other) => other.label === item.label) === index);
    },
    [promptItems, runtimeCommands, slashCommandItems, slashSkillItems],
  );

  const triggers = useMemo(
    () => [
      { trigger: "/", items: slashItems },
      { trigger: "$", items: skillItems },
      { trigger: "@", items: fileItems },
    ],
    [fileItems, skillItems, slashItems],
  );

  const {
    active: isAutocompleteOpen,
    matches: autocompleteMatches,
    highlightIndex,
    setHighlightIndex,
    moveHighlight,
    range: autocompleteRange,
    close: closeAutocomplete,
  } = useComposerAutocomplete({
    text,
    selectionStart,
    triggers,
  });
  const autocompleteAnchorIndex = autocompleteRange
    ? Math.max(0, autocompleteRange.start - 1)
    : null;

  const applyAutocomplete = useCallback(
    (item: AutocompleteItem) => {
      if (!autocompleteRange) {
        return;
      }
      const triggerIndex = Math.max(0, autocompleteRange.start - 1);
      const triggerChar = text[triggerIndex] ?? "";
      const cursor = selectionStart ?? autocompleteRange.end;
      const promptRange =
        triggerChar === "@" ? findPromptArgRangeAtCursor(text, cursor) : null;
      const before =
        triggerChar === "@"
          ? text.slice(0, triggerIndex)
          : text.slice(0, autocompleteRange.start);
      const after = text.slice(autocompleteRange.end);
      const insert = item.insertText ?? item.label;
      const actualInsert = triggerChar === "@"
        ? insert.replace(/^@+/, "")
        : insert;
      const needsSpace = promptRange
        ? false
        : after.length === 0
          ? true
          : !/^\s/.test(after);
      const nextText = `${before}${actualInsert}${needsSpace ? " " : ""}${after}`;
      setText(nextText);
      closeAutocomplete();
      requestAnimationFrame(() => {
        const textarea = textareaRef.current;
        if (!textarea) {
          return;
        }
        const insertCursor = Math.min(
          actualInsert.length,
          Math.max(0, item.cursorOffset ?? actualInsert.length),
        );
        const cursor =
          before.length +
          insertCursor +
          (item.cursorOffset === undefined ? (needsSpace ? 1 : 0) : 0);
        textarea.focus();
        textarea.setSelectionRange(cursor, cursor);
        setSelectionStart(cursor);
      });
    },
    [
      autocompleteRange,
      closeAutocomplete,
      selectionStart,
      setSelectionStart,
      setText,
      text,
      textareaRef,
    ],
  );

  const handleTextChange = useCallback(
    (next: string, cursor: number | null) => {
      setText(next);
      setSelectionStart(cursor);
    },
    [setSelectionStart, setText],
  );

  const handleSelectionChange = useCallback(
    (cursor: number | null) => {
      setSelectionStart(cursor);
    },
    [setSelectionStart],
  );

  const handleInputKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLTextAreaElement>) => {
      if (disabled) {
        return;
      }
      if (isComposingEvent(event)) {
        return;
      }
      if (isAutocompleteOpen) {
        if (event.key === "ArrowDown") {
          event.preventDefault();
          moveHighlight(1);
          return;
        }
        if (event.key === "ArrowUp") {
          event.preventDefault();
          moveHighlight(-1);
          return;
        }
        if (event.key === "Enter" && !event.shiftKey) {
          event.preventDefault();
          const selected =
            autocompleteMatches[highlightIndex] ?? autocompleteMatches[0];
          if (selected) {
            applyAutocomplete(selected);
          }
          return;
        }
        if (event.key === "Tab") {
          event.preventDefault();
          const selected =
            autocompleteMatches[highlightIndex] ?? autocompleteMatches[0];
          if (selected) {
            applyAutocomplete(selected);
          }
          return;
        }
        if (event.key === "Escape") {
          event.preventDefault();
          closeAutocomplete();
          return;
        }
      }
      if (event.key === "Tab") {
        const cursor = selectionStart ?? text.length;
        const nextCursor = findNextPromptArgCursor(text, cursor);
        if (nextCursor !== null) {
          event.preventDefault();
          requestAnimationFrame(() => {
            const textarea = textareaRef.current;
            if (!textarea) {
              return;
            }
            textarea.focus();
            textarea.setSelectionRange(nextCursor, nextCursor);
            setSelectionStart(nextCursor);
          });
        }
      }
    },
    [
      applyAutocomplete,
      autocompleteMatches,
      closeAutocomplete,
      disabled,
      highlightIndex,
      isAutocompleteOpen,
      moveHighlight,
      selectionStart,
      setSelectionStart,
      text,
      textareaRef,
    ],
  );

  return {
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
  };
}
