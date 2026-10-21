#!/usr/bin/env node
"use strict";

// Populates the platform packages with the release binaries before publishing.
// The binaries are intentionally NOT committed (60 MB of blobs in git); this
// script downloads them from the GitHub release that matches package.json.

const fs = require("fs");
const path = require("path");
const https = require("https");

const VERSION = require("./package.json").version;
const REPO = "Shinick-Han/ccn";
const ROOT = __dirname;

const TARGETS = [
  { dir: "platforms/linux-x64", asset: "ccn-linux-x64", file: "ccn" },
  { dir: "platforms/darwin-x64", asset: "ccn-macos-x64", file: "ccn" },
  { dir: "platforms/darwin-arm64", asset: "ccn-macos-arm64", file: "ccn" },
  { dir: "platforms/win32-x64", asset: "ccn-windows-x64.exe", file: "ccn.exe" }
];

function download(url, destination, redirects) {
  return new Promise((resolve, reject) => {
    const request = https.get(url, { headers: { "User-Agent": "ccn-npm/" + VERSION } }, (response) => {
      const status = response.statusCode || 0;
      if (status >= 300 && status < 400 && response.headers.location) {
        response.resume();
        if ((redirects || 0) > 5) {
          reject(new Error("too many redirects"));
          return;
        }
        download(response.headers.location, destination, (redirects || 0) + 1).then(resolve, reject);
        return;
      }
      if (status !== 200) {
        response.resume();
        reject(new Error("HTTP " + status + " for " + url));
        return;
      }
      const out = fs.createWriteStream(destination, { mode: 0o755 });
      response.pipe(out);
      out.on("finish", () => out.close(resolve));
      out.on("error", reject);
      response.on("error", reject);
    });
    request.on("error", reject);
  });
}

async function main() {
  for (const target of TARGETS) {
    const dir = path.join(ROOT, target.dir, "vendor");
    fs.mkdirSync(dir, { recursive: true });
    const destination = path.join(dir, target.file);
    const url = "https://github.com/" + REPO + "/releases/download/v" + VERSION + "/" + target.asset;
    process.stdout.write("downloading " + target.asset + " ... ");
    await download(url, destination, 0);
    fs.chmodSync(destination, 0o755);
    console.log(fs.statSync(destination).size + " bytes");
  }
  console.log("platform packages prepared for v" + VERSION);
}

main().catch((error) => {
  console.error(error && error.message ? error.message : error);
  process.exit(1);
});
