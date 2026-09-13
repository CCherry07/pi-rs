#!/usr/bin/env node

import { PiEvalNodeHost } from "../src/index.js";

try {
  await new PiEvalNodeHost().run();
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  process.stderr.write(`pi-eval: ${message}\n`);
  process.exitCode = 1;
}
