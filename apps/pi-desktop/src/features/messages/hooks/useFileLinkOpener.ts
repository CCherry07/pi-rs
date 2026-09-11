import { useCallback } from "react";
import { useTranslation } from "react-i18next";
import type { MouseEvent } from "react";
import { Menu, MenuItem, PredefinedMenuItem } from "@tauri-apps/api/menu";
import { LogicalPosition } from "@tauri-apps/api/dpi";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { reportOpenFailure } from "../../../services/sentryPrivacy";
import { openWorkspaceIn } from "../../../services/tauri";
import { pushErrorToast } from "../../../services/toasts";
import type { OpenAppTarget } from "../../../types";
import {
  type ParsedFileLocation,
  toFileUrl,
} from "../../../utils/fileLinks";
import {
  isAbsolutePath,
  joinWorkspacePath,
  revealInFileManagerLabel,
} from "../../../utils/platformPaths";
import { resolveMountedWorkspacePath } from "../utils/mountedWorkspacePaths";

type OpenTarget = {
  id: string;
  label: string;
  appName?: string | null;
  kind: OpenAppTarget["kind"];
  command?: string | null;
  args: string[];
};

const DEFAULT_OPEN_TARGET: OpenTarget = {
  id: "vscode",
  label: "VS Code",
  appName: "Visual Studio Code",
  kind: "app",
  command: null,
  args: [],
};

const resolveAppName = (target: OpenTarget) => (target.appName ?? "").trim();
const resolveCommand = (target: OpenTarget) => (target.command ?? "").trim();

const canOpenTarget = (target: OpenTarget) => {
  if (target.kind === "finder") {
    return true;
  }
  if (target.kind === "command") {
    return Boolean(resolveCommand(target));
  }
  return Boolean(resolveAppName(target));
};

function resolveOpenTarget(
  openTargets: OpenAppTarget[],
  selectedOpenAppId: string,
): OpenTarget {
  return {
    ...DEFAULT_OPEN_TARGET,
    ...(openTargets.find((entry) => entry.id === selectedOpenAppId) ??
      openTargets[0]),
  };
}

function resolveFilePath(path: string, workspacePath?: string | null) {
  const trimmed = path.trim();
  if (!workspacePath) {
    return trimmed;
  }
  const mountedWorkspacePath = resolveMountedWorkspacePath(trimmed, workspacePath);
  if (mountedWorkspacePath) {
    return mountedWorkspacePath;
  }
  if (isAbsolutePath(trimmed)) {
    return trimmed;
  }
  return joinWorkspacePath(workspacePath, trimmed);
}

function resolveFileLinkContext(
  fileLocation: ParsedFileLocation,
  workspacePath?: string | null,
) {
  return {
    fileLocation,
    resolvedPath: resolveFilePath(fileLocation.path, workspacePath),
  };
}

export function useFileLinkOpener(
  workspacePath: string | null,
  openTargets: OpenAppTarget[],
  selectedOpenAppId: string,
) {
  const { t } = useTranslation("messages");
  const reportOpenError = useCallback(
    (error: unknown, kind: OpenTarget["kind"]) => {
      const message = error instanceof Error ? error.message : String(error);
      reportOpenFailure("file-link-open", kind);
      pushErrorToast({
        title: t("errors.openFile"),
        message,
      });
    },
    [t],
  );

  const openFileLink = useCallback(
    async (targetLocation: ParsedFileLocation) => {
      const target = resolveOpenTarget(openTargets, selectedOpenAppId);
      const { fileLocation, resolvedPath } = resolveFileLinkContext(
        targetLocation,
        workspacePath,
      );
      const openLocation = {
        ...(fileLocation.line !== null ? { line: fileLocation.line } : {}),
        ...(fileLocation.column !== null ? { column: fileLocation.column } : {}),
      };

      try {
        if (!canOpenTarget(target)) {
          return;
        }
        if (target.kind === "finder") {
          await revealItemInDir(resolvedPath);
          return;
        }

        if (target.kind === "command") {
          const command = resolveCommand(target);
          if (!command) {
            return;
          }
          await openWorkspaceIn(resolvedPath, {
            command,
            args: target.args,
            ...openLocation,
          });
          return;
        }

        const appName = resolveAppName(target);
        if (!appName) {
          return;
        }
        await openWorkspaceIn(resolvedPath, {
          appName,
          args: target.args,
          ...openLocation,
        });
      } catch (error) {
        reportOpenError(error, target.kind);
      }
    },
    [openTargets, reportOpenError, selectedOpenAppId, workspacePath],
  );

  const showFileLinkMenu = useCallback(
    async (event: MouseEvent, targetLocation: ParsedFileLocation) => {
      event.preventDefault();
      event.stopPropagation();
      const target = resolveOpenTarget(openTargets, selectedOpenAppId);
      const { fileLocation, resolvedPath } = resolveFileLinkContext(
        targetLocation,
        workspacePath,
      );
      const appName = resolveAppName(target);
      const command = resolveCommand(target);
      const canOpen = canOpenTarget(target);
      const openLabel =
        target.kind === "finder"
          ? revealInFileManagerLabel()
          : target.kind === "command"
            ? command
              ? t("composer.openIn", { target: target.label })
              : t("composer.setCommand")
            : appName
              ? t("composer.openIn", { target: appName })
              : t("composer.setAppName");
      const items = [
        await MenuItem.new({
          text: openLabel,
          enabled: canOpen,
          action: async () => {
            await openFileLink(fileLocation);
          },
        }),
        ...(target.kind === "finder"
          ? []
          : [
              await MenuItem.new({
                text: revealInFileManagerLabel(),
                action: async () => {
                  try {
                    await revealItemInDir(resolvedPath);
                  } catch (error) {
                    reportOpenError(error, "finder");
                  }
                },
              }),
            ]),
        await MenuItem.new({
          text: t("composer.downloadLinkedFile"),
          enabled: false,
        }),
        await MenuItem.new({
          text: t("composer.copyLink"),
          action: async () => {
            const link = toFileUrl(resolvedPath, fileLocation.line, fileLocation.column);
            try {
              await navigator.clipboard.writeText(link);
            } catch {
              // Clipboard failures are non-fatal here.
            }
          },
        }),
        await PredefinedMenuItem.new({ item: "Separator" }),
        await PredefinedMenuItem.new({ item: "Services" }),
      ];

      const menu = await Menu.new({ items });
      const window = getCurrentWindow();
      const position = new LogicalPosition(event.clientX, event.clientY);
      await menu.popup(position, window);
    },
    [openFileLink, openTargets, reportOpenError, selectedOpenAppId, t, workspacePath],
  );

  return { openFileLink, showFileLinkMenu };
}
