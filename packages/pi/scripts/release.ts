import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { z } from "zod";

import {
  currentNativeTarget,
  nativePackageName,
  nativeTargetForRustTarget,
  supportedNativeTargets,
  type NativeTarget,
} from "../src/native-target.js";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const packageDirectory = resolve(scriptDirectory, "..");
const workspaceDirectory = resolve(packageDirectory, "../..");
const sourcePackagePath = join(packageDirectory, "package.json");
const releaseDirectory = join(workspaceDirectory, "dist", "release");
const npmDirectory = join(workspaceDirectory, "dist", "npm");
const npmRegistry = "https://registry.npmjs.org";
const publishedPackagePollAttempts = 12;
const publishedPackageInitialDelayMs = 1_000;
const publishedPackageMaxDelayMs = 10_000;
const sleepBuffer = new Int32Array(new SharedArrayBuffer(Int32Array.BYTES_PER_ELEMENT));

const packageManifestSchema = z.looseObject({
  name: z.string().min(1),
  version: z.string().regex(/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/),
  private: z.literal(true),
  description: z.string().optional(),
  type: z.string().optional(),
  bin: z.record(z.string(), z.string()),
  files: z.array(z.string()),
  exports: z.unknown(),
  dependencies: z.record(z.string(), z.string()),
  engines: z.record(z.string(), z.string()),
  license: z.string().optional(),
  repository: z.unknown().optional(),
});

const publishIdentitySchema = z.looseObject({
  name: z.string().min(1),
  version: z.string().min(1),
});

const stagedPackageSchema = publishIdentitySchema.extend({
  os: z.array(z.string()).optional(),
  cpu: z.array(z.string()).optional(),
  libc: z.array(z.string()).optional(),
  optionalDependencies: z.record(z.string(), z.string()).optional(),
});

const publishedPackageSchema = stagedPackageSchema.extend({
  dist: z.looseObject({
    integrity: z.string().min(1),
    tarball: z.string().url(),
  }),
});

const cargoMetadataSchema = z.object({
  packages: z.array(
    z.object({
      name: z.string().min(1),
      manifest_path: z.string().min(1),
    }),
  ),
});

type PackageManifest = z.infer<typeof packageManifestSchema>;

const runnerByTarget = {
  "aarch64-apple-darwin": "macos-15",
  "x86_64-apple-darwin": "macos-15-intel",
  "aarch64-unknown-linux-gnu": "ubuntu-22.04-arm",
  "x86_64-unknown-linux-gnu": "ubuntu-22.04",
  "aarch64-pc-windows-msvc": "windows-11-arm",
  "x86_64-pc-windows-msvc": "windows-2025",
} as const satisfies Record<(typeof supportedNativeTargets)[number]["rustTarget"], string>;

export interface ReleaseConfiguration {
  version: string;
  packageManifest: PackageManifest;
}

export interface ReleaseMatrixEntry {
  target: string;
  napiSuffix: string;
  runner: string;
}

function parsePackageManifest(path: string): PackageManifest {
  const value = JSON.parse(readFileSync(path, "utf8")) as unknown;
  const parsed = packageManifestSchema.safeParse(value);
  if (!parsed.success) {
    throw new Error(`Invalid package manifest ${path}: ${z.prettifyError(parsed.error)}`, {
      cause: parsed.error,
    });
  }
  return parsed.data;
}

function workspaceVersion(): string {
  const cargoManifest = readFileSync(join(workspaceDirectory, "Cargo.toml"), "utf8");
  const workspacePackage = cargoManifest.match(
    /\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m,
  );
  const version = workspacePackage?.[1];
  if (!version) throw new Error("Cargo.toml [workspace.package] must define the product version");
  return version;
}

function assertWorkspaceVersion(packagePath: string): void {
  const contents = readFileSync(packagePath, "utf8");
  if (!/^version\.workspace\s*=\s*true$/m.test(contents)) {
    throw new Error(`${packagePath} must inherit version.workspace`);
  }
}

export function workspaceVersionPackageNames(): string[] {
  const result = runCapture("cargo", [
    "metadata",
    "--locked",
    "--no-deps",
    "--format-version",
    "1",
  ]);
  if (result.status !== 0) {
    throw new Error(`Cannot inspect Cargo workspace metadata:\n${result.stderr}`);
  }

  let value: unknown;
  try {
    value = JSON.parse(result.stdout) as unknown;
  } catch (error) {
    throw new Error(`Cannot parse Cargo workspace metadata: ${result.stdout}`, {
      cause: error,
    });
  }
  const parsed = cargoMetadataSchema.safeParse(value);
  if (!parsed.success) {
    throw new Error(`Invalid Cargo workspace metadata: ${z.prettifyError(parsed.error)}`, {
      cause: parsed.error,
    });
  }

  return parsed.data.packages
    .filter(({ manifest_path: manifestPath }) => {
      const contents = readFileSync(manifestPath, "utf8");
      return /^version\.workspace\s*=\s*true$/m.test(contents);
    })
    .map(({ name }) => name)
    .sort();
}

