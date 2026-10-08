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
hash-bound authored input, not proof of complete transitive preferred sources.
The retained Cargo registry union covers that producer lock; it does not verify
the producer's local packages, Yarn preferred sources, or source-to-binary
reproduction. The producer's complete preferred-source dependency closure and
other native implementation sources remain unqualified; this inventory does not
qualify frontend or full-product source completeness.

## Producer distribution inputs

The separate
`producer-dependencies/lightningcss-1d680fa14e9a089c92dc0929f869d6757ae91c30/yarn-originals/`
namespace stores original distributions from the producer's authored Yarn lock,
their complete expanded published inventories, and `yarn-source-closure.json`.
It is not the consumer npm lock or part of the authored producer archive. Shared
distribution identities remain separate inputs; optional and other-platform
archives are retained rather than filtered by the preparation host.

`scripts/verify_lightning_yarn_sources.py` independently rederives the raw-lock
selectors, root dependency edges and reachability. Every registry archive must
match both the lock's SHA-512 integrity and the resolved URL's SHA-1 fragment
before bounded decompression. Expanded files, implicit and empty directories,
canonical report bytes, supplied licenses and archive-derived executable intent
must match. Local `link:.` entries bind to the authored producer, not fabricated
registry packages or proof of an installer placement.

`scripts/import_lightning_yarn_sources.py` publishes only complete previously
acquired local inputs into a fresh declared destination. It rechecks prepared and
staged bytes before publication, never installs or executes package sources,
uses no dependency cache and performs no acquisition. Stable unlinked ancestors
are required; publication does not promise hostile-concurrency isolation or
crash durability. Complete inventories include the two authenticated published
`.DS_Store` members explicitly exempted by the root ignore rules. Unrelated local
operating-system files remain excluded.

Git does not preserve empty directories. Strict verification and export require
the complete authenticated directory inventory; a checkout containing the exact
regular files alone is not evidence that this requirement is met. Reproducible
export transport retains those verified empty directories with canonical modes.
Directory transport and raw archive metadata do not prove fresh Git restoration.

### Explicit source metadata restoration after checkout

Git records regular file bytes and a single executable bit, not empty directories
or full POSIX permission modes. Authenticated upstream files require the project's
byte-preserving attributes; line-ending conversion would change their identities.
The restoration utility cannot repair modified or missing regular source files.
Archive-derived normalized `755`/`644` file intent remains authoritative even when
the preparation host or Git index does not expose those permissions faithfully.

`scripts/restore_lightning_source_inputs.py` is a separate explicit preparation
step for the fixed producer and producer-dependency namespaces. It treats reports
only as bounded untrusted staging hints, copies existing regular bytes into a
temporary clone, applies hinted metadata there, and requires the original strict
archive/lock/report verifiers to authenticate that clone before changing metadata
in the original tree. It restores only authenticated missing directories and,
with `--restore-file-modes`, existing POSIX regular files' normalized modes. It
never creates, replaces, writes or deletes original regular source files, and
preserves existing directory permissions. Strict verification runs again after
restoration and regular-file byte conservation is checked independently.

On POSIX, explicitly permit authenticated file-mode normalization before export:

```sh
python3 -B scripts/restore_lightning_source_inputs.py --project . --restore-file-modes
python3 -B scripts/verify_lightning_yarn_sources.py --project .
python3 -B scripts/prepare_rust_source_bundle.py --destination <fresh-export-directory>
```

On Windows, `python -B scripts/restore_lightning_source_inputs.py --project .`
restores authenticated missing directories, not POSIX permission or executable
semantics. Git commands handling this full inventory may require a command-scoped
long-path option; do not shorten or relocate authenticated source identities:

```sh
git -c core.longpaths=true clone <repository> <fresh-checkout-directory>
git -c core.longpaths=true add -- source-inputs/producer-dependencies
```

These Git examples do not authorize filtering authenticated members or force-adding
unrelated ignored files. Required byte-preserving attributes and narrow exemptions
for authenticated upstream ignore-file matches remain part of the source contract.

Only complete namespace-parent absence is optional; partial, foreign, linked or
extra inventories reject. Producer metadata is restored before Yarn because Yarn
verification requires the complete producer association. Stable unlinked ancestors
and nonhostile concurrency are required. Restoration is not crash-atomic: a later
failure may leave only already authenticated directory or mode changes, and a safe
rerun reauthenticates them. No acquisition, dependency cache, installer, package
execution or Git-index operation occurs in the utility.

Exporters and verifiers never invoke restoration implicitly and remain strict
about missing directories and POSIX file modes. The CI checkout/export callers
invoke this preparation explicitly; restored tar exports already retain their
verified empty-directory inventory and archive-derived file intent. Restoration
evidence does not prove preferred-source completeness, native source-to-binary
reproduction, legal-notice completeness, empty dependency caches, OS-denied
networking, or full-product build and packaging acceptance.

### Controlled local installation and read-only placement observation

`scripts/install_lightning_from_archives.py` stages strictly authenticated
producer bytes and original Yarn archives from immutable source inputs into a
separate fresh work tree and a genuinely empty dependency cache. It preserves the
authored manifest, raw lock, selectors, integrity pins and local `link:.` associations
without URL or lock rewriting. An exact archive-derived offline mirror supplies
the original distributions; mirror collisions, missing inputs, tool mismatch,
source drift, nonzero child results and output reuse reject. The work root and
every ancestor must contain no entry named `package.json` or `yarn.json`.
No source acquisition, lifecycle hooks, cleanup or native build is performed.

The caller supplies a reviewed direct Node binary with its expected SHA-256 and
the pinned Yarn CLI file, not a PATH or tool-manager shim. Declared Node and Yarn
versions are prerequisites; this utility does not acquire them. Yarn runs with
`--offline --ignore-scripts --frozen-lockfile` and a controlled environment.
Closed bounded child-log identities and final strict source reauthentication
qualify local installer completion, not installed-tree acceptance. An offline
flag and empty cache do not independently prove OS-enforced network denial;
that boundary remains a separate caller responsibility and an unqualified claim.

`scripts/verify_lightning_installed_placements.py` is a separate read-only
observer for a closed, stable installed producer root and authenticated source
inputs. It checks every original producer member and observed registry package
member's exact bytes, nonoptional ordinary-root and lock-stanza bindings, and
only the graph-declared local producer-root roles. Dependency lookup stops at the
nearest existing candidate; an archive's distinct stanza-edge obligations are
retained. Published fixture node_modules payloads remain ordinary archive bytes,
not installed dependency placements. Imported first-party helper origins must
match the exact caller-declared helper directory. No package code, installer,
child, cache or tool executable is consulted.

The observer only enumerates bounded generated .bin and top-level
node_modules/.yarn-integrity entries with lengths and hashes; bin winners,
wrappers, peers, ABI and metadata semantics remain unqualified. Omitted registry
identities are listed without proving optional or platform selection policy.
Successful member-byte, required-lock-placement and declared-root-role checks do
not qualify the whole installed tree, preferred-source completeness, legal-notice
completeness, provenance signatures, lifecycle/native rebuild, independently
OS-denied networking, or full-product build and packaging acceptance. Those
broader claims remain false in installation and observation evidence.

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
