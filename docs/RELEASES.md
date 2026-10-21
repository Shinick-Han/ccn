# Releases

## Cutting a release

Releases are built by `.github/workflows/release.yml` on a `v*` tag push:

```sh
git tag v0.7.0
git push --tags
```

The workflow can also be started manually with `workflow_dispatch` (mainly to
dry-run the builds). Three jobs build `cargo build --release --locked` in
parallel and attach the binary to the same GitHub release with generated release
notes.

## Assets

Raw single-file binaries, no archives:

| Asset                  | Runner           | Source path              |
| ---------------------- | ---------------- | ------------------------ |
| `ccn-linux-x64`        | `ubuntu-latest`  | `target/release/ccn`     |
| `ccn-macos-x64`        | `macos-latest`   | `target/release/ccn`     |
| `ccn-windows-x64.exe`  | `windows-latest` | `target/release/ccn.exe` |

Install by downloading the asset, `chmod +x` on Unix, and putting it on `PATH`.

## How the action uses these assets

`action.yml` asks the runner for its platform and downloads the matching asset
from `https://github.com/Shinick-Han/ccn/releases/latest/download/<asset>`. It
places the binary at `<action_path>/target/release/ccn` (`ccn.exe` on Windows,
where the extensionless path still resolves), which is where the scan step
looks. If the download fails, the action logs `prebuilt` vs `source` and falls
back to `cargo build --release --locked` inside the checked-out action.

## Private repositories

Downloading release assets from a **private** repository without authentication
returns HTTP 404, so an unauthenticated `curl` from another repository fails and
the action builds from source instead (this always works, but is slower and
needs a Rust toolchain on the runner).

The action sends `Authorization: Bearer $GITHUB_TOKEN` when a token is present
and retries anonymously, then falls back. `GITHUB_TOKEN` only helps when that
token can read the `Shinick-Han/ccn` repository — the calling repository's own
`github.token` cannot see a different private repository, in which case a PAT
with `contents: read` would be needed. For public repositories the anonymous
path is used and everything works without a token.

## macOS architecture

`macos-latest` currently runs on Apple silicon (`arm64`), so `ccn-macos-x64` is
a native `arm64` binary despite its name — it is neither a universal binary nor
an Intel-native build. The action maps both `arm64` and `x86_64` Darwin to this
single asset, so on an Intel runner the downloaded binary is the wrong
architecture and will not run. Intel users should build from source (the action
falls back automatically only when the download itself fails). The asset name is
kept stable so action downloads do not depend on the runner image of the day.

## npm

`npm/` contains four packages:

| package | contents |
|---|---|
| `@shinick/ccn` | 2 KB launcher (`bin/ccn.js`), `optionalDependencies` on the three platform packages |
| `@shinick/ccn-linux-x64` | `ccn-linux-x64` (glibc 2.28) |
| `@shinick/ccn-darwin-arm64` | `ccn-macos-x64` (arm64) |
| `@shinick/ccn-win32-x64` | `ccn-windows-x64.exe` |

No install script runs: npm picks the platform package through its `os`/`cpu`
fields (the esbuild pattern), so `npm install --ignore-scripts` works.

Publish flow:

```powershell
npm login
node npm/prepare.js          # downloads the release binaries into platforms/*/vendor
pwsh -File npm/publish.ps1   # publishes the three platform packages, then the main one
```

The binaries are gitignored (`npm/platforms/*/vendor/`) so the repository does
not grow by 60 MB per release. `prepare.js` fetches exactly the version in
`npm/package.json`; bump both `Cargo.toml` and `npm/package.json` together.
