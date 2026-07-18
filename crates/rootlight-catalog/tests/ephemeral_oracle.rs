//! Public-contract coverage for the nondurable in-memory oracle.

use rootlight_cancel::Cancellation;
use rootlight_catalog::EphemeralOracleWriter;
use rootlight_ids::{GenerationIdentity, RepositoryId, content_hash, derive_generation};
use rootlight_ir::{ExtensionSupport, IrLimits, NormalizedIrDocument};
use rootlight_storage::{
    GENERATION_CONTRACT_VERSION, GenerationBudget, GenerationContext, GenerationManifestRecipe,
    GenerationMetadata, GenerationReader, IdentityVerifiedGeneration,
};

#[test]
fn ephemeral_oracle_round_trips_an_identity_verified_generation() {
    let repository = RepositoryId::from_bytes([31; 16]);
    let configuration_hash = content_hash(b"ephemeral-oracle-configuration");
    let manifest_hash = GenerationManifestRecipe::new(repository, configuration_hash, Vec::new())
        .expect("empty manifest recipe is valid")
        .canonical_hash()
        .expect("manifest recipe encodes");
    let provider_set_hash = content_hash(b"ephemeral-oracle-provider-set");
    let generation = derive_generation(GenerationIdentity {
        repository,
        parent: None,
        manifest_hash,
        config_hash: configuration_hash,
        provider_set_hash,
        format_version: generation_format_version(),
    })
    .id();
    let metadata = GenerationMetadata::new(
        repository,
        generation,
        None,
        manifest_hash,
        configuration_hash,
        provider_set_hash,
    )
    .expect("generation metadata is valid");
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let verified = IdentityVerifiedGeneration::verify(
        metadata,
        NormalizedIrDocument::empty(repository, generation),
        &IrLimits::default(),
        &ExtensionSupport::default(),
        &context,
    )
    .expect("empty generation is identity verified");

    let reader = EphemeralOracleWriter::create()
        .expect("ephemeral oracle initializes")
        .seal(verified, &context)
        .expect("verified generation seals");
    let reopened = reader
        .read_generation(&context)
        .expect("sealed generation reads and verifies");

    assert_eq!(reopened.metadata(), metadata);
    assert_eq!(
        reopened.document(),
        &NormalizedIrDocument::empty(repository, generation)
    );
}

fn generation_format_version() -> u32 {
    (u32::from(GENERATION_CONTRACT_VERSION.major()) << 16)
        | u32::from(GENERATION_CONTRACT_VERSION.minor())
}
