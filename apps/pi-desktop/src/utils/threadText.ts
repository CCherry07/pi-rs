import i18n from "@/i18n";
import type { ConversationItem } from "../types";

function formatMessage(item: Extract<ConversationItem, { kind: "message" }>) {
  const roleLabel = i18n.t(
    item.role === "user" ? "transcript.user" : "transcript.assistant",
    { ns: "messages" },
  );
  return `${roleLabel}: ${item.text}`;
}

function formatReasoning(item: Extract<ConversationItem, { kind: "reasoning" }>) {
  const parts = [i18n.t("transcript.reasoning", { ns: "messages" })];
  if (item.summary) {
    parts.push(item.summary);
  }
  if (item.content) {
    parts.push(item.content);
  }
  return parts.join("\n");
}

function formatUserInput(item: Extract<ConversationItem, { kind: "userInput" }>) {
  const lines = item.questions.map((entry, index) => {
    const title =
      entry.question ||
      entry.header ||
      i18n.t("transcript.question", { ns: "messages", number: index + 1 });
    const answers =
      entry.answers.length > 0
        ? entry.answers.join(" | ")
        : i18n.t("transcript.noAnswer", { ns: "messages" });
    return `- ${title}: ${answers}`;
  });
  return [i18n.t("transcript.inputAnswered", { ns: "messages" }), ...lines].join("\n");
}

function formatTool(item: Extract<ConversationItem, { kind: "tool" }>) {
  const parts = [i18n.t("transcript.tool", { ns: "messages", title: item.title })];
  if (item.detail) {
    parts.push(item.detail);
  }
  if (item.status) {
    parts.push(i18n.t("transcript.status", { ns: "messages", status: item.status }));
  }
  if (item.output) {
    parts.push(item.output);
  }
  if (item.changes && item.changes.length > 0) {
    parts.push(
      `${i18n.t("transcript.changes", { ns: "messages" })}\n` +
        item.changes
          .map((change) => `- ${change.path}${change.kind ? ` (${change.kind})` : ""}`)
          .join("\n"),
    );
  }
  return parts.join("\n");
}

function formatDiff(item: Extract<ConversationItem, { kind: "diff" }>) {
  const header = i18n.t("transcript.diff", {
    ns: "messages",
    title: item.title,
  });
  const status = item.status
    ? i18n.t("transcript.status", { ns: "messages", status: item.status })
    : null;
  return [header, status, item.diff].filter(Boolean).join("\n");
}

function formatExplore(item: Extract<ConversationItem, { kind: "explore" }>) {
  const title = i18n.t(
    item.status === "exploring" ? "transcript.exploring" : "transcript.explored",
    { ns: "messages" },
  );
  const lines = item.entries.map((entry) => {
    const prefix = entry.kind[0].toUpperCase() + entry.kind.slice(1);
    return `- ${prefix} ${entry.label}${entry.detail ? ` (${entry.detail})` : ""}`;
  });
  return [title, ...lines].join("\n");
}

export function buildThreadTranscript(items: ConversationItem[]) {
  return items
    .map((item) => {
      switch (item.kind) {
        case "message":
          return formatMessage(item);
        case "userInput":
          return formatUserInput(item);
        case "reasoning":
          return formatReasoning(item);
        case "explore":
          return formatExplore(item);
        case "tool":
          return formatTool(item);
        case "diff":
          return formatDiff(item);
      }
      return "";
    })
    .filter((value) => value.trim().length > 0)
    .join("\n\n");
}
