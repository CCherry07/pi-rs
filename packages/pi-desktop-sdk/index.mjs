const RUNTIME_KEY = Symbol.for("pi.desktop.runtime.v1");

function runtime() {
  const value = globalThis[RUNTIME_KEY];
  if (!value || value.apiVersion !== 1) {
    throw new Error("This extension requires the Pi Desktop API version 1 host.");
  }
  return value;
}

export function defineDesktopExtension(definition) {
  return Object.freeze({ ...definition, apiVersion: 1 });
}

export function useDesktopContext() {
  return runtime().useDesktopContext();
}

function validWidgetKey(key) {
  return typeof key === "string"
    && key.length > 0
    && key.length <= 128
    && key.includes(".")
    && key.split(".").every(Boolean)
    && /^[a-zA-Z0-9._-]+$/.test(key);
}

export function defineWidgetChannel(channel) {
  if (!channel || !validWidgetKey(channel.key) || typeof channel.decode !== "function") {
    throw new Error("Desktop widget channels require a namespaced key and decoder.");
  }
  return Object.freeze({ key: channel.key, decode: channel.decode });
}

export function useWidget(channel) {
  const host = runtime();
  const context = host.useDesktopContext();
  const present = Object.prototype.hasOwnProperty.call(context.widgets, channel.key);
  const raw = context.widgets[channel.key];
  return host.modules.react.useMemo(() => {
    if (!present) return Object.freeze({ status: "missing", value: undefined, error: null });
    try {
      return Object.freeze({ status: "ready", value: channel.decode(raw), error: null });
    } catch (reason) {
      const error = reason instanceof Error ? reason : new Error(String(reason));
      return Object.freeze({ status: "invalid", value: undefined, error });
    }
  }, [channel, present, raw]);
}

function defaultCommandEncoder(args) {
  if (args === undefined) return "";
  const encoded = JSON.stringify(args);
  if (typeof encoded !== "string") throw new Error("Desktop command arguments must be JSON serializable.");
  return encoded;
}

export function usePluginCommand(name, encode = defaultCommandEncoder) {
  if (typeof name !== "string" || !name || /[\s/]/.test(name) || typeof encode !== "function") {
    throw new Error("Desktop commands require an exact registered command name.");
  }
  const host = runtime();
  const React = host.modules.react;
  const context = host.useDesktopContext();
  const inFlight = React.useRef(false);
  const mounted = React.useRef(false);
  const [state, setState] = React.useState({ pending: false, error: null });
  React.useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  const clearError = React.useCallback(() => {
    if (mounted.current) setState(current => ({ ...current, error: null }));
  }, []);
  const run = React.useCallback(async args => {
    if (inFlight.current || context.signal.aborted) return false;
    inFlight.current = true;
    setState({ pending: true, error: null });
    try {
      await context.runCommand(name, encode(args));
      return true;
    } catch (reason) {
      if (mounted.current && !context.signal.aborted) {
        setState({ pending: false, error: reason instanceof Error ? reason.message : String(reason) });
      }
      return false;
    } finally {
      inFlight.current = false;
      if (mounted.current && !context.signal.aborted) setState(current => ({ ...current, pending: false }));
    }
  }, [context, encode, name]);
  return React.useMemo(
    () => Object.freeze({ pending: state.pending, error: state.error, run, clearError }),
    [state.pending, state.error, run, clearError],
  );
}

export function ToolCard(props) {
  const host = runtime();
  return host.modules.react.createElement(host.components.ToolCard, props);
}

export function StatusBadge(props) {
  const host = runtime();
  return host.modules.react.createElement(host.components.StatusBadge, props);
}

export function Markdown(props) {
  const host = runtime();
  return host.modules.react.createElement(host.components.Markdown, props);
}

export function SessionView(props) {
  const host = runtime();
  return host.modules.react.createElement(host.components.SessionView, props);
}

export function InlineCommandForm({
  ariaLabel, placeholder, submitLabel, pendingLabel, pending = false, error = null,
  disabled = false, className, onSubmit,
}) {
  const React = runtime().modules.react;
  const [draft, setDraft] = React.useState("");
  const submitting = React.useRef(false);
  const mounted = React.useRef(false);
  React.useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  const submit = async event => {
    event.preventDefault();
    const value = draft.trim();
    if (!value || pending || disabled || submitting.current) return;
    submitting.current = true;
    try {
      if (await onSubmit(value) && mounted.current) setDraft("");
    } finally {
      submitting.current = false;
    }
  };
  return React.createElement(
    "div",
    { className: `pi-inline-command${className ? ` ${className}` : ""}` },
    React.createElement(
      "form",
      { className: "pi-inline-command-form", onSubmit: event => void submit(event) },
      React.createElement("input", {
        "aria-label": ariaLabel,
        placeholder,
        value: draft,
        disabled: disabled || pending,
        onChange: event => setDraft(event.target.value),
      }),
      React.createElement(
        "button",
        { type: "submit", disabled: disabled || pending || !draft.trim() },
        pending && pendingLabel ? pendingLabel : submitLabel,
      ),
    ),
    error && React.createElement("p", { role: "alert", className: "pi-inline-command-error" }, error),
  );
}
