#!/usr/bin/env node
// Inventory the local TypeScript oracle without executing tests or tool calls.
// Ordinary Cargo tests validate the frozen inventory without Node or legacy/pi.
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const oracleFiles = [
  "tools.test.ts",
  "read-streaming.test.ts",
  "path-utils.test.ts",
  "truncate-surrogates.test.ts",
  "edit-tool-legacy-input.test.ts",
  "edit-tool-diff-before-write.test.ts",
  "edit-diff-window.test.ts",
  "file-mutation-queue.test.ts",
  "grep-operations.test.ts",
  "find-abort-before-spawn.test.ts",
  "bash-executor-truncation.test.ts",
  "exec-utf8-chunks.test.ts",
  "exec-runtime-limit.test.ts",
  "exec.test.ts",
  "shell-kill-process-tree.test.ts",
  "shell-path-exists.test.ts",
  "powershell-tool.test.ts",
  "powershell-config.test.ts",
  "windows-shell-config.test.ts",
  "bash-close-hang-windows.test.ts",
  "builtin-tool-strict-mode.test.ts",
  "tool-result-images.test.ts",
  "image-resize-callers.test.ts",
  "image-process.test.ts",
  "image-processing.test.ts",
  "image-resize-worker-teardown.test.ts",
];

const sha256 = (value) => createHash("sha256").update(value).digest("hex");
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const fixturePath = resolve(root, "domains/coding/tests/fixtures/tool-test-parity.json");

// These are test *definitions*: each/loop-generated cases remain inside their
// definition. Their Rust mapping must exercise the entire parameter matrix.
export function collectDefinitions(ts, source, file) {
  const parsed = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true);
  const definitions = [];
  function baseName(expression) {
    if (ts.isIdentifier(expression)) return expression.text;
    if (ts.isCallExpression(expression) || ts.isPropertyAccessExpression(expression)) {
      return baseName(expression.expression);
    }
    return undefined;
  }
  function visit(node, suites) {
    if (ts.isCallExpression(node)) {
      const name = baseName(node.expression);
      const [title, callback] = node.arguments;
      const registration = name === "describe" || name === "it" || name === "test";
      if (registration && title && (ts.isStringLiteralLike(title) || ts.isTemplateExpression(title)) &&
          (!callback || !(ts.isArrowFunction(callback) || ts.isFunctionExpression(callback)))) {
        throw new Error(`Unsupported oracle registration in ${file}: ${node.getText(parsed)}`);
      }
      if (registration && title && callback &&
          (ts.isStringLiteralLike(title) || ts.isTemplateExpression(title)) &&
          (ts.isArrowFunction(callback) || ts.isFunctionExpression(callback))) {
        const label = ts.isStringLiteralLike(title) ? title.text : title.getText(parsed).slice(1, -1);
        if (name === "describe") {
          ts.forEachChild(callback.body, (child) => visit(child, [...suites, label]));
        } else {
          definitions.push({
            id: `${file}::${[...suites, label].join(" > ")}`,
            file,
            line: parsed.getLineAndCharacterOfPosition(node.getStart(parsed)).line + 1,
            title: label,
            suites,
            definitionHash: sha256(node.getText(parsed)),
          });
        }
        return;
      }
    }
    ts.forEachChild(node, (child) => visit(child, suites));
  }
  visit(parsed, []);
  return definitions;
}

