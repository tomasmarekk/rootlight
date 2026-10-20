# Source-backed Zig fixtures

The original Ghostty `src/os/pipe.zig` and six explicit parser-recovery
witness files are retained with the original MIT notice. Their exact
physical byte identities are recorded in `source-pins.json`.
The 772-byte pipe fixture supplies written function/local-variable
definitions and lexical containment. Platform execution, types, imports
and resolved calls are not inferred. Recovery fixtures remain applicable
known parser gaps; they are never counted as clean language support.
