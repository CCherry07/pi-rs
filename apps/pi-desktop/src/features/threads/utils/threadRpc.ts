import { asString } from "./threadNormalize";

const SIDEBAR_HIDDEN_SUBAGENT_KINDS = new Set(["memory_consolidation"]);

function asRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== "object") {
    return null;
  }
  return value as Record<string, unknown>;
}

function firstNonEmptyString(...values: unknown[]): string | null {
  for (const value of values) {
    const normalized = asString(value);
    if (normalized) {
      return normalized;
    }
  }
  return null;
}

function normalizeSubagentKind(value: string): string {
  const normalized = value
    .trim()
    .toLowerCase()
    .replace(/[\s-]/g, "_");
  if (normalized.startsWith("subagent_")) {
    return normalized.slice("subagent_".length);
  }
  if (normalized.startsWith("sub_agent_")) {
    return normalized.slice("sub_agent_".length);
  }
  return normalized;
}

function getSubagentKind(source: unknown): string | null {
  if (typeof source === "string") {
    const normalized = normalizeSubagentKind(source);
    return normalized || null;
  }

  const sourceRecord = asRecord(source);
  if (!sourceRecord) {
    return null;
  }

  const subAgentRaw =
    sourceRecord.subAgent ?? sourceRecord.sub_agent ?? sourceRecord.subagent;
  if (typeof subAgentRaw === "string") {
    const normalized = normalizeSubagentKind(subAgentRaw);
    return normalized || null;
  }

  const subAgentRecord = asRecord(subAgentRaw);
  if (!subAgentRecord) {
    return null;
  }

  const explicitKind = asString(
    subAgentRecord.kind ??
      subAgentRecord.type ??
      subAgentRecord.name ??
      subAgentRecord.id,
  );
  if (explicitKind) {
    const normalized = normalizeSubagentKind(explicitKind);
    return normalized || null;
  }

  const candidateKeys = Object.keys(subAgentRecord).filter(
    (key) => key !== "thread_spawn" && key !== "threadSpawn",
  );
  if (candidateKeys.length !== 1) {
    return null;
  }
  const normalized = normalizeSubagentKind(candidateKeys[0] ?? "");
  return normalized || null;
}

export function isSubagentThreadSource(source: unknown): boolean {
  if (typeof source === "string") {
    const normalized = source.trim().toLowerCase();
    return normalized.startsWith("subagent") || normalized.startsWith("sub_agent");
  }

  const sourceRecord = asRecord(source);
  if (!sourceRecord) {
    return false;
  }

  const subAgent =
    sourceRecord.subAgent ?? sourceRecord.sub_agent ?? sourceRecord.subagent;
  if (subAgent === null || subAgent === undefined) {
    return false;
  }
  if (typeof subAgent === "string") {
    return subAgent.trim().length > 0;
  }
  return typeof subAgent === "object";
}

export function shouldHideSubagentThreadFromSidebar(source: unknown): boolean {
  const subagentKind = getSubagentKind(source);
  if (!subagentKind) {
    return false;
  }
  return SIDEBAR_HIDDEN_SUBAGENT_KINDS.has(subagentKind);
}

export function getParentThreadIdFromSource(source: unknown): string | null {
  const sourceRecord = asRecord(source);
  if (!sourceRecord) {
    return null;
  }
  const subAgent = asRecord(
    sourceRecord.subAgent ?? sourceRecord.sub_agent ?? sourceRecord.subagent,
  );
  if (!subAgent) {
    return null;
  }
  const threadSpawn = asRecord(subAgent.thread_spawn ?? subAgent.threadSpawn);
  if (!threadSpawn) {
    return null;
  }
  const parentId = asString(
    threadSpawn.parent_thread_id ?? threadSpawn.parentThreadId,
  );
  return parentId || null;
}

export function getParentThreadIdFromThread(
  thread: Record<string, unknown>,
): string | null {
  const sourceParentId = getParentThreadIdFromSource(thread.source);
  if (sourceParentId) {
    return sourceParentId;
  }
  const directParentId = asString(
    thread.parentThreadId ??
      thread.parent_thread_id ??
      thread.parentId ??
      thread.parent_id ??
      thread.senderThreadId ??
      thread.sender_thread_id,
  );
  if (directParentId) {
    return directParentId;
  }
  const spawnRaw =
    thread.threadSpawn ??
    thread.thread_spawn ??
    thread.spawn ??
    thread.subAgent ??
    thread.subagent;
  const spawn =
    spawnRaw && typeof spawnRaw === "object"
      ? (spawnRaw as Record<string, unknown>)
      : null;
  if (!spawn) {
    return null;
  }
  const spawnParentId = asString(
    spawn.parentThreadId ??
      spawn.parent_thread_id ??
      spawn.parentId ??
      spawn.parent_id,
  );
  return spawnParentId || null;
}

export function getSubagentMetadataFromThread(
  thread: Record<string, unknown>,
): { nickname: string | null; role: string | null } {
  const sourceRecord = asRecord(thread.source);
  const subAgent = asRecord(
    sourceRecord?.subAgent ?? sourceRecord?.sub_agent ?? sourceRecord?.subagent,
  );
  const threadSpawn = asRecord(subAgent?.threadSpawn ?? subAgent?.thread_spawn);

  return {
    nickname: firstNonEmptyString(
      thread.agentNickname,
      thread.agent_nickname,
      thread.nickname,
      subAgent?.agentNickname,
      subAgent?.agent_nickname,
      threadSpawn?.agentNickname,
      threadSpawn?.agent_nickname,
    ),
    role: firstNonEmptyString(
      thread.agentRole,
      thread.agent_role,
      thread.agentType,
      thread.agent_type,
      thread.role,
      subAgent?.agentRole,
      subAgent?.agent_role,
      subAgent?.agentType,
      subAgent?.agent_type,
      threadSpawn?.agentRole,
      threadSpawn?.agent_role,
      threadSpawn?.agentType,
      threadSpawn?.agent_type,
    ),
  };
}

