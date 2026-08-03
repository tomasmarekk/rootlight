# Package verification

Rootlight distributions are deterministic ZIP archives intended for per-user,
least-privilege installation. The archive does not enable daemon autostart.
Registration of the packaged platform template requires an explicit user
choice.

Run the package contract and ownership checks:

```text
cargo run --locked --package xtask -- package-check
```

Build a package from release binaries:

```text
cargo run --locked --package xtask -- package-build \
  --target x86_64-unknown-linux-gnu \
  --version 0.1.0 \
  --source-revision 0123456789abcdef0123456789abcdef01234567 \
  --bin-dir target/release \
  --web-assets-dir apps/rootlight-web/frontend/dist \
  --web-notices artifacts/web/rootlight-web-third-party-notices.txt \
  --output-dir artifacts/packages
```

The command writes one immutable archive and a matching `.sha256` file. The
archive contains `package-manifest.json`, the license, all required binaries,
the stable launcher, an inert autostart template, and the exact bounded
`share/rootlight/web` asset inventory. The web asset directory must already
come from the pinned deterministic front-end build; Node.js is not copied into
or required by the native package. Verify the detached digests before opening
the archive, then independently compare every entry against the embedded
manifest.

Exercise the exact candidate lifecycle with an older archive built from the
same source revision:

```text
cargo run --locked --package xtask -- package-smoke \
  --baseline-archive artifacts/baseline/rootlight-0.0.0-x86_64-unknown-linux-gnu.zip \
  --archive artifacts/packages/rootlight-0.1.0-x86_64-unknown-linux-gnu.zip \
  --source-revision 0123456789abcdef0123456789abcdef01234567 \
  --output artifacts/packages/x86_64-unknown-linux-gnu.lifecycle.json
```

The lifecycle command installs the baseline side by side, applies and launches
the exact candidate, checks health and clean recovery, observes rollback after
a rejected health check, uninstalls owned files, and verifies that user and
unowned data remain unchanged. The immutable JSON report binds those
observations to the candidate archive digest and source revision.

Installers must preserve user data, activate new versions side by side, retain
the last good version for rollback, and remove only paths and platform
resources recorded in `state/install-manifest.json`.
