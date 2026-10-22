import {
  CURSOR_MARKER,
  stripTerminalSequences,
  visibleWidth,
} from "@pi-rs/pi-tui-runtime";
import { z } from "zod";

export const uiSnapshotSchema = z.object({
  columns: z.number(),
  rows: z.number(),
  editorText: z.string(),
  toolsExpanded: z.boolean(),
});
export type UiSnapshot = z.infer<typeof uiSnapshotSchema>;
export const uiEventSchema = z.discriminatedUnion("type", [
  z.object({ type: z.literal("refresh"), snapshot: uiSnapshotSchema }),
  z.object({
    type: z.literal("input"),
    data: z.string(),
    sequence: z.number(),
    snapshot: uiSnapshotSchema,
  }),
  z.object({ type: z.literal("closed") }),
]);
type Rgb = [number, number, number];
export interface TextSpan {
  text: string;
  foreground: Rgb | null;
  background: Rgb | null;
  bold: boolean;
  italic: boolean;
  underline: boolean;
  inverse: boolean;
}
export type Placement =
  | "header"
  | "aboveEditor"
  | "belowEditor"
  | "footer"
  | "editor"
  | "overlay";
export interface Region {
  placement: Placement;
  lines: TextSpan[][];
  cursor: [number, number] | null;
  width: number;
  row: number | null;
  column: number | null;
}
const palette: Rgb[] = [
  [0, 0, 0],
  [205, 49, 49],
  [13, 188, 121],
  [229, 229, 16],
  [36, 114, 200],
  [188, 63, 188],
  [17, 168, 205],
  [229, 229, 229],
  [102, 102, 102],
  [241, 76, 76],
  [35, 209, 139],
  [245, 245, 67],
  [59, 142, 234],
  [214, 112, 214],
  [41, 184, 219],
  [255, 255, 255],
];
function indexed(n: number): Rgb {
  if (n < 16) return palette[n] ?? [255, 255, 255];
  if (n >= 232) return [n * 10 - 2312, n * 10 - 2312, n * 10 - 2312];
  const cube = [0, 95, 135, 175, 215, 255];
  n -= 16;
  return [
    cube[Math.floor(n / 36)] ?? 0,
    cube[Math.floor(n / 6) % 6] ?? 0,
    cube[n % 6] ?? 0,
  ];
}
const blank = (): Omit<TextSpan, "text"> => ({
  foreground: null,
  background: null,
  bold: false,
  italic: false,
  underline: false,
  inverse: false,
});
/** Decode styles into data. No extension escape sequence reaches the terminal. */
export function componentRegion(
  lines: string[],
  placement: Placement,
  width: number,
): Region {
  let cursor: [number, number] | null = null;
  const result = lines.slice(0, 4096).map((line, row) => {
    const marker = line.indexOf(CURSOR_MARKER);
    if (marker >= 0) cursor = [row, visibleWidth(line.slice(0, marker))];
    line = line.replaceAll(CURSOR_MARKER, "");
    const spans: TextSpan[] = [];
    let style = blank();
    let start = 0;
    const append = (text: string) => {
      text = stripTerminalSequences(text).replace(/[\x00-\x1f\x7f]/g, "");
      if (text) spans.push({ ...style, text });
    };
    for (const match of line.matchAll(/\x1b\[([0-9;:]*)m/g)) {
      append(line.slice(start, match.index));
      start = match.index + match[0].length;
      const codes = (match[1] || "0").split(/[;:]/).map(Number);
      for (let i = 0; i < codes.length; i++) {
        const code = codes[i]!;
        if (code === 0) style = blank();
        else if (code === 1) style.bold = true;
        else if (code === 22) style.bold = false;
        else if (code === 3 || code === 23) style.italic = code === 3;
        else if (code === 4 || code === 24) style.underline = code === 4;
        else if (code === 7 || code === 27) style.inverse = code === 7;
        else if (code === 39) style.foreground = null;
        else if (code === 49) style.background = null;
        else if (code >= 30 && code <= 37)
          style.foreground = indexed(code - 30);
        else if (code >= 90 && code <= 97)
          style.foreground = indexed(code - 82);
        else if (code >= 40 && code <= 47)
          style.background = indexed(code - 40);
        else if (code >= 100 && code <= 107)
          style.background = indexed(code - 92);
        else if (code === 38 || code === 48) {
          const mode = codes[++i];
          let color: Rgb | undefined;
          if (mode === 5)
            color = indexed(Math.min(255, Math.max(0, codes[++i] ?? 0)));
          else if (mode === 2)
            color = [codes[++i] ?? 0, codes[++i] ?? 0, codes[++i] ?? 0].map(
              (n) => Math.max(0, Math.min(255, n)),
            ) as Rgb;
          if (color) {
            if (code === 38) style.foreground = color;
            else style.background = color;
          }
        }
      }
    }
    append(line.slice(start));
    return spans;
  });
  return { placement, lines: result, cursor, width, row: null, column: null };
}
