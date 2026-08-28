#!/usr/bin/env node

import { PiNodeHost } from "../src/index.js";

try {
  await new PiNodeHost().run();
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  process.stderr.write(`pi: ${message}\n`);
  process.exitCode = 1;
}
