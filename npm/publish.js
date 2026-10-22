#!/usr/bin/env node
"use strict";

// Automated npm publish for @shinick/ccn and its platform packages.
//
//   node npm/publish.js                 # publish missing packages
//   node npm/publish.js --dry-run       # show the plan, publish nothing
//   node npm/publish.js --otp 123456    # interactive/local fallback
//
// Authentication:
//   - CI: NODE_AUTH_TOKEN (npm Automation/Granular token with bypass 2FA)
//   - local: .npmrc _authToken, or NPM_TOKEN / NODE_AUTH_TOKEN
//
// The script is idempotent: a package already at the target version is skipped.
// It verifies the registry after publishing and fails if any package is missing.

const fs = require("fs");
const https = require("https");
const path = require("path");
const { spawnSync } = require("child_process");

const ROOT = __dirname;
const VERSION = require(path.join(ROOT, "package.json")).version;
const DRY_RUN = process.argv.includes("--dry-run");
const OTP_INDEX = process.argv.indexOf("--otp");
const OTP =
  OTP_INDEX >= 0 && process.argv[OTP_INDEX + 1] ? process.argv[OTP_INDEX + 1] : process.env.NPM_OTP || "";

const PLATFORMS = [
  { dir: "platforms/linux-x64", asset: "ccn-linux-x64", file: "ccn" },
  { dir: "platforms/darwin-x64", asset: "ccn-macos-x64", file: "ccn" },
  { dir: "platforms/darwin-arm64", asset: "ccn-macos-arm64", file: "ccn" },
  { dir: "platforms/win32-x64", asset: "ccn-windows-x64.exe", file: "ccn.exe" }
];
const TARGETS = [...PLATFORMS.map((p) => p.dir), "."];

function run(command, args, cwd, env) {
  const result = spawnSync(command, args, {
    cwd,
    env: { ...process.env, ...env },
    stdio: "inherit",
    shell: process.platform === "win32"
  });
  if (result.error) throw result.error;
  return result.status === null ? 1 : result.status;
}

function getJson(url, redirects) {
  return new Promise((resolve, reject) => {
    const request = https.get(
      url,
      { headers: { "User-Agent": "ccn-publish/" + VERSION }, timeout: 20000 },
      (response) => {
        const status = response.statusCode || 0;
        if (status >= 300 && status < 400 && response.headers.location) {
          response.resume();
          if ((redirects || 0) > 5) {
            reject(new Error("too many redirects"));
            return;
          }
          getJson(response.headers.location, (redirects || 0) + 1).then(resolve, reject);
          return;
        }
        if (status === 404) {
          response.resume();
          resolve(null);
          return;
        }
        if (status !== 200) {
          response.resume();
          reject(new Error("HTTP " + status + " for " + url));
          return;
        }
        let body = "";
        response.setEncoding("utf8");
        response.on("data", (chunk) => (body += chunk));
        response.on("end", () => {
          try {
            resolve(JSON.parse(body));
          } catch (error) {
            reject(error);
          }
        });
        response.on("error", reject);
      }
    );
    request.on("timeout", () => request.destroy(new Error("registry timeout")));
    request.on("error", reject);
  });
}

async function published(name) {
  const url = "https://registry.npmjs.org/" + encodeURIComponent(name).replace("%2F", "%2f");
  const document = await getJson(url, 0);
  return Boolean(document && document.versions && document.versions[VERSION]);
}

async function verify() {
  const packages = ["@shinick/ccn", ...PLATFORMS.map((p) => require(path.join(ROOT, p.dir, "package.json")).name)];
  const missing = [];
  for (const name of packages) {
    const ok = await published(name);
    console.log((ok ? "ok " : "?? ") + name + "@" + VERSION);
    if (!ok) missing.push(name);
  }
  if (missing.length > 0) {
    throw new Error("release incomplete: " + missing.join(", "));
  }
  console.log("npm release " + VERSION + " is consistent.");
}

async function main() {
  console.log("ccn npm publish " + VERSION + (DRY_RUN ? " (dry run)" : ""));

  if (!DRY_RUN) {
    const who = spawnSync("npm", ["whoami"], { stdio: "pipe", encoding: "utf8", shell: process.platform === "win32" });
    if (who.status !== 0) {
      throw new Error("npm is not authenticated. Set NODE_AUTH_TOKEN (Automation token) or run npm login.");
    }
    console.log("npm user: " + who.stdout.trim());
  }

  const plan = [];
  for (const dir of TARGETS) {
    const manifestPath = path.join(ROOT, dir, "package.json");
    const manifest = require(manifestPath);
    if (manifest.version !== VERSION) {
      throw new Error(manifest.name + " is " + manifest.version + " but the target is " + VERSION);
    }
    if (dir !== ".") {
      const target = PLATFORMS.find((p) => p.dir === dir);
      const binary = path.join(ROOT, dir, "vendor", target.file);
      if (!fs.existsSync(binary)) {
        throw new Error(manifest.name + " has no vendor/" + target.file + " - run node npm/prepare.js");
      }
      if (!DRY_RUN && process.platform !== "win32") {
        // npm pack on Windows stores 0644. CI packs on Linux, but local
        // Windows runs still need a valid executable in the tarball.
        fs.chmodSync(binary, 0o755);
      }
    }
    const exists = await published(manifest.name);
    console.log((exists ? "skip  " : "publish ") + manifest.name + "@" + VERSION);
    if (!exists) plan.push({ dir, name: manifest.name });
  }

  if (DRY_RUN) {
    console.log("\nplan: " + (plan.length ? plan.map((p) => p.name).join(", ") : "nothing to publish"));
    return;
  }
  if (plan.length === 0) {
    await verify();
    return;
  }

  for (const item of plan) {
    const args = ["publish", "--access", "public", "--fetch-timeout=120000"];
    if (OTP) args.push("--otp=" + OTP);
    console.log("\n>> publishing " + item.name + "@" + VERSION);
    const status = run("npm", args, path.join(ROOT, item.dir));
    if (status !== 0) {
      throw new Error(
        "npm publish failed for " + item.name +
          ". If the error is EOTP, create an npm Automation token (bypass 2FA) " +
          "and set NODE_AUTH_TOKEN, or rerun with --otp <fresh-code>. " +
          "Packages already published are skipped on the next run."
      );
    }
  }

  // Registry metadata can lag after a 202; poll the exact version.
  const deadline = Date.now() + 6 * 60 * 1000;
  for (;;) {
    const missing = [];
    for (const item of plan) {
      if (!(await published(item.name))) missing.push(item.name);
    }
    if (missing.length === 0) break;
    if (Date.now() >= deadline) throw new Error("registry still lacks: " + missing.join(", "));
    await new Promise((resolve) => setTimeout(resolve, 10000));
  }
  await verify();
}

main().catch((error) => {
  console.error("\n" + (error && error.message ? error.message : error));
  process.exit(1);
});
