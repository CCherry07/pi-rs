/**
 * Import-compatible, terminal-inert Pi TUI surface.
 *
 * JavaScript extensions may combine agent tools and commands with optional Pi
 * renderers in one module graph. Resolving those imports must not discard the
 * non-UI contributions, but Node extensions must never own the Rust terminal.
 * These exports therefore provide only pure text helpers and inert objects.
 */

const ANSI_CSI_PATTERN = /\u001B\[[0-?]*[ -/]*[@-~]/g
const ANSI_OSC_PATTERN = /\u001B\][^\u0007]*(?:\u0007|\u001B\\)/g
const COMBINING_MARK_PATTERN = /\p{Mark}/u

function plainText(value: unknown): string {
  return typeof value === 'string' ? stripTerminalSequences(value) : ''
}

function codePointWidth(value: string): number {
  const codePoint = value.codePointAt(0) ?? 0
  if (codePoint === 0 || codePoint < 0x20 || (codePoint >= 0x7f && codePoint < 0xa0)) return 0
  if (COMBINING_MARK_PATTERN.test(value)) return 0
  if (
    codePoint >= 0x1100 && (
      codePoint <= 0x115f
      || codePoint === 0x2329
      || codePoint === 0x232a
      || (codePoint >= 0x2e80 && codePoint <= 0xa4cf && codePoint !== 0x303f)
      || (codePoint >= 0xac00 && codePoint <= 0xd7a3)
      || (codePoint >= 0xf900 && codePoint <= 0xfaff)
      || (codePoint >= 0xfe10 && codePoint <= 0xfe19)
      || (codePoint >= 0xfe30 && codePoint <= 0xfe6f)
      || (codePoint >= 0xff00 && codePoint <= 0xff60)
      || (codePoint >= 0xffe0 && codePoint <= 0xffe6)
      || (codePoint >= 0x1f300 && codePoint <= 0x1faff)
      || (codePoint >= 0x20000 && codePoint <= 0x3fffd)
    )
  ) return 2
  return 1
}

export function stripTerminalSequences(value: string): string {
  return value.replace(ANSI_OSC_PATTERN, '').replace(ANSI_CSI_PATTERN, '')
}

export function visibleWidth(value: string): number {
  return [...stripTerminalSequences(value)].reduce((width, character) => width + codePointWidth(character), 0)
}

function takeColumns(value: string, width: number): string {
  if (width <= 0) return ''
  let result = ''
  let used = 0
  for (const character of [...stripTerminalSequences(value)]) {
    const characterWidth = codePointWidth(character)
    if (used + characterWidth > width) break
    result += character
    used += characterWidth
  }
  return result
}

export function truncateToWidth(
  value: string,
  width: number,
  ellipsis = '',
  _preserveAnsi = false,
): string {
  const targetWidth = Math.max(0, Math.floor(width))
  if (visibleWidth(value) <= targetWidth) return value
  const suffix = takeColumns(ellipsis, targetWidth)
  return `${takeColumns(value, targetWidth - visibleWidth(suffix))}${suffix}`
}

export function sliceByColumn(value: string, start: number, end = Number.MAX_SAFE_INTEGER): string {
  const plain = stripTerminalSequences(value)
  const prefix = takeColumns(plain, Math.max(0, end))
  const removed = takeColumns(prefix, Math.max(0, start))
  return prefix.slice(removed.length)
}

export function wrapTextWithAnsi(value: string, width: number): string[] {
  const lines: string[] = []
  for (const sourceLine of stripTerminalSequences(value).split('\n')) {
    let remaining = sourceLine
    do {
      const line = takeColumns(remaining, Math.max(1, width))
      lines.push(line)
      remaining = remaining.slice(line.length)
    } while (remaining.length > 0)
  }
  return lines
}

const inertTui = Object.freeze({ requestRender(): void {} })

class InertComponent {
  text: string
  value = ''
  tui: { requestRender(): void }
  onSelect?: (value: unknown) => void
  onCancel?: () => void

  constructor(...arguments_: unknown[]) {
    this.text = plainText(arguments_[0])
    this.value = this.text
    this.tui = typeof arguments_[0] === 'object' && arguments_[0] !== null
      && typeof Reflect.get(arguments_[0], 'requestRender') === 'function'
      ? arguments_[0] as { requestRender(): void }
      : inertTui
  }

  render(width = Number.MAX_SAFE_INTEGER): string[] {
    return this.text.split('\n').map(line => truncateToWidth(line, width))
  }

  invalidate(): void {}
  handleInput(_data: string): void {}
  setText(value: string): void { this.text = value; this.value = value }
  getText(): string { return this.text }
  setValue(value: string): void { this.setText(value) }
  getValue(): string { return this.value }
  insertTextAtCursor(value: string): void { this.setText(`${this.text}${value}`) }
  setAutocompleteProvider(_provider: unknown): void {}
  addChild(_child: unknown): void {}
  removeChild(_child: unknown): void {}
  clear(): void { this.setText('') }
  matches(_data: string, _key: string): boolean { return false }
  start(): void {}
  stop(): void {}
  dispose(): void {}
}

