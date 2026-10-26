import type {
  ThreadTokenUsage,
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

import i18n from "@/i18n";
