import type {
  CollabAgentRef,
  CollabAgentStatus,
  ConversationItem,
  ThreadSummary,
} from "../types";
import { asString, normalizeStringList } from "./threadItems.shared";

function buildCollabAgentRef(
  threadIdValue: unknown,
  nicknameValue?: unknown,
  roleValue?: unknown,
): CollabAgentRef | null {
  const threadId = asString(threadIdValue).trim();
  if (!threadId) {
    return null;
  }
  const nickname = asString(nicknameValue ?? "").trim() || undefined;
  const role = asString(roleValue ?? "").trim() || undefined;
  return { threadId, nickname, role };
}

function parseCollabAgentRef(value: unknown): CollabAgentRef | null {
  if (!value || typeof value !== "object") {
    return null;
  }
  const record = value as Record<string, unknown>;
  return buildCollabAgentRef(
    record.threadId ?? record.thread_id ?? record.id,
    record.agentNickname ?? record.agent_nickname ?? record.nickname,
    record.agentRole ??
      record.agent_role ??
      record.agentType ??
      record.agent_type ??
      record.role,
  );
}

function parseCollabAgentRefs(value: unknown) {
  if (!value) {
    return [];
  }
  if (Array.isArray(value)) {
    return value
      .map((entry) => parseCollabAgentRef(entry))
      .filter((entry): entry is CollabAgentRef => Boolean(entry));
  }
  const single = parseCollabAgentRef(value);
  return single ? [single] : [];
}

function mergeCollabAgentRefs(...lists: CollabAgentRef[][]) {
  const byThreadId = new Map<string, CollabAgentRef>();
  lists.forEach((list) => {
    list.forEach((entry) => {
      const existing = byThreadId.get(entry.threadId);
      if (!existing) {
        byThreadId.set(entry.threadId, { ...entry });
        return;
      }
      byThreadId.set(entry.threadId, {
        threadId: existing.threadId,
        nickname: existing.nickname ?? entry.nickname,
        role: existing.role ?? entry.role,
      });
    });
  });
  return Array.from(byThreadId.values());
}

function buildCollabAgentStatus(
  threadIdValue: unknown,
  statusValue: unknown,
  nicknameValue?: unknown,
  roleValue?: unknown,
): CollabAgentStatus | null {
  const status = asString(statusValue).trim();
  if (!status) {
    return null;
  }
  const base = buildCollabAgentRef(threadIdValue, nicknameValue, roleValue);
  if (!base) {
    return null;
  }
  return { ...base, status };
}

function parseCollabAgentStatuses(value: unknown) {
  if (!Array.isArray(value)) {
    return [];
  }
  return value
    .map((entry) => {
      if (!entry || typeof entry !== "object") {
        return null;
      }
      const record = entry as Record<string, unknown>;
      return buildCollabAgentStatus(
        record.threadId ?? record.thread_id ?? record.id,
        record.status,
        record.agentNickname ?? record.agent_nickname ?? record.nickname,
        record.agentRole ??
          record.agent_role ??
          record.agentType ??
          record.agent_type ??
          record.role,
      );
    })
    .filter((entry): entry is CollabAgentStatus => Boolean(entry));
}

function parseCollabAgentStatusesFromMap(value: unknown) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return [];
  }
  return Object.entries(value as Record<string, unknown>)
    .map(([threadId, state]) => {
      const stateRecord =
        state && typeof state === "object"
          ? (state as Record<string, unknown>)
          : null;
      const status = asString(stateRecord?.status ?? state ?? "").trim();
      if (!status || !threadId) {
        return null;
      }
      return buildCollabAgentStatus(
        threadId,
        status,
        stateRecord?.agentNickname ??
          stateRecord?.agent_nickname ??
          stateRecord?.nickname,
        stateRecord?.agentRole ??
          stateRecord?.agent_role ??
          stateRecord?.agentType ??
          stateRecord?.agent_type ??
          stateRecord?.role,
      );
    })
    .filter((entry): entry is CollabAgentStatus => Boolean(entry));
}

function mergeCollabAgentStatuses(...lists: CollabAgentStatus[][]) {
  const byThreadId = new Map<string, CollabAgentStatus>();
  lists.forEach((list) => {
    list.forEach((entry) => {
      const existing = byThreadId.get(entry.threadId);
      if (!existing) {
        byThreadId.set(entry.threadId, { ...entry });
        return;
      }
      byThreadId.set(entry.threadId, {
        threadId: existing.threadId,
        status: existing.status || entry.status,
        nickname: existing.nickname ?? entry.nickname,
        role: existing.role ?? entry.role,
      });
    });
  });
  return Array.from(byThreadId.values());
}

