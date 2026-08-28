import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

import { PiNodeHost } from "../src/index.js";

const testDirectory = dirname(fileURLToPath(import.meta.url));

test("runs a TypeScript command through Node, NAPI, and the Rust session runtime", async () => {
  const agentDirectory = await mkdtemp(join(tmpdir(), "pi-rs-napi-agent-"));
  const extension = join(testDirectory, "fixtures", "command.ts");

  await new PiNodeHost({
    arguments: [
      "--cwd",
      testDirectory,
      "--agent-dir",
      agentDirectory,
      "--no-extensions",
      "--extension",
      extension,
      "--no-approve",
      "--print",
      "/bridge-smoke",
    ],
  }).run();
});

test("loads a settings extension through PackageManager discovery and NAPI", async () => {
  const agentDirectory = await mkdtemp(join(tmpdir(), "pi-rs-napi-settings-"));
  const extension = join(agentDirectory, "configured/settings-command.js");
  await mkdir(dirname(extension), { recursive: true });
  await writeFile(
    extension,
    `
      export default function (pi) {
        pi.registerCommand("settings-bridge-smoke", {
          description: "Verify settings-driven extension discovery",
          async handler() {}
        });
      }
    `,
  );
  await writeFile(
    join(agentDirectory, "settings.json"),
    JSON.stringify({ extensions: ["./configured/settings-command.js"] }),
  );

  await new PiNodeHost({
    arguments: [
      "--cwd",
      testDirectory,
      "--agent-dir",
      agentDirectory,
      "--no-approve",
      "--print",
      "/settings-bridge-smoke",
    ],
  }).run();
});

test("runs a PackageManager command through the Node/NAPI launcher", async () => {
  const root = await mkdtemp(join(tmpdir(), "pi-rs-napi-package-command-"));
  const agentDirectory = join(root, "agent");
  const projectDirectory = join(root, "project");
  const extension = join(projectDirectory, "extension");
  await mkdir(extension, { recursive: true });
  await mkdir(agentDirectory, { recursive: true });
  await writeFile(join(extension, "index.ts"), "export default function () {}\n");
  await writeFile(
    join(agentDirectory, "settings.json"),
    JSON.stringify({ packages: [extension] }),
  );

  await new PiNodeHost({
    arguments: [
      "--cwd",
      projectDirectory,
      "--agent-dir",
      agentDirectory,
      "--no-approve",
      "list",
    ],
  }).run();
});
