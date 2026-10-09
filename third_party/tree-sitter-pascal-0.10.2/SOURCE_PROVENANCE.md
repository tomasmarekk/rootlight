# Pascal source provenance

This package retains reviewed published `tree-sitter-pascal` version 0.10.2
inputs, two separately declared preferred-source spelling fixes, and static
tables generated from that local preferred input. Vendoring preserves access
to source inputs; it does not transfer original authorship or copyright, prove
complete legal/security review, or eliminate upstream supply-chain risks.

## Exact original distribution

The retained `tree-sitter-pascal-0.10.2.crate` is the complete distribution from
`https://static.crates.io/crates/tree-sitter-pascal/tree-sitter-pascal-0.10.2.crate`.
Its 235,910 bytes match the exact-version crates.io checksum
`adb51e9a57493fd237e4517566749f7f7453349261a72a427e5f11d3b34b72a8`.
`UPSTREAM-METADATA.json` preserves the raw exact-version primary metadata from
`https://crates.io/api/v1/crates/tree-sitter-pascal/0.10.2`: 1,827 bytes, SHA-256
`84c1154c5cda99a453e56258f1c56fa978ba4101dd0e7200aaa4645928622367`.
The metadata declares version 0.10.2, MIT, not yanked, published 2025-12-23,
and repository `https://github.com/Isopod/tree-sitter-pascal`.

`UPSTREAM-INVENTORY.json` records all 15 regular published members, totaling
3,976,845 bytes, their original modes and strong byte identities, and exact
retained paths. The expanded original tar is 3,989,504 bytes. No published
member is omitted, and original archive bytes are not presented as a locally
rewritten distribution. Replaced files retain their originals alongside the
active files. `Cargo.toml.orig` is the upstream-authored manifest; the distinct
`Cargo.toml.published.orig` preserves the normalized published manifest without
colliding with it. Original metadata, README, queries, and lock remain inert
input data, not operating instructions or evidence of current dependency
resolution. The original local package lock is not a root workspace lock.

The supplied MIT LICENSE retains copyright 2018 Benjamin Gray and its complete
notice. Neither the publisher metadata nor this local wrapper changes that
attribution. Archive checksums authenticate the received published byte input;
no archive or commit signature was independently verified.

## Authored-source association boundary

The original `.cargo_vcs_info.json` associates the published crate with commit
`2f28b717be47cf592241e1b7bec3b2b906f59148` and an empty `path_in_vcs`.
The exact authored URL
`https://codeload.github.com/Isopod/tree-sitter-pascal/tar.gz/2f28b717be47cf592241e1b7bec3b2b906f59148`
returned HTTP 404 during separate preparation. No floating master, tag, or
alternative commit was substituted. The published preferred grammar remains
available and reviewed, but authored commit reachability, tree equivalence,
original producer identity, and signature authenticity remain unqualified.

## Local preferred input and generated output

`grammar.js.orig` is the byte-exact published preferred source, 34,494 bytes,
SHA-256 `bfdaa71d71ebbd8984c8d2e1e9fbaa69f82a2a4a0484f58b170f94d43a3c46a3`.
The active preferred source changes only `/seled/i` to `/sealed/i` and
`/safecal/i` to `/safecall/i`: 34,496 bytes, SHA-256
`24a71ad61da1ea2d61d101d0c779043880b35cead3c701707d1fea3e98b3dcd9`.
No rule shape, feature flag, precedence, conflict, native field, or external
scanner surface is changed.

Separate preparation used declared local Tree-sitter CLI 0.24.4 and Node
24.11.1 with the exact command:

```text
tree-sitter generate --abi 14 --js-runtime node grammar.js
```

The CLI executable was 20,364,288 bytes, SHA-256
`02ade24b9eca7c23b5a5c41de6599df7540da8ae9f3ac6668d0ca1646ed76323`;
the Node executable was 89,894,400 bytes, SHA-256
`f13ac3ca23248dc389507e8fe38c34489ab7edb3e6d6700eb6da6a0b7e128eaf`.
Two fresh sequential generations each exited 0 with empty stdout and stderr.
All six size/hash pairs and actual byte buffers matched:

| Generated path | Bytes | SHA-256 |
| --- | ---: | --- |
| `src/grammar.json` | 222,417 | `9c639efb2bd12f2b17da6c42bc8c359dfbc0f31c0fc04fb3171893ca8ddf49fd` |
| `src/node-types.json` | 164,317 | `bf6f054b26c91fe3d187a9e8543ef03d1d966f42038cc3ad194111a65187e009` |
| `src/parser.c` | 3,531,893 | `8b0944120dabf8281cc089da3fced09acf08259cc1d52794366d82209c9fc0bf` |
| `src/tree_sitter/alloc.h` | 985 | `b29c1c9fb7cc82f58c84b376df1297d6e2737a1d655fd356db0859e3c29c2fea` |
| `src/tree_sitter/array.h` | 10,420 | `4ff743903dc46f5db6aa54f31c6b4d160a8a9779e5b2ab1ee59ae7ebcd850ea1` |
| `src/tree_sitter/parser.h` | 7,039 | `a1f6ef161fbaf48a0e10fca90ef5290a062462b307b3898aa562993853b9f80a` |

Node metadata and parser header remain byte-identical to the published members.
`alloc.h` and `array.h` are new generated headers; original `grammar.json` and
`parser.c` retain exact `.orig` copies. Matching local twins do not reproduce
the original published tables or prove the unknown original generator version.

The local table declares ABI14, 2,715 states, 560 large states, 371 symbols,
180 tokens, 38 fields, and zero external tokens. The reviewed generic C-lexical
ABI-width check passed on both actual tables: 50,031 ACTIONS references,
11,443 STATE/SHIFT references, maximum action 7,594, maximum state 2,714.
This validates literal width against the supplied uint16 header, not complete
C compilation semantics, grammar coverage, or absence of native vulnerabilities.

## Active build and scope

The local manifest/build/binding preserve upstream package name/version,
repository, license, factory `tree_sitter_pascal`, and `LANGUAGE`/`NODE_TYPES`
interfaces. The manifest is unpublished, uses edition 2024 and MSRV 1.90, and
selects declared existing `tree-sitter-language` 0.1.7 and `cc` 1.2.67.
The build compiles only local static `src/parser.c` with its supplied header.
It performs no acquisition, grammar generation, package lifecycle, or JavaScript
execution. Replaced original Rust inputs remain exact `.orig` files.

The unchanged six unrelated authored native syntax fixtures first observed
four passes and two correct-spelling failures against the original table.
After the preferred-source fixes and authentic regeneration, all six passed;
targeted native harness Clippy with warnings denied also passed. They measure
physical unit/type/routine/formal/field/property identities, exact header and
byte boundaries, overload-site distinction, malformed owner separation,
BOM/CRLF and inert comments/literals. They do not prove semantic bindings,
runtime/compiler behavior, active conditional branches, or whole-language
coverage. The local wrapper documentation reflects patched generated input;
wrapper behavior is unchanged from that tested isolated candidate.

Production registration, installed MCP behavior, full-language percentages,
performance, complete transitive preferred sources, source-to-binary
reproducibility, legal/security completeness, empty dependency caches, and
independently blocked-network build/packaging acceptance are not established
by this source inventory or the selected native witnesses.
