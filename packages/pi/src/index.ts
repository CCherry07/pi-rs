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
    const binding = this.nativeBinding ?? loadNativeBinding();
    await binding.runPi(this.arguments, (operation, context) =>
      this.extensionHost.dispatch(operation, context),
    );
  }
}

export class PiEvalNodeHost extends PiNodeHost {
  override async run(): Promise<void> {
    const binding = this.nativeBinding ?? loadNativeBinding();
    await binding.runPiEval(this.arguments, (operation, context) =>
      this.extensionHost.dispatch(operation, context),
    );
  }
}

export type {
  UiNode, UiTone, UiCompletion, UiInteraction, UiComponentEvent, UiMultiSelectAction,
  UiMultiSelectOption, UiMultiSelectRequest, UiMultiSelectResponse,
} from "./extension-ui-frame.js";
export type { UiMount, MountOptions, DialogOptions, UiTheme } from "./extension-ui.js";
export type { MessageRenderer, EntryRenderer, MarkdownTransformer, ToolRenderers, ToolRenderContext, ToolRendererResolver } from "./extension-ui-transcript.js";
