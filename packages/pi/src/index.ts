import { ExtensionHost } from "./extension-host.js";
import { loadNativeBinding, type NativeBinding } from "./native-binding.js";

export type { NativeBinding } from "./native-binding.js";
export type {
  PiCommandOptions,
  PiExtensionApi,
  PiExtensionContext,
  PiToolDefinition,
  PiToolResult,
} from "./extension-api.js";

export { ExtensionHost } from "./extension-host.js";

export interface PiApplicationOptions {
  arguments?: string[];
  extensionHost?: ExtensionHost;
  nativeBinding?: NativeBinding;
}

export class PiApplication {
  readonly arguments: string[];
  readonly extensionHost: ExtensionHost;
  readonly nativeBinding?: NativeBinding;

  constructor(options: PiApplicationOptions = {}) {
    this.arguments = options.arguments ?? process.argv.slice(2);
    this.extensionHost = options.extensionHost ?? new ExtensionHost();
    this.nativeBinding = options.nativeBinding;
  }

  async run(): Promise<void> {
    const binding = this.nativeBinding ?? loadNativeBinding();
    await binding.runPi(this.arguments, (operation) => this.extensionHost.dispatch(operation));
  }
}
