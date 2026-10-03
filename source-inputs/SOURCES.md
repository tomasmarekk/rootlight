# Versioned dependency inputs

These inputs are part of the project, not a dependency cache. External libraries
are permitted; compilation and packaging must not acquire source code.

## Rust inputs

`crates/` retains the original archives authenticated by the union of `Cargo.lock`,
`tests/native-grammars/Cargo.lock`, and `fuzz/Cargo.lock`, plus the producer's
authored Cargo lock when its complete reviewed tree is present. `cargo/` contains
their complete published file inventories and generated Cargo checksum manifests.
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
Repeated `--lock <project-relative-lock>` arguments select an explicit complete
union for a separate preparation. `--destination <fresh-directory>` stages only
Cargo inputs without replacing existing Rust, npm, or producer sources.
`scripts/prepare_rust_source_bundle.py` exports already versioned inputs without
consulting a dependency cache or fetching, resolving, or compiling code.

## Frontend distribution inputs

The canonical frontend lock resolves every non-root record to a versioned local
archive, including nested, development, optional, and other-platform packages.
`npm/upstream-package-lock.json` retains the exact original registry pins.
`npm/archives/` retains unchanged original distributions; declared replacements
live separately in `npm/local-distributions/`, with their own integrity and
redacted provenance. `npm/local-distributions.json` binds those replacements and
the original lock. No modified archive is presented as original upstream bytes.

`npm/npm-source-closure.json` records each locked placement, current archive
integrity, member inventory and modes, supplied licenses, provenance metadata,
and observed native binary files. Verification requires the canonical lock to
equal the upstream lock except for derived local paths and explicitly declared
replacement integrity. Original registry verification remains a separate scope.

`scripts/prepare_npm_source_bundle.py` reads the retained upstream lock for
separately authorized acquisition or copying previously acquired archives.
It does not install or execute packages.
`scripts/import_npm_source_bundle.py` verifies and publishes only already acquired
local inputs into a new destination; it never uses the network or a package cache.
`scripts/verify_npm_source_bundle.py` independently checks original registry
inputs, including strong SRI and package manifest identities.
`scripts/verify_local_npm_sources.py` checks the current all-local project inputs
and their exact canonical-lock projection without acquisition or installation.
The browser release gate independently checks the same origin projection and
current archive integrity before producing release evidence. It does not treat
report metadata or retained binaries as preferred-source acceptance.
Archive members retain their authored top-level directory; npm strips this layer
on installation and does not require its name to be `package`.

Fixture replacements preserve the license, manifest, runtime code and every
other member. Their evidence distinguishes historical verification of the
removed original archive from rechecking the currently retained replacement;
redacted removed bytes cannot prove reconstruction of the upstream archive.

`scripts/install_frontend_from_archives.py` stages authenticated inputs from an
immutable export into a separate work tree and uses the all-local canonical lock.
It requires a fresh dependency cache and an independently enforced OS network
boundary supplied by its caller. An offline option alone is not that boundary.
Distribution verification and installation do not establish complete preferred
sources for native, WASI, or bundled implementations. Their reports deliberately
leave preferred-source, frontend, signature, and build/packaging acceptance
claims false. Retained binaries are not implementation-source completeness.

## Native producer inputs

`producers/lightningcss-1d680fa14e9a089c92dc0929f869d6757ae91c30/`
retains the original source archive and complete authored tree associated with
the exact locked Lightning CSS distribution. Both supplied MPL-2.0 licenses,
upstream patches, Cargo manifests and lock, and the raw Yarn lock are preserved.
Upstream ignore files are inert data, not rules for excluding producer sources.

`scripts/import_lightning_producer_sources.py` publishes only already reviewed
local inputs into a fresh declared namespace. It does not acquire, compile or
execute producer code. `scripts/verify_lightning_producer_sources.py` rederives
the exact report, source inventory and archive-derived executable file intent;
unrecognized fields, changed inputs, extra files and traversal errors fail closed.
Raw archive directory modes are provenance, not Git-restored directory permissions.

Retained publisher metadata matches the authenticated local npm distribution
pins. Its separate capture is a caller observation, not independently proved
transport, a signature check or source-to-binary reproduction. The Yarn lock is
hash-bound authored input, not a verified transitive source closure. The retained
Cargo registry union covers that producer lock; it does not verify the producer's
local packages, Yarn dependencies, or source-to-binary reproduction. Missing
producer dependencies and other native implementation sources remain open;
this inventory does not qualify frontend or full-product source completeness.

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
