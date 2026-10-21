# Compatibility

`ccn` is a single Rust binary. There is **no Python dependency at build time or
runtime**, so a Python 3.8 environment (AlmaLinux 8's default `python3`) does not
need to be upgraded, patched, or considered. If you drive `ccn` from a Python
3.8 script, invoke it as a subprocess and read `--json` / `--report-json`; the
JSON is plain UTF-8 with no version-specific features.

## Supported targets

| Platform | Asset | Minimum |
|---|---|---|
| Linux x86_64 | `ccn-linux-x64` | glibc **2.28** (AlmaLinux/RHEL/Rocky 8+, Debian 10+, Ubuntu 20.04+) |
| macOS (Apple Silicon runner) | `ccn-macos-x64` | macOS 11+ |
| Windows x86_64 | `ccn-windows-x64.exe` | Windows 10+ (MSVC runtime is linked statically where possible) |

These minimums describe the **prebuilt release assets**. Running a downloaded
binary never requires Rust, Cargo, a compiler, or Python; the Linux column is a
glibc (C library) contract, not a toolchain contract. The Rust requirement below
applies only when you build `ccn` from source.

### Why the Linux binary is built on AlmaLinux 8

A binary built on `ubuntu-latest` links against glibc 2.39 and fails on
AlmaLinux 8 with:

```
./ccn: /lib64/libc.so.6: version `GLIBC_2.34' not found
```

The release workflow therefore builds inside an `almalinux:8` container and
**fails the release** if the resulting binary references a symbol newer than
`GLIBC_2.28`. Verify a downloaded asset yourself:

```bash
objdump -T ccn-linux-x64 | grep -o 'GLIBC_[0-9.]*' | sort -V | tail -1
# => GLIBC_2.28 (or older)
```

## Rust version for source builds

The minimum supported Rust version (MSRV) is **1.90**, and it is enforced by
`rust-version = "1.90"` in `Cargo.toml`. The supported path is:

```bash
cargo build --release --locked    # stable Rust >= 1.90
```

The MSRV is set by the dependency tree, not by `ccn`'s own code. The locked set
declares:

| Crate | Declared `rust-version` |
|---|---|
| `tree-sitter` 0.27.1, `tree-sitter-language` 0.1.9 | **1.90** |
| `darling` / `darling_macro` / `darling_core` 0.24.1, `ignore` 0.4.33, `encoding_rs` 0.8.42, `globset` 0.4.20 | 1.88 |
| `clap-cargo` 0.18.3 | 1.86 |
| `unicode-segmentation` 1.13.3 | 1.85 |

Because Cargo checks these declarations, an older compiler rejects the package
up front with an explicit "requires rustc 1.90" error. That is the intended
failure mode: you get one clear message instead of a confusing failure deep
inside a dependency's build script. Do not lower `rust-version` without also
resolving those dependencies, and do not run `cargo update` to make a release
build pass — the MSRV is derived from the committed `Cargo.lock`.

Re-derive the requirement from the lockfile at any time:

```bash
cargo metadata --format-version 1 --locked \
  | jq -r '.packages[] | select(.rust_version) | "\(.rust_version) \(.name) \(.version)"' \
  | sort -V | tail -5
```

## Building from source on AlmaLinux 8

The AlmaLinux 8 build below is about matching the release binary's **glibc
2.28** floor, which is unrelated to the Rust version. `rustup` installs current
stable, which already satisfies the 1.90 MSRV.

```bash
sudo dnf install -y gcc gcc-c++ make git curl tar gzip
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
. "$HOME/.cargo/env"
cargo build --release --locked
# target/release/ccn
```

The Rust toolchain is independent of the system Python, and replacing the
prebuilt binary is optional — if you only need a working `ccn` on AlmaLinux 8,
download `ccn-linux-x64` and skip this section.

## Python 3.8 examples

```python
import json, subprocess

out = subprocess.run(
    ["ccn", ".", "--report-json"],
    check=True, capture_output=True, text=True,
).stdout
report = json.loads(out)
print(report["health"]["score"], report["health"]["grade"])
```

`text=True` is safe: the report path, function names and messages are valid
UTF-8. On Windows, pass `encoding="utf-8"` explicitly if the system locale is
not UTF-8.
