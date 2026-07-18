# macOS ACL FFI companion implementation and evidence plan

Document type: non-decision companion plan

Canonical decision record:
[`ADR-026-private-handle-tree-boundary.md`](./ADR-026-private-handle-tree-boundary.md)

Decision dependency: canonical ADR-026 remains Proposed

Target module: `rootlight_vfs::platform::os`

Authorized implementation in this change: none

This file is not an ADR, a second ADR-026 decision, or an entry for the policy
decision index. It only supplies implementation steps and required evidence for
the canonical ADR named above. It does not accept ADR-026, authorize an
unsafe-code policy exception, or enable private-tree operations on macOS.
Rootlight must continue to fail closed before path inspection or filesystem
mutation until the decision, policy inventory, implementation, and native
evidence are all approved.

## 1. Minimum boundary

The future macOS implementation must keep all raw ACL values and calls inside
the private `rootlight_vfs::platform::os` module. The safe parent module owns the
public state machine, bounded single-component names, parent/child lifetimes,
publication states, and source-redacted errors.

The minimum native ACL surface to evaluate is:

| Native operation | Intended wrapper responsibility |
| --- | --- |
| `acl_init(0)` | Allocate one empty extended ACL owned by an opaque RAII guard. |
| `acl_set_fd_np(fd, acl, ACL_TYPE_EXTENDED)` | Apply the empty ACL to the exact open object. |
| `acl_get_fd_np(fd, ACL_TYPE_EXTENDED)` | Read the ACL from the exact open object for verification. |
| `acl_get_entry(acl, ACL_FIRST_ENTRY, &entry)` | Prove that the returned ACL has no ACEs. |
| `acl_free(pointer)` | Release each ACL allocation exactly once. |

Do not bind or call `acl_delete_fd_np`. It is absent from the current public
header and Apple's current libc implementation returns `ENOTSUP`.

Before implementation, a native APFS probe must prove that applying an ACL
created by `acl_init(0)` produces the same security result as removing the
extended ACL, and that a subsequent descriptor-bound read reports no ACEs. If
that probe fails on either supported macOS architecture, stop. Do not fall back
to a path command, `/dev/fd`, an empty textual ACL, or Apple's private
`_FILESEC_REMOVE_ACL` sentinel without a separately reviewed ADR amendment.

Use safe dependency APIs outside this ACL shim:

- retained `cap_std::fs::Dir` or equivalent directory owners for namespace
  authority;
- `rustix` descriptor metadata and effective-user operations;
- `rustix::fs::renameat_with` with `RenameFlags::NOREPLACE`, which maps to
  Apple `RENAME_EXCL`;
- ordinary Rust owners for file descriptors and deterministic closure.

No native pointer, ACL handle, borrowed entry pointer, raw file descriptor, or
platform structure may cross from `platform::os` into the safe API.

## 2. Implementation sequence after approval

1. Confirm ADR-026 is Accepted by the primary user. Do not infer acceptance
   from this plan, an implementation branch, or a passing prototype.
2. Land the compiler-expanded unsafe inventory and cargo-geiger evidence gates
   required by ADR-026 before enabling any successful platform operation.
3. Change only `rootlight-vfs` from workspace `unsafe_code = "forbid"` to
   `unsafe_code = "deny"` and scope the sole allow to
   `rootlight_vfs::platform::os`.
4. Add an Apple-only opaque ACL owner. It stores one non-null `acl_t`, is not
   `Clone` or `Copy`, and calls `acl_free` exactly once in `Drop`.
5. Add a private descriptor-borrowing function that applies an empty extended
   ACL. It accepts `BorrowedFd`, never an integer supplied by a caller.
6. Add a separate descriptor-borrowing verifier that fetches the extended ACL
   and succeeds only when `acl_get_entry(..., ACL_FIRST_ENTRY, ...)` proves
   there is no entry.
7. Treat null pointers, unexpected return values, `ENOTSUP`, and filesystems
   without the required behavior as `UnsupportedPlatform` or a source-redacted
   platform I/O error. Never downgrade them to success.
