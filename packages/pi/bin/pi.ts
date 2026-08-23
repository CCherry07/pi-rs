#!/usr/bin/env node

import { PiApplication } from "../src/index.js";

try {
  await new PiApplication().run();
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  process.stderr.write(`pi: ${message}\n`);
  process.exitCode = 1;
}
