# Rootlight

Rootlight is a local-first repository intelligence engine and MCP server.
This package installs the official native Rootlight command-line distribution
for the current supported platform.

```sh
npm install --global @tomasmarekk/rootlight
```

The installation registers and starts Rootlight as a private per-user local
service. The complete Web UI and its backend are ready when the npm command
returns:

<http://127.0.0.1:43127/>

No terminal process needs to remain open. `rootlight web` can reopen the local
application later, while `rootlight service status`, `rootlight service stop`,
and `rootlight service restart` provide explicit lifecycle control.

Update the npm-managed installation with:

```sh
npm update --global @tomasmarekk/rootlight
```

Remove the complete npm-managed installation with either command:

```sh
rootlight uninstall
# or
npm uninstall --global @tomasmarekk/rootlight
```

Uninstall stops the Web UI and backend, removes login autostart, and removes the
npm executables. Indexed repositories and Rootlight's local user data are
preserved.

The installer selects a signed release package for macOS (Apple Silicon or
Intel), Linux glibc (Arm64 or x64), or Windows x64. Release archives,
checksums, SBOMs, signatures, and provenance are published with the matching
[GitHub release](https://github.com/tomasmarekk/rootlight/releases).

Source, security policy, and issue tracking live in the
[Rootlight repository](https://github.com/tomasmarekk/rootlight).
