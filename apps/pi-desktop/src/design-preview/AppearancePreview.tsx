import { useEffect, useRef, useState, type CSSProperties } from "react";
import { ArrowLeft, Check, FileCode2, Folder, GitBranch, Moon, PanelLeft, PanelRight, Plus, SlidersHorizontal, Sun, X } from "lucide-react";
import { I18nextProvider } from "react-i18next";
import { i18n } from "../i18n";
import type { ConversationItem, ThemePreference, ThreadListOrganizeMode, ThreadListSortKey } from "../types";
import { MainHeader } from "../features/app/components/MainHeader";
import { PiBrand } from "../features/app/components/PiBrand";
import { SidebarHeader } from "../features/app/components/SidebarHeader";
import { SidebarSearchBar } from "../features/app/components/SidebarSearchBar";
import { SidebarBottomRail } from "../features/app/components/SidebarBottomRail";
import { WorkspaceCard } from "../features/app/components/WorkspaceCard";
import { ThreadRow } from "../features/app/components/ThreadRow";
import { DesktopLayout } from "../features/layout/components/DesktopLayout";
import { PanelTabs, type PanelTabId } from "../features/layout/components/PanelTabs";
import { Messages } from "../features/messages/components/Messages";
import { DiffRow } from "../features/messages/components/MessageRows";
import { Markdown } from "../features/messages/components/Markdown";
import { Composer } from "../features/composer/components/Composer";
import { HomeLatestAgentsSection } from "../features/home/components/HomeLatestAgentsSection";
import { ModalShell } from "../features/design-system/components/modal/ModalShell";
import { PanelFrame, PanelHeader, PanelMeta } from "../features/design-system/components/panel/PanelPrimitives";
import { changedFiles, conversation, inventoryFor, projects, sampleDiff, threads } from "./fixtures";
import "../styles/app.css";
import "./preview.css";

const noop = () => undefined;
const unpinned = () => false;
const themes: { id: ThemePreference; name: string; description: string }[] = [
  { id: "light", name: "清透白", description: "清爽、通透，适合白天工作" },
  { id: "dark", name: "石墨黑", description: "柔和、安静，适合沉浸阅读" },
  { id: "dim", name: "柔和深色", description: "沿用深色语义，降低表面对比" },
  { id: "system", name: "跟随系统", description: "随系统外观自动切换" },
];

