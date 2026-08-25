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
