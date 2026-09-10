import { invoke } from "@tauri-apps/api/core";

export type ManagedSkill = {
  path: string;
  name: string;
  description: string;
  diagnostic: string | null;
  modelVisible: boolean;
  writable: boolean;
  deletePath: string;
};
export type SkillDocument = { skill: ManagedSkill; content: string; revision: string };
export type SkillLibrary = { skills: ManagedSkill[]; destination: string | null; projectTrusted: boolean };
export type SkillOperation =
  | { kind: "read"; path: string }
  | { kind: "save"; path: string; revision: string; content: string }
  | { kind: "create"; name: string; content: string }
  | { kind: "import"; source: string }
  | { kind: "trash"; path: string; revision: string };

export function listSkillLibrary(workspaceId: string | null): Promise<SkillLibrary> {
  return invoke("pi_skill_library_list", { workspaceId });
}
export function skillLibraryOperation(workspaceId: string | null, operation: SkillOperation): Promise<SkillDocument | null> {
  return invoke("pi_skill_library_operation", { workspaceId, operation });
}