export function synchronizeCargoLockWorkspaceVersions(
  contents: string,
  version: string,
  packageNames = workspaceVersionPackageNames(),
): string {
  const sections = contents.split(/(?=^\[\[package\]\]\r?$)/m);
  for (const packageName of packageNames) {
    const matches = sections.filter((section) =>
      new RegExp(`^name = "${packageName}"\\r?$`, "m").test(section),
    );
    if (matches.length !== 1) {
      throw new Error(
        `Cargo.lock must contain exactly one ${packageName} package, found ${matches.length}`,
      );
    }
    const section = matches[0];
    if (!section) throw new Error(`Cargo.lock package ${packageName} disappeared`);
    const updated = section.replace(
      /^version = "[^"]+"(\r?)$/m,
      `version = "${version}"$1`,
    );
    if (updated === section && !new RegExp(`^version = "${version}"\\r?$`, "m").test(section)) {
      throw new Error(`Cargo.lock package ${packageName} has no version`);
    }
    sections[sections.indexOf(section)] = updated;
  }
  return sections.join("");
}

export function synchronizeWorkspaceCargoLock(version: string): void {
  const lockPath = join(workspaceDirectory, "Cargo.lock");
  const contents = readFileSync(lockPath, "utf8");
  const synchronized = synchronizeCargoLockWorkspaceVersions(contents, version);
  if (synchronized !== contents) writeFileSync(lockPath, synchronized);
}

export function validateReleaseConfiguration(tag?: string): ReleaseConfiguration {
  const version = workspaceVersion();
  const packageManifest = parsePackageManifest(sourcePackagePath);
  if (packageManifest.version !== version) {
    throw new Error(
      `Product version mismatch: Cargo workspace is ${version}, ${sourcePackagePath} is ${packageManifest.version}`,
    );
  }
  assertWorkspaceVersion(join(workspaceDirectory, "apps", "pi-cli", "Cargo.toml"));
  assertWorkspaceVersion(join(workspaceDirectory, "bindings", "pi-napi", "Cargo.toml"));

  if (tag && tag !== `v${version}`) {
    throw new Error(`Release tag ${tag} does not match product version v${version}`);
  }
  if (packageManifest.files.some((entry) => entry.includes("pi-napi"))) {
    throw new Error("The root npm source package must not embed platform NAPI artifacts");
  }

  const rustTargets = new Set(supportedNativeTargets.map((target) => target.rustTarget));
  const napiSuffixes = new Set(supportedNativeTargets.map((target) => target.napiSuffix));
  if (
    rustTargets.size !== supportedNativeTargets.length ||
    napiSuffixes.size !== supportedNativeTargets.length
  ) {
    throw new Error("Release targets must have unique Rust triples and NAPI suffixes");
  }
  for (const target of supportedNativeTargets) {
    if (!runnerByTarget[target.rustTarget]) {
      throw new Error(`Release target ${target.rustTarget} has no CI runner`);
    }
  }

  return { version, packageManifest };
}

export function releaseMatrix(): { include: ReleaseMatrixEntry[] } {
  validateReleaseConfiguration();
  return {
    include: supportedNativeTargets.map((target) => ({
      target: target.rustTarget,
      napiSuffix: target.napiSuffix,
      runner: runnerByTarget[target.rustTarget],
    })),
  };
}

function executableName(command: string): string {
  return process.platform === "win32" && command === "npm" ? "npm.cmd" : command;
}

interface CommandResult {
  status: number;
  stdout: string;
  stderr: string;
}

