import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";

import {
  assertCompatibleGlibcReferences,
  assertNpmPublishResultMatches,
  assertPublishedPackageMatches,
  glibcVersionsFromReadelf,
  isNpmAlreadyPublishedError,
  maximumLinuxGlibcVersion,
  npmCommandArguments,
  parseNpmPublishOutput,
  parseSingleNpmViewOutput,
  publishPackageDirectories,
  releaseMatrix,
  synchronizeCargoLockWorkspaceVersions,
  validateReleaseConfiguration,
  workspaceVersionPackageNames,
} from "../scripts/release.js";
import { VERSION } from "../src/compat-api.js";
import { supportedNativeTargets } from "../src/native-target.js";

test("release configuration keeps Cargo and npm product versions together", () => {
  const configuration = validateReleaseConfiguration();
  assert.doesNotThrow(() => validateReleaseConfiguration(`v${configuration.version}`));
  assert.equal(configuration.packageManifest.version, configuration.version);
  assert.equal(VERSION, configuration.version);
});

test("release builds synchronize inherited workspace versions in Cargo.lock", () => {
  const packageNames = workspaceVersionPackageNames();
  const workspacePackages = packageNames.map((name) => `[[package]]
name = "${name}"
version = "0.1.0"
dependencies = []
`).join("\n");
  const lock = `version = 4

${workspacePackages}
[[package]]
name = "third-party"
version = "0.1.0"
`;

  const synchronized = synchronizeCargoLockWorkspaceVersions(lock, "0.2.1", packageNames);

  for (const name of packageNames) {
    assert.match(synchronized, new RegExp(`name = "${name}"\\nversion = "0\\.2\\.1"`));
  }
  assert.match(synchronized, /name = "third-party"\nversion = "0\.1\.0"/);
});

test("release builds discover every crate inheriting the workspace version", () => {
  assert.deepEqual(
    new Set(workspaceVersionPackageNames()),
    new Set([
      "pi-acp",
      "pi-cli",
      "pi-eval",
      "pi-eval-cli",
      "pi-js-package-manager",
      "pi-mcp",
      "pi-media",
      "pi-memory-loader",
      "pi-napi",
      "pi-plugin-memory-hermes",
      "pi-rpc",
      "pi-sdk",
      "pi-settings",
    ]),
  );
});

test("release matrix has one native runner per supported target", () => {
  const matrix = releaseMatrix();
  assert.deepEqual(
    new Set(matrix.include.map((entry) => entry.target)),
    new Set(supportedNativeTargets.map((target) => target.rustTarget)),
  );
  assert.ok(matrix.include.every((entry) => entry.runner.length > 0));
});

