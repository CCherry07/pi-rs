import type { ConversationItem, ThreadSummary, TurnPlan, WorkspaceInfo } from "../types";
import type { GitInventory } from "../features/git/gitContext";

// Presentation fixtures only. This entry never opens projects or calls a provider.
export const projects: WorkspaceInfo[] = [
  { id: "pi", name: "pi-rs", path: "/projects/pi-rs", settings: { sidebarCollapsed: false } },
  { id: "notes", name: "personal-notes", path: "/projects/personal-notes", settings: { sidebarCollapsed: false } },
];

export function inventoryFor(project: WorkspaceInfo): GitInventory {
  return {
    workspace: {
      primaryRoot: project.id,
      executionDir: project.path,
      roots: [{ id: project.id, name: project.name, path: project.path, ownership: { kind: "external" } }],
    },
    checkouts: [{ key: project.id, workdir: project.path, gitDir: `${project.path}/.git`, commonDir: `${project.path}/.git`, rootIds: [project.id] }],
    directoryRootIds: [],
    defaultCheckoutKey: project.id,
    errors: [],
  };
}
export const threads: Record<string, ThreadSummary[]> = {
  pi: [
    { id: "search", name: "让会话搜索更好用", updatedAt: 3, modelId: "GPT-6 Astra" },
    { id: "theme", name: "打磨桌面端的视觉细节", updatedAt: 2, modelId: "GPT-6 Astra" },
    { id: "session", name: "梳理会话恢复流程", updatedAt: 1, modelId: "GPT-6 Astra" },
  ],
  notes: [
    { id: "reading", name: "整理本周的阅读笔记", updatedAt: 2 },
    { id: "ideas", name: "一个小工具的想法", updatedAt: 1 },
  ],
};
export const conversation: ConversationItem[] = [
  { id: "user-1", kind: "message", role: "user", text: "给会话列表加一个搜索功能，支持按标题筛选。保持界面简洁。" },
  { id: "assistant-1", kind: "message", role: "assistant", text: "我会先看看侧栏的结构，让搜索自然地融入现有的工作流。" },
  { id: "explore-1", kind: "explore", status: "explored", entries: [
    { kind: "read", label: "Sidebar.tsx" },
    { kind: "read", label: "threadSearchUtils.ts" },
  ] },
  { id: "tool-1", kind: "tool", toolType: "commandExecution", title: "npm run typecheck", detail: "npm run typecheck", status: "completed", output: "> pi-desktop typecheck\n> tsc --noEmit\n\nProcess exited with code 0", durationMs: 1420 },
  { id: "assistant-2", kind: "message", role: "assistant", text: "### 更轻盈的会话搜索\n\n搜索入口已经放在项目标题旁边。输入时即时筛选，清空后恢复完整列表；项目分组和当前会话都会保留。\n\n```tsx\nconst visibleThreads = threads.filter((thread) =>\n  thread.name.toLocaleLowerCase().includes(query),\n);\n```\n\n你可以点击左侧的搜索图标，体验筛选效果。" },
];
export const plan: TurnPlan = {
  turnId: "preview-turn",
  explanation: null,
  steps: [
    { step: "梳理侧栏与会话结构", status: "completed" },
    { step: "接入标题筛选", status: "completed" },
    { step: "检查键盘与空状态", status: "completed" },
  ],
};
export const changedFiles = [
  { name: "Sidebar.tsx", path: "src/features/app/components", added: 18, removed: 4 },
  { name: "threadSearchUtils.ts", path: "src/features/app/components", added: 12, removed: 2 },
  { name: "sidebar.css", path: "src/styles", added: 24, removed: 8 },
];
export const sampleDiff = "diff --git a/Sidebar.tsx b/Sidebar.tsx\n--- a/Sidebar.tsx\n+++ b/Sidebar.tsx\n@@ -1,3 +1,6 @@\n-const visibleThreads = threads;\n+const normalizedQuery = query.trim().toLocaleLowerCase();\n+const visibleThreads = threads.filter((thread) =>\n+  thread.name.toLocaleLowerCase().includes(normalizedQuery),\n+);\n \n return visibleThreads;\n";