function runCapture(
  command: string,
  arguments_: string[],
  options: { cwd?: string; env?: NodeJS.ProcessEnv } = {},
): CommandResult {
  const result = spawnSync(executableName(command), arguments_, {
    cwd: options.cwd ?? workspaceDirectory,
    env: options.env ?? process.env,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.error) throw result.error;
  return {
    status: result.status ?? 1,
    stdout: result.stdout ?? "",
    stderr: result.stderr ?? "",
  };
}

export function parseSingleNpmViewOutput(stdout: string, identity: string): unknown {
  let value: unknown;
  try {
    value = JSON.parse(stdout) as unknown;
  } catch (error) {
    throw new Error(`Cannot parse npm metadata for ${identity}: ${stdout}`, {
      cause: error,
    });
  }
  if (!Array.isArray(value)) return value;
  if (value.length !== 1) {
    throw new Error(
      `Expected exactly one npm metadata result for ${identity}, received ${value.length}`,
    );
  }
  return value[0];
}

function run(
  command: string,
  arguments_: string[],
  options: { cwd?: string; env?: NodeJS.ProcessEnv } = {},
): void {
  process.stdout.write(`$ ${command} ${arguments_.join(" ")}\n`);
  const result = spawnSync(executableName(command), arguments_, {
    cwd: options.cwd ?? workspaceDirectory,
    env: options.env ?? process.env,
    stdio: "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${command} exited with status ${result.status ?? "unknown"}`);
  }
}

function option(arguments_: string[], name: string): string | undefined {
  const index = arguments_.indexOf(name);
  if (index === -1) return undefined;
  const value = arguments_[index + 1];
  if (!value || value.startsWith("--")) throw new Error(`${name} requires a value`);
  return value;
}

function requiredOption(arguments_: string[], name: string): string {
  const value = option(arguments_, name);
  if (!value) throw new Error(`${name} is required`);
  return value;
}

function resolveWorkspacePath(value: string | undefined, fallback: string): string {
  if (!value) return fallback;
  return isAbsolute(value) ? value : resolve(workspaceDirectory, value);
}

function targetOutputDirectory(target: NativeTarget, profile: "debug" | "release"): string {
  return join(workspaceDirectory, "target", target.rustTarget, profile);
}

function nativeLibraryName(target: NativeTarget): string {
  if (target.platform === "win32") return "pi_napi.dll";
  if (target.platform === "darwin") return "libpi_napi.dylib";
  return "libpi_napi.so";
}

function standaloneBinaryName(target: NativeTarget): string {
  return target.platform === "win32" ? "pi.exe" : "pi";
}

function cargoBuild(
  target: NativeTarget,
  profile: "debug" | "release",
  packages: string[],
): void {
  synchronizeWorkspaceCargoLock(workspaceVersion());
  const arguments_ = ["build", "--locked", "--target", target.rustTarget];
  if (profile === "release") arguments_.push("--release");
  for (const packageName of packages) arguments_.push("-p", packageName);
  run("cargo", arguments_);
}

function assertNativeHost(target: NativeTarget): void {
  const host = currentNativeTarget();
  if (host.rustTarget !== target.rustTarget) {
    throw new Error(
      `Release artifacts must be built and tested natively: host is ${host.rustTarget}, requested ${target.rustTarget}`,
    );
  }
}

function stripArtifact(path: string, target: NativeTarget): void {
  if (target.platform === "win32") return;
  run("strip", target.platform === "darwin" ? ["-x", path] : [path]);
}

function signMacArtifact(path: string, target: NativeTarget): void {
  if (target.platform !== "darwin") return;
  run("codesign", ["--force", "--sign", "-", path]);
}

function copyNativeArtifact(
  target: NativeTarget,
  profile: "debug" | "release",
  destination: string,
): void {
  const source = join(targetOutputDirectory(target, profile), nativeLibraryName(target));
  if (!existsSync(source)) throw new Error(`NAPI build output does not exist: ${source}`);
  mkdirSync(dirname(destination), { recursive: true });
  copyFileSync(source, destination);
  if (profile === "release") stripArtifact(destination, target);
  // Copying a Mach-O changes the final path covered by its code signature.
  signMacArtifact(destination, target);
}

function sha256(path: string): string {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function sha512Integrity(path: string): string {
  const digest = createHash("sha512").update(readFileSync(path)).digest("base64");
  return `sha512-${digest}`;
}

function writeChecksum(path: string): void {
  writeFileSync(`${path}.sha256`, `${sha256(path)}  ${basename(path)}\n`);
}

function buildNative(arguments_: string[]): void {
  const release = arguments_.includes("--release");
  const requestedTarget = option(arguments_, "--target");
  const target = requestedTarget
    ? nativeTargetForRustTarget(requestedTarget)
    : currentNativeTarget();
  assertNativeHost(target);
  const profile = release ? "release" : "debug";
  cargoBuild(target, profile, ["pi-napi"]);
  const destination = join(packageDirectory, `pi-napi.${target.napiSuffix}.node`);
  copyNativeArtifact(target, profile, destination);
  process.stdout.write(`${destination}\n`);
}

function smokeStandalone(binary: string, version: string, stageDirectory: string): void {
  const versionResult = runCapture(binary, ["--version"]);
  if (versionResult.status !== 0 || !versionResult.stdout.includes(version)) {
    throw new Error(
      `Packaged binary version smoke failed:\n${versionResult.stdout}${versionResult.stderr}`,
    );
  }

  const agentDirectory = join(stageDirectory, "agent");
  const shellResult = runCapture(
    binary,
    [
      "--agent-dir",
      agentDirectory,
      "--no-approve",
      "--print",
      "!echo pi-release-smoke",
    ],
    { cwd: workspaceDirectory },
  );
  if (shellResult.status !== 0 || !shellResult.stdout.includes("pi-release-smoke")) {
    throw new Error(
      `Packaged binary shell smoke failed:\n${shellResult.stdout}${shellResult.stderr}`,
    );
  }
}

function archiveStandalone(
  target: NativeTarget,
  version: string,
  builtBinary: string,
): string {
  mkdirSync(releaseDirectory, { recursive: true });
  const temporaryRoot = mkdtempSync(join(tmpdir(), `pi-release-${target.napiSuffix}-`));
  const packageName = `pi-${version}-${target.rustTarget}`;
  const stageDirectory = join(temporaryRoot, packageName);
  mkdirSync(stageDirectory, { recursive: true });
  const stagedBinary = join(stageDirectory, standaloneBinaryName(target));

  try {
    copyFileSync(builtBinary, stagedBinary);
    if (target.platform !== "win32") chmodSync(stagedBinary, 0o755);
    stripArtifact(stagedBinary, target);
    signMacArtifact(stagedBinary, target);
    smokeStandalone(stagedBinary, version, stageDirectory);
    writeFileSync(
      join(stageDirectory, "README.txt"),
      [
        `pi ${version} for ${target.rustTarget}`,
        "",
        "This standalone Rust build supports TUI, print, NDJSON, and native plugins.",
        "Pi-compatible JavaScript/TypeScript extensions require the @pi-rs/cli npm package.",
        "",
        `Run ./${standaloneBinaryName(target)} --help for command usage.`,
        "",
      ].join("\n"),
    );

    const archive = join(
      releaseDirectory,
      target.platform === "win32" ? `${packageName}.zip` : `${packageName}.tar.gz`,
    );
    if (existsSync(archive)) rmSync(archive);
    if (target.platform === "win32") {
      run("tar", ["-a", "-cf", archive, "-C", temporaryRoot, packageName]);
    } else {
      run("tar", ["-czf", archive, "-C", temporaryRoot, packageName], {
        env:
          target.platform === "darwin"
            ? { ...process.env, COPYFILE_DISABLE: "1" }
            : process.env,
      });
    }
    writeChecksum(archive);
    return archive;
  } finally {
    rmSync(temporaryRoot, { recursive: true, force: true });
  }
}

function buildDistribution(arguments_: string[]): void {
  const { version } = validateReleaseConfiguration(option(arguments_, "--tag"));
  const target = nativeTargetForRustTarget(requiredOption(arguments_, "--target"));
  assertNativeHost(target);
  cargoBuild(target, "release", ["pi-cli", "pi-napi"]);

  mkdirSync(releaseDirectory, { recursive: true });
  const napiArtifact = join(releaseDirectory, `pi-napi.${target.napiSuffix}.node`);
  copyNativeArtifact(target, "release", napiArtifact);
  writeChecksum(napiArtifact);

  const builtBinary = join(
    targetOutputDirectory(target, "release"),
    standaloneBinaryName(target),
  );
  if (!existsSync(builtBinary)) throw new Error(`CLI build output does not exist: ${builtBinary}`);
  const archive = archiveStandalone(target, version, builtBinary);
  process.stdout.write(`Created ${archive}\nCreated ${napiArtifact}\n`);
}

function safeResetDirectory(path: string): void {
  const relativePath = relative(workspaceDirectory, path);
  if (!relativePath || relativePath.startsWith("..") || isAbsolute(relativePath)) {
    throw new Error(`Refusing to reset release directory outside the workspace: ${path}`);
  }
  if (relativePath !== "dist/npm" && !relativePath.startsWith("dist/npm/")) {
    throw new Error(`Refusing to reset non-npm release directory: ${path}`);
  }
  rmSync(path, { recursive: true, force: true });
  mkdirSync(path, { recursive: true });
}

function writeJson(path: string, value: unknown): void {
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);
}

function stagePlatformPackage(
  target: NativeTarget,
  configuration: ReleaseConfiguration,
  artifactsDirectory: string,
  destinationRoot: string,
): string {
  const destination = join(destinationRoot, target.napiSuffix);
  mkdirSync(destination, { recursive: true });
  const artifactName = `pi-napi.${target.napiSuffix}.node`;
  const sourceArtifact = join(artifactsDirectory, artifactName);
  if (!existsSync(sourceArtifact)) {
    throw new Error(`Missing NAPI release artifact for ${target.rustTarget}: ${sourceArtifact}`);
  }
  copyFileSync(sourceArtifact, join(destination, artifactName));

  const manifest: Record<string, unknown> = {
    name: nativePackageName(configuration.packageManifest.name, target),
    version: configuration.version,
    description: `Native binding for ${configuration.packageManifest.name} on ${target.napiSuffix}`,
    license: configuration.packageManifest.license ?? "MIT",
    repository: configuration.packageManifest.repository,
    os: [target.platform],
    cpu: [target.arch],
    main: `./${artifactName}`,
    files: [artifactName, "README.md"],
    publishConfig: { access: "public", registry: npmRegistry },
  };
  if (target.libc) manifest.libc = [target.libc];
  writeJson(join(destination, "package.json"), manifest);
  writeFileSync(
    join(destination, "README.md"),
    `# ${String(manifest.name)}\n\nPlatform binding for ${configuration.packageManifest.name}. ` +
      `Install the root package instead of depending on this package directly.\n`,
  );
  return destination;
}

function stageRootPackage(
  configuration: ReleaseConfiguration,
  destinationRoot: string,
): string {
  const destination = join(destinationRoot, "root");
  mkdirSync(join(destination, "dist"), { recursive: true });
  const builtDirectory = join(packageDirectory, "dist");
  for (const child of ["bin", "src"] as const) {
    const source = join(builtDirectory, child);
    if (!existsSync(source)) throw new Error(`TypeScript build output does not exist: ${source}`);
    cpSync(source, join(destination, "dist", child), { recursive: true });
  }
  copyFileSync(join(packageDirectory, "README.md"), join(destination, "README.md"));

  const optionalDependencies = Object.fromEntries(
    supportedNativeTargets.map((target) => [
      nativePackageName(configuration.packageManifest.name, target),
      configuration.version,
    ]),
  );
  const manifest = {
    name: configuration.packageManifest.name,
    version: configuration.version,
    description: configuration.packageManifest.description,
    license: configuration.packageManifest.license ?? "MIT",
    repository: configuration.packageManifest.repository,
    type: configuration.packageManifest.type,
    bin: configuration.packageManifest.bin,
    files: ["dist/bin", "dist/src", "README.md"],
    exports: configuration.packageManifest.exports,
    dependencies: configuration.packageManifest.dependencies,
    optionalDependencies,
    engines: configuration.packageManifest.engines,
    publishConfig: { access: "public", registry: npmRegistry },
  };
  writeJson(join(destination, "package.json"), manifest);
  return destination;
}

function releaseFileNames(version: string): string[] {
  return supportedNativeTargets.flatMap((target) => [
    target.platform === "win32"
      ? `pi-${version}-${target.rustTarget}.zip`
      : `pi-${version}-${target.rustTarget}.tar.gz`,
    `pi-napi.${target.napiSuffix}.node`,
  ]);
}

function writeChecksumManifest(directory: string, names: string[]): void {
  const lines = [...names]
    .sort()
    .map((name) => {
      const path = join(directory, name);
      if (!existsSync(path)) throw new Error(`Missing release artifact: ${path}`);
      return `${sha256(path)}  ${name}`;
    });
  writeFileSync(join(directory, "SHA256SUMS"), `${lines.join("\n")}\n`);
}

function npmCommandArguments(packageSpec: string, dryRun = false): string[] {
  const arguments_ = [
    "publish",
    packageSpec,
    "--access",
    "public",
    "--ignore-scripts",
    `--registry=${npmRegistry}`,
  ];
  if (dryRun) arguments_.push("--dry-run");
  return arguments_;
}

function packStagedPackage(directory: string, tarballDirectory: string): void {
  run(
    "npm",
    ["pack", "--ignore-scripts", "--pack-destination", tarballDirectory],
    { cwd: directory },
  );
}

function npmTarballName(name: string, version: string): string {
  const normalized = (name.startsWith("@") ? name.slice(1) : name).replaceAll("/", "-");
  return `${normalized}-${version}.tgz`;
}

function npmTarballPath(directory: string, name: string, version: string): string {
  return join(directory, "packages", npmTarballName(name, version));
}

function smokeInstalledNpmPackage(
  configuration: ReleaseConfiguration,
  tarballDirectory: string,
): void {
  const target = currentNativeTarget();
  const rootTarball = join(
    tarballDirectory,
    npmTarballName(configuration.packageManifest.name, configuration.version),
  );
  const nativeTarball = join(
    tarballDirectory,
    npmTarballName(
      nativePackageName(configuration.packageManifest.name, target),
      configuration.version,
    ),
  );
  const installation = mkdtempSync(join(tmpdir(), `pi-npm-smoke-${target.napiSuffix}-`));
  try {
    run("npm", [
      "install",
      "--prefix",
      installation,
      "--ignore-scripts",
      "--no-package-lock",
      "--no-save",
      "--omit=optional",
      rootTarball,
      nativeTarball,
      `--registry=${npmRegistry}`,
    ]);
    const smoke = runCapture(
      "node",
      [
        "--input-type=module",
        "-e",
        'const { PiNodeHost } = await import("@pi-rs/cli"); await new PiNodeHost({ arguments: ["--version"] }).run();',
      ],
      { cwd: installation },
    );
    if (smoke.status !== 0 || !smoke.stdout.includes(configuration.version)) {
      throw new Error(`Installed npm package smoke failed:\n${smoke.stdout}${smoke.stderr}`);
    }
  } finally {
    rmSync(installation, { recursive: true, force: true });
  }
}

export function publishPackageDirectories(directory = npmDirectory): string[] {
  return [
    ...supportedNativeTargets.map((target) => join(directory, target.napiSuffix)),
    join(directory, "root"),
  ];
}

export function verifyNpmStaging(directory = npmDirectory): void {
  const configuration = validateReleaseConfiguration();
  const expectedPackages = new Map(
    supportedNativeTargets.map((target) => [
      nativePackageName(configuration.packageManifest.name, target),
      target,
    ]),
  );

  for (const target of supportedNativeTargets) {
    const packagePath = join(directory, target.napiSuffix, "package.json");
    const manifest = JSON.parse(readFileSync(packagePath, "utf8")) as Record<string, unknown>;
    if (manifest.name !== nativePackageName(configuration.packageManifest.name, target)) {
      throw new Error(`Unexpected platform package name in ${packagePath}`);
    }
    if (manifest.version !== configuration.version) {
      throw new Error(`Unexpected platform package version in ${packagePath}`);
    }
    if (!stringArrayEquals(manifest.os, [target.platform])) {
      throw new Error(`Unexpected npm os selector in ${packagePath}`);
    }
    if (!stringArrayEquals(manifest.cpu, [target.arch])) {
      throw new Error(`Unexpected npm cpu selector in ${packagePath}`);
    }
    const expectedLibc = "libc" in target ? [target.libc] : undefined;
    if (
      expectedLibc
        ? !stringArrayEquals(manifest.libc, expectedLibc)
        : manifest.libc !== undefined
    ) {
      throw new Error(`Unexpected npm libc selector in ${packagePath}`);
    }
    const artifact = join(directory, target.napiSuffix, `pi-napi.${target.napiSuffix}.node`);
    if (!existsSync(artifact)) throw new Error(`Platform package is missing ${artifact}`);
  }

  const rootPath = join(directory, "root", "package.json");
  const root = JSON.parse(readFileSync(rootPath, "utf8")) as Record<string, unknown>;
  const optionalDependencies = root.optionalDependencies;
  if (
    typeof optionalDependencies !== "object" ||
    optionalDependencies === null ||
    Array.isArray(optionalDependencies)
  ) {
    throw new Error(`Root package ${rootPath} has no optionalDependencies`);
  }
  const entries = Object.entries(optionalDependencies);
  if (entries.length !== expectedPackages.size) {
    throw new Error(`Root package ${rootPath} does not list every platform package`);
  }
  for (const [name, version] of entries) {
    if (!expectedPackages.has(name) || version !== configuration.version) {
      throw new Error(`Unexpected optional dependency ${name}@${String(version)}`);
    }
  }
}

function stringArrayEquals(value: unknown, expected: string[]): boolean {
  return (
    Array.isArray(value) &&
    value.length === expected.length &&
    value.every((entry, index) => entry === expected[index])
  );
}

function assembleNpm(arguments_: string[]): void {
  const configuration = validateReleaseConfiguration(option(arguments_, "--tag"));
  const artifactsDirectory = resolveWorkspacePath(
    option(arguments_, "--artifacts"),
    releaseDirectory,
  );
  const destination = resolveWorkspacePath(option(arguments_, "--output"), npmDirectory);
  safeResetDirectory(destination);

  run("npm", ["run", "build"], { cwd: packageDirectory });
  for (const target of supportedNativeTargets) {
    stagePlatformPackage(target, configuration, artifactsDirectory, destination);
  }
  stageRootPackage(configuration, destination);
  verifyNpmStaging(destination);

  const tarballDirectory = join(destination, "packages");
  mkdirSync(tarballDirectory, { recursive: true });
  for (const packagePath of publishPackageDirectories(destination)) {
    packStagedPackage(packagePath, tarballDirectory);
  }
  const tarballs = readFileNames(tarballDirectory).filter((name) => name.endsWith(".tgz"));
  if (tarballs.length !== supportedNativeTargets.length + 1) {
    throw new Error(
      `Expected ${supportedNativeTargets.length + 1} npm tarballs, found ${tarballs.length}`,
    );
  }
  writeChecksumManifest(tarballDirectory, tarballs);
  writeChecksumManifest(artifactsDirectory, releaseFileNames(configuration.version));
  smokeInstalledNpmPackage(configuration, tarballDirectory);
  process.stdout.write(`Assembled npm release packages in ${destination}\n`);
}

function readFileNames(directory: string): string[] {
  return existsSync(directory) ? readdirSync(directory) : [];
}

function packageIsPublished(name: string, version: string): boolean {
  const result = runCapture("npm", [
    "view",
    `${name}@${version}`,
    "version",
    "--json",
    `--registry=${npmRegistry}`,
  ]);
  if (result.status === 0) {
    const publishedVersion = parseSingleNpmViewOutput(result.stdout, `${name}@${version}`);
    return publishedVersion === version;
  }
  const output = `${result.stdout}\n${result.stderr}`;
  if (output.includes("E404") || output.includes("404 Not Found")) return false;
  throw new Error(`Cannot query ${name}@${version}:\n${output}`);
}

function publishedPackage(name: string, version: string): z.infer<typeof publishedPackageSchema> {
  const result = runCapture("npm", [
    "view",
    `${name}@${version}`,
    "--json",
    `--registry=${npmRegistry}`,
  ]);
  if (result.status !== 0) {
    throw new Error(`Cannot query published package ${name}@${version}:\n${result.stderr}`);
  }

  const value = parseSingleNpmViewOutput(result.stdout, `${name}@${version}`);
  const parsed = publishedPackageSchema.safeParse(value);
  if (!parsed.success) {
    throw new Error(
      `Invalid npm metadata for ${name}@${version}: ${z.prettifyError(parsed.error)}`,
      { cause: parsed.error },
    );
  }
  return parsed.data;
}

function stringRecordEquals(
  value: Record<string, string> | undefined,
  expected: Record<string, string> | undefined,
): boolean {
  if (value === undefined || expected === undefined) return value === expected;
  const valueEntries = Object.entries(value);
  const expectedEntries = Object.entries(expected);
  return (
    valueEntries.length === expectedEntries.length &&
    expectedEntries.every(([key, expectedValue]) => value[key] === expectedValue)
  );
}

export function assertPublishedPackageMatches(
  stagedValue: unknown,
  publishedValue: unknown,
  expectedIntegrity: string,
): void {
  const staged = stagedPackageSchema.parse(stagedValue);
  const published = publishedPackageSchema.parse(publishedValue);
  const identity = `${staged.name}@${staged.version}`;
  if (published.name !== staged.name || published.version !== staged.version) {
    throw new Error(`npm registry returned the wrong identity for ${identity}`);
  }
  for (const selector of ["os", "cpu", "libc"] as const) {
    const stagedSelector = staged[selector];
    const publishedSelector = published[selector];
    if (
      stagedSelector === undefined
        ? publishedSelector !== undefined
        : !stringArrayEquals(publishedSelector, stagedSelector)
    ) {
      throw new Error(`npm registry ${selector} selector differs for ${identity}`);
    }
  }
  if (!stringRecordEquals(published.optionalDependencies, staged.optionalDependencies)) {
    throw new Error(`npm registry optionalDependencies differ for ${identity}`);
  }
  if (published.dist.integrity !== expectedIntegrity) {
    throw new Error(`npm registry tarball integrity differs for ${identity}`);
  }
}

export interface PublishedPackagePollOptions {
  attempts?: number;
  initialDelayMs?: number;
  maxDelayMs?: number;
  query?: (name: string, version: string) => unknown;
  sleep?: (delayMs: number) => void;
  onRetry?: (error: unknown, attempt: number, delayMs: number) => void;
}

function sleepSynchronously(delayMs: number): void {
  Atomics.wait(sleepBuffer, 0, 0, delayMs);
}

export function waitForPublishedPackage(
  stagedValue: unknown,
  expectedIntegrity: string,
  options: PublishedPackagePollOptions = {},
): void {
  const staged = stagedPackageSchema.parse(stagedValue);
  const identity = `${staged.name}@${staged.version}`;
  const attempts = options.attempts ?? publishedPackagePollAttempts;
  const initialDelayMs = options.initialDelayMs ?? publishedPackageInitialDelayMs;
  const maxDelayMs = options.maxDelayMs ?? publishedPackageMaxDelayMs;
  if (!Number.isInteger(attempts) || attempts < 1) {
    throw new Error("Published package polling attempts must be a positive integer");
  }
  if (initialDelayMs < 1 || maxDelayMs < initialDelayMs) {
    throw new Error("Published package polling delays must be positive and ordered");
  }

  const query = options.query ?? publishedPackage;
  const sleep = options.sleep ?? sleepSynchronously;
  const onRetry =
    options.onRetry ??
    ((_error: unknown, attempt: number, delayMs: number) => {
      process.stdout.write(
        `Waiting for ${identity} to become verifiable ` +
          `(attempt ${attempt}/${attempts}; retrying in ${delayMs}ms)\n`,
      );
    });
  let delayMs = initialDelayMs;
  let lastError: unknown;

  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      assertPublishedPackageMatches(
        staged,
        query(staged.name, staged.version),
        expectedIntegrity,
      );
      return;
    } catch (error) {
      lastError = error;
      if (attempt === attempts) break;
      onRetry(error, attempt, delayMs);
      sleep(delayMs);
      delayMs = Math.min(delayMs * 2, maxDelayMs);
    }
  }

  throw new Error(`Published package ${identity} was not verifiable after ${attempts} attempts`, {
    cause: lastError,
  });
}