8. Establish the private parent before any sensitive bytes or child names
   exist: verify owner, object type, mode, link count, and absence of ACL
   entries through retained handles.
9. After ACL removal, re-read descriptor metadata and ACL state. Require the
   same object identity before and after the operation.
10. Enumerate the new private directory while it is still empty. Any injected
    child or unverifiable enumeration result fails closed and retains an
    orphan for explicit recovery.
11. Create child directories and files only relative to that retained,
    ACL-free private parent. Set mode `0700` or `0600` at creation, remove and
    verify any inherited ACL through the new descriptor, and verify owner,
    type, link count, and identity before returning a safe owner.
12. Prohibit writes until the file has passed every verification. The safe
    `PrivateFile` constructor is the only route to `Write`.
13. Before publication, synchronize all files and directories, ensure no live
    child borrow exists, revalidate the retained source parent, and compare the
    source entry's current identity with the retained directory owner.
14. Publish with directory-relative `RENAME_EXCL`. `EEXIST`, cross-volume
    moves, identity mismatch, unsupported filesystem flags, or any symlink
    resolution failure is not committed.
15. After successful rename, transition state exactly once to published.
    Synchronize the destination parent. A destination-directory sync failure
    returns `CommittedButDurabilityUnknown` with the retained published owner;
    it must never be reported as not committed.
16. Cleanup uses the retained private parent and an identity comparison before
    unlinking an entry. A mismatch leaves the object or foreign replacement
    untouched and reports an orphan. There is no recursive path fallback.
17. Keep the current support-bundle CLI preflight until the VFS boundary,
    support-output consumer migration, and all native evidence land together.
18. Remove the public macOS degradation notice only after native evidence
    proves the feature and the product contract is updated. Do not silently
    change the stable capability error.

## 3. Required SAFETY proofs

Every future native call needs an immediate proof-style `SAFETY` comment that
states the applicable items below rather than merely naming the function.

### ACL allocation

- The requested entry count is zero and representable as the native `int`.
- A non-null result is exclusively owned by one RAII value.
- The pointer came from the matching Apple ACL allocator.
- No alias is retained beyond a synchronous native call.

### ACL application

- The file descriptor is borrowed from a live Rust owner for the full call.
- The descriptor refers to the exact object whose identity was retained.
- The ACL pointer is non-null, initialized, and remains alive for the full
  call.
- `ACL_TYPE_EXTENDED` has the SDK value used to compile the binary.
- The OS does not retain the supplied ACL pointer after the call.
- Failure leaves the object unusable by safe callers and no bytes are written.

### ACL retrieval and entry inspection

- The descriptor borrow remains valid for the entire retrieval call.
- A non-null returned ACL pointer transfers one ownership obligation to the
  RAII guard.
- The out-parameter for `acl_get_entry` is valid and initialized for the call.
- Any returned entry pointer is borrowed from the ACL owner, never exposed,
  and never used after that ACL is freed.
- Only the documented “no entry” result is accepted as ACL-free; an error or
  unknown return value fails closed.

### ACL release

- The pointer was returned by an Apple ACL allocation or retrieval function.
- It has not already been freed.
- No entry pointer or other borrow derived from it remains live.
- The return value is handled according to the selected ownership contract;
  no second release is attempted.

### Descriptor and publication operations

- Every raw descriptor passed to a native boundary comes from a live owned or
  borrowed descriptor and is not closed concurrently.
- Metadata comparisons use the full selected volume and file identity without
  truncation.
- Source and destination names are one validated native component with no NUL,
  slash, backslash, `.` or `..`.
- The retained source parent is verified account-private and ACL-free.
- The source entry still names the retained object immediately before rename.
- `RENAME_EXCL` is the only publication mode and replacement is impossible.
- State is changed to committed only after the rename reports success.
- Cleanup never unlinks an entry whose identity is not the retained identity.

## 4. Native APFS hostile test matrix

All macOS tests must run natively. Cross-compilation is a compile check, not
security evidence.

### ACL establishment

