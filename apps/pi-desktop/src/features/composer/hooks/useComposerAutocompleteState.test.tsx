/** @vitest-environment jsdom */
import { createRef } from "react";
import { renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { useComposerAutocompleteState } from "./useComposerAutocompleteState";

function renderAutocomplete(text: string, options?: {
  files?: string[];
  runtimeCommands?: import("../../../utils/desktopCommands").RuntimeCommand[];
  skills?: Array<{ name: string; description: string }>;
}) {
  const textareaRef = createRef<HTMLTextAreaElement>();
  textareaRef.current = {
    focus: vi.fn(),
    setSelectionRange: vi.fn(),
  } as unknown as HTMLTextAreaElement;

  return renderHook(() =>
    useComposerAutocompleteState({
      text,
      selectionStart: text.length,
      disabled: false,
      skills: options?.skills ?? [],
      runtimeCommands: options?.runtimeCommands,
      prompts: [],
      files: options?.files ?? [],
      textareaRef,
      setText: vi.fn(),
      setSelectionStart: vi.fn(),
    }),
  );
}

describe("useComposerAutocompleteState", () => {
  it("completes native names and hints while Desktop descriptions win collisions", () => {
    const { result } = renderAutocomplete("/", { runtimeCommands: [
      { name: "native", description: "Native command", argumentHint: "[action]" },
      { name: "compact", description: "Wrong runtime description" },
      { name: "COMPACT", description: "Wrong uppercase description" },
    ] });
    const items = result.current.autocompleteMatches;
    expect(items.find((item) => item.label === "native")).toMatchObject({ insertText: "native", hint: "[action]", description: "Native command" });
    expect(items.filter((item) => item.label.toLowerCase() === "compact")).toHaveLength(1);
    expect(items.find((item) => item.label === "compact")?.description).not.toContain("Wrong");
  });

  it("suggests a file even if it is already mentioned earlier", () => {
    const { result } = renderAutocomplete(
      "Please review @src/App.tsx and also @",
      { files: ["src/App.tsx", "src/main.tsx"] },
    );

    expect(result.current.autocompleteMatches.map((item) => item.label)).toContain(
      "src/App.tsx",
    );
  });

  it("marks root-level file suggestions as Files", () => {
    const { result } = renderAutocomplete("@", {
      files: ["AGENTS.md", "src/main.tsx"],
    });

    expect(
      result.current.autocompleteMatches.find((item) => item.label === "AGENTS.md")
        ?.group,
    ).toBe("Files");
  });

  it("lists the supported built-in slash commands", () => {
    const { result } = renderAutocomplete("/");

    expect(result.current.autocompleteMatches.map((item) => item.label)).toEqual([
      "compact",
      "reload",
    ]);
  });

  it("includes skills in slash completion using Pi skill commands", () => {
    const { result } = renderAutocomplete("/", {
      skills: [
        { name: "review", description: "Review changes" },
        { name: "release", description: "Prepare a release" },
      ],
    });

    const skillMatches = result.current.autocompleteMatches.filter(
      (item) => item.group === "Skills",
    );
    expect(skillMatches).toEqual([
      expect.objectContaining({
        id: "slash-skill:review",
        label: "skill:review",
        insertText: "skill:review",
      }),
      expect.objectContaining({
        id: "slash-skill:release",
        label: "skill:release",
        insertText: "skill:release",
      }),
    ]);
  });

  it("filters slash skills by the skill command prefix", () => {
    const { result } = renderAutocomplete("/skill:rel", {
      skills: [
        { name: "review", description: "Review changes" },
        { name: "release", description: "Prepare a release" },
      ],
    });

    expect(result.current.autocompleteMatches.map((item) => item.label)).toEqual([
      "skill:release",
    ]);
  });

  it("keeps dollar-triggered skill completions", () => {
    const { result } = renderAutocomplete("$", {
      skills: [
        { name: "skill-a", description: "Skill A" },
        { name: "skill-b", description: "Skill B" },
      ],
    });

    expect(result.current.autocompleteMatches.map((item) => item.id)).toEqual([
      "skill:skill-a",
      "skill:skill-b",
    ]);
    expect(result.current.autocompleteMatches.map((item) => item.group)).toEqual([
      "Skills",
      "Skills",
    ]);
  });
});
