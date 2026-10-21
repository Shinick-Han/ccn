#!/usr/bin/env node
"use strict";

// Thin launcher. The binary comes from the platform package npm installed via
// optionalDependencies (the esbuild pattern), so no install script runs and
// `npm install --ignore-scripts` still works.

const fs = require("fs");
const path = require("path");
const { spawnSync } = require("child_process");

const target = process.platform + "-" + process.arch;
const binaryName = process.platform === "win32" ? "ccn.exe" : "ccn";

function resolveBinary() {
  if (process.env.CCN_BINARY_PATH) {
    return process.env.CCN_BINARY_PATH;
  }
  try {
    const manifest = require.resolve("@shinick/ccn-" + target + "/package.json");
    return path.join(path.dirname(manifest), "vendor", binaryName);
  } catch (_) {
    // Fall back to a hand-placed binary (offline installs).
    return path.join(__dirname, "..", "vendor", binaryName);
  }
}

const binary = resolveBinary();
if (!fs.existsSync(binary)) {
  console.error(
    "ccn: no binary for " + target + ".\n" +
      "Supported: linux-x64, darwin-arm64, win32-x64.\n" +
      "Build from source (https://github.com/Shinick-Han/ccn) or set CCN_BINARY_PATH."
  );
  process.exit(1);
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });

if (result.error) {
  console.error("ccn: " + result.error.message);
  process.exit(1);
}
if (result.signal) {
  process.kill(process.pid, result.signal);
  process.exit(1);
}
process.exit(result.status === null ? 1 : result.status);
