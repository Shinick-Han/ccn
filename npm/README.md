# @shinick/ccn

Repository code-health scanner: per-file LOC, per-function CCN, distribution
histograms, trend and hotspot ranking.

The binary is installed through a platform package
(`@shinick/ccn-linux-x64`, `@shinick/ccn-darwin-arm64`,
`@shinick/ccn-win32-x64`) selected by npm's `os`/`cpu` fields, so there is no
install script and `npm install --ignore-scripts` works normally.

```console
$ npx @shinick/ccn .                     # terminal summary + ccn-report.html
$ npx @shinick/ccn . --tui               # interactive dashboard
$ npx @shinick/ccn . --view              # temp report + open in browser
$ npx @shinick/ccn . --baseline origin/main --fail-on-new
$ npx @shinick/ccn . --json
```

The exit code is the CLI's exit code, so it works as a CI gate:

```yaml
- run: npx --yes @shinick/ccn . --baseline origin/${{ github.base_ref }} --fail-on-new --no-report
```

## Platforms

| Platform | Package |
|---|---|
| Linux x86_64 (glibc 2.28+, AlmaLinux/RHEL 8+) | `@shinick/ccn-linux-x64` |
| macOS Apple Silicon | `@shinick/ccn-darwin-arm64` |
| Windows x86_64 | `@shinick/ccn-win32-x64` |

Intel Macs, Linux arm64 and Windows arm64 have no prebuilt package; build from
source (`https://github.com/Shinick-Han/ccn`).

## Offline / custom binary

```console
$ export CCN_BINARY_PATH=/usr/local/bin/ccn
```

The launcher is ~2 KB; the binary lives in the platform package.
