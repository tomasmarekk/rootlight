# Incident response

Owner: `@tomasmarekk`

This runbook covers security and reliability incidents affecting Rootlight's
local-first product boundary. It applies to the daemon, CLI, MCP bridge,
adapter host, persistent catalog, package artifacts, and opt-in updater.

## Safety invariants

- Preserve the last known-good binary and generation until replacements verify.
- Treat repository content and adapter output as untrusted data.
- Keep network egress denied for core and adapter execution.
- Collect only allow-listed, source-free diagnostics by default.
- Never publish or install an artifact without independently verified digest,
  signature, provenance, SBOM, license status, and rollback path.
- Never delete or rewrite evidence before its digest and custody are recorded.

## Detection and triage

1. Assign the incident to `@tomasmarekk`, record a private incident identifier,
   and select the highest plausible severity.
2. Capture the Rootlight version, platform, process boundary, stable diagnostic
   codes, health summary, package digest, and source-free support bundle.
3. Hash each evidence artifact and record who collected it. Do not collect
   source bodies, credentials, environment tokens, raw prompts, or untrusted
   tool text.
4. Determine whether source exposure, sandbox escape, persistent corruption,
   release compromise, or loss of the local service boundary is still active.
5. If the scope is uncertain, contain first and widen diagnostics only through
   explicit user consent.

Severity is critical when source or signing material may have escaped, an
untrusted process crossed its sandbox, or an official artifact may be
malicious. Persistent corruption with no verified last-good state is high.
Recoverable update and availability incidents are high or medium according to
user impact.

## Evidence preservation

Preserve detached hashes, signed metadata bytes, signatures, provenance,
SBOMs, package manifests, update state, catalog and generation identifiers,
health codes, bounded structured events, process exit status, and the
source-free support bundle. Store evidence read-only with access limited to the
incident owner. Record exclusions and failed collection attempts.

Do not attach repository files, segment payloads, source snippets, credentials,
private paths, raw adapter output, raw MCP prompts, or complete environment
dumps. When a file itself is suspected, record its digest and metadata first;
transfer content only after explicit authorization and minimization.

## Containment

Containment actions must be reversible and scoped to the affected boundary:

- stop a rollout without deleting the previous package;
- terminate an affected adapter process tree and retain its source-free exit
  evidence;
- revoke a compromised adapter trust entry or release signing key;
- quarantine corrupt state while preserving the last-good generation;
- disable the suspected output path or integration;
- stop the per-user daemon and use the standalone fallback when available.

Do not weaken sandboxing, signature checks, source exclusion, or catalog
validation to restore service.

## Recovery and verification

Recovery uses a newly verified state, not in-place mutation of the only known
good state. Verify package and metadata signatures, artifact digests,
compatibility, migration space, catalog state, and health before activation.
Run a dry-run repair before catalog reconstruction. Retain the old binary and
generation until post-activation health succeeds, then keep the documented
rollback window.

After recovery, repeat the scenario-specific negative control, confirm that
support evidence remains source-free, and monitor bounded health and error
signals for recurrence.

## Communication

Keep the initial report private. Tell affected users what boundary failed, which
versions and platforms are affected, which containment is safe, and whether
source exposure is confirmed, suspected, or ruled out. Do not quote private
repository content. Publish remediation and verification instructions only
after fixed artifacts and their release evidence verify independently.

For a signing incident, publish the revoked key identity, replacement trust
metadata, affected artifact digests, and a re-verification procedure through an
authenticated channel.

## Scenario playbooks

### Source exfiltration

Detection includes unexpected outbound attempts, source-like values in a
diagnostic artifact, or a report that repository content crossed a declared
boundary. Disable the suspected output path, deny egress, preserve redacted
evidence, and determine the minimum affected versions. Verify that support
bundles and structured events contain no source. Notify affected users with the
scope and safe containment. Remove the release from distribution if exposure
cannot be ruled out.

### Sandbox escape

Terminate the adapter process tree, retain exit and sandbox-policy evidence,
deny egress, and revoke the adapter trust entry. Audit the host boundary for
child processes, changed files, handles, and network attempts without reading
repository bodies. Patch and re-run the escape negative controls on every
supported platform before restoring trust.

### Corrupt catalog

Stop publication to the affected catalog, preserve the last-good generation,
and run repair in dry-run mode. Quarantine corrupt catalog or segment state by
identity and digest. Rebuild into a new generation and activate it only after
manifest, checksum, compatibility, and health verification. Never delete the
only known-good generation as a repair shortcut.

### Bad update

Stop the rollout and revoke the affected signed metadata. Preserve the failed
package, signature, metadata, health result, and rollback result by digest.
Restore the last-good binary, verify catalog and protocol compatibility, and
confirm service health. A corrected update needs a higher monotonic version and
fresh independently verifiable release evidence.

### Signing-key compromise

Stop all release publication and updater metadata distribution. Revoke the
compromised key, rotate trust metadata through an authenticated path, and
inventory every artifact signed by the key. Reverify published artifacts from
their source revision, SBOM, provenance, and reproducibility evidence. Notify
users of affected digests and require trust metadata refresh before updates
resume.

### Service unavailable

Preserve local catalog and user data. Capture source-free health and lifecycle
diagnostics, release stale process ownership safely, and use the standalone
fallback where compatibility allows. Restore the per-user daemon from the
last-good binary and verify its endpoint permissions, catalog state, protocol
compatibility, and bounded resource health.

## Post-incident actions

1. Record the root cause, affected boundary, confirmed impact, excluded impact,
   detection gap, containment, recovery proof, and residual risk.
2. Add a regression or negative-control test before closing remediation.
3. Rotate affected keys and trust entries, review artifact custody, and audit
   any emergency access.
4. Update this runbook only for durable process changes.
5. Conduct a follow-up tabletop and retain its deterministic evidence.

## Tabletop exercise

The machine-readable scenarios in `operations/tabletop.toml` cover every
playbook above. Run:

```text
cargo run --locked --package xtask -- incident-tabletop \
  --output artifacts/incident-tabletop.json \
  --source-revision 0123456789abcdef0123456789abcdef01234567
```

The resulting report binds the exact runbook and scenario bytes. Passing the
tabletop proves that every required control is declared and source-free; it
does not claim that a real incident occurred or that an external responder
reviewed the exercise.
