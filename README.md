![Banner](https://i.imgur.com/etzXu2S.png)

[![CI](https://repo-badges.46.224.229.218.sslip.io/github/ci/tomasmarekk/rootlight.svg?workflow=313866905&branch=main&variant=secondary&v=ci-313866905)](https://github.com/tomasmarekk/rootlight/actions/workflows/ci.yml)
![badge](https://repo-badges.46.224.229.218.sslip.io/badge/Win%20•%20MacOS%20•%20Linux.svg?variant=secondary&logo=ri%3ABsLaptop&valueColor=ffffff&labelTextColor=ffffff)
[![Repo License](https://repo-badges.46.224.229.218.sslip.io/github/license/tomasmarekk/rootlight.svg?variant=secondary&v=public)](https://github.com/tomasmarekk/rootlight/blob/main/LICENSE)

This repository is currently in the development phase.

## Platform capability status

### macOS support-bundle file output

Rootlight does not currently support writing support-bundle archives on macOS. A
valid `rootlight support-bundle --output <file>` request fails closed before
runtime-directory resolution, daemon discovery or startup, bundle generation,
output-path inspection, or filesystem mutation.

The versioned CLI error reports:

- exit family `degraded`;
- error code `UNSUPPORTED_CAPABILITY`;
- capability detail `support_bundle_output`;
- platform detail `macos`.

The error never includes the requested path. Do not work around this boundary by
weakening file permissions or redirecting sensitive output through an
unprotected temporary path. macOS support-bundle file output remains unavailable
until the Proposed ADR-026 private-tree boundary is accepted, implemented
without weakening Rootlight's unsafe-code policy, and verified by native hostile
APFS tests.

This limitation is specific to support-bundle file publication and is not a
claim that the complete Rootlight product is supported on macOS.

## License

Rootlight is open source under the [GNU Affero General Public License v3.0 only](./LICENSE)
(`AGPL-3.0-only`).