function Preview() {
  const queryTheme = new URLSearchParams(window.location.search).get("theme");
  const [theme, setTheme] = useState<ThemePreference>(themes.find((item) => item.id === queryTheme)?.id ?? "light");
  const [screen, setScreen] = useState<"chat" | "home">("chat");
  const [activeThread, setActiveThread] = useState("search");
  const [activeProject, setActiveProject] = useState("pi");
  const [searchOpen, setSearchOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<ThreadListSortKey>("updated_at");
  const [organize, setOrganize] = useState<ThreadListOrganizeMode>("by_project");
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});
  const [sidebarHidden, setSidebarHidden] = useState(false);
  const [panelHidden, setPanelHidden] = useState(false);
  const [panel, setPanel] = useState<PanelTabId>("git");
  const [centerMode, setCenterMode] = useState<"chat" | "diff">("chat");
  const [selectedFile, setSelectedFile] = useState(changedFiles[0]);
  const [draft, setDraft] = useState("");
  const [extraMessages, setExtraMessages] = useState<ConversationItem[]>([]);
  const [branch, setBranch] = useState("feat/thread-search");
  const [model, setModel] = useState("gpt-6-astra");
  const [effort, setEffort] = useState("high");
  const [running, setRunning] = useState(false);
  const [settings, setSettings] = useState(false);
  const [reducedTransparency, setReducedTransparency] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const settingsCloseRef = useRef<HTMLButtonElement | null>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (theme === "system") document.documentElement.removeAttribute("data-theme");
    else document.documentElement.dataset.theme = theme;
    const url = new URL(window.location.href);
    url.searchParams.set("theme", theme);
    window.history.replaceState(null, "", url);
  }, [theme]);

  useEffect(() => {
    if (!settings) return;
    previousFocusRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    settingsCloseRef.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setSettings(false);
      if (event.key !== "Tab") return;
      const card = settingsCloseRef.current?.closest(".settings-window");
      const focusable = card?.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled), select:not(:disabled)");
      if (!focusable?.length) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => { document.removeEventListener("keydown", onKeyDown); previousFocusRef.current?.focus(); };
  }, [settings]);

  function selectThread(projectId: string, threadId: string) {
    setActiveProject(projectId);
    setActiveThread(threadId);
    setExtraMessages([]);
    setCenterMode("chat");
    setScreen("chat");
  }
  const currentProject = projects.find((item) => item.id === activeProject) ?? projects[0];
  const currentThread = threads[activeProject]?.find((item) => item.id === activeThread);
  const sampleMessages = activeThread === "search" ? conversation : [
    { id: "sample-user", kind: "message", role: "user", text: currentThread?.name ?? "开始一个新任务" },
    { id: "sample-assistant", kind: "message", role: "assistant", text: "我们可以从这里开始。\n\n这是一个使用真实桌面端组件的样式预览，你可以试试切换主题、展开工具记录、搜索会话和输入消息。" },
  ] satisfies ConversationItem[];
  const composer = <Composer
    onSend={(text) => {
      if (!text.trim()) return;
      setExtraMessages((previous) => [...previous,
        { id: `preview-user-${previous.length}`, kind: "message", role: "user", text },
        { id: `preview-reply-${previous.length}`, kind: "message", role: "assistant", text: "已收到。这条回复用于预览排版；此页面不会调用模型或修改项目。" },
      ]);
      setDraft(""); setScreen("chat"); setCenterMode("chat");
    }}
    onStop={() => setRunning(false)} canStop={running} isProcessing={running}
    steerAvailable={false} followUpMessageBehavior="queue"
    models={[
      { id: "gpt-6-astra", model: "gpt-6-astra", displayName: "GPT-6 Astra" },
      { id: "claude-sonnet-4-6", model: "claude-sonnet-4-6", displayName: "Claude Sonnet 4.6" },
    ]}
    selectedModelId={model} onSelectModel={setModel}
    reasoningOptions={["low", "medium", "high"]} selectedEffort={effort} onSelectEffort={setEffort}
    reasoningSupported
    skills={[{ name: "code-review", description: "检查代码变更与潜在问题" }]}
    prompts={[]} files={[]} draftText={draft} onDraftChange={setDraft}
    textareaRef={textareaRef} dictationEnabled={false} sendLabel="发送"
    tokenUsage={{ totalTokens: 12840, contextTokens: 12840, modelContextWindow: 200000 }}
  />;

  const visibleProjects = projects.filter((project) => project.name.toLowerCase().includes(query.toLowerCase()) || threads[project.id].some((thread) => thread.name.toLowerCase().includes(query.toLowerCase())));
  const sidebar = <aside className={`sidebar${searchOpen ? " search-open" : ""}`}>
    <PiBrand />
    <SidebarHeader onSelectHome={() => setScreen("home")} onAddWorkspace={() => setScreen("home")}
      onToggleSearch={() => setSearchOpen((open) => !open)} isSearchOpen={searchOpen}
      threadListSortKey={sort} onSetThreadListSortKey={setSort}
      threadListOrganizeMode={organize} onSetThreadListOrganizeMode={setOrganize}
      onRefreshAllThreads={() => { setQuery(""); setExtraMessages([]); }} />
    <SidebarSearchBar isSearchOpen={searchOpen} searchQuery={query} onSearchQueryChange={setQuery} onClearSearch={() => setQuery("")} />
    <div className="preview-projects">
      <div className="workspace-list">
        {visibleProjects.map((project) => {
          const rows = [...threads[project.id]].filter((thread) => project.name.toLowerCase().includes(query.toLowerCase()) || thread.name.toLowerCase().includes(query.toLowerCase())).sort((a, b) => sort === "updated_at" ? b.updatedAt - a.updatedAt : a.updatedAt - b.updatedAt);
          const content = <div className="thread-list">{rows.map((thread) => <ThreadRow
            key={thread.id} thread={thread} depth={0} workspaceId={project.id} indentUnit={14}
            activeWorkspaceId={screen === "chat" ? activeProject : null} activeThreadId={activeThread}
            threadStatusById={{ theme: { hasUnread: true }, search: { isProcessing: running } }}
            getThreadTime={(item) => item.id === "search" ? "刚刚" : item.updatedAt === 2 ? "2 小时" : "昨天"}
            isThreadPinned={unpinned} onSelectThread={selectThread} onShowThreadMenu={(event) => event.preventDefault()}
          />)}</div>;
          if (organize === "threads_only") return <div key={project.id}>{content}</div>;
          return <WorkspaceCard key={project.id} workspace={project} summary={`${rows.length} 个会话`}
            isActive={activeProject === project.id} isCollapsed={Boolean(collapsed[project.id])}
            actions={[{ id: "new", label: "新建任务", onSelect: () => { setActiveProject(project.id); setScreen("home"); } }]}
            onSelectWorkspace={(id) => { setActiveProject(id); setScreen("home"); }}
            onShowWorkspaceMenu={(event) => event.preventDefault()}
            onToggleWorkspaceCollapse={(id, value) => setCollapsed((previous) => ({ ...previous, [id]: value }))}
          >{content}</WorkspaceCard>;
        })}
        {!visibleProjects.length && <p className="empty">没有匹配的会话</p>}
      </div>
    </div>
    <SidebarBottomRail onOpenSettings={() => setSettings(true)} onOpenDebug={noop} showDebugButton={false} />
  </aside>;

  const rightPanel = <PanelFrame className="diff-panel">
    <PanelHeader className="git-panel-header">
      <PanelTabs active={panel} onSelect={setPanel} />
      <span className="preview-panel-label">{panel === "git" ? "变更" : panel === "files" ? "文件" : "提示词"}</span>
    </PanelHeader>
    <PanelMeta><GitBranch size={13} aria-hidden /><span>{branch}</span><span className="preview-clean-dot" /></PanelMeta>
    <div className="diff-section">
      <div className="diff-section-title diff-section-title--row"><span className="diff-section-label">{panel === "prompts" ? "项目提示词" : "工作区"}</span><span className="diff-section-count">{panel === "prompts" ? 1 : 3}</span></div>
      {panel === "prompts" ? <button type="button" className="preview-file" onClick={() => setDraft("/skill:code-review ")}><FileCode2 size={15} /><span className="preview-file-name">Code review</span><Plus size={13} /></button> : changedFiles.map((file) => <button
        type="button" key={file.name} className={`preview-file${centerMode === "diff" && file.name === selectedFile.name ? " is-active" : ""}`}
        onClick={() => { setSelectedFile(file); setCenterMode("diff"); setScreen("chat"); }}>
        {panel === "files" ? <Folder size={15} aria-hidden /> : <FileCode2 size={15} aria-hidden />}
        <span className="preview-file-name">{file.name}<span>{file.path}</span></span>
        {panel === "git" && <span className="preview-file-stat"><span>+{file.added}</span><span>−{file.removed}</span></span>}
      </button>)}
    </div>
    {panel === "git" && <div className="preview-change-summary"><span><Check size={13} />3 个文件</span><span><b>+54</b> <em>−14</em></span></div>}
  </PanelFrame>;

  const home = <div className="workspace-home">
    <div className="workspace-home-hero"><div><div className="preview-eyebrow">一个想法，从这里开始</div><h1 className="workspace-home-title">今天想一起做点什么？</h1><div className="workspace-home-path"><Folder size={13} /> {currentProject.name} <span>·</span> 本地工作区</div></div></div>
    <div className="workspace-home-composer">{composer}</div>
    <div className="preview-starters">
      {["了解这个项目", "检查最近的变更", "一起实现一个想法"].map((label) => <button type="button" key={label} className="ghost" onClick={() => { setDraft(label); textareaRef.current?.focus(); }}><Plus size={14} />{label}</button>)}
    </div>
    <HomeLatestAgentsSection isLoadingLatestAgents={false} onSelectThread={selectThread}
      latestAgentRuns={threads.pi.map((thread, index) => ({ message: thread.name, timestamp: Date.now() - (index + 1) * 3600000, projectName: "pi-rs", workspaceId: "pi", threadId: thread.id, isProcessing: false }))} />
  </div>;

  return <div className="appearance-preview">
    <header className="preview-toolbar">
      <div className="preview-toolbar-title"><span className="preview-toolbar-dot" />Pi <span>外观预览</span></div>
      <div className="preview-screens" role="group" aria-label="预览页面">
        <button type="button" className={screen === "chat" ? "is-active" : ""} onClick={() => setScreen("chat")}>会话</button>
        <button type="button" className={screen === "home" ? "is-active" : ""} onClick={() => setScreen("home")}>新任务</button>
        <button type="button" onClick={() => setSettings(true)}>外观规范</button>
      </div>
      <span className="preview-fixture-note">示例内容 · 可交互</span>
      <div className="preview-theme-switch" role="group" aria-label="切换预览主题">
        <button type="button" aria-pressed={theme === "light"} onClick={() => setTheme("light")}><Sun size={14} />清透白</button>
        <button type="button" aria-pressed={theme === "dark"} onClick={() => setTheme("dark")}><Moon size={14} />石墨黑</button>
      </div>
    </header>
    <div className={`app layout-desktop${sidebarHidden ? " sidebar-collapsed" : ""}${panelHidden ? " right-panel-collapsed" : ""}${reducedTransparency ? " reduced-transparency" : ""}`}
      style={{ "--sidebar-width": "248px", "--right-panel-width": "300px", "--sidebar-top-padding": "24px" } as CSSProperties}>
      <DesktopLayout sidebarNode={sidebar} updateToastNode={null} errorToastsNode={null} homeNode={home}
        showHome={screen === "home"} showWorkspace={screen === "chat"}
        topbarLeftNode={<div className="preview-heading">
          <button type="button" className="ghost icon-button preview-chrome-button" aria-label="折叠项目侧栏" onClick={() => setSidebarHidden((hidden) => !hidden)}><PanelLeft size={16} /></button>
          {centerMode === "diff" && <button type="button" className="ghost icon-button preview-chrome-button" aria-label="返回会话" onClick={() => setCenterMode("chat")}><ArrowLeft size={15} /></button>}
          <MainHeader workspace={currentProject} workspaceInventory={inventoryFor(currentProject)} branchName={branch}
            branches={[{ name: "main", lastCommit: 0 }, { name: "feat/thread-search", lastCommit: 0 }]}
            onCheckoutBranch={setBranch} onCreateBranch={setBranch}
            openTargets={[]} showWorkspaceTools={false}
            openAppIconById={{}} selectedOpenAppId="" onSelectOpenAppId={noop}
            onToggleTerminal={noop} isTerminalOpen={false} showTerminalButton={false}
            canCopyThread onCopyThread={() => navigator.clipboard.writeText(sampleMessages.map((item) => item.kind === "message" ? item.text : "").join("\n\n"))}
          />
        </div>}
        topbarActionsNode={<div className="preview-heading-actions">
          <button type="button" className={`ghost preview-run${running ? " is-running" : ""}`} aria-pressed={running} onClick={() => setRunning((value) => !value)}><span />{running ? "运行中" : "已完成"}</button>
          <button type="button" className="ghost icon-button preview-chrome-button" aria-label="折叠文件面板" onClick={() => setPanelHidden((hidden) => !hidden)}><PanelRight size={16} /></button>
        </div>}
        centerMode={centerMode} preloadGitDiffs={false} splitChatDiffView={false}
        messagesNode={<Messages items={[...sampleMessages, ...extraMessages]} threadId={`preview-${activeThread}`} snapshot
          isThinking={running} lastDurationMs={running ? null : 18400} openTargets={[]} selectedOpenAppId="" onQuoteMessage={setDraft} />}
        gitDiffViewerNode={<div className="diff-viewer"><div className="preview-diff-heading"><FileCode2 size={17} /><strong>{selectedFile.name}</strong><span>差异预览</span></div><DiffRow item={{ id: "preview-diff", kind: "diff", title: selectedFile.name, diff: sampleDiff }} /></div>}
        gitDiffPanelNode={rightPanel} composerNode={composer}
        terminalDockNode={null} debugPanelNode={null}
        onSidebarResizeStart={noop} onChatDiffSplitPositionResizeStart={noop} onRightPanelResizeStart={noop}
      />
      {settings && <ModalShell className="settings-overlay" cardClassName="settings-window" ariaLabel="外观规范" onBackdropClick={() => setSettings(false)}>
        <div className="settings-titlebar"><span className="settings-title">外观规范</span><button type="button" ref={settingsCloseRef} className="ghost icon-button" aria-label="关闭外观规范" onClick={() => setSettings(false)}><X size={16} /></button></div>
        <div className="settings-body"><nav className="settings-sidebar"><div className="preview-settings-nav"><SlidersHorizontal size={16} />显示与主题</div><p className="preview-settings-caption">同一套界面，两种工作氛围。</p></nav>
          <div className="settings-content">
            <h2 className="preview-section-title">为专注而设计</h2><p className="preview-description">清透的白、安静的石墨灰，以及点到为止的蓝。</p>
            <div className="preview-theme-cards">{themes.map((item) => <button type="button" key={item.id} className={`preview-theme-card theme-${item.id}${theme === item.id ? " is-active" : ""}`} onClick={() => setTheme(item.id)} aria-pressed={theme === item.id}><span className="preview-theme-mini"><i /><i /><i /></span><span><strong>{item.name}</strong>{theme === item.id && <Check size={14} />}</span><small>{item.description}</small></button>)}</div>
            <div className="preview-settings-row"><div><strong>减少透明度</strong><p>使用实色侧栏，保持文字清晰。</p></div><input type="checkbox" aria-label="减少透明度" checked={reducedTransparency} onChange={(event) => setReducedTransparency(event.target.checked)} /></div>
            <div className="preview-swatches">{["surface-messages", "text-primary", "accent-primary", "diff-added", "diff-removed", "skill"].map((token) => <div key={token}><span style={{ background: `var(--${token})` }} /><code>{token.replace("surface-", "")}</code></div>)}</div>
            <div className="preview-component-samples"><button className="primary" type="button" onClick={() => { setSettings(false); setScreen("home"); }}>开始新任务</button><button className="secondary" type="button" onClick={() => setSettings(false)}>返回会话</button><button className="ghost" type="button" disabled>暂不可用</button></div>
            <Markdown value="正文采用系统字体，保持中文清晰。代码使用等宽字体：`const focus = true`。常用控件采用 6–10 px 圆角，输入框为 16 px。" />
          </div>
        </div>
      </ModalShell>}
    </div>
  </div>;
}

export function AppearancePreview() {
  return <I18nextProvider i18n={i18n}><Preview /></I18nextProvider>;
}
