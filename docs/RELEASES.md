# Releases

## Cutting a release

Releases are built by `.github/workflows/release.yml` on a `v*` tag push:

```sh
git tag v0.7.0
git push --tags
```

The workflow can also be started manually with `workflow_dispatch` (mainly to
dry-run the builds); the publish steps need a tag ref.

The job graph is a gate, not a fan-out:

```
tag push
  -> checks                          (gate, ubuntu-latest)
       cargo fmt --check
       cargo clippy --locked --all-targets -- -D warnings
       cargo test --release --locked
  -> linux, macos-x64, macos-arm64, windows   (needs: checks)
  -> checksums                       (needs: [linux, macos-x64, macos-arm64, windows])
```

`checks` must be green before any build job starts, so a tag that fails
formatting, lints or tests cannot produce a release asset at all. The Linux job
still runs `cargo build --release --locked` inside an AlmaLinux 8 container, but
the suite is not repeated there: the ubuntu job is the gate.

The four build jobs then run in parallel and attach their binary to the same
GitHub release with generated release notes. The macOS jobs assert their runner
architecture (`macos-15-intel` = `x86_64`, `macos-latest` = `arm64`) before building,
so an asset name cannot quietly describe the wrong CPU. Finally `checksums`
downloads every published `ccn-*` asset, hashes exactly the bytes a consumer
would get, and uploads `SHA256SUMS` to the same release. It does not regenerate
release notes.

## Verifying a download

Every release carries `SHA256SUMS` next to the four binaries. Download it and
the asset(s) into the same directory (`ccn-*` covers all of them):

```sh
gh release download v0.7.5 --dir dist
cd dist
```

Then verify:

```sh
# Linux
sha256sum -c SHA256SUMS --ignore-missing
```

```sh
# macOS (sha256sum is not installed by default)
shasum -a 256 -c SHA256SUMS
```

```powershell
# Windows PowerShell: compare the printed hash with the ccn-windows-x64.exe
# line in SHA256SUMS (Get-FileHash prints uppercase hex, the file is lowercase)
Get-FileHash -Algorithm SHA256 .\ccn-windows-x64.exe
```

`--ignore-missing` lets you verify a subset of platforms; without it
`sha256sum` fails on assets you did not download. A checksum catches a
truncated, corrupted or substituted download, but it is not a signature: it
comes from the same release, so it does not prove who built the binary. Treat
it as an integrity check, not provenance.

## Assets

Raw single-file binaries, no archives:

| Asset                  | Runner           | Architecture | Source path              |
| ---------------------- | ---------------- | ------------ | ------------------------ |
| `ccn-linux-x64`        | `ubuntu-latest`  | x86_64       | `target/release/ccn`     |
| `ccn-macos-x64`        | `macos-15-intel`       | x86_64       | `target/release/ccn`     |
| `ccn-macos-arm64`      | `macos-latest`   | arm64        | `target/release/ccn`     |
| `ccn-windows-x64.exe`  | `windows-latest` | x86_64       | `target/release/ccn.exe` |

Install by downloading the asset, `chmod +x` on Unix, and putting it on `PATH`.
`SHA256SUMS` is published by the `checksums` job after all four binaries, so it
never lists an asset that does not exist.

## How the action uses these assets

`action.yml` asks the runner for its platform and downloads the matching asset
from `https://github.com/Shinick-Han/ccn/releases/latest/download/<asset>`. Set
the `version` input to pin the binary to a release tag instead:

```yaml
- uses: Shinick-Han/ccn@v0.7.5
  with:
    version: v0.7.5
```

With a tag, the URL becomes
`https://github.com/Shinick-Han/ccn/releases/download/<version>/<asset>`; the
default `latest` keeps the old `releases/latest/download` behavior. Pinning the
action ref alone is not enough: the ref decides which `action.yml` runs, while
this input decides which binary it downloads.

The action places the binary at `<action_path>/target/release/ccn` (`ccn.exe` on
Windows, where the extensionless path still resolves), which is where the scan
step looks.

Before using a downloaded binary the action:

