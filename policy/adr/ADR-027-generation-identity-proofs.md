# ADR-027: Generation identity proofs

- Status: Proposed
- Date: 2026-07-18
- Deciders: Rootlight maintainers

## Context

ADR-008 and `rootlight-ids` already define the stable derivations for
`GenerationId`, `FileId`, `SymbolId`, and `FactId`. The normalized IR 1.1
document does not retain all inputs to those derivations:

- file records retain a presentation path, not the VFS path-identity bytes;
- entity records omit container-identity bytes, signature discriminators, and
  build-context discriminators;
- fact records omit the producer's domain and canonical semantic payload; and
- generation metadata retains only a manifest hash, not canonical discovery
  manifest bytes or evidence that its entries equal the stored file inputs.

Recomputing IDs from display names, serialized storage rows, or other partial
projections would create a second identity algorithm and could falsely attest
to data that ADR-008 does not identify. Treating an opaque `(domain, payload)`
pair as sufficient would prove only that it hashes to an ID, not that the
payload describes the associated normalized record.

## Proposed prototype

Prototype an additive, versioned identity-claim envelope plus an opaque
verified capability. A claim is deliberately not named or typed as verified:
any producer may construct one, and storage must distrust it until every field
has been compared with canonical IR and every ID has been recomputed.

The prototype selects typed, versioned fact recipes in neutral IR rather than a
producer verifier registry:

- common fact records use shared `rootlight.<record>/v2` recipes over the
  canonical typed record with only its asserted `id` omitted;
- file and symbol claims carry the ADR-008 inputs that the common record cannot
  retain, while repeating structured path, owner, kind, container, declared
  identity, content, length, and build-context fields for independent
  comparison;
- `GenerationManifestRecipe` hashes the repository, configuration identity,
  recipe version, and the canonical exact file-claim ledger;
- generation contract 1.2 is part of `GenerationId.format_version`, preventing
  v1 and v2 fact recipes from being mixed silently; and
- noncritical claim envelopes use the existing IR 1.1 extension transport and
  schema-2 oracle table. This preserves legacy readability without treating an
  extension envelope as authority.

An accepted version must continue to meet all of these requirements:

1. Carry the exact ADR-008 inputs for every file and symbol, with exact
   one-to-one cardinality against the canonical normalized document, and use
   one shared typed recipe for every common fact record.
2. Recompute every compact ID through `derive_file`, `derive_symbol`, or
   `derive_fact`; no storage-local derivation is permitted.
3. Validate the semantic fields in each claim against its associated typed IR
   record. Matching only the record ID is insufficient.
4. Carry canonical discovery-manifest material, recompute `manifest_hash`, and
   verify that included file identity, path, content hash, and byte length
   entries equal the generation's canonical file inputs.
5. Seal the canonical claim sidecar and support policy into the oracle. The
   claim contract version is part of the generation format identity.
6. Preserve bounded streaming, cancellation checkpoints, strict decoding, and
   exact ledger validation while producing verified evidence.
7. Keep legacy generation-contract 1.1/schema-2 oracles readable for
   compatibility while preventing
   them from entering identity-verified write or query APIs.

`IdentityVerifiedGeneration` is the opaque capability produced only after all
checks pass. Its only public construction path runs the verifier; the public
claim constructors do not grant it. Backend-neutral writers and verified query
readers require that capability.

The discovery manifest remains owned by `rootlight-discovery`. This prototype
places only the lower-layer neutral file-entry recipe in `rootlight-storage`;
future discovery integration must create the same recipe rather than teaching
storage about discovery producer code.

`container_identity` and signature discriminators remain producer-supplied
claim inputs because they cannot be recovered from `EntityRecord`. The
verifier nevertheless recomputes `SymbolId` from them and independently
compares repository, language, kind, structured container, declared identity,
and referenced provenance build context with the typed entity.

## Deciding experiment

Prototype claim emission for real Tree-sitter output and one independent
adapter-SDK fixture. The prototype must:

- reproduce every current Tree-sitter ID using unchanged ADR-008 functions;
- detect a same-cardinality mutation in each file, entity, and fact claim;
- reject a claim payload that hashes correctly but disagrees with its IR
  record;
- round-trip canonical discovery inputs and recompute the generation manifest
  hash; and
- demonstrate that the chosen contract supports a second producer without
  importing producer code into storage.

The deciding experiment is implemented by:

- `real_treesitter_generation_obtains_verified_capability_and_round_trips`,
  which runs the real Rust Tree-sitter provider, obtains the opaque capability,
  seals and reopens the catalog, exercises the backend-neutral query API, and
  rejects fact IDs plus self-consistent file/symbol claim envelopes that
  disagree with typed records; and
- `independent_sdk_producer_uses_the_same_identity_verifier`, which emits a
  file/provenance/claim stream through the adapter SDK mock analyzer and obtains
  the same storage capability without importing producer code into storage.

These tests are acceptance evidence for the proposed typed-recipe direction.
They do not by themselves change this ADR to Accepted.

## Consequences while proposed

- No ADR status changes to Accepted.
- Legacy oracle materialization remains available for compatibility.
- Production backend-neutral write and verified-query paths fail closed for
  generations without an `IdentityVerifiedGeneration` capability.
- Current 1.2 generations can obtain the capability only through independent
  recipe verification; arbitrary caller IDs and incomplete claims are rejected.
- Test-only unverified catalog scaffolding remains available solely for legacy,
  streaming, and corruption fixtures and cannot enter the verified API.
