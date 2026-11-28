import { ExtensionHost } from "./extension-host.js";
import {
  loadNativeBinding,
  type NativeBinding,
  type NativeExtensionContext,
} from "./native-binding.js";

export type { NativeBinding, NativeExtensionContext } from "./native-binding.js";
export type {
  PiContextUsage,
  PiExtensionCommandContext,
  PiCommandOptions,
  PiExtensionApi,
  PiExtensionContext,
  PiExtensionUI,
  PiModelRegistry,
  PiReadonlySessionManager,
  PiToolDefinition,
  PiToolNamespace,
  PiToolResult,
} from "./extension-api.js";
export type {
  McpExposure,
  McpHttpServerConfig,
  McpOAuthConfig,
  McpServerConfig,
  McpServersChangeEvent,
  McpStdioServerConfig,
  RegisteredMcpServer,
} from "./mcp-servers.js";

export { ExtensionHost } from "./extension-host.js";

export interface PiNodeHostOptions {
  arguments?: string[];
  extensionHost?: ExtensionHost;
  nativeBinding?: NativeBinding;
}

let activeNodeHostRuns = 0;
let offlineNodeHostRunActive = false;

function offlineMode(arguments_: string[]): boolean {
  return arguments_.includes("--offline")
    || /^(1|true|yes)$/i.test(process.env.PI_OFFLINE ?? "");
}

function beginNodeHostRun(offline: boolean): () => void {
  if ((offline && activeNodeHostRuns > 0) || (!offline && offlineNodeHostRunActive)) {
    throw new Error(
      "PiNodeHost cannot overlap offline and other runs because PI_OFFLINE and "
      + "PI_SKIP_VERSION_CHECK are process-wide",
    );
  }
  activeNodeHostRuns += 1;
  if (offline) offlineNodeHostRunActive = true;

  let active = true;
  return () => {
    if (!active) return;
    active = false;
    activeNodeHostRuns -= 1;
    if (offline) offlineNodeHostRunActive = false;
  };
}

async function withNodeHostEnvironment<T>(
  arguments_: string[],
  run: () => Promise<T>,
): Promise<T> {
  const offline = offlineMode(arguments_);
  const finish = beginNodeHostRun(offline);
  const previousOffline = process.env.PI_OFFLINE;
  const previousSkipVersionCheck = process.env.PI_SKIP_VERSION_CHECK;
  try {
    if (offline) {
      process.env.PI_OFFLINE = "1";
      process.env.PI_SKIP_VERSION_CHECK = "1";
    }
    return await run();
  } finally {
    if (offline) {
      if (previousOffline === undefined) delete process.env.PI_OFFLINE;
      else process.env.PI_OFFLINE = previousOffline;
      if (previousSkipVersionCheck === undefined) delete process.env.PI_SKIP_VERSION_CHECK;
      else process.env.PI_SKIP_VERSION_CHECK = previousSkipVersionCheck;
    }
    finish();
  }
}

export class PiNodeHost {
  readonly arguments: string[];
  readonly extensionHost: ExtensionHost;
  readonly nativeBinding?: NativeBinding;

  constructor(options: PiNodeHostOptions = {}) {
    this.arguments = options.arguments ?? process.argv.slice(2);
    this.extensionHost = options.extensionHost ?? new ExtensionHost();
    this.nativeBinding = options.nativeBinding;
  }

  async run(): Promise<void> {
    const code = await this.runWithExitCode();
    if (code !== 0) throw new Error(`Pi command exited with status ${code}`);
  }

  async runWithExitCode(): Promise<number> {
    return withNodeHostEnvironment(this.arguments, async () => {
      const rpc = this.arguments.some((argument, index) => argument === "--mode=rpc"
        || (argument === "--mode" && this.arguments[index + 1] === "rpc"));
      // Replace Node's default terminating handlers before Rust installs its chained
      // signal handlers. RpcSignals owns graceful disposal and repeated-signal exit.
      const deferToNative = (): void => {};
      const signals: NodeJS.Signals[] = rpc && process.platform !== "win32" ? ["SIGTERM", "SIGHUP"] : [];
      for (const signal of signals) process.on(signal, deferToNative);
      const originalWrite = process.stdout.write;
      const diagnosticsWrite = process.stderr.write.bind(process.stderr);
      // Rust owns the protocol descriptor. Extension logging follows Pi's stderr policy.
      if (rpc) process.stdout.write = diagnosticsWrite;
      try {
        const binding = this.nativeBinding ?? loadNativeBinding();
        const code = await binding.runPi(this.arguments, (operation, context) =>
          this.extensionHost.dispatch(operation, context),
        );
        return code ?? 0;
      } finally {
        for (const signal of signals) process.off(signal, deferToNative);
        if (rpc && process.stdout.write === diagnosticsWrite) process.stdout.write = originalWrite;
      }
    });
  }
}

export class PiEvalNodeHost extends PiNodeHost {
  override async run(): Promise<void> {
    await withNodeHostEnvironment(this.arguments, async () => {
      const binding = this.nativeBinding ?? loadNativeBinding();
      await binding.runPiEval(this.arguments, (operation, context) =>
        this.extensionHost.dispatch(operation, context),
      );
    });
  }

  override async runWithExitCode(): Promise<number> {
    await this.run();
    return 0;
  }
}

export type {
  UiNode, UiTone, UiCompletion, UiInteraction, UiMouseEvent, UiMouseResult, UiComponentEvent, UiMultiSelectAction,
  UiMultiSelectOption, UiMultiSelectRequest, UiMultiSelectResponse,
} from "./extension-ui-frame.js";
export type { UiMount, MountOptions, DialogOptions, UiTheme } from "./extension-ui.js";
export type { SlotFactory, FooterFactory } from "./extension-ui.js";
export type { FooterDataProvider } from "./extension-footer.js";
export type { CustomOptions, CustomFactory, CustomTui, PiComponent, PiTheme } from "./pi-component-adapter.js";
export type { MessageRenderer, EntryRenderer, MarkdownTransformer, ToolRenderers, ToolRenderContext, ToolRendererResolver } from "./extension-ui-transcript.js";