1. downloads `SHA256SUMS` from the same release and compares the asset's digest
   (`sha256sum`, `shasum -a 256`, or `Get-FileHash`/`certutil` on Windows);
2. runs `<binary> --version` and, for a pinned tag, requires the reported
   version to equal the tag with its leading `v` stripped. For `latest` it logs
   the reported version.

Failure rules:

- with `version: latest`, a missing asset, missing `SHA256SUMS`, or a download
  failure only warns and falls back to `cargo build --release --locked` inside
  the checked-out action;
- with a pinned tag, any of those is a step failure. The action never
  substitutes its own checkout for the requested release, because that would
  silently run a different version. A `SHA256SUMS` file that exists but does not
  list the selected asset is always an error.

## Action pinning

Every third-party action in `.github/workflows/` is pinned to a full 40-character
commit SHA with the resolved version in a trailing comment:

```yaml
- uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4.4.0
```

Mutable references such as `@v4` or `@stable` are re-pointed upstream whenever a
new release ships. A moved — or maliciously moved — tag silently changes the
code that runs in this repository's release pipeline, with no commit here to
review. A commit SHA cannot move. `dtolnay/rust-toolchain` has no tagged
releases, so it is pinned to the commit that was the head of its `stable` branch
when the pin was taken; `with: toolchain: stable` keeps the selected toolchain
explicit.

To bump a pin, resolve the new commit and update the SHA and the comment
together:

```sh
# The ^{} line is the commit the tag points at; the other line is the tag
# object for annotated tags (identical for lightweight tags).
git ls-remote https://github.com/actions/checkout 'refs/tags/v4.4.0*'
```

Never copy a SHA from a blog post or guess one — resolve it from the upstream
repository and confirm it appears in the pinned form above.

## Private repositories

Downloading release assets from a **private** repository without authentication
returns HTTP 404. With `version: latest` the action then builds from source
(slower, and it needs a Rust toolchain on the runner); with a pinned tag the
step fails rather than running a checkout that may be a different version.

The action sends `Authorization: Bearer $GITHUB_TOKEN` when a token is present
and retries anonymously, then falls back. `GITHUB_TOKEN` only helps when that
token can read the `Shinick-Han/ccn` repository — the calling repository's own
`github.token` cannot see a different private repository, in which case a PAT
with `contents: read` would be needed. For public repositories the anonymous
path is used and everything works without a token.

## macOS architecture

macOS gets two native binaries: `ccn-macos-x64` from `macos-15-intel` (Intel) and
`ccn-macos-arm64` from `macos-latest` (Apple silicon). Neither is a universal
binary; the action selects by `uname -m` (`x86_64` → `ccn-macos-x64`, `arm64` →
`ccn-macos-arm64`) and the npm packages select by npm's `cpu` field. Running the
wrong one fails at exec time, so the build jobs assert the runner architecture
rather than trusting the image label.

## npm

`npm/` contains five packages:

| package | contents |
|---|---|
| `@shinick/ccn` | 2 KB launcher (`bin/ccn.js`), `optionalDependencies` on the four platform packages |
| `@shinick/ccn-linux-x64` | `ccn-linux-x64` (glibc 2.28) |
| `@shinick/ccn-darwin-x64` | `ccn-macos-x64` (Intel) |
| `@shinick/ccn-darwin-arm64` | `ccn-macos-arm64` (Apple silicon) |
| `@shinick/ccn-win32-x64` | `ccn-windows-x64.exe` |

No install script runs: npm picks the platform package through its `os`/`cpu`
fields (the esbuild pattern), so `npm install --ignore-scripts` works.

Publish flow:

```powershell
npm login
node npm/prepare.js          # downloads the release binaries into platforms/*/vendor
pwsh -File npm/publish.ps1   # publishes the platform packages, then the main one
```

The binaries are gitignored (`npm/platforms/*/vendor/`) so the repository does
not grow by 60 MB per release. `prepare.js` fetches exactly the version in
`npm/package.json`; bump both `Cargo.toml` and `npm/package.json` together.
