# Retained Rocq parser source

The authored source is pinned to `aruzdh/tree-sitter-rocq` revision
`300fe33fc299c30f736fd56d8ef8a28b08acd4e6`. This local static Rust/C binding is
unpublished. Its package version is the authored manifest version, not evidence
that an upstream crate release exists at that version.

All 91 authored regular files, their original bytes and archive executable intent
are retained. `upstream/acquisition-inventory.json` records the acquisition.
`upstream/retained-source-inventory.json` maps every authored file to its retained
location. The active Cargo manifest, two Rust binding files and named core
grammar and generated tables have local overlays; their exact originals are in
`upstream/original-inputs`. The authored
`.gitattributes` and `.gitignore` are inert `.upstream` files. The complete original
archive is retained beside the inventories. The preferred grammar and tables are
reproducibly generated as described in `PATCHES.md`; authored queries, tests and
other language bindings remain unchanged.

`upstream/source-origin.json` records the authored repository, revision and archive
digest. `upstream/local-overlay.json` records the local binding inputs. Compilation
uses the declared local C toolchain, pinned `cc` and `tree-sitter-language` sources.
It does not run Node, a grammar generator, acquisition commands, or package hooks.
The separately declared pinned maintenance CLI regenerates only checked-in tables.

The authored grammar and four package metadata files declare MIT, but this authored
archive does not contain the full license notice referred to by its manifest.
`upstream/license-declarations.json` preserves the exact five declarations.
`LICENSE-MIT-REFERENCE.txt` and `upstream/spdx-MIT-reference.json` are separately
labelled SPDX MIT references, not an authored upstream notice. Their copyright
placeholders remain unfilled; no copyright holder or missing upstream text has
been invented.

This grammar is a bounded syntax frontend. Error recovery and generic sentence or
tactic nodes do not establish complete declarations, module/type support, plugin
semantics, dynamic notation semantics, or proof checking. Parser controls and a
per-file census are separate evidence from production registry admission.