function rustTestNames(source) {
  const names = new Set();
  let pending = false;
  let ignored = false;
  for (const line of source.split("\n").map((value) => value.trim())) {
    if (line.startsWith("#[test]") || line.startsWith("#[tokio::test")) pending = true;
    else if (line.startsWith("#[ignore")) ignored = true;
    else {
      const declaration = /^(?:async )?fn ([A-Za-z_][A-Za-z0-9_]*)\s*\(/.exec(line);
      if (declaration) {
        if (pending && !ignored) names.add(declaration[1]);
        pending = ignored = false;
      } else if (line && !line.startsWith("#[") && !line.startsWith("//")) pending = ignored = false;
    }
  }
  return names;
}

export function validateMappings(inventory, readSource) {
  const errors = [];
  const ids = new Set();
  const cache = new Map();
  if (inventory.schemaVersion !== 1 || !Array.isArray(inventory.cases) || inventory.cases.length === 0) {
    return ["Expected schemaVersion 1 and a non-empty cases array"];
  }
  for (const entry of inventory.cases) {
    if (ids.has(entry.id)) errors.push(`Duplicate oracle definition: ${entry.id}`);
    ids.add(entry.id);
    if (entry.disposition === "not-applicable") {
      if (!entry.reason?.trim()) errors.push(`Missing boundary rationale: ${entry.id}`);
      if (entry.rustTests?.length) errors.push(`Inapplicable case must not claim Rust coverage: ${entry.id}`);
      continue;
    }
    if (entry.disposition !== "covered" || !entry.rustTests?.length) {
      errors.push(`Unmapped oracle definition: ${entry.id}`);
      continue;
    }
    for (const test of entry.rustTests) {
      if (!/^[\w/-]+\.rs$/.test(test.path) || test.path.split("/").includes("..") ||
          !/^[A-Za-z_][A-Za-z_0-9]*$/.test(test.name)) {
        errors.push(`Invalid Rust test reference: ${entry.id}`);
        continue;
      }
      try {
        if (!cache.has(test.path)) cache.set(test.path, rustTestNames(readSource(test.path)));
        if (!cache.get(test.path).has(test.name)) errors.push(`Missing executable Rust test ${test.path}::${test.name} (${entry.id})`);
      } catch (error) {
        errors.push(`Cannot read ${test.path}: ${error.message}`);
      }
    }
  }
  return errors;
}

export function refreshInventory(previous, definitions, files, oracleRevision) {
  const oldCases = new Map(previous.cases.map((entry) => [entry.id, entry]));
  const oldFiles = new Map(previous.files.map((entry) => [entry.file, entry.sha256]));
  const currentFiles = new Map(files.map((entry) => [entry.file, entry.sha256]));
  return {
    schemaVersion: 1,
    oracleRevision,
    files,
    cases: definitions.map((entry) => {
      const old = oldCases.get(entry.id);
      // A fixture/table/helper change can alter assertions without changing a
      // test body. Require review of every definition in any changed file.
      return old?.definitionHash === entry.definitionHash &&
          oldFiles.get(entry.file) === currentFiles.get(entry.file)
        ? { ...old, ...entry }
        : { ...entry, disposition: "pending", rustTests: [] };
    }),
  };
}

function main() {
  const refresh = process.argv.includes("--refresh");
  const localOracle = process.argv.includes("--oracle") || refresh;
  let inventory = JSON.parse(readFileSync(fixturePath, "utf8"));
  if (localOracle) {
    const legacy = resolve(root, "legacy/pi");
    const require = createRequire(resolve(root, "packages/pi/package.json"));
    const ts = require("typescript");
    const files = [];
    const definitions = [];
    for (const file of oracleFiles) {
      const source = readFileSync(resolve(legacy, "packages/coding-agent/test", file), "utf8");
      files.push({ file, sha256: sha256(source) });
      definitions.push(...collectDefinitions(ts, source, file));
    }
    if (refresh) {
      inventory = refreshInventory(inventory, definitions, files,
        execFileSync("git", ["-C", legacy, "rev-parse", "HEAD"], { encoding: "utf8" }).trim());
      writeFileSync(fixturePath, `${JSON.stringify(inventory, null, 2)}\n`);
    } else {
      if (JSON.stringify(files) !== JSON.stringify(inventory.files)) {
        throw new Error("Legacy tool tests changed; run --refresh, review and map every changed definition.");
      }
      const frozen = inventory.cases.map(({ id, file, line, title, suites, definitionHash }) =>
        ({ id, file, line, title, suites, definitionHash }));
      if (JSON.stringify(definitions) !== JSON.stringify(frozen)) {
        throw new Error("Frozen inventory omits or changes legacy definitions; refresh and review the mapping.");
      }
    }
  }
  const errors = validateMappings(inventory, (path) => readFileSync(resolve(root, path), "utf8"));
  if (errors.length) throw new Error(errors.join("\n"));
  const covered = inventory.cases.filter((entry) => entry.disposition === "covered").length;
  console.log(`Tool test parity: ${covered} mapped definitions; ${inventory.cases.length - covered} documented boundary differences.`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
