import { StrictMode, useState } from "react";
import { createRoot } from "react-dom/client";
import { DesktopExtensionsProvider, DesktopItemView } from "../src/features/extensions/ExtensionHost";
import type { DesktopViewHost } from "../src/features/extensions/types";
import { Markdown } from "../src/features/messages/components/Markdown";
import source from "../../../plugins/features/pi-plugin-subagents/desktop/dist/index.js?raw";
import css from "../../../plugins/features/pi-plugin-subagents/desktop/dist/style.css?raw";
import "../src/styles/base.css";
import "../src/styles/ds-tokens.css";
import "../src/styles/desktop-extensions.css";
const catalog = async () => ({ extensions: [{ id: "pi.subagents", revision: "preview", javascript: source, css }], projectTrusted: false });
function Preview() {
    const [expanded, setExpanded] = useState(true);
    const [state, setState] = useState("running");
    const [task, setTask] = useState("检查桌面插件的 React 展示与任务操作");
    const [log, setLog] = useState("正在检查插件的独立构建和组件状态。");
    const host: DesktopViewHost = { workspaceId: "preview", threadId: "parent", locale: "zh-CN", readOnly: false,
        widgets: { "subagents.tasks": { version: 1, ownerSessionId: "parent", liveAgentIds: ["reviewer-1"], agents: { "reviewer-1": {
                        agentId: "reviewer-1", agent: "reviewer", task, state, session: { sessionId: "child", ownerSessionId: "parent" }, totalTokens: 1234,
                    } } } },
        sessionStatus: () => ({ isProcessing: state === "running", totalTokens: 1234 }),
        renderMarkdown: content => <Markdown value={content}/>,
        renderSession: () => <div style={{ padding: 16, background: "var(--surface-hover)", borderRadius: 8 }}><Markdown value={log}/></div>,
        runCommand: async (name, args) => {
            if (name === "subagents:interrupt") {
                setState("interrupted");
                setLog("任务已中断。可以追加任务继续使用这个子代理。");
            }
            else {
                setTask(JSON.parse(args).task);
                setState("running");
                setLog(`收到追加任务：**${JSON.parse(args).task}**`);
            }
        },
    };
    return <main style={{ maxWidth: 850, margin: "64px auto", padding: 24 }}>
    <p style={{ opacity: 0.6 }}>PI DESKTOP / PLUGIN PREVIEW</p><h1>插件自己的界面，通用组件负责基础交互</h1>
    <p>这个页面动态加载独立构建的 subagent 插件，使用模拟任务验证停止、追加、展开和 React 状态。</p>
    <DesktopItemView host={host} item={{ id: "spawn-1", toolName: "spawn_agent", toolType: "mcpToolCall", title: "spawn_agent", detail: "", status: "completed", data: { arguments: { agent: "reviewer", task }, details: { agentId: "reviewer-1", sessionId: "child", state: "running" } } }} expanded={expanded} onToggle={() => setExpanded(value => !value)} fallback={<p>Plugin unavailable</p>}/>
  </main>;
}
const root = createRoot(document.getElementById("root")!);
root.render(<StrictMode><DesktopExtensionsProvider workspaceKey="preview" loadCatalog={catalog}><Preview /></DesktopExtensionsProvider></StrictMode>);
import.meta.hot?.dispose(() => root.unmount());
