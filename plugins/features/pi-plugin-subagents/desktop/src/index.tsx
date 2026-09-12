import {
  defineDesktopExtension,
  defineWidgetChannel,
  InlineCommandForm,
  Markdown,
  SessionView,
  StatusBadge,
  ToolCard,
  useDesktopContext,
  usePluginCommand,
  useWidget,
  type DesktopItemViewProps,
} from '@pi-rs/desktop-sdk'
import { decodeSubagentsWidget, isControllable, itemTasks, widgetTask, type Task } from './model'
import './style.css'

const tasksChannel = defineWidgetChannel({
  key: 'subagents.tasks',
  decode: decodeSubagentsWidget,
})

const labels = {
  en: {
    title: 'Subagent',
    starting: 'Starting',
    running: 'processing',
    interrupting: 'Stopping',
    idle: 'Completed',
    failed: 'Failed',
    interrupted: 'Interrupted',
    timed_out: 'Timed out',
    history: 'History',
    unknown: 'Unavailable',
    stop: 'Stop task',
    followup: 'Follow up',
    placeholder: 'Give this agent more work…',
    pending: 'Sending…',
    tokens: 'tokens',
    expand: 'Expand child-agent chat',
    collapse: 'Collapse child-agent chat',
    legacy_idle: 'idle',
  },
  zh: {
    title: '子代理',
    starting: '启动中',
    running: '执行中',
    interrupting: '停止中',
    idle: '已完成',
    failed: '失败',
    interrupted: '已中断',
    timed_out: '已超时',
    history: '历史记录',
    unknown: '状态不可用',
    stop: '停止任务',
    followup: '追加任务',
    placeholder: '给这个子代理追加任务…',
    pending: '发送中…',
    tokens: 'tokens',
    expand: '展开子代理对话',
    collapse: '收起子代理对话',
    legacy_idle: '空闲',
  },
}

function useLabels() {
  return labels[useDesktopContext().locale.toLowerCase().startsWith('zh') ? 'zh' : 'en']
}

function TaskStatus({ task, live }: { task: Task; live: boolean }) {
  const context = useDesktopContext()
  const words = useLabels()
  const active = ['starting', 'running', 'interrupting'].includes(task.state ?? '')
  const observed = context.sessionStatus(task.session)
  const state =
    !task.agentId && observed
      ? observed.isProcessing
        ? 'running'
        : 'legacy_idle'
      : !task.agentId && (task.session.sessionId || task.session.isolatedSessionId)
        ? 'history'
        : active && !live
          ? 'history'
          : (task.state ?? 'unknown')
  const tone =
    state === 'idle' || state === 'completed'
      ? 'completed'
      : state === 'failed' || state === 'timed_out'
        ? 'failed'
        : state === 'interrupted'
          ? 'interrupted'
          : ['starting', 'running', 'interrupting'].includes(state)
            ? 'processing'
            : 'unknown'
  const tokens = live || !task.agentId ? (observed?.totalTokens ?? task.totalTokens) : task.totalTokens
  const formattedTokens = tokens !== undefined ? `${(tokens / 1000).toFixed(1)}k` : undefined
  const statusLabel = words[state as keyof typeof words] ?? state
  return (
    <span className="pi-subagents-status">
      <StatusBadge state={tone} label={`${statusLabel}${formattedTokens ? ` · ${formattedTokens} ${words.tokens}` : ''}`} />
    </span>
  )
}

function TaskActions({ task }: { task: Task }) {
  const words = useLabels()
  const interrupt = usePluginCommand<{ target: string }>('subagents:interrupt')
  const followup = usePluginCommand<{ target: string; task: string }>('subagents:followup')
  const pending = interrupt.pending || followup.pending
  const active = ['starting', 'running', 'interrupting'].includes(task.state ?? '')
  return (
    <div className="pi-subagents-actions">
      {active && (
        <button
          type="button"
          disabled={pending || task.state === 'interrupting'}
          onClick={() => task.agentId && void interrupt.run({ target: task.agentId })}
        >
          {words.stop}
        </button>
      )}
      <InlineCommandForm
        ariaLabel={words.followup}
        placeholder={words.placeholder}
        submitLabel={words.followup}
        pendingLabel={words.pending}
        pending={pending}
        error={followup.error}
        disabled={!task.agentId}
        onSubmit={value => followup.run({ target: task.agentId!, task: value })}
      />
      {interrupt.error && (
        <p role="alert" className="pi-subagents-error">
          {interrupt.error}
        </p>
      )}
    </div>
  )
}

function SubagentView({ item, expanded, onToggle }: DesktopItemViewProps) {
  const context = useDesktopContext()
  const words = useLabels()
  const { value: widget } = useWidget(tasksChannel)
  const tasks = itemTasks(item, context.threadId).map(task => widgetTask(widget, task.agentId) ?? task)
  const live = (task: Task) => isControllable(widget, task, context.threadId)
  const first = tasks[0]
  return (
    <ToolCard
      title={`${words.title} · ${first?.agent ?? 'Agent'}`}
      expanded={expanded}
      onToggle={onToggle}
      className="pi-subagents-card"
      summary={first?.task}
      toggleLabel={expanded ? words.collapse : words.expand}
      status={first && <TaskStatus task={first} live={live(first)} />}
    >
      {tasks.map((task, index) => (
        <section
          key={task.agentId ?? task.session.sessionId ?? index}
          aria-label={context.locale.toLowerCase().startsWith('zh') ? `${task.agent} 的对话` : `${task.agent} conversation`}
          className="pi-subagents-child subagent-chat-conversation"
        >
          {tasks.length > 1 && (
            <header>
              <strong>{task.agent}</strong>
              <TaskStatus task={task} live={live(task)} />
            </header>
          )}
          {task.task && (
            <div className="pi-subagents-task">
              <Markdown>{task.task}</Markdown>
            </div>
          )}
          {task.session.sessionId || task.session.isolatedSessionId ? (
            <SessionView reference={task.session} className="pi-subagents-session" />
          ) : (
            <div role={item.status === 'failed' ? 'alert' : 'status'}>
              <Markdown>{item.output ?? item.detail}</Markdown>
            </div>
          )}
          {!context.readOnly && live(task) && <TaskActions task={task} />}
        </section>
      ))}
    </ToolCard>
  )
}

export default defineDesktopExtension({
  id: 'pi.subagents',
  views: [{ id: 'task', slot: 'tool.result', target: { tool: 'spawn_agent' }, component: SubagentView }],
})
