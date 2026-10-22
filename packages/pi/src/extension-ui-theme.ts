import {
  Editor,
  getKeybindings,
  matchesKey,
  type EditorOptions,
  type EditorTheme,
  type KeyId,
  type TUI,
} from "@pi-rs/pi-tui-runtime";

const colors: Record<string, number> = {
  accent: 81,
  border: 60,
  borderAccent: 81,
  borderMuted: 240,
  success: 78,
  error: 203,
  warning: 221,
  muted: 245,
  dim: 242,
  text: 252,
  thinkingText: 245,
  toolTitle: 81,
  toolOutput: 252,
};
export const extensionTheme = {
  name: "dark",
  fg: (name: string, text: string) =>
    `\x1b[38;5;${colors[name] ?? 252}m${text}\x1b[39m`,
  bg: (name: string, text: string) =>
    `\x1b[48;5;${name === "selectedBg" ? 238 : 235}m${text}\x1b[49m`,
  bold: (text: string) => `\x1b[1m${text}\x1b[22m`,
  italic: (text: string) => `\x1b[3m${text}\x1b[23m`,
  underline: (text: string) => `\x1b[4m${text}\x1b[24m`,
  inverse: (text: string) => `\x1b[7m${text}\x1b[27m`,
  strikethrough: (text: string) => `\x1b[9m${text}\x1b[29m`,
  getFgAnsi: (name: string) => `\x1b[38;5;${colors[name] ?? 252}m`,
  getBgAnsi: () => "\x1b[48;5;235m",
};
export function getSelectListTheme() {
  const fg = (name: string) => (s: string) => extensionTheme.fg(name, s);
  return {
    selectedPrefix: fg("accent"),
    selectedText: fg("accent"),
    description: fg("muted"),
    scrollInfo: fg("dim"),
    noMatch: fg("warning"),
  };
}
export function getEditorTheme(): EditorTheme {
  return {
    borderColor: (s) => extensionTheme.fg("border", s),
    selectList: getSelectListTheme(),
  };
}
export function getMarkdownTheme() {
  const fg = (name: string) => (s: string) => extensionTheme.fg(name, s);
  return {
    heading: fg("accent"),
    link: fg("accent"),
    linkUrl: fg("dim"),
    code: fg("warning"),
    codeBlock: fg("text"),
    codeBlockBorder: fg("border"),
    quote: fg("muted"),
    quoteBorder: fg("border"),
    hr: fg("border"),
    listBullet: fg("accent"),
    bold: extensionTheme.bold,
    italic: extensionTheme.italic,
    strikethrough: extensionTheme.strikethrough,
    underline: extensionTheme.underline,
  };
}
const appKeys: Record<string, KeyId[]> = {
  "app.interrupt": ["escape"],
  "app.exit": ["ctrl+d"],
  "app.clipboard.pasteImage": ["ctrl+v"],
  "app.tools.expand": ["ctrl+o"],
};
export const extensionKeybindings = {
  matches(data: string, action: string): boolean {
    const keys = appKeys[action];
    return keys
      ? keys.some((k) => matchesKey(data, k))
      : getKeybindings().matches(
          data,
          action as Parameters<ReturnType<typeof getKeybindings>["matches"]>[1],
        );
  },
  getKeys(action: string): KeyId[] {
    return (
      appKeys[action] ??
      getKeybindings().getKeys(
        action as Parameters<ReturnType<typeof getKeybindings>["getKeys"]>[0],
      )
    );
  },
};
/** App actions remain callbacks into the Rust frontend. */
export class CustomEditor extends Editor {
  actionHandlers = new Map<string, () => void>();
  onEscape?: () => void;
  onCtrlD?: () => void;
  onPasteImage?: () => void;
  onExtensionShortcut?: (data: string) => boolean;
  constructor(
    tui: TUI,
    theme: EditorTheme,
    private keybindings = extensionKeybindings,
    options?: EditorOptions,
  ) {
    super(tui, theme, options);
  }
  onAction(action: string, handler: () => void) {
    this.actionHandlers.set(action, handler);
  }
  override handleInput(data: string) {
    if (this.onExtensionShortcut?.(data)) return;
    if (
      this.keybindings.matches(data, "app.interrupt") &&
      !this.isShowingAutocomplete() &&
      this.onEscape
    )
      return this.onEscape();
    if (
      this.keybindings.matches(data, "app.exit") &&
      !this.getText() &&
      this.onCtrlD
    )
      return this.onCtrlD();
    for (const [action, handler] of this.actionHandlers)
      if (this.keybindings.matches(data, action)) return handler();
    super.handleInput(data);
  }
}