export type ResumedTurnState = {
  activeTurnId: string | null;
  activeTurnStartedAtMs: number | null;
  confidentNoActiveTurn: boolean;
};

function normalizeTurnStatus(value: unknown): string {
  return String(value ?? "")
    .trim()
    .toLowerCase()
    .replace(/[\s_-]/g, "");
}

function normalizeTimestampMs(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value) && value > 0) {
    return value < 10_000_000_000 ? Math.trunc(value * 1000) : Math.trunc(value);
  }
  if (typeof value === "string" && value.trim()) {
    const parsedNumber = Number(value);
    if (Number.isFinite(parsedNumber) && parsedNumber > 0) {
      return parsedNumber < 10_000_000_000
        ? Math.trunc(parsedNumber * 1000)
        : Math.trunc(parsedNumber);
    }
    const parsedDate = Date.parse(value);
    if (Number.isFinite(parsedDate) && parsedDate > 0) {
      return parsedDate;
    }
  }
  return null;
}

function turnStartedAtMs(turn: Record<string, unknown>): number | null {
  return (
    normalizeTimestampMs(
      turn.startedAt ??
        turn.started_at ??
        turn.startTime ??
        turn.start_time ??
        turn.createdAt ??
        turn.created_at,
    ) ?? null
  );
}

type TurnStatusKind = "active" | "terminal" | "unknown";

function classifyTurnStatus(status: string): TurnStatusKind {
  if (!status) {
    return "unknown";
  }
  if (
    status === "inprogress" ||
    status === "running" ||
    status === "processing" ||
    status === "pending" ||
    status === "started" ||
    status === "queued" ||
    status === "waiting" ||
    status === "blocked" ||
    status === "needsinput" ||
    status === "requiresaction" ||
    status === "awaitinginput" ||
    status === "waitingforinput"
  ) {
    return "active";
  }
  if (
    status === "completed" ||
    status === "done" ||
    status === "failed" ||
    status === "error" ||
    status === "canceled" ||
    status === "cancelled" ||
    status === "aborted" ||
    status === "stopped" ||
    status === "interrupted"
  ) {
    return "terminal";
  }
  return "unknown";
}

function getExplicitActiveTurnState(
  thread: Record<string, unknown>,
): {
  explicit: boolean;
  activeTurnId: string | null;
  activeTurnStartedAtMs: number | null;
} {
  const hasExplicitTurnId =
    "activeTurnId" in thread || "active_turn_id" in thread;
  const activeTurnId = asString(thread.activeTurnId ?? thread.active_turn_id).trim();
  if (hasExplicitTurnId) {
    return {
      explicit: true,
      activeTurnId: activeTurnId || null,
      activeTurnStartedAtMs: null,
    };
  }

  const activeTurnRaw =
    thread.activeTurn ??
    thread.active_turn ??
    thread.currentTurn ??
    thread.current_turn;
  const hasExplicitTurnObject =
    "activeTurn" in thread ||
    "active_turn" in thread ||
    "currentTurn" in thread ||
    "current_turn" in thread;
  const activeTurn = asRecord(activeTurnRaw);
  if (!hasExplicitTurnObject) {
    return {
      explicit: false,
      activeTurnId: null,
      activeTurnStartedAtMs: null,
    };
  }
  const objectTurnId = asString(
    activeTurn?.id ?? activeTurn?.turnId ?? activeTurn?.turn_id,
  ).trim();
  return {
    explicit: true,
    activeTurnId: objectTurnId || null,
    activeTurnStartedAtMs: activeTurn ? turnStartedAtMs(activeTurn) : null,
  };
}

export function getResumedTurnState(
  thread: Record<string, unknown>,
): ResumedTurnState {
  const explicitState = getExplicitActiveTurnState(thread);
  if (explicitState.explicit) {
    return {
      activeTurnId: explicitState.activeTurnId,
      activeTurnStartedAtMs: explicitState.activeTurnStartedAtMs,
      confidentNoActiveTurn: !explicitState.activeTurnId,
    };
  }

  const turns = Array.isArray(thread.turns)
    ? (thread.turns as Array<Record<string, unknown>>)
    : [];
  let sawTerminalStatus = false;
  let sawUnknownStatus = false;
  for (let index = turns.length - 1; index >= 0; index -= 1) {
    const turn = turns[index];
    if (!turn || typeof turn !== "object") {
      sawUnknownStatus = true;
      continue;
    }
    const status = classifyTurnStatus(
      normalizeTurnStatus(
        turn.status ?? turn.turnStatus ?? turn.turn_status,
      ),
    );
    if (status === "active") {
      const turnId = asString(turn.id ?? turn.turnId ?? turn.turn_id).trim();
      if (turnId) {
        return {
          activeTurnId: turnId,
          activeTurnStartedAtMs: turnStartedAtMs(turn),
          confidentNoActiveTurn: false,
        };
      }
      sawUnknownStatus = true;
      continue;
    }
    if (status === "terminal") {
      sawTerminalStatus = true;
      continue;
    }
    sawUnknownStatus = true;
  }
  return {
    activeTurnId: null,
    activeTurnStartedAtMs: null,
    confidentNoActiveTurn: sawTerminalStatus && !sawUnknownStatus,
  };
}

export function getResumedActiveTurnId(thread: Record<string, unknown>): string | null {
  return getResumedTurnState(thread).activeTurnId;
}
