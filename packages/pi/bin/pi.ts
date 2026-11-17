#!/usr/bin/env node

import { PiNodeHost } from "../src/index.js";

try {
  process.exitCode = await new PiNodeHost().runWithExitCode();
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  process.stderr.write(`pi: ${message}\n`);
  process.exitCode = 1;
}
