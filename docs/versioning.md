# Versioning

ccn uses semantic versioning: `MAJOR.MINOR.PATCH`.

| Change | Version bump |
|---|---|
| Bug fix, copy change, small UX polish | PATCH (`0.14.1`) |
| New user-facing capability or a coherent feature slice | MINOR (`0.15.0`) |
| Breaking CLI/output/session-format change | MINOR while pre-1.0; MAJOR after 1.0 |
| Stable public surface, config and session format | `1.0.0` |

While the project is `0.y.z`, minor versions may include changes that are not
fully backward compatible. We still avoid removing commands silently and keep
older keys as hidden compatibility aliases where practical.

## Release checklist

1. Bump `Cargo.toml` and every `npm/**/package.json`, then `cargo build --release`.
2. Update `CHANGELOG.md` and user-facing key/setup docs.
3. `cargo fmt --check`, tests, clippy (default and `--no-default-features`).
4. Commit, push, run `workbench-check`, tag `vX.Y.Z`, let `release.yml` publish
   binaries and `SHA256SUMS`.
5. Install the Windows asset locally and verify `ccn --version`.
6. `node npm/prepare.js`, then `node npm/publish.js` with the vault automation
   token, then `node npm/verify-release.js`.

Small post-release fixes should normally ship as a PATCH; group several of them
before cutting a MINOR.
