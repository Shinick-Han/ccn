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

## Building from source on AlmaLinux 8

```bash
sudo dnf install -y gcc gcc-c++ make git curl tar gzip
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
. "$HOME/.cargo/env"
cargo build --release --locked
# target/release/ccn
```

Rust 1.80 or newer is required (`rust-version` in `Cargo.toml`); the toolchain
is independent of the system Python.

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
