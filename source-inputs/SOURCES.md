# Versioned dependency inputs

These inputs are part of the project, not a dependency cache. External libraries
are permitted; compilation and packaging must not acquire source code.

## Rust inputs

`crates/` retains the original archives authenticated by the union of `Cargo.lock`,
`tests/native-grammars/Cargo.lock`, and `fuzz/Cargo.lock`. `cargo/` contains their
complete published file inventories and generated Cargo checksum manifests.
Cargo resolves crates.io packages only through this local directory source.

`cargo-source-closure.json` binds package identities, original archive checksums,
expanded file bytes, supplied license metadata, and executable intent. Verify it
against the current lockfiles and original archives before exporting sources.

Published upstream files, including ignore rules, package lockfiles, and agent
metadata, remain inert dependency data. They are not operating instructions or
rules for filtering this project's declared source inventory. Import the exact
authenticated inventory with its recorded file modes; do not alter upstream bytes
or add unrelated ignored local files. Retain license and provenance records.

`scripts/vendor_rust_sources.py --archives <reviewed-archive-directory>` is a
separate local preparation step. It accepts exactly the locked archive inventory,
performs no acquisition, and refuses an existing `source-inputs/` destination.
`scripts/prepare_rust_source_bundle.py` exports already versioned inputs without
consulting a dependency cache or fetching, resolving, or compiling code.

## Acceptance boundaries

Rust archive and directory-source verification proves the published input
inventories, not complete preferred sources or rebuild evidence for every
precompiled component. Import libraries, generated binary fixtures, and packaged
compiler executables require separate classification and provenance review.
Retained binaries are not substitutes for any missing implementation sources.
Frontend source completeness and legal-notice completeness are also unproven.
Verification does not establish original authorship, exclusive ownership, or
the absence of upstream vulnerabilities.

Full-product acceptance requires complete reviewed sources for every build and
packaging input, a preinstalled declared toolchain, initially empty dependency
caches, independently blocked network access, and successful build and packaging
verification. Neither an offline flag nor a prefetched cache proves this contract.
