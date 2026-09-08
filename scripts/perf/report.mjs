import { readFile, writeFile, rename } from "node:fs/promises";
import { join } from "node:path";
import { DEFINITIONS, embeddedJson, summarize, toCsv } from "./core.mjs";

export async function renderDashboard(result) {
  const template = await readFile(new URL("./dashboard.html", import.meta.url), "utf8");
  return template.replace("__PERF_DATA__", embeddedJson(result));
}

export async function writeReports(result, directory, latestPath) {
  result.summary = summarize(result.metrics);
  result.definitions = DEFINITIONS;
  const html = await renderDashboard(result);
  const artifacts = [
    ["results.json", JSON.stringify(result, null, 2) + "\n"],
    ["summary.csv", toCsv(result.summary)],
    ["index.html", html],
  ];
  for (const [name, contents] of artifacts) {
    const target = join(directory, name);
    await writeFile(target + ".tmp", contents);
    await rename(target + ".tmp", target);
  }
  // The latest report is self-contained; file:// does not need fetch or a local server.
  // Use a run-specific staging path so simultaneous completed runs cannot corrupt it.
  if (latestPath) {
    const staging = latestPath + "." + result.runId + ".tmp";
    await writeFile(staging, html);
    await rename(staging, latestPath);
  }
}
