# Local Apex-only binding

This package retains the identity `tree-sitter-sfapex` version `3.0.1`, but its
active Rust binding is an unpublished local Apex-only integration. It is not an
unmodified copy of the published four-component Rust package and is not the
separately published `tree-sitter-apex` package.

## Exact original inputs

All 45 regular members of the published crate are retained byte-for-byte. Their
original sizes, SHA-256 values, archive modes, and retention paths are listed in
`SOURCE-PROVENANCE.json`. The original registry-normalized `Cargo.toml` is saved
as `Cargo.toml.upstream`; the authentic authored `Cargo.toml.orig` stays at its
original path. The original four-component `bindings/rust/build.rs` and
`bindings/rust/lib.rs` remain unchanged and inactive.

The exact published crate is retained as
`upstream/tree-sitter-sfapex-3.0.1.crate`:

- 530,869 bytes; SHA-256
  `e902b5d7951d1477ff43ad5ec8b2e6adae029cce593a770c64e10362ee7dec7b`.
- All 45 published members total 9,858,531 bytes.
- The published VCS metadata identifies revision
  `da568eee10ce4724a40f368b2b7e795dacb4f1ba`.

The exact authored revision archive is also retained as
`upstream/tree-sitter-sfapex-da568eee10ce4724a40f368b2b7e795dacb4f1ba.tar.gz`:

- 977,159 bytes; SHA-256
  `ae9e1b2e96f01501b6906f361fdd3206e8128862d137477c7d17d14ba9373ae1`.
- Its 196 regular members total 12,849,778 bytes. The complete authored inventory
  is recorded in the provenance document; the complete archive remains inert.
- 42 published members match their mapped authored counterparts exactly.
  `.cargo_vcs_info.json` and `Cargo.lock` are publication-only members, and the
  normalized `Cargo.toml` differs from the authored manifest. Published
  `Cargo.toml.orig` matches authored `Cargo.toml` exactly.

The revision association is supported by the observed published VCS metadata
and the exact authored archive's commit comment. It is not a verified archive
signature or a claim about a current upstream branch.

## Three local binding inputs

1. `Cargo.toml` replaces only the active normalized manifest. It keeps the
   original package name/version, disables publication, selects root `build.rs`
   and `src/lib.rs`, and uses the existing exact dependency versions
   `tree-sitter-language =0.1.7` and `cc =1.2.67`. The original publication lock
   remains unchanged and is not the lock used by Rootlight or the isolated
   native validation harness.
2. The new root `build.rs` compiles only the unchanged `apex/src/parser.c` as C11
   with its supplied `apex/src/tree_sitter` headers. It performs no acquisition,
   grammar generation, JavaScript evaluation, or package lifecycle hooks.
3. The new `src/lib.rs` retains the upstream `apex::LANGUAGE` and
   `apex::NODE_TYPES` API without separately exposing the other three native
   parsers. Its safety contract identifies the supplied `tree_sitter_apex` C
   factory and its immutable process-lifetime language table.

No parser, preferred grammar, generated metadata, scanner, header, or query
bytes are patched. The package retains the Apex, SOQL, SOSL, and SFLOG static
tables, all preferred grammar entry points, and the shared `common` JavaScript
sources. In particular, Apex's source grammar imports the retained common SOSL
and SOQL closure; these files are not omitted because only Apex is built.
All four supplied parsers declare ABI 15 and require no external scanner.

The three local binding inputs are byte-identical to the reviewed inputs used
in native validation; formatting did not change them. Their exact identities
are listed under `local_binding_changes` in `SOURCE-PROVENANCE.json`.

## Authentic notices and remaining attribution uncertainty

The published crate did **not** contain a license file. Active `LICENSE` is the
authentic MIT notice from the exact authored revision archive, including
`Copyright (c) 2012 Anthony Heber`. Its 1,079 bytes have SHA-256
`c3d045abbc0a8dec1b00737c0668f6dbb028d248521b74e79778c726671925ed`.
It is not synthesized from the crate's SPDX declaration.

The retained Apex grammar explicitly attributes adaptation from Tree-sitter's
Java grammar, but does not identify the historical Java revision. `LICENSE-JAVA`
is a supplemental exact copy of the existing project's authenticated Java
0.23.5 MIT notice, including `Copyright (c) 2017 Ayman Nadeem`. Its 1,069 bytes
have SHA-256
`52ed137b039cd9c46409bc22e89938af911c95b157feae2d040b51e6084369a7`.
That existing Java input's revision is not claimed to be Apex's ancestor.
The historical ancestry and complete derived-notice chain remain unqualified;
retaining this authentic supplemental notice does not establish them.

## Actual bounded native observations

An independent lexical check accepted the literal generated C `ACTIONS`,
`STATE`, `SHIFT`, and `SHIFT_REPEAT` indexes against the supplied `uint16_t`
table/state widths for all four parsers. The maximum action/state indexes were
Apex 8,635/5,340, SOQL 1,724/811, SOSL 1,920/935, and SFLOG 152/52. This is a
width check, not a full C analysis or a compiler/semantic qualification. The
observation receipt is SHA-256
`3a487a7bca6845886076b3115777f0c2b65859e214575596a13195218e0ad89f`.

The local binding and unchanged Apex parser subsequently passed six native
fixture tests using Tree-sitter 0.26.11, Rust 1.97.1, and the Windows MSVC host:

- Written class, enum, constructor, method, and formal ownership with exact
  original physical spans, including CRLF.
- Interface prototype extent and written parameter type.
- Distinct nested class members and multi-name fields with shared type syntax.
- Annotation/sharing modifiers and Apex `switch on`/`when` tokens.
- Comment and quoted declaration text exclusion.
- Malformed-header ownership recovery without borrowing a following class.

Actual `cargo test --offline --locked` completed with six passes and no failed
or ignored tests. Both the isolated all-targets/all-features harness Clippy gate
and explicit all-targets local-wrapper Clippy gate passed with `-D warnings`.
The isolated native checks receipt is SHA-256
`6c2112596ff9346782dd63ade32d91b0aa93beed4335e45a3b7823f78381ae55`;
source conservation receipt is SHA-256
`25511715e5b819e03b6a37d8784c9f4a778f31cbf50ff6acdef6b9b8b328030d`.
Those receipts are observation evidence, not additional build inputs embedded
in this source package.

Only the executed fixture copy changed under formatting: original fixture
SHA-256 `36138943f7f66c46c27b4ca4b56831f7a9592194bb2fd0cb21be9762023bc3e2`,
executed formatted fixture SHA-256
`6fb133c28c856789f9877ba4942ea04f2d741faddc4ee6423b44228c84a86994`.
The declared MSRV of Rust 1.90 was not tested; the actual toolchain was 1.97.1.
The isolated offline lock resolution produced 26 package rows including its
test harness. It used warm existing dependency inputs and did not establish
empty-cache source self-containment or an independently network-denied build.

## Deliberately unqualified boundaries

These source-retention and ordinary native observations do not qualify
Rootlight discovery, extraction, resolution, service or installed MCP behavior.
They do not establish corpus or whole-language coverage, Salesforce compiler
acceptance, Apex dispatch/overload behavior, SOQL/SOSL runtime semantics, source
security, complete historical Java notices, or a final offline release build.
Rootlight's shared dependency lock, language registry, policy, and public test
integration are separate work.

The canonical `rootlight-vendored-source-v1` identity is computed externally
over the final sorted file roster and SHA-256 values, including this document
and `SOURCE-PROVENANCE.json`. Neither document embeds that complete identity,
which would make the hash self-referential.