1. Create an APFS parent with an inheritable `everyone` read ACE. Verify the
   boundary rejects it as a staging parent or removes and verifies the ACL
   before any sensitive child exists, according to the accepted design.
2. Repeat with inheritable write, append, delete-child, file-inherit,
   directory-inherit, inherit-only, and deny ACE combinations.
3. Prove an empty ACL applied through the descriptor is read back with no ACE.
4. Inject `acl_init`, set, get, entry, and free failures independently.
5. Run on default case-insensitive APFS and a case-sensitive APFS test volume.
6. Run on arm64 and x86_64 where the release toolchain permits.
7. Exercise a filesystem or mounted image that rejects the ACL operation and
   assert an explicit unsupported result with zero sensitive writes.

### Exposure-window test

8. Pause deterministically after object creation but before ACL hardening.
   Race a helper process that has access only through the inherited ACE.
9. Require that no helper can retain a readable descriptor that later observes
   bytes. If the proposed parent strategy cannot prove this, the design fails
   and remains disabled.
10. Repeat the race for private directories before the first child is created.
11. On a privileged native runner, use a second unprivileged account to prove
    the boundary protects against principals outside the current-account trust
    boundary, not merely another process with the same UID.

### Identity and namespace attacks

12. Substitute a symlink, hard link, regular file, and directory at the source
    name between each validation hook and publication.
13. Rename the staging entry away and replace its name with a foreign object.
    Cleanup must not delete the replacement.
14. Race two publishers to the same destination. Exactly one may commit and
    the loser must return already-exists without altering the winner.
15. Pre-create dangling and non-dangling destination symlinks. Neither may be
    followed or replaced.
16. Attempt cross-volume publication and require a not-committed result.
17. Exercise Unicode normalization variants, case aliases, maximum native name
    length, embedded NUL rejection, separators, `.` and `..`.
18. Add a hard link after file verification and before publication. The
    identity/link-count revalidation must fail.

### Durability and cleanup

19. Inject file sync, source-directory sync, rename, and destination-directory
    sync failures separately.
20. Prove that only the post-rename destination sync failure returns
    `CommittedButDurabilityUnknown`.
21. Crash at every durable-state transition, reopen the retained state, and
    verify the allowed on-disk state and recovery classification.
22. Drop unpublished owners with and without children. Destructors may close
    handles but must not perform path-based recursive cleanup.
23. Inject an identity mismatch during explicit cleanup and prove both the
    foreign entry and retained orphan remain untouched.
24. Confirm all errors, debug output, logs, and test artifacts redact native
    paths, ACL subjects, and sensitive bytes.

## 5. Required gates

Before the macOS implementation or any consumer can be enabled:

1. Focused safe-state and error-state unit tests pass.
2. The complete hostile APFS matrix passes natively.
3. macOS arm64 and x86_64 compile and test where the supported toolchains
   permit.
4. Current stable and MSRV 1.90 format, check, clippy, test, and rustdoc pass.
5. Miri passes for the safe wrapper state machine; OS FFI exclusions are
   recorded.
6. The compiler-expanded unsafe inventory reports only the accepted
   `rootlight_vfs::platform::os` boundary.
7. cargo-geiger matches the exact reviewed inventory.
8. `cargo deny`, advisory, source, license, offline, architecture, and policy
   checks pass.
9. A security reviewer signs off on every SAFETY proof and hostile-test result.
10. The M04/M05 evidence records the platform, filesystem, architecture,
    source revision, commands, raw artifacts, exclusions, and residual risks.

## 6. Remaining blocker

The exact blocker is not `RENAME_EXCL`; that primitive exists. The blocker is
explicit acceptance of the canonical
[`ADR-026-private-handle-tree-boundary.md`](./ADR-026-private-handle-tree-boundary.md)
decision and native proof of a minimal descriptor-bound ACL removal and
verification mechanism that closes the inherited-ACL exposure window while
preserving exact identity through no-replace publication. This companion plan
cannot supply or imply that acceptance. Until the canonical ADR is explicitly
Accepted and that proof passes on native APFS, macOS private output must remain
unavailable with zero filesystem mutation.
