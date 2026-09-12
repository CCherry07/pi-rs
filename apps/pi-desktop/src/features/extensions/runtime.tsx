import * as React from "react";
import * as ReactDOM from "react-dom";
import * as ReactDOMClient from "react-dom/client";
import * as JSXRuntime from "react/jsx-runtime";
import * as JSXDevRuntime from "react/jsx-dev-runtime";
import * as DesktopSDK from "@pi-rs/desktop-sdk";
import type {
  DesktopSessionRef,
  DesktopViewContext,
  StatusBadgeProps,
  ToolCardProps,
} from "@pi-rs/desktop-sdk";
import type { DesktopViewHost } from "./types";

type ScopedDesktopContext = {
  context: DesktopViewContext;
  renderMarkdown: DesktopViewHost["renderMarkdown"];
};

export const DesktopContext = React.createContext<ScopedDesktopContext | null>(null);

export function useDesktopContext(): DesktopViewContext {
  const scope = React.useContext(DesktopContext);
  if (!scope) throw new Error("Desktop extension components must be mounted by the desktop extension host.");
  return scope.context;
}

/** Controlled disclosure keeps view state in the transcript's existing expansion store. */
export function ToolCard({
  title, summary, status, actions, expanded, onToggle, children, className, label, toggleLabel,
}: ToolCardProps) {
  const bodyId = React.useId();
  return (
    <section className={`pi-tool-card${expanded ? " is-expanded" : ""}${className ? ` ${className}` : ""}`} aria-label={label}>
      <div className="pi-tool-card-header">
        <button
          type="button"
          className="pi-tool-card-toggle"
          aria-expanded={expanded}
          aria-controls={bodyId}
          aria-label={toggleLabel}
          onClick={onToggle}
        >
          <span className="pi-tool-card-chevron" aria-hidden>›</span>
          <span className="pi-tool-card-heading">
            <span className="pi-tool-card-title">{title}</span>
            {summary && <> <span className="pi-tool-card-summary">{summary}</span></>}
          </span>
          {status && <> <span className="pi-tool-card-status">{status}</span></>}
        </button>
        {actions && <div className="pi-tool-card-actions">{actions}</div>}
      </div>
      {expanded && <div className="pi-tool-card-body" id={bodyId}>{children}</div>}
    </section>
  );
}

export function StatusBadge({ state, label }: StatusBadgeProps) {
  return (
    <span className={`pi-status-badge pi-status-${state}`} role="status">
      <span className="pi-status-badge-dot" aria-hidden />
      {label}
    </span>
  );
}

export function Markdown({ children, className }: { children: string; className?: string }) {
  const scope = React.useContext(DesktopContext);
  if (!scope) throw new Error("Markdown requires the desktop extension host.");
  return <div className={className}>{scope.renderMarkdown(children)}</div>;
}

export function SessionView({ reference, className }: { reference: DesktopSessionRef; className?: string }) {
  const context = useDesktopContext();
  return <div className={className}>{context.renderSession(reference)}</div>;
}

/** Bundles externalize React/JSX imports to these exact host module instances. */
const runtime = Object.freeze({
  apiVersion: 1,
  reactVersion: React.version,
  useDesktopContext,
  components: Object.freeze({ ToolCard, StatusBadge, Markdown, SessionView }),
  modules: Object.freeze({
    react: React,
    "react-dom": ReactDOM,
    "react-dom/client": ReactDOMClient,
    "react/jsx-runtime": JSXRuntime,
    "react/jsx-dev-runtime": JSXDevRuntime,
    "@pi-rs/desktop-sdk": DesktopSDK,
  }),
});

Object.defineProperty(globalThis, Symbol.for("pi.desktop.runtime.v1"), {
  configurable: true,
  value: runtime,
});