export {
  InertComponent as Box,
  InertComponent as CancellableLoader,
  InertComponent as CombinedAutocompleteProvider,
  InertComponent as Container,
  InertComponent as Editor,
  InertComponent as HStack,
  InertComponent as Image,
  InertComponent as Input,
  InertComponent as KeybindingsManager,
  InertComponent as Loader,
  InertComponent as Markdown,
  InertComponent as Marked,
  InertComponent as ProcessTerminal,
  InertComponent as ScrollView,
  InertComponent as SelectList,
  InertComponent as SettingsList,
  InertComponent as Spacer,
  InertComponent as StdinBuffer,
  InertComponent as Text,
  InertComponent as TruncatedText,
  InertComponent as TuiAltScreen,
  InertComponent as TuiMainScreen,
  InertComponent as VStack,
}

export const CURSOR_MARKER = ''
export const Key = Object.freeze({
  escape: 'escape',
  esc: 'esc',
  enter: 'enter',
  return: 'return',
  tab: 'tab',
  space: 'space',
  backspace: 'backspace',
  delete: 'delete',
  insert: 'insert',
  clear: 'clear',
  home: 'home',
  end: 'end',
  pageUp: 'pageUp',
  pageDown: 'pageDown',
  up: 'up',
  down: 'down',
  left: 'left',
  right: 'right',
  f1: 'f1',
  f2: 'f2',
  f3: 'f3',
  f4: 'f4',
  f5: 'f5',
  f6: 'f6',
  f7: 'f7',
  f8: 'f8',
  f9: 'f9',
  f10: 'f10',
  f11: 'f11',
  f12: 'f12',
  backtick: '`',
  hyphen: '-',
  equals: '=',
  leftbracket: '[',
  rightbracket: ']',
  backslash: '\\',
  semicolon: ';',
  quote: "'",
  comma: ',',
  period: '.',
  slash: '/',
  exclamation: '!',
  at: '@',
  hash: '#',
  dollar: '$',
  percent: '%',
  caret: '^',
  ampersand: '&',
  asterisk: '*',
  leftparen: '(',
  rightparen: ')',
  underscore: '_',
  plus: '+',
  pipe: '|',
  tilde: '~',
  leftbrace: '{',
  rightbrace: '}',
  colon: ':',
  lessthan: '<',
  greaterthan: '>',
  question: '?',
  ctrl: (key: string): string => `ctrl+${key}`,
  shift: (key: string): string => `shift+${key}`,
  alt: (key: string): string => `alt+${key}`,
  super: (key: string): string => `super+${key}`,
  ctrlShift: (key: string): string => `ctrl+shift+${key}`,
  shiftCtrl: (key: string): string => `shift+ctrl+${key}`,
  ctrlAlt: (key: string): string => `ctrl+alt+${key}`,
  altCtrl: (key: string): string => `alt+ctrl+${key}`,
  shiftAlt: (key: string): string => `shift+alt+${key}`,
  altShift: (key: string): string => `alt+shift+${key}`,
  ctrlSuper: (key: string): string => `ctrl+super+${key}`,
  superCtrl: (key: string): string => `super+ctrl+${key}`,
  shiftSuper: (key: string): string => `shift+super+${key}`,
  superShift: (key: string): string => `super+shift+${key}`,
  altSuper: (key: string): string => `alt+super+${key}`,
  superAlt: (key: string): string => `super+alt+${key}`,
  ctrlShiftAlt: (key: string): string => `ctrl+shift+alt+${key}`,
  ctrlShiftSuper: (key: string): string => `ctrl+shift+super+${key}`,
})
export const TUI_KEYBINDINGS = Object.freeze({})

export function getKeybindings(): InertComponent { return new InertComponent() }
export function setKeybindings(_value: unknown): void {}
export function matchesKey(_data: string, _key: string): boolean { return false }
export function isKeyRelease(_data: string): boolean { return false }
export function isKeyRepeat(_data: string): boolean { return false }
export function isKittyProtocolActive(): boolean { return false }
export function setKittyProtocolActive(_active: boolean): void {}
export function decodeKittyPrintable(_data: string): undefined { return undefined }
export function parseKey(_data: string): undefined { return undefined }

export function fuzzyFilter<T>(_query: string, values: readonly T[]): T[] { return [...values] }
export function fuzzyMatch(_query: string, _value: string): undefined { return undefined }
export function renderLatex(value: string): string { return plainText(value) }
export function parseOsc11BackgroundColor(_value: string): undefined { return undefined }
export function parseTerminalColorSchemeReport(_value: string): undefined { return undefined }
export function getOsc8LinkAtColumn(_value: string, _column: number): undefined { return undefined }
export function compositeTuiLine(...values: unknown[]): string { return values.map(plainText).join('') }
export function isFocusable(_value: unknown): boolean { return false }
export function isViewportTUI(_value: unknown): boolean { return false }

export function allocateImageId(): number { return 0 }
export function calculateImageRows(): number { return 0 }
export function deleteAllKittyImages(): void {}
export function deleteKittyImage(): void {}
export function detectCapabilities(): Record<string, never> { return {} }
export function getCapabilities(): Record<string, never> { return {} }
export function resetCapabilitiesCache(): void {}
export function setCapabilities(_value: unknown): void {}
export function setCellDimensions(_value: unknown): void {}
export function getCellDimensions(): undefined { return undefined }
export function getGifDimensions(): undefined { return undefined }
export function getImageDimensions(): undefined { return undefined }
export function getJpegDimensions(): undefined { return undefined }
export function getPngDimensions(): undefined { return undefined }
export function getWebpDimensions(): undefined { return undefined }
export function encodeITerm2(): string { return '' }
export function encodeKitty(): string { return '' }
export function hyperlink(_url: string, text: string): string { return plainText(text) }
export function imageFallback(): string { return '' }
export function renderImage(): string[] { return [] }
