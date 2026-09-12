import type { DesktopSessionRef, DesktopToolItem } from "@pi-rs/desktop-sdk";

export type Task = {
  agentId?: string;
  agent: string;
  task?: string;
  state?: string;
  session: DesktopSessionRef;
  totalTokens?: number;
};

export type SubagentsWidget = {
  version: 1;
  runtimeId?: string;
  ownerSessionId: string;
  agents: Readonly<Record<string, unknown>>;
  liveAgentIds: readonly string[];
};

export function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown> : {};
}

function text(value: unknown): string | undefined {
  return typeof value === "string" && value.length ? value : undefined;
}

function number(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : undefined;
}

function reference(value: unknown): DesktopSessionRef {
  const data = record(value);
  return {
    sessionId: text(data.sessionId),
    isolatedSessionId: text(data.isolatedSessionId),
    ownerSessionId: text(data.ownerSessionId),
  };
}

export function decodeSubagentsWidget(value: unknown): SubagentsWidget {
  const data = record(value);
  const ownerSessionId = text(data.ownerSessionId);
  if (data.version !== 1 || !ownerSessionId || !data.agents || typeof data.agents !== "object"
    || Array.isArray(data.agents) || !Array.isArray(data.liveAgentIds)
    || !data.liveAgentIds.every((id) => typeof id === "string" && id.length > 0)) {
    throw new Error("Invalid subagent task widget version or shape.");
  }
  return {
    version: 1,
    runtimeId: text(data.runtimeId),
    ownerSessionId,
    agents: data.agents as Record<string, unknown>,
    liveAgentIds: data.liveAgentIds,
  };
}

function legacyIdentity(value: unknown) {
  const data = record(value);
  return {
    id: text(data.threadId ?? data.thread_id ?? data.id),
    name: text(data.agentNickname ?? data.agent_nickname ?? data.nickname)
      ?? text(data.agentRole ?? data.agent_role ?? data.role),
  };
}

function legacyStatuses(data: Record<string, unknown>): Record<string, unknown> {
  const statuses = data.agentStatuses ?? data.agent_statuses;
  if (!Array.isArray(statuses)) return record(statuses ?? data.statuses ?? data.agentsStates);
  return Object.fromEntries(statuses.flatMap((status) => {
    const { id } = legacyIdentity(status);
    return id ? [[id, status]] : [];
  }));
}

export function widgetTask(widget: SubagentsWidget | undefined, agentId?: string): Task | undefined {
  if (!widget || !agentId) return undefined;
  const task = record(widget.agents[agentId]);
  if (task.agentId !== agentId) return undefined;
  return {
    agentId, agent: text(task.agent) ?? agentId,
    task: text(task.task), state: text(task.state),
    session: reference(task.session), totalTokens: number(task.totalTokens),
  };
}

export function isControllable(widget: SubagentsWidget | undefined, task: Task, owner: string | null): boolean {
  return widget?.ownerSessionId === owner && !!owner
    && task.session.ownerSessionId === owner && !!task.agentId
    && widget.liveAgentIds.includes(task.agentId);
}

/** Compatibility with old desktop projections belongs to this plugin. */
export function itemTasks(item: DesktopToolItem, ownerSessionId: string | null): Task[] {
  const data = record(item.data);
  const args = record(data.arguments);
  const details = record(data.details);
  const legacy = record(data.result);
  const agentId = text(details.agentId) ?? text(legacy.agentId);
  const sessionId = text(details.sessionId) ?? text(legacy.sessionId) ?? text(data.newThreadId ?? data.new_thread_id);
  const isolatedSessionId = text(details.isolatedSessionId) ?? text(legacy.isolatedSessionId);
  const task = text(args.task) ?? text(data.prompt);
  const statuses = legacyStatuses(data);
  const primaryStatus = sessionId ? record(statuses[sessionId]) : {};
  const primary: Task = {
    agentId,
    agent: text(details.agent) ?? text(args.agent)
      ?? text(data.newAgentNickname ?? data.new_agent_nickname)
      ?? text(data.newAgentRole ?? data.new_agent_role) ?? "Agent",
    task,
    state: text(details.state) ?? text(legacy.state) ?? text(primaryStatus.status)
      ?? (item.status === "failed" ? "failed" : undefined),
    session: { sessionId, isolatedSessionId, ownerSessionId: ownerSessionId ?? undefined },
    totalTokens: number(primaryStatus.totalTokens),
  };
  if (agentId || isolatedSessionId) return [primary];
  const receivers = new Map<string, string | undefined>();
  if (sessionId) receivers.set(sessionId, primary.agent);
  const receiverId = text(data.receiverThreadId ?? data.receiver_thread_id);
  if (receiverId) receivers.set(receiverId, text(data.receiverAgentNickname) ?? text(data.receiverAgentRole));
  const receiverIds = data.receiverThreadIds ?? data.receiver_thread_ids;
  if (Array.isArray(receiverIds)) for (const id of receiverIds) {
    if (typeof id === "string" && id.length && !receivers.has(id)) receivers.set(id, undefined);
  }
  const receiverAgents = data.receiverAgents ?? data.receiver_agents;
  if (Array.isArray(receiverAgents)) for (const receiver of receiverAgents) {
    const {id, name} = legacyIdentity(receiver);
    if (id) receivers.set(id, name ?? receivers.get(id));
  }
  const legacyTasks = Array.from(receivers, ([id, name]) => {
    const status = record(statuses[id]);
    return {
      ...primary,
      agent: name ?? legacyIdentity(status).name ?? id,
      state: text(status.status) ?? text(statuses[id]),
      totalTokens: number(status.totalTokens ?? status.total_tokens),
      session: { sessionId: id, ownerSessionId: ownerSessionId ?? undefined },
    };
  });
  return legacyTasks.length ? legacyTasks : [primary];
}
