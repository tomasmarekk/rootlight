# ADR-026: Confine native private-tree operations to the VFS platform boundary

**Status:** Proposed
**Owner:** @tomasmarekk
**Decision date:** not accepted
**Related baseline:** ADR-010, REQ-N-004, REQ-N-008, CMP-VFS, M05, M12, R-009

## Context

Immutable benchmark and generation evidence must be private before it contains
bytes, must retain an exact object identity, and must be published without
replacement. Cleanup must target the opened object and must never delete a
foreign replacement.

The standard library and the existing capability filesystem expose safe
path-relative operations, but they do not expose all guarantees needed on the
supported native platforms:

- Windows needs a protected owner-only DACL in the create call, complete
  `FILE_ID_INFO`, an opened-source-handle rename into an opened destination
  parent, and opened-handle disposition.
- macOS needs file-descriptor-bound inspection of inherited ACLs and
  descriptor-bound ACL removal and verification before bytes or children are
  created.
- Unix publication APIs still name the source entry, so the source must remain
  inside a verified account-private parent and must be revalidated against its
  retained identity immediately before atomic no-replace publication.

Path reopening, post-create path ACL changes, and path-based recursive deletion
do not meet this boundary because concurrent namespace mutation can redirect
them.

## Proposed decision

Add a safe `rootlight_vfs::platform` API with opaque RAII types for private
directories and files. Confine all native calls and raw representations to the
private `rootlight-vfs::platform::os` module named in `policy/unsafe.toml`.

The safe contract is:

- directory and file names are one bounded native component;
- private objects are verified before their handles reach a caller;
- `PlatformFileIdentity` contains the full volume and file identity;
- raw handles, pointers, platform structures, and borrowed native buffers
  never cross the module boundary;
- each owned handle is closed exactly once by its Rust owner;
- publication is no-replace and consumes the unpublished tree;
- child directories and files borrow their parent so a live writer or child
  prevents publication and removal of the containing tree;
- a post-rename directory-flush failure is reported as committed with unknown
  durability, never as a pre-commit failure;
- cleanup operates from retained handles and leaves an orphan on identity
  mismatch rather than deleting by an untrusted path;
- unsupported or unproven platform behavior fails closed.

The workspace-wide default remains `unsafe_code = "forbid"`. While this ADR is
`Proposed`, `rootlight-vfs` must inherit that exact forbid and the inventory
must remain zero. After acceptance, only `rootlight-vfs` may mirror the
workspace lint table with `unsafe_code = "deny"`, and only the exact private
native module may receive a scoped allow. The policy scanner and cargo-geiger
validation reject an ADR-status or lint mismatch, any other native-code use,
and any unexpected count change.

This proposal does not alter `RepositoryRoot`, `RelativePath`,
`SourceSnapshot`, discovery semantics, or stable repository path identity.
Private staging identity is transient and never becomes a public Rootlight ID.

## Safety and ownership invariants

If accepted, every native operation must maintain all of these invariants:

1. A returned Rust owner exclusively owns one valid handle and closes it once.
2. Native buffers live through the call and are neither retained nor exposed.
3. Every size and offset is checked before conversion or allocation.
4. Windows creation receives the final protected owner-only descriptor in the
   atomic create call; no byte is written before handle-based verification.
5. Apple creation rejects an insecure parent and clears and verifies the child
   ACL through its descriptor before creating bytes or children.
6. Unix named-source publication is allowed only from a retained, reverified
   account-private parent whose entry matches the opened object identity.
7. Publication never replaces a destination.
8. Cleanup either disposes the retained object or leaves it untouched; it does
   not fall back to deleting a path whose identity is unknown.
9. Reparse points, symbolic links, hard-linked files, cross-volume moves, and
   unsupported filesystems fail closed.

Every native block needs an immediate proof-style `SAFETY` comment. There are
no public native functions and no unchecked indexing, transmute, custom
allocation, or native plugin loading.

## Public contracts and data impact

The new public surface is limited to `PrivateDirectory`, `PrivateFile`,
`PublishedPrivateDirectory`, `PlatformFileIdentity`, `PlatformError`, and
`PublishError`. Errors distinguish not-committed publication from an already
committed tree whose directory-flush durability is unknown.

No wire schema, stored schema, migration, or stable identity changes. M05
benchmark bundles and later M12 generation publication consume only this safe
API.

## Security review

The protected principal is the current account. Other processes running as the
same account remain inside the trust boundary, but namespace races must still
produce an explicit error rather than a false success.

The implementation must be reviewed for link and reparse substitution,
inherited ACLs, partial creation, destination collision, cross-volume behavior,
post-commit error reporting, cleanup substitution, handle inheritance, and
full-width identity. Native platform CI is mandatory; cross-compilation alone
is not acceptance evidence.

## Dependency review

The proposal reuses workspace-pinned dependencies only:

| Dependency | Owner and purpose | Surface and replacement |
| --- | --- | --- |
| `cap-std`, `cap-fs-ext` | VFS capability handles and safe relative operations | Existing public implementation dependency; replace with equivalent capability handles only |
| `rustix`, `nix` | VFS Unix no-replace rename, metadata, and effective-user checks | Target-specific, default features disabled; no types escape |
| `windows`, `nt-token`, `windows-permissions` | VFS Windows handle calls, current SID, and descriptor construction and inspection | Target-specific, workspace-pinned, default features disabled; no types escape |

No package is new to the lockfile. The dependencies build offline from the
existing lock and policy inputs. Their licenses, advisories, native links,
build scripts, proc macros, transitive packages, MSRV, and source origins
remain governed by the existing supply-chain inventory. Direct feature changes
must pass `cargo deny`, MSRV 1.90, current stable, and native platform CI.

## Alternatives considered

| Alternative | Reason rejected |
| --- | --- |
| Apply permissions after ordinary creation | Exposes a Windows or inherited-ACL object before the boundary is installed |
| Reopen and compare paths after rename | Detects some races only after committing and cannot make path mutation handle-bound |
| Use path-based recursive removal | Can delete a foreign replacement after namespace substitution |
| Keep native code in the benchmark crate | Duplicates a security boundary and violates VFS ownership in document 07 |
| Disable Windows or macOS publication silently | Violates supported-platform truthfulness; unimplemented targets must return an explicit unsupported error |

## Implementation, tests, and evidence

The implementation belongs to the M05 evidence-publication repair and is
reused by M12 generation publication. Acceptance requires:

- unit tests for validation, identity, ownership, commit-state errors, and
  cleanup;
- hostile native tests for symlink, reparse, hardlink, ACL, source-entry,
  destination, cleanup, and cross-volume races;
- native Windows, macOS, and Linux CI results;
- MSRV and current-stable format, check, clippy, test, and rustdoc;
- exact policy-scanner positive and negative controls;
- cargo-geiger count matching `policy/unsafe.toml`;
- supply-chain, license, advisory, offline, and architecture checks;
- Miri for applicable safe-wrapper state logic, with OS FFI exclusions
  documented.

This is a correctness and security boundary, not a performance optimization;
no benchmark authorizes weaker behavior.

## Rollback

Before acceptance, Windows and macOS operations remain fail-closed. After
acceptance, rollback removes the consumers and native implementation, restores
the VFS crate to workspace lint inheritance, and deletes the exact policy
entry. Existing repository snapshot behavior remains usable throughout.

## Reconsideration

Reconsider this decision if stable Rust or an audited capability dependency
provides equivalent private-at-birth creation, full handle identity,
handle-bound no-replace rename, ACL control, and exact-handle cleanup on every
supported platform. Replace the native module when the safe alternative passes
the same hostile native tests.