function withCollabAgentMetadata(
  statuses: CollabAgentStatus[],
  agents: CollabAgentRef[],
) {
  if (statuses.length === 0 || agents.length === 0) {
    return statuses;
  }
  const byThreadId = new Map(agents.map((agent) => [agent.threadId, agent]));
  return statuses.map((entry) => {
    const metadata = byThreadId.get(entry.threadId);
    if (!metadata) {
      return entry;
    }
    return {
      ...entry,
      nickname: entry.nickname ?? metadata.nickname,
      role: entry.role ?? metadata.role,
    };
  });
}

function formatCollabAgentLabel(agent: CollabAgentRef) {
  const nickname = agent.nickname?.trim();
  const role = agent.role?.trim();
  if (nickname && role) {
    return `${nickname} [${role}]`;
  }
  if (nickname) {
    return nickname;
  }
  if (role) {
    return `${agent.threadId} [${role}]`;
  }
  return agent.threadId;
}

function formatCollabAgentStatuses(value: CollabAgentStatus[]) {
  if (value.length === 0) {
    return "";
  }
  return value
    .map((entry) => `${formatCollabAgentLabel(entry)}: ${entry.status}`)
    .join("\n");
}

function enrichCollabAgentRef(
  agent: CollabAgentRef | undefined,
  metadataByThreadId: ReadonlyMap<
    string,
    { nickname?: string | null; role?: string | null }
  >,
): CollabAgentRef | undefined {
  if (!agent) {
    return agent;
  }
  const metadata = metadataByThreadId.get(agent.threadId);
  if (!metadata) {
    return agent;
  }
  const nickname = agent.nickname ?? metadata.nickname ?? undefined;
  const role = agent.role ?? metadata.role ?? undefined;
  if (nickname === agent.nickname && role === agent.role) {
    return agent;
  }
  return {
    ...agent,
    ...(nickname ? { nickname } : {}),
    ...(role ? { role } : {}),
  };
}

function enrichCollabAgentStatuses(
  statuses: CollabAgentStatus[] | undefined,
  metadataByThreadId: ReadonlyMap<
    string,
    { nickname?: string | null; role?: string | null }
  >,
): CollabAgentStatus[] | undefined {
  if (!statuses || statuses.length === 0) {
    return statuses;
  }
  let didChange = false;
  const next = statuses.map((entry) => {
    const metadata = metadataByThreadId.get(entry.threadId);
    if (!metadata) {
      return entry;
    }
    const nickname = entry.nickname ?? metadata.nickname ?? undefined;
    const role = entry.role ?? metadata.role ?? undefined;
    if (nickname === entry.nickname && role === entry.role) {
      return entry;
    }
    didChange = true;
    return {
      ...entry,
      ...(nickname ? { nickname } : {}),
      ...(role ? { role } : {}),
    };
  });
  return didChange ? next : statuses;
}

function extractCollabPrompt(
  output: string | undefined,
  statuses: CollabAgentStatus[] | undefined,
) {
  const text = output ?? "";
  if (!text) {
    return "";
  }
  const statusText = formatCollabAgentStatuses(statuses ?? []);
  if (!statusText || !text.endsWith(statusText)) {
    return text;
  }
  return text.slice(0, text.length - statusText.length).replace(/\n\n$/, "");
}

function buildCollabDetail(
  sender: CollabAgentRef | undefined,
  receivers: CollabAgentRef[],
) {
  const detailParts = [sender ? `From ${formatCollabAgentLabel(sender)}` : ""]
    .concat(
      receivers.length > 0
        ? `→ ${receivers.map((entry) => formatCollabAgentLabel(entry)).join(", ")}`
        : "",
    )
    .filter(Boolean);
  return detailParts.join(" ");
}

function buildCollabOutput(prompt: string, statuses: CollabAgentStatus[]) {
  const promptText = prompt.trim();
  const statusesText = formatCollabAgentStatuses(statuses);
  return [promptText, statusesText].filter(Boolean).join("\n\n");
}

