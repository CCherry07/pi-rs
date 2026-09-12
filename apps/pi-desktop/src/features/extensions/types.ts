import type { ReactNode } from "react";
import type {
  DesktopExtension,
  DesktopSessionRef,
  DesktopSessionStatus,
} from "@pi-rs/desktop-sdk";

/** App adapters implement presentation and existing command invocation only. */
export type DesktopViewHost = {
  workspaceId: string | null;
  threadId: string | null;
  locale: string;
  readOnly: boolean;
  /** Frozen inherited transcript snapshots bypass live extension rendering entirely. */
  frozen?: boolean;
  widgets: Readonly<Record<string, unknown>>;
  runCommand(name: string, args: string): Promise<void>;
  renderSession(ref: DesktopSessionRef): ReactNode;
  sessionStatus(ref: DesktopSessionRef): DesktopSessionStatus | undefined;
  renderMarkdown(content: string): ReactNode;
};

export type LoadedDesktopExtension = {
  definition: DesktopExtension;
  revision: string;
  css?: string;
  scope?: "builtin" | "global" | "project";
};

/** JavaScript and CSS are read from a trusted package by the native host. */
export type DesktopExtensionSource = {
  id: string;
  revision: string;
  javascript: string;
  css: string;
  scope?: "global" | "project";
};

export type DesktopExtensionCatalog = {
  extensions: readonly DesktopExtensionSource[];
  projectTrusted: boolean;
  error?: string | null;
};

export type DesktopExtensionImporter = (source: DesktopExtensionSource) => Promise<unknown>;
