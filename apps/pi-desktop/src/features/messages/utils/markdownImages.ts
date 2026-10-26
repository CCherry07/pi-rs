import { convertFileSrc } from "@tauri-apps/api/core";
import { parseFileUrlLocation, toFileUrl } from "../../../utils/fileLinks";

export function resolveMarkdownImageSource(
  source: string,
  workspacePath?: string | null,
): string | undefined {
  const url = source.trim();
  if (!url) return undefined;
  if (url.startsWith("//")) return `https:${url}`;
  if (
    /^(?:https?:\/\/|blob:|asset:\/\/)/i.test(url) ||
    /^data:image\/[a-z0-9.+-]+(?:;[^,]*)?,/i.test(url)
  ) {
    return url;
  }

  const windowsPath = /^[a-z]:[\\/]/i.test(url);
  const fileUrl = /^file:\/\//i.test(url);
  if (/^[a-z][a-z0-9+.-]*:/i.test(url) && !windowsPath && !fileUrl) {
    return undefined;
  }

  try {
    let resolvedUrl: string;
    if (windowsPath) {
      resolvedUrl = toFileUrl(decodeURIComponent(url), null, null);
    } else if (fileUrl) {
      resolvedUrl = url;
    } else if (url.startsWith("/")) {
      resolvedUrl = new URL(url, "file:///").href;
    } else if (workspacePath) {
      const base = `${toFileUrl(workspacePath, null, null).replace(/\/$/, "")}/`;
      resolvedUrl = new URL(url, base).href;
    } else {
      return url;
    }
    const target = parseFileUrlLocation(resolvedUrl);
    return target ? convertFileSrc(target.path) : undefined;
  } catch {
    return undefined;
  }
}
