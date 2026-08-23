import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { z } from "zod";

import { currentNativeTarget, nativePackageName } from "./native-target.js";

export interface NativeBinding {
  runPi(arguments_: string[], dispatch: (operation: string) => Promise<string>): Promise<void>;
}

const nativeBindingSchema = z.looseObject({
  runPi: z.custom<NativeBinding["runPi"]>(
    (value) => typeof value === "function",
    "runPi must be a function",
  ),
});

const require = createRequire(import.meta.url);
function packageRoot(): string {
  const moduleDirectory = dirname(fileURLToPath(import.meta.url));
  for (const candidate of [resolve(moduleDirectory, ".."), resolve(moduleDirectory, "../..")]) {
    if (existsSync(join(candidate, "package.json"))) return candidate;
  }
  throw new Error(`Cannot locate @pi-rs/cli package root from ${moduleDirectory}`);
}

const packageDirectory = packageRoot();
const workspaceDirectory = resolve(packageDirectory, "../..");

function developmentLibraryName(): string {
  if (process.platform === "win32") return "pi_napi.dll";
  if (process.platform === "darwin") return "libpi_napi.dylib";
  return "libpi_napi.so";
}

function candidatePaths(platformArtifact: string): string[] {
  return [
    join(packageDirectory, platformArtifact),
    join(workspaceDirectory, "bindings/pi-napi", platformArtifact),
    join(workspaceDirectory, "target/release", developmentLibraryName()),
    join(workspaceDirectory, "target/debug", developmentLibraryName()),
  ].filter((candidate): candidate is string => Boolean(candidate));
}

function parseNativeBinding(value: unknown, path: string): NativeBinding {
  const result = nativeBindingSchema.safeParse(value);
  if (!result.success) {
    throw new Error(`Native binding has an invalid shape (${path}): ${z.prettifyError(result.error)}`, {
      cause: result.error,
    });
  }
  return result.data;
}

function loadDynamicLibrary(path: string): NativeBinding {
  let loaded: unknown;
  if (path.endsWith(".node")) {
    loaded = require(path) as unknown;
  } else {
    const nativeModule: { exports: unknown } = { exports: {} };
    process.dlopen(nativeModule, path);
    loaded = nativeModule.exports;
  }
  return parseNativeBinding(loaded, path);
}

export function loadNativeBinding(): NativeBinding {
  const override = process.env.PI_RS_NATIVE_BINDING;
  if (override) {
    if (!existsSync(override)) {
      throw new Error(`PI_RS_NATIVE_BINDING does not exist: ${override}`);
    }
    return loadDynamicLibrary(override);
  }

  const target = currentNativeTarget();
  const platformArtifact = `pi-napi.${target.napiSuffix}.node`;
  const candidates = candidatePaths(platformArtifact);
  for (const candidate of candidates) {
    if (existsSync(candidate)) return loadDynamicLibrary(candidate);
  }

  const platformPackage = nativePackageName("@pi-rs/cli", target);
  let resolvedPackage: string;
  try {
    resolvedPackage = require.resolve(platformPackage);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw new Error(
      `Cannot find the pi_rs NAPI binding for ${target.rustTarget}. ` +
        `Install ${platformPackage} through @pi-rs/cli's optional dependencies, ` +
        `run \"npm run build:native\", or set PI_RS_NATIVE_BINDING. Checked:\n` +
        `${candidates.join("\n")}\n${platformPackage}: ${message}`,
      { cause: error },
    );
  }

  try {
    return parseNativeBinding(require(platformPackage) as unknown, resolvedPackage);
  } catch (error) {
    throw new Error(`Cannot load the native package ${platformPackage} (${resolvedPackage})`, {
      cause: error,
    });
  }
}
