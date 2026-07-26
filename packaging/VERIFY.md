# Package verification

Rootlight distributions are deterministic ZIP archives intended for per-user,
least-privilege installation. The archive does not enable daemon autostart.
Registration of the packaged platform template requires an explicit user
choice.

Run the package contract and ownership checks:

```text
cargo run --locked --package xtask -- package-check
cargo run --locked --package xtask -- package-smoke --target x86_64-unknown-linux-gnu
```

Build a package from release binaries:

```text
cargo run --locked --package xtask -- package-build \
  --target x86_64-unknown-linux-gnu \
  --version 0.1.0 \
  --source-revision 0123456789abcdef0123456789abcdef01234567 \
  --bin-dir target/release \
  --output-dir artifacts/packages
```

The command writes one immutable archive and a matching `.sha256` file. The
archive contains `package-manifest.json`, the license, all required binaries,
and an inert autostart template. Verify the detached digest before opening the
archive, then independently compare every entry against the embedded manifest.

Installers must preserve user data, activate new versions side by side, retain
the last good version for rollback, and remove only paths and platform
resources recorded in `state/install-manifest.json`.
