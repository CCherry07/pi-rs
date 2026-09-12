import type { ComponentType, ReactNode } from "react";

export type JsonValue = null | boolean | number | string | readonly JsonValue[] | { readonly [key: string]: JsonValue };

/** The original tool/custom-message payload remains available in data. */
export type DesktopToolItem = {
  id: string;
  toolName?: string;
  customType?: string;
  toolType: string;
  title: string;
  detail: string;
  status?: string;
  output?: string;
  data?: Readonly<Record<string, unknown>>;
};

/** Session identifiers are opaque. The host resolves them in the current workspace. */
export type DesktopSessionRef = {
  sessionId?: string;
  isolatedSessionId?: string;
  ownerSessionId?: string;
};

export type DesktopSessionStatus = {
  isProcessing: boolean;
  totalTokens?: number;
};

export type DesktopViewContext = {
  pluginId: string;
  workspaceId: string | null;
  threadId: string | null;
  locale: string;
  readOnly: boolean;
  /** Aborted when the view is unmounted, its session changes, or its generation retires. */
  signal: AbortSignal;
  /** Opaque live UI values; use plugin-qualified keys such as "my-plugin.progress". */
  widgets: Readonly<Record<string, unknown>>;
  /** Invokes an existing registered command in this view's session. */
  runCommand(name: string, args: string): Promise<void>;
  renderSession(ref: DesktopSessionRef): ReactNode;
  sessionStatus(ref: DesktopSessionRef): DesktopSessionStatus | undefined;
};

export type DesktopItemViewProps = {
  item: DesktopToolItem;
  expanded: boolean;
  onToggle(): void;
};

export type DesktopView =
  | {
      id: string;
      slot: "tool.result";
      target: { tool: string } | { customType: string };
      component: ComponentType<DesktopItemViewProps>;
    }
  | {
      id: string;
      slot: "workspace.panel";
      title: string;
      component: ComponentType;
    };

export type DesktopExtension = {
  apiVersion: 1;
  id: string;
  views: readonly DesktopView[];
};

export function defineDesktopExtension(definition: Omit<DesktopExtension, "apiVersion">): DesktopExtension;
export function useDesktopContext(): DesktopViewContext;

export type WidgetChannel<T> = Readonly<{
  key: string;
  decode(value: unknown): T;
}>;

export type WidgetSnapshot<T> =
  | { status: "missing"; value: undefined; error: null }
  | { status: "ready"; value: T; error: null }
  | { status: "invalid"; value: undefined; error: Error };

/** Defines a plugin-owned decoder for one namespaced widget value. */
export function defineWidgetChannel<T>(channel: WidgetChannel<T>): WidgetChannel<T>;
/** Reads and decodes a widget without exposing malformed values to the view. */
export function useWidget<T>(channel: WidgetChannel<T>): WidgetSnapshot<T>;

export type PluginCommandController<TArgs> = Readonly<{
  pending: boolean;
  error: string | null;
  run(args: TArgs): Promise<boolean>;
  clearError(): void;
}>;

/** Owns serialization, pending/error state, and retired-view handling. */
export function usePluginCommand<TArgs = undefined>(
  name: string,
  encode?: (args: TArgs) => string,
): PluginCommandController<TArgs>;

export type ToolCardProps = {
  title: ReactNode;
  summary?: ReactNode;
  status?: ReactNode;
  actions?: ReactNode;
  expanded: boolean;
  onToggle(): void;
  children?: ReactNode;
  className?: string;
  label?: string;
  toggleLabel?: string;
};

/** Actions are separate from the disclosure button, so they never toggle the card. */
export function ToolCard(props: ToolCardProps): ReactNode;
export type StatusBadgeProps = {
  state: "idle" | "processing" | "completed" | "failed" | "warning" | "interrupted" | "unknown";
  label: string;
};
export function StatusBadge(props: StatusBadgeProps): ReactNode;
export function Markdown(props: { children: string; className?: string }): ReactNode;
/** A lazy, host-rendered conversation. Unmounting the view never stops the session. */
export function SessionView(props: { reference: DesktopSessionRef; className?: string }): ReactNode;

export type InlineCommandFormProps = {
  ariaLabel: string;
  placeholder?: string;
  submitLabel: string;
  pendingLabel?: string;
  pending?: boolean;
  error?: string | null;
  disabled?: boolean;
  className?: string;
  onSubmit(value: string): Promise<boolean>;
};

/** Keeps a draft until an asynchronous command succeeds. */
export function InlineCommandForm(props: InlineCommandFormProps): ReactNode;
