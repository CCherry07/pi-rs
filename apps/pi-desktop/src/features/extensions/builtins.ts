import subagents from "../../../../../plugins/features/pi-plugin-subagents/desktop/src/index";
import type { LoadedDesktopExtension } from "./types";

/** Product composition lives here; the extension host remains adapter-agnostic. */
export const bundledDesktopExtensions: readonly LoadedDesktopExtension[] = [
  { definition: subagents, revision: "builtin", scope: "builtin" },
];
