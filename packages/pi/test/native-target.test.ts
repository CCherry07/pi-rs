import assert from "node:assert/strict";
import test from "node:test";

import {
  detectLinuxLibc,
  nativePackageName,
  nativeTargetForRustTarget,
  resolveNativeTarget,
  supportedNativeTargets,
} from "../src/native-target.js";

test("native targets have unique Rust triples and NAPI suffixes", () => {
  assert.equal(
    new Set(supportedNativeTargets.map((target) => target.rustTarget)).size,
    supportedNativeTargets.length,
  );
  assert.equal(
    new Set(supportedNativeTargets.map((target) => target.napiSuffix)).size,
    supportedNativeTargets.length,
  );
});

test("runtime platform names resolve to exact Rust targets", () => {
  assert.equal(resolveNativeTarget("darwin", "arm64").rustTarget, "aarch64-apple-darwin");
  assert.equal(resolveNativeTarget("darwin", "x64").rustTarget, "x86_64-apple-darwin");
  assert.equal(
    resolveNativeTarget("linux", "arm64", "glibc").rustTarget,
    "aarch64-unknown-linux-gnu",
  );
  assert.equal(
    resolveNativeTarget("linux", "x64", "glibc").rustTarget,
    "x86_64-unknown-linux-gnu",
  );
  assert.equal(
    resolveNativeTarget("win32", "arm64").rustTarget,
    "aarch64-pc-windows-msvc",
  );
  assert.equal(
    resolveNativeTarget("win32", "x64").rustTarget,
    "x86_64-pc-windows-msvc",
  );
});

test("Linux libc detection distinguishes glibc from musl", () => {
  assert.equal(detectLinuxLibc({ header: { glibcVersionRuntime: "2.35" } }), "glibc");
  assert.equal(detectLinuxLibc({ header: {} }), "musl");
});

test("unsupported runtimes fail before attempting to load a binary", () => {
  assert.throws(
    () => resolveNativeTarget("linux", "x64", "musl"),
    /Unsupported native runtime linux-x64-musl/,
  );
  assert.throws(() => nativeTargetForRustTarget("wasm32-wasip1"), /Unsupported Rust target/);
});

test("platform package names derive from the root package", () => {
  const target = nativeTargetForRustTarget("aarch64-apple-darwin");
  assert.equal(nativePackageName("@pi-rs/cli", target), "@pi-rs/cli-darwin-arm64");
});
