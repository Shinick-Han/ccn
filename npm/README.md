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

## Publishing (maintainers)

Local automation uses an npm **Automation/Granular token with bypass 2FA** so
publishing no longer stops at an OTP prompt. Create it once:

```console
$ npm token create --name ccn-automation --packages-all \
    --packages-and-scopes-permission read-write --bypass-2fa
$ npm config set //registry.npmjs.org/:_authToken <token>
```

Then release locally:

```powershell
pwsh -File npm\publish.ps1  # downloads v<version> assets, then publishes (idempotent)
pwsh -File npm\publish.ps1 -Check
```

`publish.js` skips versions already on the registry, publishes platform
packages before the main package, and polls the exact versions afterwards.
Without an automation token, `-Otp <code>` still works but the OTP expires
quickly across five packages.

GitHub Actions npm publishing is not used because the workflow requires hosted
runner minutes; the local script is the supported path.

## Offline / custom binary

```console
$ export CCN_BINARY_PATH=/usr/local/bin/ccn
```

The launcher is ~2 KB; the binary lives in the platform package.
