// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ask, open } from "@tauri-apps/plugin-dialog";
import { listSkillLibrary, skillLibraryOperation, type SkillDocument } from "@/services/skills";
import { SettingsSkillsSection } from "./SettingsSkillsSection";

vi.mock("@/services/skills", () => ({ listSkillLibrary: vi.fn(), skillLibraryOperation: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ ask: vi.fn(), open: vi.fn() }));
const doc: SkillDocument = {
  skill: { name: "code-review", description: "Review changes", path: "/agent/skills/code-review/SKILL.md", deletePath: "/agent/skills/code-review", diagnostic: null, modelVisible: true, writable: true },
  content: "---\nname: code-review\ndescription: Review changes\ncustom: retained\n---\n# Instructions\nReview the code.",
  revision: "revision-one",
};
const list = { skills: [doc.skill], destination: "/agent/skills", projectTrusted: false };
const copy = vi.fn();

beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(listSkillLibrary).mockResolvedValue(list);
  vi.mocked(skillLibraryOperation).mockResolvedValue(doc);
  vi.mocked(ask).mockResolvedValue(true);
  Object.defineProperty(navigator, "clipboard", { value: { writeText: copy.mockResolvedValue(undefined) }, configurable: true });
});
afterEach(cleanup);

function mount() {
  const onDirtyChange = vi.fn();
  render(<SettingsSkillsSection projects={[]} onDirtyChange={onDirtyChange} />);
  return onDirtyChange;
}

