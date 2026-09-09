import type {
  ThreadTokenUsage,
  TurnPlan,
  TurnPlanStep,
  TurnPlanStepStatus,
} from "@/types";

export function asString(value: unknown) {
  return typeof value === "string" ? value : value ? String(value) : "";
}

export function normalizeStringList(value: unknown) {
  if (Array.isArray(value)) {
    return value.map((entry) => asString(entry)).filter(Boolean);
  }
  const single = asString(value);
  return single ? [single] : [];
}

export function normalizeRootPath(value: string) {
  const normalized = value.replace(/\\/g, "/").replace(/\/+$/, "");
  if (!normalized) {
    return "";
  }

  let withoutNamespace = normalized;
  if (/^\/\/\?\/unc\//i.test(withoutNamespace)) {
    withoutNamespace = `//${withoutNamespace.slice(8)}`;
  } else if (/^\/\/[?.]\//.test(withoutNamespace)) {
    withoutNamespace = withoutNamespace.slice(4);
  }

  if (!withoutNamespace) {
    return "";
  }

  if (/^[A-Za-z]:\//.test(withoutNamespace)) {
    return withoutNamespace.toLowerCase();
  }
  if (withoutNamespace.startsWith("//")) {
    return withoutNamespace.toLowerCase();
  }
  return withoutNamespace;
}

export function extractRpcErrorMessage(response: unknown) {
  if (!response || typeof response !== "object") {
    return null;
  }
  const record = response as Record<string, unknown>;
  if (!record.error) {
    return null;
  }
  const errorValue = record.error;
  if (typeof errorValue === "string") {
    return errorValue;
  }
  if (typeof errorValue === "object" && errorValue) {
    const message = asString((errorValue as Record<string, unknown>).message);
    return message || i18n.t("errors.requestFailed", { ns: "messages" });
  }
  return i18n.t("errors.requestFailed", { ns: "messages" });
}

export function normalizeTokenUsage(
  raw: Record<string, unknown> | null | undefined,
): ThreadTokenUsage {
  const source = raw ?? {};
  return {
    totalTokens: (() => {
      const value = source.totalTokens ?? source.total_tokens;
      if (typeof value === "number") {
        return Number.isFinite(value) ? value : null;
      }
      if (typeof value === "string") {
        const parsed = Number(value);
        return Number.isFinite(parsed) ? parsed : null;
      }
      return null;
    })(),
    contextTokens: (() => {
      const value = source.contextTokens ?? source.context_tokens;
      if (typeof value === "number") {
        return Number.isFinite(value) ? value : null;
      }
      if (typeof value === "string") {
        const parsed = Number(value);
        return Number.isFinite(parsed) ? parsed : null;
      }
      return null;
    })(),
    modelContextWindow: (() => {
      const value = source.modelContextWindow ?? source.model_context_window;
      if (typeof value === "number") {
        return value;
      }
      if (typeof value === "string") {
        const parsed = Number(value);
        return Number.isFinite(parsed) ? parsed : null;
      }
      return null;
    })(),
  };
}

function normalizePlanStepStatus(value: unknown): TurnPlanStepStatus {
  const raw = typeof value === "string" ? value : "";
  const normalized = raw.replace(/[_\s-]/g, "").toLowerCase();
  if (normalized === "inprogress") {
    return "inProgress";
  }
  if (normalized === "completed") {
    return "completed";
  }
  return "pending";
}

export function normalizePlanUpdate(
  turnId: string,
  explanation: unknown,
  plan: unknown,
): TurnPlan | null {
  const planRecord =
    plan && typeof plan === "object" && !Array.isArray(plan)
      ? (plan as Record<string, unknown>)
      : null;
  const rawSteps = (() => {
    if (Array.isArray(plan)) {
      return plan;
    }
    if (planRecord) {
      const candidate =
        planRecord.steps ??
        planRecord.plan ??
        planRecord.items ??
        planRecord.entries ??
        null;
      return Array.isArray(candidate) ? candidate : [];
    }
    return [];
  })();
  const steps = rawSteps
    .map((entry) => {
      if (!entry || typeof entry !== "object") {
        return null;
      }
      const record = entry as Record<string, unknown>;
      const step = asString(record.step ?? record.text ?? record.title ?? "");
      if (!step) {
        return null;
      }
      return {
        step,
        status: normalizePlanStepStatus(record.status),
      } satisfies TurnPlanStep;
    })
    .filter((entry): entry is TurnPlanStep => Boolean(entry));
  const note = asString(explanation ?? planRecord?.explanation ?? planRecord?.note).trim();
  if (!steps.length && !note) {
    return null;
  }
  return {
    turnId,
    explanation: note ? note : null,
    steps,
  };
}

import i18n from "@/i18n";
