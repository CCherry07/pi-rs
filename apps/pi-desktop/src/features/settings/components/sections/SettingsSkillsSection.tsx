import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import ReactMarkdown from 'react-markdown'
import BookOpen from 'lucide-react/dist/esm/icons/book-open'
import ChevronDown from 'lucide-react/dist/esm/icons/chevron-down'
import Copy from 'lucide-react/dist/esm/icons/copy'
import Download from 'lucide-react/dist/esm/icons/download'
import Trash2 from 'lucide-react/dist/esm/icons/trash-2'
import Plus from 'lucide-react/dist/esm/icons/plus'
import RefreshCw from 'lucide-react/dist/esm/icons/refresh-cw'
import ArrowLeft from 'lucide-react/dist/esm/icons/arrow-left'
import FolderInput from 'lucide-react/dist/esm/icons/folder-input'
import FileInput from 'lucide-react/dist/esm/icons/file-input'
import { ask, open } from '@tauri-apps/plugin-dialog'
import type { WorkspaceInfo } from '@/types'
import { useMenuController } from '@/features/app/hooks/useMenuController'
import { MenuTrigger, PopoverMenuItem, PopoverSurface } from '@/features/design-system/components/popover/PopoverPrimitives'
import { SettingsSection } from '@/features/design-system/components/settings/SettingsPrimitives'
import { listSkillLibrary, skillLibraryOperation, type ManagedSkill, type SkillDocument, type SkillLibrary } from '@/services/skills'

type Props = { projects: WorkspaceInfo[]; onDirtyChange: (dirty: boolean) => void }
const EMPTY_DOCUMENT = '---\nname: my-skill\ndescription: \n---\n\n'

function previewBody(source: string) {
  const frontmatter = /^---\r?\n[\s\S]*?\r?\n---(?:\r?\n|$)/.exec(source)
  return frontmatter ? source.slice(frontmatter[0].length).trimStart() : source
}

