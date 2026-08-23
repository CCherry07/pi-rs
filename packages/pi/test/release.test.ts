import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";

import {
  assertPublishedPackageMatches,
  publishPackageDirectories,
  releaseMatrix,
  validateReleaseConfiguration,
} from "../scripts/release.js";
import { supportedNativeTargets } from "../src/native-target.js";

test("release configuration keeps Cargo and npm product versions together", () => {
  const configuration = validateReleaseConfiguration("v0.1.0");
  assert.equal(configuration.version, "0.1.0");
  assert.equal(configuration.packageManifest.version, configuration.version);
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
  const product = config.packages["."];
  assert.equal(product.draft, true);
  assert.equal(product["initial-version"], validateReleaseConfiguration().version);
  assert.equal(product["force-tag-creation"], true);
  assert.deepEqual(
    new Set(product["extra-files"].map((entry) => entry.path)),
    new Set([
      "Cargo.toml",
      "Cargo.lock",
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
});
