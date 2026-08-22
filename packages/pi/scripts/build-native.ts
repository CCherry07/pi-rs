import { copyFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const packageDirectory = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const workspaceDirectory = resolve(packageDirectory, "../..");
const release = process.argv.includes("--release");
const profile = release ? "release" : "debug";
const cargoArguments = ["build", "-p", "pi-napi"];
if (release) cargoArguments.push("--release");

const build = spawnSync("cargo", cargoArguments, {
  cwd: workspaceDirectory,
  stdio: "inherit",
});
if (build.error) throw build.error;
if (build.status !== 0) process.exit(build.status ?? 1);

const libraryName =
  process.platform === "win32"
    ? "pi_napi.dll"
    : process.platform === "darwin"
      ? "libpi_napi.dylib"
      : "libpi_napi.so";
const source = join(workspaceDirectory, "target", profile, libraryName);
const destination = join(
  packageDirectory,
  `pi-napi.${process.platform}-${process.arch}.node`,
);
copyFileSync(source, destination);
process.stdout.write(`${destination}\n`);