function verifyPublishedPackagePath(directory: string, packagePath: string): void {
  const manifest = stagedPackageSchema.parse(
    JSON.parse(readFileSync(join(packagePath, "package.json"), "utf8")) as unknown,
  );
  const tarball = npmTarballPath(directory, manifest.name, manifest.version);
  if (!existsSync(tarball)) throw new Error(`Publish tarball does not exist: ${tarball}`);
  waitForPublishedPackage(manifest, sha512Integrity(tarball));
  process.stdout.write(`Verified published ${manifest.name}@${manifest.version}\n`);
}

export function verifyPublishedNpmRelease(directory = npmDirectory): void {
  verifyNpmStaging(directory);
  for (const packagePath of publishPackageDirectories(directory)) {
    verifyPublishedPackagePath(directory, packagePath);
  }
}

function publishNpm(arguments_: string[]): void {
  const directory = resolveWorkspacePath(option(arguments_, "--npm-dir"), npmDirectory);
  const dryRun = arguments_.includes("--dry-run");
  verifyNpmStaging(directory);

  for (const packagePath of publishPackageDirectories(directory)) {
    const packagePathname = join(packagePath, "package.json");
    const manifest = publishIdentitySchema.parse(
      JSON.parse(readFileSync(packagePathname, "utf8")) as unknown,
    );
    if (!dryRun && packageIsPublished(manifest.name, manifest.version)) {
      process.stdout.write(`Skipping already-published ${manifest.name}@${manifest.version}\n`);
      verifyPublishedPackagePath(directory, packagePath);
      continue;
    }
    const tarball = npmTarballPath(directory, manifest.name, manifest.version);
    if (!existsSync(tarball)) throw new Error(`Publish tarball does not exist: ${tarball}`);
    run("npm", npmCommandArguments(tarball, dryRun));
    if (!dryRun) verifyPublishedPackagePath(directory, packagePath);
  }
}

