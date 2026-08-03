# Rootlight

Rootlight is a local-first repository intelligence engine and MCP server.
This package installs the official native Rootlight command-line distribution
for the current supported platform.

```sh
npm install --global @tomasmarekk/rootlight
```

Start the CLI-connected local Web UI with:

```sh
rootlight web
```

Rootlight starts its private local backend automatically, opens an authenticated
loopback URL, and stops the backend it owns when the Web UI command exits. If a
browser cannot be opened automatically, use the complete URL printed in the
terminal. Keep the terminal process running while using the Web UI.

Update the npm-managed installation with:

```sh
npm update --global @tomasmarekk/rootlight
```

Close active Rootlight commands and sessions before removing the package:

```sh
npm uninstall --global @tomasmarekk/rootlight
```

Uninstalling the npm package removes its executables but preserves repositories
and Rootlight's local user data.

The installer selects a signed release package for macOS (Apple Silicon or
Intel), Linux glibc (Arm64 or x64), or Windows x64. Release archives,
checksums, SBOMs, signatures, and provenance are published with the matching
[GitHub release](https://github.com/tomasmarekk/rootlight/releases).

Source, security policy, and issue tracking live in the
[Rootlight repository](https://github.com/tomasmarekk/rootlight).