export function SettingsSkillsSection({ projects, onDirtyChange }: Props) {
  const { t } = useTranslation('settings')
  const [workspaceId, setWorkspaceId] = useState<string | null>(null)
  const [library, setLibrary] = useState<SkillLibrary | null>(null)
  const [document, setDocument] = useState<SkillDocument | null>(null)
  const [editing, setEditing] = useState(false)
  const [creating, setCreating] = useState(false)
  const [name, setName] = useState('')
  const [content, setContent] = useState('')
  const [raw, setRaw] = useState(false)
  const [query, setQuery] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const request = useRef(0)
  const importMenu = useMenuController()
  const dirty = creating ? content !== EMPTY_DOCUMENT || name !== '' : editing && content !== document?.content
  useEffect(() => {
    onDirtyChange(dirty)
    return () => onDirtyChange(false)
  }, [dirty, onDirtyChange])

  useEffect(() => {
    let cancelled = false
    const id = ++request.current
    setBusy(true)
    setLibrary(null)
    setError(null)
    void listSkillLibrary(workspaceId)
      .then(value => {
        if (!cancelled && request.current === id) setLibrary(value)
      })
      .catch((reason: unknown) => {
        if (!cancelled && request.current === id) setError(String(reason))
      })
      .finally(() => {
        if (!cancelled && request.current === id) setBusy(false)
      })
    return () => {
      cancelled = true
    }
  }, [workspaceId])

  async function run(action: () => Promise<void>) {
    const id = ++request.current
    setBusy(true)
    setError(null)
    setNotice(null)
    try {
      await action()
    } catch (reason) {
      if (request.current === id) setError(String(reason))
    } finally {
      if (request.current === id) setBusy(false)
    }
  }
  async function refresh() {
    setLibrary(await listSkillLibrary(workspaceId))
  }
  async function discard() {
    return !dirty || (await ask(t('skills.discard'), { title: t('skills.title'), kind: 'warning' }))
  }
  async function back() {
    if (busy || !(await discard())) return
    setDocument(null)
    setCreating(false)
    setEditing(false)
    setError(null)
  }
  async function view(skill: ManagedSkill) {
    await run(async () => {
      const next = await skillLibraryOperation(workspaceId, { kind: 'read', path: skill.path })
      if (!next) throw new Error(t('skills.missingDocument'))
      setDocument(next)
      setContent(next.content)
      setEditing(false)
      setRaw(false)
    })
  }
  async function copyPath(path: string) {
    try {
      await navigator.clipboard.writeText(path)
      setNotice(t('skills.copied'))
    } catch (reason) {
      setError(t('skills.copyFailed', { error: String(reason) }))
    }
  }
  async function save() {
    await run(async () => {
      const next = await skillLibraryOperation(workspaceId, creating ? { kind: 'create', name, content } : { kind: 'save', path: document!.skill.path, revision: document!.revision, content })
      if (!next) throw new Error(t('skills.missingDocument'))
      setDocument(next)
      setContent(next.content)
      setCreating(false)
      setEditing(raw && next.skill.writable)
      setNotice(t('skills.saved'))
      await refresh()
    })
  }
  async function remove(skill: ManagedSkill) {
    await run(async () => {
      // Confirmation and revision refer to exactly the freshly read document.
      const current = await skillLibraryOperation(workspaceId, { kind: 'read', path: skill.path })
      if (!current) throw new Error(t('skills.missingDocument'))
      if (
        !(await ask(t('skills.deleteConfirm', { name: current.skill.name, path: current.skill.deletePath }), {
          title: t('skills.delete'),
          kind: 'warning',
          okLabel: t('skills.trash'),
          cancelLabel: t('skills.cancel'),
        }))
      )
        return
      await skillLibraryOperation(workspaceId, { kind: 'trash', path: skill.path, revision: current.revision })
      setDocument(null)
      setEditing(false)
      setNotice(t('skills.deleted'))
      await refresh()
    })
  }
  async function importSkill(directory: boolean) {
    importMenu.close()
    await run(async () => {
      const source = await open({ directory, multiple: false, title: t('skills.import'), ...(!directory ? { filters: [{ name: 'Markdown', extensions: ['md'] }] } : {}) })
      if (typeof source !== 'string') return
      const next = await skillLibraryOperation(workspaceId, { kind: 'import', source })
      setDocument(next)
      setContent(next?.content ?? '')
      setEditing(false)
      setRaw(false)
      setNotice(t('skills.saved'))
      await refresh()
    })
  }
  function showSource() {
    setRaw(true)
    if (creating || document?.skill.writable) setEditing(true)
  }
  function discardDraft() {
    setError(null)
    if (creating) {
      setCreating(false)
      setDocument(null)
      setEditing(false)
      return
    }
    if (document) setContent(document.content)
  }
  const rows = (library?.skills ?? []).filter(skill => `${skill.name} ${skill.description} ${skill.path}`.toLowerCase().includes(query.toLowerCase()))
  const detail = document !== null || creating

  return (
    <SettingsSection title={t('skills.title')} subtitle={t('skills.subtitle')}>
      {notice && (
        <div className="settings-help" role="status">
          {notice}
        </div>
      )}
      {error && (
        <div className="settings-group-error" role="alert">
          {error}
        </div>
      )}
      {!detail ? (
        <>
          <div className="settings-field">
            <label className="settings-field-label" htmlFor="skills-scope">
              {t('skills.scope')}
            </label>
            <select
              id="skills-scope"
              className="settings-select"
              value={workspaceId ?? ''}
              disabled={busy}
              onChange={event => {
                setWorkspaceId(event.target.value || null)
                setNotice(null)
              }}
            >
              <option value="">{t('skills.global')}</option>
              {projects.map(project => (
                <option key={project.id} value={project.id}>
                  {project.name}
                </option>
              ))}
            </select>
            {workspaceId && <div className="settings-help">{t('skills.projectView')}</div>}
            {workspaceId && library && !library.projectTrusted && <div className="settings-group-error">{t('skills.untrusted')}</div>}
            {library?.destination && <div className="settings-help settings-skills-path">{t('skills.destination', { path: library.destination })}</div>}
            <div className="settings-field-actions settings-skills-library-actions">
              <button
                className="primary settings-button-compact settings-skills-action-button"
                disabled={busy || !library?.destination}
                onClick={() => {
                  importMenu.close()
                  setCreating(true)
                  setEditing(true)
                  setContent(EMPTY_DOCUMENT)
                  setName('')
                  setRaw(true)
                  setError(null)
                }}
              >
                <Plus aria-hidden />
                {t('skills.add')}
              </button>
              <div className="settings-skills-import-menu" ref={importMenu.containerRef}>
                <MenuTrigger
                  isOpen={importMenu.isOpen}
                  activeClassName="is-active"
                  className="ghost settings-button-compact settings-skills-action-button settings-skills-import-trigger"
                  disabled={busy || !library?.destination}
                  onClick={importMenu.toggle}
                >
                  <Download aria-hidden />
                  {t('skills.import')}
                  <ChevronDown className="settings-skills-import-chevron" aria-hidden />
                </MenuTrigger>
                {importMenu.isOpen && (
                  <PopoverSurface className="settings-skills-import-popover" role="menu">
                    <PopoverMenuItem role="menuitem" icon={<FolderInput />} onClick={() => void importSkill(true)}>
                      {t('skills.importDirectory')}
                    </PopoverMenuItem>
                    <PopoverMenuItem role="menuitem" icon={<FileInput />} onClick={() => void importSkill(false)}>
                      {t('skills.importFile')}
                    </PopoverMenuItem>
                  </PopoverSurface>
                )}
              </div>
            </div>
            <div className="settings-skills-search-row">
              <input className="settings-input" aria-label={t('skills.search')} placeholder={t('skills.search')} value={query} onChange={event => setQuery(event.target.value)} />
              <button
                className="ghost icon-button settings-skills-icon-button"
                type="button"
                aria-label={t('skills.refresh')}
                title={t('skills.refresh')}
                disabled={busy}
                onClick={() => void run(refresh)}
              >
                <RefreshCw aria-hidden />
              </button>
            </div>
          </div>
          {busy && (
            <div className="settings-help" role="status">
              {t('skills.loading')}
            </div>
          )}
          {!busy && rows.length === 0 && <div className="settings-empty">{t('skills.empty')}</div>}
          <div className="settings-archived-list">
            {rows.map(skill => (
              <div className="settings-archived-row settings-skills-row" key={skill.path}>
                <div className="settings-archived-info">
                  <button className="settings-skills-name" disabled={busy} onClick={() => void view(skill)}>
                    <BookOpen aria-hidden />
                    {skill.name}
                  </button>
                  <div className="settings-help">{skill.description}</div>
                  <div className="settings-archived-path" title={skill.path}>
                    {skill.path}
                  </div>
                  {!skill.modelVisible && !skill.diagnostic && <div className="settings-help">{t('skills.manual')}</div>}
                  {!skill.writable && <div className="settings-help">{t('skills.readOnly')}</div>}
                  {skill.diagnostic && <div className="settings-group-error">{skill.diagnostic}</div>}
                </div>
                <div className="settings-archived-actions">
                  <button className="ghost icon-button settings-skills-icon-button" aria-label={t('skills.copy')} title={t('skills.copy')} onClick={() => void copyPath(skill.path)}>
                    <Copy aria-hidden />
                  </button>
                  <button
                    className="ghost danger icon-button settings-skills-icon-button"
                    aria-label={t('skills.delete')}
                    title={t('skills.delete')}
                    disabled={busy || !skill.writable}
                    onClick={() => void remove(skill)}
                  >
                    <Trash2 aria-hidden />
                  </button>
                </div>
              </div>
            ))}
          </div>
        </>
      ) : (
        <div className="settings-skills-detail">
          <div className="settings-skills-toolbar">
            <button className="ghost settings-button-compact settings-skills-action-button" disabled={busy} onClick={() => void back()}>
              <ArrowLeft aria-hidden />
              {t('skills.back')}
            </button>
          </div>
          <div className="settings-skills-detail-heading">
            <div className="settings-skills-detail-title">{creating ? t('skills.add') : document?.skill.name}</div>
            <div className="settings-help settings-skills-path">{creating ? library?.destination : document?.skill.path}</div>
          </div>
          {document?.skill.diagnostic && <div className="settings-group-error">{document.skill.diagnostic}</div>}
          {creating && (
            <div className="settings-field settings-skills-create-field">
              <label className="settings-field-label" htmlFor="skill-directory-name">
                {t('skills.directoryName')}
              </label>
              <input id="skill-directory-name" className="settings-input" value={name} disabled={busy} onChange={event => setName(event.target.value)} placeholder="my-skill" />
              <div className="settings-help">{t('skills.nameHelp')}</div>
            </div>
          )}
          <div className="settings-skills-view-toggle">
            <button className={`ghost settings-button-compact${!raw ? ' is-active' : ''}`} aria-pressed={!raw} onClick={() => setRaw(false)}>
              {t('skills.preview')}
            </button>
            <button className={`ghost settings-button-compact${raw ? ' is-active' : ''}`} aria-pressed={raw} onClick={showSource}>
              {t('skills.source')}
            </button>
          </div>
          {raw ? (
            editing ? (
              <textarea
                className="settings-agents-textarea settings-skills-editor"
                aria-label={t('skills.content')}
                value={content}
                onChange={event => setContent(event.target.value)}
                disabled={busy}
                spellCheck={false}
              />
            ) : (
              <pre className="settings-skills-preview settings-skills-source">{document?.content}</pre>
            )
          ) : (
            <div className="settings-skills-preview settings-skills-markdown">
              <ReactMarkdown skipHtml disallowedElements={['img']}>
                {previewBody(editing ? content : (document?.content ?? ''))}
              </ReactMarkdown>
            </div>
          )}
          {editing && (creating || dirty) && (
            <div className="settings-field-actions settings-skills-save-actions">
              <button className="ghost settings-button-compact" disabled={busy} onClick={discardDraft}>
                {creating ? t('skills.cancel') : t('skills.discardChanges')}
              </button>
              <button className="primary settings-button-compact" disabled={busy || (creating ? !name.trim() || !content.trim() : !dirty)} onClick={() => void save()}>
                {busy ? t('skills.saving') : t('skills.save')}
              </button>
            </div>
          )}
        </div>
      )}
    </SettingsSection>
  )
}