test("release matrix builds Linux artifacts in the glibc 2.36 container", () => {
  const linuxBuilds = new Map(
    releaseMatrix().include
      .filter((entry) => entry.target.endsWith("unknown-linux-gnu"))
      .map((entry) => [entry.target, { runner: entry.runner, container: entry.container }]),
  );
  assert.deepEqual(
    linuxBuilds,
    new Map([
      [
        "aarch64-unknown-linux-gnu",
        { runner: "ubuntu-24.04-arm", container: "rust:1.98.0-bookworm" },
      ],
      [
        "x86_64-unknown-linux-gnu",
        { runner: "ubuntu-24.04", container: "rust:1.98.0-bookworm" },
      ],
    ]),
  );
  assert.equal(maximumLinuxGlibcVersion, "2.36");

  const releaseWorkflow = readFileSync(
    new URL("../../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  assert.match(releaseWorkflow, /container: \$\{\{ matrix\.container \|\| '' \}\}/);
});

test("release GLIBC audit rejects symbols newer than 2.36", () => {
  const compatible = `
    0x0010: Name: GLIBC_2.2.5  Flags: none  Version: 12
    0x0020: Name: GLIBC_2.34   Flags: none  Version: 11
    0x0030: Name: GLIBC_2.36   Flags: none  Version: 10
  `;
  assert.deepEqual(glibcVersionsFromReadelf(compatible), ["2.2.5", "2.34", "2.36"]);
  assert.doesNotThrow(() => assertCompatibleGlibcReferences(compatible, "pi"));
  assert.throws(
    () => assertCompatibleGlibcReferences(`${compatible}\nName: GLIBC_2.37`, "pi-napi.node"),
    /pi-napi\.node requires GLIBC_2\.37, newer than supported GLIBC_2\.36/,
  );
  assert.throws(
    () => assertCompatibleGlibcReferences("no version section", "pi"),
    /pi has no readable GLIBC version requirements/,
  );
});

test("npm publication always puts the root package last", () => {
  const directory = "/release/npm";
  const packages = publishPackageDirectories(directory);
  assert.equal(packages.length, supportedNativeTargets.length + 1);
  assert.equal(packages.at(-1), join(directory, "root"));
});

test("registry verification requires the exact staged tarball and selectors", () => {
  const staged = {
    name: "@pi-rs/cli-linux-x64-gnu",
    version: "0.1.0",
    os: ["linux"],
    cpu: ["x64"],
    libc: ["glibc"],
  };
  const published = {
    ...staged,
    dist: {
      integrity: "sha512-release",
      tarball: "https://registry.npmjs.org/@pi-rs/cli-linux-x64-gnu/-/cli-linux-x64-gnu-0.1.0.tgz",
    },
  };

  assert.doesNotThrow(() =>
    assertPublishedPackageMatches(staged, published, "sha512-release"),
  );
  assert.throws(
    () => assertPublishedPackageMatches(staged, published, "sha512-other"),
    /tarball integrity differs/,
  );
});

test("npm publish JSON proves the registry accepted the exact staged tarball", () => {
  const staged = {
    name: "@pi-rs/cli-win32-x64-msvc",
    version: "0.4.1",
    os: ["win32"],
    cpu: ["x64"],
  };
  const publishResult = {
    id: "@pi-rs/cli-win32-x64-msvc@0.4.1",
    name: "@pi-rs/cli-win32-x64-msvc",
    version: "0.4.1",
    integrity: "sha512-release",
  };

  const keyedOutput = JSON.stringify({ [staged.name]: publishResult });
  assert.ok(npmCommandArguments("release.tgz").includes("--json"));
  const parsed = parseNpmPublishOutput(keyedOutput, staged.name, staged.version);
  assert.deepEqual(parsed, publishResult);
  assert.doesNotThrow(() =>
    assertNpmPublishResultMatches(staged, parsed, "sha512-release"),
  );

  assert.deepEqual(
    parseNpmPublishOutput(JSON.stringify([publishResult]), staged.name, staged.version),
    publishResult,
  );
});

test("npm publish JSON rejects the wrong package identity or tarball", () => {
  const staged = {
    name: "@pi-rs/cli-win32-x64-msvc",
    version: "0.4.1",
  };
  const publishResult = {
    id: "@pi-rs/cli-win32-x64-msvc@0.4.1",
    name: "@pi-rs/cli-win32-x64-msvc",
    version: "0.4.1",
    integrity: "sha512-release",
  };

  assert.throws(
    () => assertNpmPublishResultMatches(staged, publishResult, "sha512-other"),
    /npm publish tarball integrity differs/,
  );
  assert.throws(
    () =>
      assertNpmPublishResultMatches(
        staged,
        { ...publishResult, id: "@pi-rs/cli-win32-x64-msvc@0.4.0" },
        "sha512-release",
      ),
    /npm publish returned the wrong identity/,
  );
});

test("only an exact already-published error activates registry recovery", () => {
  assert.equal(
    isNpmAlreadyPublishedError(
      "You cannot publish over the previously published versions: 0.4.1.",
    ),
    true,
  );
  assert.equal(isNpmAlreadyPublishedError("npm error code EPUBLISHCONFLICT"), true);
  assert.equal(
    isNpmAlreadyPublishedError("npm error code E403: package publish access denied"),
    false,
  );
});

test("registry verification accepts npm 12 singleton view results", () => {
  const identity = "@pi-rs/cli-darwin-arm64@0.3.0";
  const published = {
    name: "@pi-rs/cli-darwin-arm64",
    version: "0.3.0",
    os: ["darwin"],
    cpu: ["arm64"],
    dist: {
      integrity: "sha512-release",
      tarball:
        "https://registry.npmjs.org/@pi-rs/cli-darwin-arm64/-/cli-darwin-arm64-0.3.0.tgz",
    },
  };

  const parsed = parseSingleNpmViewOutput(JSON.stringify([published]), identity);

  assert.doesNotThrow(() =>
    assertPublishedPackageMatches(published, parsed, "sha512-release"),
  );
  assert.deepEqual(parseSingleNpmViewOutput(JSON.stringify(published), identity), published);
  assert.equal(parseSingleNpmViewOutput('["0.3.0"]', identity), "0.3.0");
  assert.throws(
    () => parseSingleNpmViewOutput("[]", identity),
    /Expected exactly one npm metadata result.*received 0/,
  );
  assert.throws(
    () => parseSingleNpmViewOutput('["0.3.0", "0.3.1"]', identity),
    /Expected exactly one npm metadata result.*received 2/,
  );
});

test("Release Please owns version PRs and dispatches the product release workflow", () => {
  const config = JSON.parse(
    readFileSync(new URL("../../../release-please-config.json", import.meta.url), "utf8"),
  ) as {
    packages: {
      ".": {
        draft: boolean;
        "initial-version": string;
        "force-tag-creation": boolean;
        "extra-files": Array<{ path: string; jsonpath: string }>;
      };
    };
  };
  const manifest = JSON.parse(
    readFileSync(new URL("../../../.release-please-manifest.json", import.meta.url), "utf8"),
  ) as Record<string, string>;
  const product = config.packages["."];
  assert.equal(product.draft, true);
  assert.equal(product["initial-version"], "0.1.0");
  assert.equal(manifest["."], validateReleaseConfiguration().version);
  assert.equal(product["force-tag-creation"], true);
  assert.deepEqual(
    new Set(product["extra-files"].map((entry) => entry.path)),
    new Set([
      "Cargo.toml",
      "packages/pi/package.json",
      "packages/pi/package-lock.json",
    ]),
  );

  const releaseWorkflow = readFileSync(
    new URL("../../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  const releasePleaseWorkflow = readFileSync(
    new URL("../../../.github/workflows/release-please.yml", import.meta.url),
    "utf8",
  );
  assert.doesNotMatch(releaseWorkflow, /NPM_TOKEN|NODE_AUTH_TOKEN/);
  assert.doesNotMatch(releaseWorkflow, /release:verify-published/);
  assert.match(releaseWorkflow, /Publish and verify platform packages, then the root package/);
  assert.match(releasePleaseWorkflow, /gh workflow run release\.yml/);
  assert.match(releasePleaseWorkflow, /if: always\(\)/);
  assert.match(releasePleaseWorkflow, /git\/ref\/tags\/\$RELEASE_TAG/);
  assert.match(releasePleaseWorkflow, /gh run list --workflow release\.yml --commit/);
});
