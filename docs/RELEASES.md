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
  -> linux, macos, windows           (needs: checks)
  -> checksums                       (needs: [linux, macos, windows])
```

`checks` must be green before any build job starts, so a tag that fails
formatting, lints or tests cannot produce a release asset at all. The Linux job
still runs `cargo build --release --locked` inside an AlmaLinux 8 container, but
the suite is not repeated there: the ubuntu job is the gate.

The three build jobs then run in parallel and attach their binary to the same
GitHub release with generated release notes. Finally `checksums` downloads the
three published assets, hashes exactly the bytes a consumer would get, and
uploads `SHA256SUMS` to the same release. It does not regenerate release notes.

## Verifying a download

Every release carries `SHA256SUMS` next to the three binaries. Download it and
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

| Asset                  | Runner           | Source path              |
| ---------------------- | ---------------- | ------------------------ |
| `ccn-linux-x64`        | `ubuntu-latest`  | `target/release/ccn`     |
| `ccn-macos-x64`        | `macos-latest`   | `target/release/ccn`     |
| `ccn-windows-x64.exe`  | `windows-latest` | `target/release/ccn.exe` |

Install by downloading the asset, `chmod +x` on Unix, and putting it on `PATH`.
`SHA256SUMS` is published by the `checksums` job after all three binaries, so it
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
step looks. If the download fails, the action logs `prebuilt` vs `source` and
falls back to `cargo build --release --locked` inside the checked-out action.

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
