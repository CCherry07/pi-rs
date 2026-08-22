import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

import { PiApplication } from "../src/index.js";

const testDirectory = dirname(fileURLToPath(import.meta.url));

test("runs a TypeScript command through Node, NAPI, and the Rust session runtime", async () => {
  const agentDirectory = await mkdtemp(join(tmpdir(), "pi-rs-napi-agent-"));
  const extension = join(testDirectory, "fixtures", "command.ts");

  await new PiApplication({
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
