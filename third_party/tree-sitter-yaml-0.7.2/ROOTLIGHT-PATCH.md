# YAML native grammar qualification

This directory retains the MIT-licensed `tree-sitter-yaml` crate `0.7.2`
for source-backed structural analysis and isolated native tests. Native syntax
qualification alone does not establish complete YAML semantics or MCP coverage.

## Provenance

- Upstream: <https://github.com/tree-sitter-grammars/tree-sitter-yaml>
- Commit: `7708026449bed86239b1cd5bce6e3c34dbca6415` (tag `v0.7.2`).
- Published crate SHA-256:
  `53c223db85f05e34794f065454843b0668ebc15d240ada63e2b5939f43ce7c97`.
- License, node metadata, schemas, query and Rust bindings are retained verbatim.
  Registry markers, Cargo.lock and README are omitted. `src/scanner.c` and
  `grammar.js` are patched; generated files are never hand-edited.

## Grammar changes

Explicit flow pairs accept an omitted key followed by a colon, with or without
a value, as specified by YAML 1.2.2 section 7.4.2. The same anonymous-pair rule
applies to mapping entries, sequence entries and single-line contexts. The
upstream rule accepted implicit empty keys but rejected the explicit form.

Regenerate `src/grammar.json` and `src/parser.c` using Tree-sitter CLI 0.25.10:

```sh
tree-sitter generate --abi 14
```

## Scanner changes

Layout coordinates and indentation lengths use checked 32-bit positions instead
of wrapping signed 16-bit values. Directive version digits and tag-handle names
use presence flags rather than wrapping counters. Parent indentation uses valid
array indices rather than constructing a pointer before the first element.

The scanner owns a fixed indentation stack. Its capacity is derived from
Tree-sitter's 1024-byte serialization buffer, not a repository-specific limit.
Every admitted stack is serialized in full. Version 1 has a 20-byte header and
five bytes per frame: four little-endian positions, a tab flag, a two-byte frame
count, and one kind byte plus a four-byte indentation per frame. There are at
most 200 non-root frames (1020 serialized bytes). The root sentinel is implicit.
All encoding uses byte accesses without alignment or host-byte-order assumptions.

Restoration validates the entire length, version, flags, position domains and
frame kinds before admitting the stack. Empty or malformed input resets prior
context. The `-1` sentinel is allowed only for implicit-key positions and string
indentation. Failed coordinate increments or stack admission persist as the
single byte `0xff`; they do not silently become an empty or truncated stack.
Allocation failure is checked and callbacks tolerate a null payload.

## Verification and limitations

The isolated tests cover exact ranges for large rows/columns, directives,
documents, anchors, aliases, complex keys and scalar styles; complete state
round trips, malformed states, guard bytes and unaligned buffers; and every-node
incremental/fresh-tree equivalence across generic edits and wide positions.
The original dependency reproduces the wide-layout failure and a 1026-byte
serialization result crossing the 1024-byte contract. Native sanitizer probes
exercise malformed state transitions and coordinate overflow. Upstream syntax
and highlighting goldens pass without modifications.

More than 200 simultaneously active indentation frames fails visibly with syntax
errors, rather than corrupting or truncating state. Coordinates beyond the signed
32-bit range are rejected, not wrapped. This qualification does not establish
unlimited nesting, schema/version validation, scalar construction, duplicate-key
validation, alias resolution, structural identity, MCP behavior or repository-wide
coverage. Those require separate adapter and end-to-end evidence.
