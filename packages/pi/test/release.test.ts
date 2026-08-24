import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";

import {
  assertPublishedPackageMatches,
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
  const lock = `version = 4

[[package]]
name = "pi-cli"
version = "0.1.0"
dependencies = []

[[package]]
name = "pi-napi"
version = "0.1.0"
dependencies = []

[[package]]
name = "pi-js-package-manager"
version = "0.1.0"
dependencies = []

[[package]]
name = "third-party"
version = "0.1.0"
`;

  const synchronized = synchronizeCargoLockWorkspaceVersions(lock, "0.2.1");

  assert.match(synchronized, /name = "pi-cli"\nversion = "0\.2\.1"/);
  assert.match(synchronized, /name = "pi-napi"\nversion = "0\.2\.1"/);
  assert.match(
    synchronized,
    /name = "pi-js-package-manager"\nversion = "0\.2\.1"/,
  );
  assert.match(synchronized, /name = "third-party"\nversion = "0\.1\.0"/);
});

test("release builds discover every crate inheriting the workspace version", () => {
  assert.deepEqual(
    new Set(workspaceVersionPackageNames()),
    new Set(["pi-cli", "pi-js-package-manager", "pi-napi"]),
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
  assert.match(releaseWorkflow, /release:verify-published/);
  assert.match(releasePleaseWorkflow, /gh workflow run release\.yml/);
  assert.match(releasePleaseWorkflow, /if: always\(\)/);
  assert.match(releasePleaseWorkflow, /git\/ref\/tags\/\$RELEASE_TAG/);
  assert.match(releasePleaseWorkflow, /gh run list --workflow release\.yml --commit/);
});