function usage(): string {
  return [
    "Usage: tsx scripts/release.ts <command> [options]",
    "",
    "Commands:",
    "  check [--tag vX.Y.Z]",
    "  sync-lock",
    "  matrix",
    "  native [--release] [--target RUST_TARGET]",
    "  dist --target RUST_TARGET [--tag vX.Y.Z]",
    "  assemble [--artifacts PATH] [--output PATH] [--tag vX.Y.Z]",
    "  verify [--npm-dir PATH]",
    "  publish [--npm-dir PATH] [--dry-run]",
    "  verify-published [--npm-dir PATH]",
    "",
  ].join("\n");
}

export function main(arguments_ = process.argv.slice(2)): void {
  const [command, ...rest] = arguments_;
  switch (command) {
    case "check": {
      const configuration = validateReleaseConfiguration(option(rest, "--tag"));
      process.stdout.write(`Release configuration is valid for v${configuration.version}\n`);
      break;
    }
    case "sync-lock": {
      const configuration = validateReleaseConfiguration();
      synchronizeWorkspaceCargoLock(configuration.version);
      process.stdout.write(
        `Cargo.lock workspace packages are synchronized to ${configuration.version}\n`,
      );
      break;
    }
    case "matrix":
      process.stdout.write(`${JSON.stringify(releaseMatrix())}\n`);
      break;
    case "native":
      buildNative(rest);
      break;
    case "dist":
      buildDistribution(rest);
      break;
    case "assemble":
      assembleNpm(rest);
      break;
    case "verify":
      verifyNpmStaging(resolveWorkspacePath(option(rest, "--npm-dir"), npmDirectory));
      process.stdout.write("Npm release staging is valid\n");
      break;
    case "publish":
      publishNpm(rest);
      break;
    case "verify-published":
      verifyPublishedNpmRelease(
        resolveWorkspacePath(option(rest, "--npm-dir"), npmDirectory),
      );
      break;
    case "help":
    case "--help":
    case "-h":
      process.stdout.write(usage());
      break;
    default:
      throw new Error(command ? `Unknown release command ${command}\n${usage()}` : usage());
  }
}

const entry = process.argv[1];
if (entry && resolve(entry) === fileURLToPath(import.meta.url)) {
  try {
    main();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    process.stderr.write(`release: ${message}\n`);
    process.exitCode = 1;
  }
}
