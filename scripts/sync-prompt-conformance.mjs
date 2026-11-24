#!/usr/bin/env node
// Read-only oracle execution; no tools/providers are executed. Rust tests consume
// the checked-in fixture so ordinary cargo tests do not require Node or legacy.
// Run: node --import ./legacy/pi/packages/coding-agent/src/experimental/source-resolver.ts scripts/sync-prompt-conformance.mjs [--check]
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";
import { createAllToolDefinitions } from "../legacy/pi/packages/coding-agent/src/core/tools/index.ts";
import { createCodemodeToolDefinition } from "../legacy/pi/packages/coding-agent/src/extensions/codemode/tool.ts";
import { createToolSearchToolDefinition } from "../legacy/pi/packages/coding-agent/src/extensions/tool-search/tool.ts";
import { buildSystemPrompt } from "../legacy/pi/packages/coding-agent/src/core/system-prompt.ts";
import { createMcpResultSchema } from "../legacy/pi/packages/mcp/src/result-schema.ts";
import { getReadmePath, getDocsPath, getExamplesPath } from "../legacy/pi/packages/coding-agent/src/config.ts";

const root = fileURLToPath(new URL("../", import.meta.url));
const target = new URL("../domains/coding/tests/fixtures/prompt-conformance.json", import.meta.url);
const paths = [[getReadmePath(), "/pi/README.md"], [getDocsPath(), "/pi/docs"], [getExamplesPath(), "/pi/examples"]];
const normalize = (text) => paths.reduce((value, [source, replacement]) => value.split(source).join(replacement), text);
const definitions = [...Object.values(createAllToolDefinitions("/project")), createCodemodeToolDefinition({ models: true }), createToolSearchToolDefinition()];
const tools = definitions.map((tool) => ({
  name: tool.name,
  label: tool.label,
  description: normalize(tool.description),
  parameters: tool.parameters,
  outputSchema: tool.outputSchema ?? null,
  constrainedSampling: tool.constrainedSampling ?? null,
  promptSnippet: tool.promptSnippet ?? null,
  promptGuidelines: tool.promptGuidelines ?? [],
}));
const skill = { name: "inspect", description: "Inspect <code> & files", filePath: "/skills/inspect/SKILL.md", baseDir: "/skills/inspect", sourceInfo: {}, disableModelInvocation: false };
const defaults = {
  cwd: "/project",
  toolSnippets: Object.fromEntries(tools.filter((tool) => tool.promptSnippet).map((tool) => [tool.name, tool.promptSnippet])),
  toolGuidelines: Object.fromEntries(tools.map((tool) => [tool.name, tool.promptGuidelines])),
};
const scenarios = [
  ["default", {}],
  ["all-tools", { selectedTools: tools.map((tool) => tool.name) }],
  ["read-only", { selectedTools: ["read", "grep", "find", "ls"] }],
  ["powershell", { selectedTools: ["powershell"] }],
  ["both-shells", { selectedTools: ["bash", "powershell"] }],
  ["skills", { skills: [skill, { ...skill, name: "manual-only", disableModelInvocation: true }] }],
  ["bash-reader", { selectedTools: ["bash"], skills: [skill] }],
  ["hidden-reader", { selectedTools: ["read", "codemode"], hiddenTools: ["read"], skills: [skill] }],
  ["no-reader", { selectedTools: [], skills: [skill] }],
  ["context-and-append", { appendSystemPrompt: "Additional instructions", contextFiles: [{ path: "/project/AGENTS.md", content: "Repository instructions\nwith another line" }], skills: [skill] }],
  ["custom", { customPrompt: "Custom role", appendSystemPrompt: "Keep addendum", contextFiles: [{ path: "/project/CLAUDE.md", content: "Project rules" }], skills: [skill] }],
  ["empty-custom", { customPrompt: "" }],
  ["forced", { forceSystemPrompt: "Exact replacement\n", appendSystemPrompt: "Not included", skills: [skill], sections: { preamble: "invalid but ignored by force" } }],
  ["forced-empty", { forceSystemPrompt: "", skills: [skill] }],
  ["sections", { sections: { z_extra: "First extra", cwd: "Override cwd", a_extra: "Last extra", empty: "" } }],
  ["rules", { selectedTools: ["read", "bash"], promptGuidelines: ["  Be concise in your responses  ", "", "Extra rule", "Extra rule"] }],
];
const fixture = {
  oracleRevision: execFileSync("git", ["-C", `${root}/legacy/pi`, "rev-parse", "HEAD"], { encoding: "utf8" }).trim(),
  tools,
  prompts: scenarios.map(([name, patch]) => {
    const options = { ...defaults, ...patch };
    return { name, options, expected: normalize(buildSystemPrompt(options)) };
  }),
};
const mcpSchemas = [
  undefined,
  { type: "object", properties: { answer: { type: "number" } }, required: ["answer"] },
  { $defs: { Item: { type: "string" } }, properties: { $ref: { $ref: "#/$defs/Item" }, self: { $ref: "#" } }, const: { $ref: "#/literal" } },
].map((schema) => ({ schema: schema ?? null, expected: createMcpResultSchema(schema) }));
const outputs = [
  [target, fixture],
  [new URL("../plugins/features/pi-plugin-mcp/tests/fixtures/output-schemas.json", import.meta.url), mcpSchemas],
];
for (const [path, value] of outputs) {
  const output = `${JSON.stringify(value, null, 2)}\n`;
  if (process.argv.includes("--check")) {
    if (readFileSync(path, "utf8") !== output) throw new Error("Conformance fixture differs from current legacy Pi; regenerate and review the diff.");
  } else {
    mkdirSync(new URL(".", path), { recursive: true });
    writeFileSync(path, output);
    console.log("Wrote " + fileURLToPath(path));
  }
}
console.log(
  "Current legacy: " + tools.length + " tool definitions, " + scenarios.length + " prompt scenarios, " + mcpSchemas.length + " MCP schemas.");