describe("SettingsSkillsSection", () => {
  it("lists real paths, copies the absolute document path, and previews without executing", async () => {
    mount();
    await screen.findByText("code-review");
    fireEvent.click(screen.getByRole("button", { name: "Copy skill path" }));
    await screen.findByText("Skill path copied");
    expect(copy).toHaveBeenCalledWith(doc.skill.path);
    fireEvent.click(screen.getByRole("button", { name: "code-review" }));
    await screen.findByRole("heading", { name: "Instructions" });
    expect(skillLibraryOperation).toHaveBeenCalledExactlyOnceWith(null, { kind: "read", path: doc.skill.path });
  });

  it("uses a compact detail hierarchy and keeps frontmatter out of the rendered preview", async () => {
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "code-review" }));
    await screen.findByRole("heading", { name: "Instructions" });

    const title = screen.getByText("code-review");
    expect(title.parentElement?.classList.contains("settings-skills-detail-heading")).toBe(true);
    const preview = screen.getByRole("button", { name: "Preview" });
    expect(preview.parentElement?.classList.contains("settings-skills-view-toggle")).toBe(true);
    expect(preview.getAttribute("aria-pressed")).toBe("true");
    expect(document.body.textContent).not.toContain("custom: retained");

    fireEvent.click(screen.getByRole("button", { name: "Source" }));
    expect(screen.getByRole("textbox", { name: "SKILL.md content" })).toHaveProperty("value", doc.content);
  });

  it("keeps primary actions at the page level and removes redundant detail actions", async () => {
    mount();
    const add = await screen.findByRole("button", { name: "New skill" });
    const libraryActions = add.closest(".settings-skills-library-actions");
    expect(libraryActions?.classList.contains("settings-skills-library-actions")).toBe(true);
    expect(libraryActions?.querySelectorAll("button")).toHaveLength(2);
    for (const button of libraryActions?.querySelectorAll("button") ?? []) {
      expect(button.querySelector("svg")).not.toBeNull();
      expect(button.classList.contains("settings-skills-action-button")).toBe(true);
    }
    const refresh = screen.getByRole("button", { name: "Refresh" });
    expect(refresh.parentElement?.classList.contains("settings-skills-search-row")).toBe(true);
    expect(refresh.classList.contains("settings-skills-icon-button")).toBe(true);

    const skillName = screen.getByRole("button", { name: "code-review" });
    expect(skillName.classList.contains("ghost")).toBe(false);
    expect(screen.queryByRole("button", { name: "Edit" })).toBeNull();
    for (const name of ["Copy skill path", "Delete skill"]) {
      expect(screen.getByRole("button", { name }).classList.contains("settings-skills-icon-button")).toBe(true);
    }

    fireEvent.click(skillName);
    const back = await screen.findByRole("button", { name: "Back to list" });
    const detailToolbar = back.closest(".settings-skills-toolbar");
    expect(detailToolbar?.querySelectorAll("button")).toHaveLength(1);
    for (const button of detailToolbar?.querySelectorAll("button") ?? []) {
      expect(button.classList.contains("settings-skills-action-button")).toBe(true);
    }
    expect(screen.queryByRole("button", { name: "Copy skill path" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Edit" })).toBeNull();
  });

  it("edits directly in Source and only offers save or discard after the source changes", async () => {
    const onDirtyChange = mount();
    await screen.findByText("code-review");
    fireEvent.click(screen.getByRole("button", { name: "code-review" }));
    fireEvent.click(await screen.findByRole("button", { name: "Source" }));
    const textarea = await screen.findByRole("textbox", { name: "SKILL.md content" });
    expect((textarea as HTMLTextAreaElement).value).toBe(doc.content);
    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Discard changes" })).toBeNull();

    const changed = doc.content + "\nUpdated";
    fireEvent.change(textarea, { target: { value: changed } });
    expect(onDirtyChange).toHaveBeenLastCalledWith(true);
    expect(screen.getByRole("button", { name: "Save" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Discard changes" })).toBeTruthy();
    vi.mocked(skillLibraryOperation).mockRejectedValueOnce("Skill changed on disk");
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await screen.findByRole("alert");
    expect(skillLibraryOperation).toHaveBeenLastCalledWith(null, { kind: "save", path: doc.skill.path, revision: doc.revision, content: changed });
    expect((textarea as HTMLTextAreaElement).value).toBe(changed);
    fireEvent.click(screen.getByRole("button", { name: "Discard changes" }));
    expect((screen.getByRole("textbox", { name: "SKILL.md content" }) as HTMLTextAreaElement).value).toBe(doc.content);
    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
    expect(ask).not.toHaveBeenCalled();
  });

  it("creates a new skill and passes full raw content", async () => {
    mount();
    await screen.findByText("code-review");
    fireEvent.click(screen.getByRole("button", { name: "New skill" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Directory name" }), { target: { value: "code-review" } });
    fireEvent.change(screen.getByRole("textbox", { name: "SKILL.md content" }), { target: { value: doc.content } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await screen.findByText("Skill saved; library updated.");
    expect(skillLibraryOperation).toHaveBeenCalledWith(null, { kind: "create", name: "code-review", content: doc.content });
  });

  it("requires deletion confirmation and preserves entries when trash fails", async () => {
    mount();
    await screen.findByText("code-review");
    vi.mocked(ask).mockResolvedValueOnce(false);
    fireEvent.click(screen.getByRole("button", { name: "Delete skill" }));
    await waitFor(() => expect(ask).toHaveBeenCalled());
    expect(skillLibraryOperation).toHaveBeenCalledTimes(1);
    await waitFor(() => expect((screen.getByRole("button", { name: "Delete skill" }) as HTMLButtonElement).disabled).toBe(false));
    vi.mocked(skillLibraryOperation).mockResolvedValueOnce(doc).mockRejectedValueOnce("Trash unavailable");
    fireEvent.click(screen.getByRole("button", { name: "Delete skill" }));
    await screen.findByText("Trash unavailable");
    expect(screen.getByText("code-review")).toBeTruthy();
    expect(skillLibraryOperation).toHaveBeenLastCalledWith(null, { kind: "trash", path: doc.skill.path, revision: doc.revision });
    expect(vi.mocked(ask).mock.calls[1][0]).toContain(doc.skill.deletePath);
  });

  it("imports a local directory and handles clipboard errors", async () => {
    mount();
    await screen.findByText("code-review");
    copy.mockRejectedValueOnce(new Error("Denied"));
    fireEvent.click(screen.getByRole("button", { name: "Copy skill path" }));
    await screen.findByRole("alert");
    vi.mocked(open).mockResolvedValueOnce("/source/skill");
    fireEvent.click(screen.getByRole("button", { name: "Import" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Import folder" }));
    await screen.findByText("Skill saved; library updated.");
    expect(skillLibraryOperation).toHaveBeenCalledWith(null, { kind: "import", source: "/source/skill" });
  });

  it("shows catalog errors and read-only skills without mutation controls", async () => {
    const readOnlyDocument = { ...doc, skill: { ...doc.skill, writable: false, diagnostic: "Invalid frontmatter" } };
    vi.mocked(listSkillLibrary).mockResolvedValueOnce({ ...list, skills: [readOnlyDocument.skill] });
    vi.mocked(skillLibraryOperation).mockResolvedValueOnce(readOnlyDocument);
    mount();
    await screen.findByText("Invalid frontmatter");
    expect(screen.queryByRole("button", { name: "Edit" })).toBeNull();
    expect((screen.getByRole("button", { name: "Delete skill" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Copy skill path" }) as HTMLButtonElement).disabled).toBe(false);

    fireEvent.click(screen.getByRole("button", { name: "code-review" }));
    fireEvent.click(await screen.findByRole("button", { name: "Source" }));
    expect(screen.queryByRole("textbox", { name: "SKILL.md content" })).toBeNull();
    expect(document.querySelector(".settings-skills-source")?.textContent).toBe(doc.content);
    fireEvent.click(screen.getByRole("button", { name: "Back to list" }));
    await screen.findByRole("button", { name: "Refresh" });

    vi.mocked(listSkillLibrary).mockRejectedValueOnce("Cannot load library");
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await screen.findByRole("alert");
  });
});
