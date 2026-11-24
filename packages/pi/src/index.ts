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

export { ExtensionHost } from "./extension-host.js";

export interface PiNodeHostOptions {
  arguments?: string[];
  extensionHost?: ExtensionHost;
  nativeBinding?: NativeBinding;
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
  }
}

export class PiEvalNodeHost extends PiNodeHost {
  override async run(): Promise<void> {
    const binding = this.nativeBinding ?? loadNativeBinding();
    await binding.runPiEval(this.arguments, (operation, context) =>
      this.extensionHost.dispatch(operation, context),
    );
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
