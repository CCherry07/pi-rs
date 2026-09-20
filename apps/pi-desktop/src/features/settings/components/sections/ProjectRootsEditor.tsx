import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "react-i18next";
import Check from "lucide-react/dist/esm/icons/check";
import Folder from "lucide-react/dist/esm/icons/folder";
import Plus from "lucide-react/dist/esm/icons/plus";
import X from "lucide-react/dist/esm/icons/x";
import { getWorkspaceProject, updateWorkspaceProject } from "@/services/tauri";
import type { ProjectDefinition, WorkspaceInfo } from "@/types";

export function ProjectRootsEditor({ workspace }: { workspace: WorkspaceInfo }) {
  const { t } = useTranslation("settings");
  const [project, setProject] = useState<ProjectDefinition | null>(workspace.project ?? null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    let active = true;
    void getWorkspaceProject(workspace.id).then(value => {
      if (active) setProject(value);
    }).catch(reason => { if (active) setError(String(reason)); });
    return () => { active = false; };
  }, [workspace.id, workspace.project]);

  async function save(next: ProjectDefinition) {
    setSaving(true);
    setError(null);
    try { setProject(await updateWorkspaceProject(next)); }
    catch (reason) { setError(String(reason)); }
    finally { setSaving(false); }
  }

  async function addRoot() {
    if (!project) return;
    const path = await open({ directory: true, multiple: false });
    if (!path || Array.isArray(path) || project.roots.some(root => root.path === path)) return;
    await save({ ...project, roots: [...project.roots, {
      id: crypto.randomUUID(), name: path.split(/[\\/]/).filter(Boolean).slice(-1)[0] ?? path,
      path, ownership: { kind: "external" },
    }] });
  }

  return (
    <div className="settings-project-roots" aria-busy={saving}>
      <div className="settings-project-root-list">
        {project?.roots.map((root) => {
          const isPrimary = project.primaryRoot === root.id;
          return (
            <div className={`settings-project-root${isPrimary ? " is-primary" : ""}`} key={root.id}>
              <label className="settings-project-root-choice">
                <input
                  className="settings-project-root-radio"
                  type="radio"
                  name={`primary-${workspace.id}`}
                  checked={isPrimary}
                  disabled={saving || project.roots.length === 1}
                  aria-label={t("projects.roots.selectPrimary", { name: root.name })}
                  onChange={() => void save({ ...project, primaryRoot: root.id })}
                />
                <Folder className="settings-project-root-icon" aria-hidden />
                <span className="settings-project-root-copy">
                  <span className="settings-project-root-heading">
                    <span className="settings-project-root-name" title={root.name}>{root.name}</span>
                    {isPrimary && (
                      <span className="settings-project-root-badge">
                        <Check aria-hidden />
                        {t("projects.roots.primary")}
                      </span>
                    )}
                  </span>
                  <span className="settings-project-root-path" title={root.path}>{root.path}</span>
                </span>
                {!isPrimary && (
                  <span className="settings-project-root-action" aria-hidden>
                    {t("projects.roots.makePrimary")}
                  </span>
                )}
              </label>
              {project.roots.length > 1 && (
                <button
                  type="button"
                  className="settings-project-root-remove"
                  disabled={saving}
                  title={t("projects.roots.remove")}
                  aria-label={t("projects.roots.removeNamed", { name: root.name })}
                  onClick={() => {
                    const roots = project.roots.filter((candidate) => candidate.id !== root.id);
                    void save({ ...project, roots, primaryRoot: isPrimary ? roots[0].id : project.primaryRoot });
                  }}
                >
                  <X aria-hidden />
                </button>
              )}
            </div>
          );
        })}
      </div>
      <div className="settings-project-roots-footer">
        <button
          type="button"
          className="settings-project-add-root"
          disabled={saving || !project}
          onClick={() => void addRoot()}
        >
          <Plus aria-hidden />
          {t("projects.roots.add")}
        </button>
        {error && <div role="alert" className="settings-group-error">{error}</div>}
      </div>
    </div>
  );
}
