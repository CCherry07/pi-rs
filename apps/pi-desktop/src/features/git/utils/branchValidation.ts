export type BranchValidationMessages = {
  dot: string;
  spaces: string;
  slashEnds: string;
  doubleSlash: string;
  lock: string;
  doubleDot: string;
  reflog: string;
  invalidChars: string;
  trailingDot: string;
};

const getDefaultMessages = (): BranchValidationMessages => ({
  dot: i18n.t("branchValidation.dot", { ns: "git" }),
  spaces: i18n.t("branchValidation.spaces", { ns: "git" }),
  slashEnds: i18n.t("branchValidation.slashEnds", { ns: "git" }),
  doubleSlash: i18n.t("branchValidation.doubleSlash", { ns: "git" }),
  lock: i18n.t("branchValidation.lock", { ns: "git" }),
  doubleDot: i18n.t("branchValidation.doubleDot", { ns: "git" }),
  reflog: i18n.t("branchValidation.reflog", { ns: "git" }),
  invalidChars: i18n.t("branchValidation.invalidChars", { ns: "git" }),
  trailingDot: i18n.t("branchValidation.trailingDot", { ns: "git" }),
});

export function validateBranchName(
  name: string,
  messages: BranchValidationMessages = getDefaultMessages(),
): string | null {
  const trimmed = name.trim();
  if (trimmed.length === 0) {
    return null;
  }
  if (trimmed === "." || trimmed === "..") {
    return messages.dot;
  }
  if (/\s/.test(trimmed)) {
    return messages.spaces;
  }
  if (trimmed.startsWith("/") || trimmed.endsWith("/")) {
    return messages.slashEnds;
  }
  if (trimmed.includes("//")) {
    return messages.doubleSlash;
  }
  if (trimmed.endsWith(".lock")) {
    return messages.lock;
  }
  if (trimmed.includes("..")) {
    return messages.doubleDot;
  }
  if (trimmed.includes("@{")) {
    return messages.reflog;
  }
  const invalidChars = ["~", "^", ":", "?", "*", "[", "\\"];
  if (invalidChars.some((char) => trimmed.includes(char))) {
    return messages.invalidChars;
  }
  if (trimmed.endsWith(".")) {
    return messages.trailingDot;
  }
  return null;
}
import i18n from "@/i18n";
