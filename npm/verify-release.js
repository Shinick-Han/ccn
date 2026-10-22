#!/usr/bin/env node
"use strict";

// Read-only npm release consistency check.
//
//   node npm/verify-release.js
//
// Fails when the main package or any platform package is missing the version in
// package.json, or when the main optionalDependencies do not pin that exact
// version. This catches the partial-publish state left behind by an expired OTP.

const https = require("https");
const path = require("path");

const VERSION = require(path.join(__dirname, "package.json")).version;
const PACKAGES = [
  "@shinick/ccn",
  "@shinick/ccn-linux-x64",
  "@shinick/ccn-darwin-x64",
  "@shinick/ccn-darwin-arm64",
  "@shinick/ccn-win32-x64"
];

function getJson(url, redirects) {
  return new Promise((resolve, reject) => {
    const request = https.get(
      url,
      { headers: { "User-Agent": "ccn-verify/" + VERSION }, timeout: 15000 },
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
        if (status !== 200) {
          response.resume();
          reject(new Error("HTTP " + status));
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

async function main() {
  const missing = [];
  for (const name of PACKAGES) {
    const url = "https://registry.npmjs.org/" + encodeURIComponent(name).replace("%2F", "%2f");
    let document;
    try {
      document = await getJson(url, 0);
    } catch (error) {
      console.log("? " + name + " -> " + error.message);
      missing.push(name);
      continue;
    }
    const present = Boolean(document.versions && document.versions[VERSION]);
    console.log((present ? "ok " : "?? ") + name + "@" + VERSION);
    if (!present) missing.push(name);
  }

  const mainUrl = "https://registry.npmjs.org/" + encodeURIComponent("@shinick/ccn").replace("%2F", "%2f");
  try {
    const main = await getJson(mainUrl, 0);
    const manifest = main.versions && main.versions[VERSION];
    if (manifest) {
      for (const [name, wanted] of Object.entries(manifest.optionalDependencies || {})) {
        if (wanted !== VERSION) {
          console.log("!! optional dependency " + name + " is " + wanted + ", expected " + VERSION);
          missing.push(name);
        }
      }
    }
  } catch (_) {
    // Already reported above.
  }

  if (missing.length > 0) {
    console.error("\nrelease incomplete for " + VERSION + ": " + missing.join(", "));
    console.error("rerun: pwsh -File npm/publish.ps1 -Otp <fresh-code>");
    process.exit(1);
  }
  console.log("\nnpm release " + VERSION + " is consistent.");
}

main().catch((error) => {
  console.error(error && error.message ? error.message : error);
  process.exit(1);
});