export function parseCollabToolCallItem(
  item: Record<string, unknown>,
): Extract<ConversationItem, { kind: "tool" }> {
  const tool = asString(item.tool ?? "");
  const status = asString(item.status ?? "");
  const senderThreadId = asString(item.senderThreadId ?? item.sender_thread_id ?? "");
  const sender = buildCollabAgentRef(
    senderThreadId,
    item.senderAgentNickname ??
      item.sender_agent_nickname ??
      item.agentNickname ??
      item.agent_nickname,
    item.senderAgentRole ??
      item.sender_agent_role ??
      item.agentRole ??
      item.agent_role ??
      item.agentType ??
      item.agent_type,
  );
  const receiverFromInteraction = buildCollabAgentRef(
    item.receiverThreadId ?? item.receiver_thread_id,
    item.receiverAgentNickname ?? item.receiver_agent_nickname,
    item.receiverAgentRole ??
      item.receiver_agent_role ??
      item.receiverAgentType ??
      item.receiver_agent_type,
  );
  const receiverFromSpawn = buildCollabAgentRef(
    item.newThreadId ?? item.new_thread_id,
    item.newAgentNickname ?? item.new_agent_nickname,
    item.newAgentRole ?? item.new_agent_role ?? item.newAgentType ?? item.new_agent_type,
  );
  const receiverIds = [
    ...normalizeStringList(item.receiverThreadId ?? item.receiver_thread_id),
    ...normalizeStringList(item.receiverThreadIds ?? item.receiver_thread_ids),
    ...normalizeStringList(item.newThreadId ?? item.new_thread_id),
  ]
    .map((entry) => buildCollabAgentRef(entry))
    .filter((entry): entry is CollabAgentRef => Boolean(entry));
  const receiverAgents = mergeCollabAgentRefs(
    receiverIds,
    parseCollabAgentRefs(item.receiverAgents ?? item.receiver_agents),
    receiverFromInteraction ? [receiverFromInteraction] : [],
    receiverFromSpawn ? [receiverFromSpawn] : [],
  );
  const collabStatuses = withCollabAgentMetadata(
    mergeCollabAgentStatuses(
      parseCollabAgentStatuses(item.agentStatuses ?? item.agent_statuses),
      parseCollabAgentStatusesFromMap(item.statuses),
      parseCollabAgentStatusesFromMap(
        item.agentStatus ?? item.agentsStates ?? item.agents_states,
      ),
    ),
    receiverAgents,
  );
  const prompt = asString(item.prompt ?? "");
  const primaryReceiver = receiverFromInteraction ?? receiverFromSpawn ?? receiverAgents[0];
  return {
    id: asString(item.id),
    kind: "tool",
    toolType: "collabToolCall",
    title: tool ? `Collab: ${tool}` : "Collab tool call",
    detail: buildCollabDetail(sender ?? undefined, receiverAgents),
    status,
    output: buildCollabOutput(prompt, collabStatuses),
    collabSender: sender ?? undefined,
    collabReceiver: primaryReceiver ?? undefined,
    collabReceivers: receiverAgents.length > 0 ? receiverAgents : undefined,
    collabStatuses: collabStatuses.length > 0 ? collabStatuses : undefined,
    collabTask: prompt || undefined,
  };
}

export function enrichConversationItemsWithThreads(
  items: ConversationItem[],
  threads: ThreadSummary[],
): ConversationItem[] {
  if (items.length === 0 || threads.length === 0) {
    return items;
  }

  const metadataByThreadId = new Map<
    string,
    { nickname?: string | null; role?: string | null }
  >();
  threads.forEach((thread) => {
    if (!thread.id || (!thread.subagentNickname && !thread.subagentRole)) {
      return;
    }
    metadataByThreadId.set(thread.id, {
      nickname: thread.subagentNickname,
      role: thread.subagentRole,
    });
  });

  if (metadataByThreadId.size === 0) {
    return items;
  }

  let didChange = false;
  const nextItems = items.map((item) => {
    if (item.kind !== "tool" || item.toolType !== "collabToolCall") {
      return item;
    }

    const nextSender = enrichCollabAgentRef(item.collabSender, metadataByThreadId);
    const nextReceivers =
      item.collabReceivers?.map((entry) =>
        enrichCollabAgentRef(entry, metadataByThreadId) ?? entry,
      ) ?? [];
    const nextStatuses =
      enrichCollabAgentStatuses(item.collabStatuses, metadataByThreadId) ?? [];
    const nextPrimaryReceiver =
      enrichCollabAgentRef(item.collabReceiver, metadataByThreadId) ??
      nextReceivers[0] ??
      item.collabReceiver;
    const detailReceivers =
      nextReceivers.length > 0
        ? nextReceivers
        : nextPrimaryReceiver
          ? [nextPrimaryReceiver]
          : [];

    const prompt = extractCollabPrompt(item.output, item.collabStatuses);
    const nextDetail = buildCollabDetail(nextSender, detailReceivers);
    const nextOutput = buildCollabOutput(prompt, nextStatuses);

    const receiversChanged = nextReceivers.some(
      (entry, index) => entry !== item.collabReceivers?.[index],
    );
    const statusesChanged = nextStatuses.some(
      (entry, index) => entry !== item.collabStatuses?.[index],
    );
    const itemChanged =
      nextSender !== item.collabSender ||
      nextPrimaryReceiver !== item.collabReceiver ||
      receiversChanged ||
      statusesChanged ||
      nextDetail !== (item.detail ?? "") ||
      nextOutput !== (item.output ?? "");

    if (!itemChanged) {
      return item;
    }

    didChange = true;
    return {
      ...item,
      detail: nextDetail,
      output: nextOutput,
      collabSender: nextSender,
      collabReceiver: nextPrimaryReceiver,
      collabReceivers: nextReceivers.length > 0 ? nextReceivers : undefined,
      collabStatuses: nextStatuses.length > 0 ? nextStatuses : undefined,
    };
  });

  return didChange ? nextItems : items;
}
