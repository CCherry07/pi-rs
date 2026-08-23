export type LinuxLibc = "glibc" | "musl";

export interface NativeTarget {
  rustTarget: string;
  napiSuffix: string;
  platform: "darwin" | "linux" | "win32";
  arch: "arm64" | "x64";
  libc?: LinuxLibc;
}

export const supportedNativeTargets = [
  {
    rustTarget: "aarch64-apple-darwin",
    napiSuffix: "darwin-arm64",
    platform: "darwin",
    arch: "arm64",
  },
  {
    rustTarget: "x86_64-apple-darwin",
    napiSuffix: "darwin-x64",
    platform: "darwin",
    arch: "x64",
  },
  {
    rustTarget: "aarch64-unknown-linux-gnu",
    napiSuffix: "linux-arm64-gnu",
    platform: "linux",
    arch: "arm64",
    libc: "glibc",
  },
  {
    rustTarget: "x86_64-unknown-linux-gnu",
    napiSuffix: "linux-x64-gnu",
    platform: "linux",
    arch: "x64",
    libc: "glibc",
  },
  {
    rustTarget: "aarch64-pc-windows-msvc",
    napiSuffix: "win32-arm64-msvc",
    platform: "win32",
    arch: "arm64",
  },
  {
    rustTarget: "x86_64-pc-windows-msvc",
    napiSuffix: "win32-x64-msvc",
    platform: "win32",
    arch: "x64",
  },
] as const satisfies readonly NativeTarget[];

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

export function detectLinuxLibc(report: unknown = process.report?.getReport()): LinuxLibc {
  if (!isRecord(report) || !isRecord(report.header)) return "musl";
  return typeof report.header.glibcVersionRuntime === "string" ? "glibc" : "musl";
}

export function resolveNativeTarget(
  platform: string,
  arch: string,
  libc: LinuxLibc | undefined = platform === "linux" ? detectLinuxLibc() : undefined,
): NativeTarget {
  const target = supportedNativeTargets.find(
    (candidate) =>
      candidate.platform === platform &&
      candidate.arch === arch &&
      (candidate.platform !== "linux" || candidate.libc === libc),
  );
  if (target) return target;

  const runtime = [platform, arch, libc].filter(Boolean).join("-");
  throw new Error(
    `Unsupported native runtime ${runtime}. Supported targets: ${supportedNativeTargets
      .map((candidate) => candidate.napiSuffix)
      .join(", ")}`,
  );
}

export function currentNativeTarget(): NativeTarget {
  return resolveNativeTarget(process.platform, process.arch);
}

export function nativeTargetForRustTarget(rustTarget: string): NativeTarget {
  const target = supportedNativeTargets.find((candidate) => candidate.rustTarget === rustTarget);
  if (target) return target;
  throw new Error(
    `Unsupported Rust target ${rustTarget}. Supported targets: ${supportedNativeTargets
      .map((candidate) => candidate.rustTarget)
      .join(", ")}`,
  );
}

export function nativePackageName(rootPackageName: string, target: NativeTarget): string {
  return `${rootPackageName}-${target.napiSuffix}`;
}
