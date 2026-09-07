import { spawn } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

// Run after building the desktop binary. A real runtime is needed: unit tests
// do not exercise Tauri's menu/core-plugin initialization order.
const projectRoot = fileURLToPath(new URL("../", import.meta.url));
const binary = process.argv[2]
  ? path.resolve(process.argv[2])
  : path.join(
      projectRoot,
      "src-tauri/target/debug",
      process.platform === "win32"
        ? "pi-desktop.exe"
        : "pi-desktop",
    );
const child = spawn(binary, [], {
  cwd: projectRoot,
  env: { ...process.env, RUST_BACKTRACE: "1" },
  stdio: ["ignore", "pipe", "pipe"],
});

let startupPassed = false;
let sawPanic = false;
let output = "";
let killTimer;
const startupTimer = setTimeout(() => {
  startupPassed = !sawPanic;
  child.kill("SIGTERM");
  killTimer = setTimeout(() => child.kill("SIGKILL"), 2000);
}, 8000);

function inspectOutput(chunk) {
  output = (output + chunk.toString()).slice(-65536);
  if (/panicked at|state\(\) called before manage\(\)/.test(output)) {
    sawPanic = true;
  }
}

child.stdout.on("data", inspectOutput);
child.stderr.on("data", inspectOutput);
child.on("error", (error) => {
  console.error(`Could not launch desktop binary: ${error.message}`);
});
child.on("close", (code, signal) => {
  clearTimeout(startupTimer);
  clearTimeout(killTimer);
  if (!startupPassed || sawPanic) {
    console.error(
      sawPanic
        ? "FAIL: native startup panicked (check menu/core-plugin initialization order)."
        : `FAIL: desktop exited during startup (code=${code}, signal=${signal}).`,
    );
    // Print only the panic headline, not app output that may contain user data.
    const panic = output.match(/state\(\) called before manage\(\)[^\r\n]*/);
    if (panic) console.error(panic[0]);
    process.exitCode = 1;
    return;
  }
  console.log("PASS: desktop stayed running for 8 seconds without a startup panic.");
});
