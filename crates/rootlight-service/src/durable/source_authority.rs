//! Compact v5 source authority, pinned by the atomic generation manifest.
//! Legacy generations retain their original complete recovery requirements.

use super::*;
use rootlight_ir::SourceMappingKind;
use rootlight_source::{SourceAuthorityError, VerifiedSourceAuthority};
use rootlight_storage::{GenerationContractVersion, GenerationResource};
use serde::ser::{SerializeSeq, SerializeStruct};
use std::io::Write;
const SCHEMA: &str = "rootlight.source-authority/1";

const FILENAME: &str = "source-authority.json";
const MAX_BYTES: u64 = MAX_SOURCE_FILE_CATALOG_BYTES;

struct AuthorityWriter<W> {
    inner: W,
    bytes: u64,
    digest: blake3::Hasher,
}

impl<W: std::io::Write> std::io::Write for AuthorityWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let end = self
            .bytes
            .checked_add(bytes.len() as u64)
            .filter(|end| *end <= MAX_BYTES)
            .ok_or_else(|| {
                std::io::Error::other("source authority exceeds its bounded envelope")
            })?;
        self.inner.write_all(bytes)?;
        self.digest.update(bytes);
        self.bytes = end;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub(super) fn manifest_source_layout(
    manifest: &DurableGenerationManifest,
) -> Result<DurableSourceLayout, FirstSliceError> {
    match (manifest.version, manifest.source_authority) {
        (LEGACY_GENERATION_MANIFEST_VERSION..=RAW_CATALOG_GENERATION_MANIFEST_VERSION, None) => {}
        (GENERATION_MANIFEST_VERSION, Some(value))
            if value.bytes > 0 && value.bytes <= MAX_BYTES => {}
        _ => return Err(FirstSliceError::CatalogCorrupt),
    }
    source_storage_layout(manifest.version, manifest.source_storage)
}

pub(super) fn validate_for_verified(
    directory: &PrivateDirectory<'_>,
    manifest: &DurableGenerationManifest,
    generation: &IdentityVerifiedGeneration,
    cancellation: &Cancellation,
) -> Result<(), FirstSliceError> {
    manifest_source_layout(manifest)?;
    let Some(descriptor) = manifest.source_authority else {
        return Ok(());
    };
    let bytes = directory
        .read_file_bounded_cancellable(OsStr::new(FILENAME), descriptor.bytes, cancellation)
        .map_err(map_private_read_error)?;
    if bytes.len() as u64 != descriptor.bytes || content_hash_bytes(&bytes) != descriptor.digest {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let mut expected = AuthorityWriter {
        inner: std::io::sink(),
        bytes: 0,
        digest: blake3::Hasher::new(),
    };
    let context = GenerationContext::new(cancellation, GenerationBudget::default());
    export_source_authority(generation, &mut expected, &context)
        .map_err(|error| authority_error(error, cancellation))?;
    if expected.bytes != descriptor.bytes
        || ContentHash::from_bytes(*expected.digest.finalize().as_bytes()) != descriptor.digest
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok(())
}

fn authority_error(
    error: rootlight_source::SourceAuthorityError,
    cancellation: &Cancellation,
) -> FirstSliceError {
    if let Err(error) = check_cancellation(cancellation) {
        return error;
    }
    match error {
        rootlight_source::SourceAuthorityError::Memory => FirstSliceError::Retention,
        rootlight_source::SourceAuthorityError::Control(_) => FirstSliceError::BudgetExceeded,
        _ => FirstSliceError::CatalogCorrupt,
    }
}

impl DurablePreparedGeneration {
    pub(crate) fn write_source_authority(
        &self,
        generation: &IdentityVerifiedGeneration,
        cancellation: &Cancellation,
    ) -> Result<u64, FirstSliceError> {
        if generation.metadata().repository() != self.repository_id
            || generation.metadata().generation() != self.generation
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let mut descriptor = self
            .source_authority
            .lock()
            .map_err(|_| FirstSliceError::Catalog)?;
        if descriptor.is_some() {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let mut file = self
            .staging()
            .create_file(OsStr::new(FILENAME))
            .map_err(|_| FirstSliceError::Catalog)?;
        let mut writer = AuthorityWriter {
            inner: BufWriter::new(&mut file),
            bytes: 0,
            digest: blake3::Hasher::new(),
        };
        let context = GenerationContext::new(cancellation, GenerationBudget::default());
        export_source_authority(generation, &mut writer, &context)
            .map_err(|error| authority_error(error, cancellation))?;
        writer.flush().map_err(|_| FirstSliceError::Catalog)?;
        let value = DurableSidecarDescriptor {
            bytes: writer.bytes,
            digest: ContentHash::from_bytes(*writer.digest.finalize().as_bytes()),
        };
        drop(writer);
        file.sync_all().map_err(|_| FirstSliceError::Catalog)?;
        self.account_staging_bytes(value.bytes)?;
        *descriptor = Some(value);
        Ok(value.bytes)
    }
}

impl DurableCatalog {
    pub(crate) fn legacy_source_generation_parent(
        &self,
        repository: RepositoryId,
        generation: GenerationId,
        cancellation: &Cancellation,
    ) -> Result<Option<GenerationId>, FirstSliceError> {
        let repository_dir = PrivateDirectory::open(
            self.repositories.capability(),
            OsStr::new(&repository.to_string()),
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let directory = PrivateDirectory::open(
            repository_dir.capability(),
            OsStr::new(&generation.to_string()),
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let bytes = directory
            .read_file_bounded_cancellable(
                OsStr::new(MANIFEST_FILENAME),
                MAX_MANIFEST_BYTES,
                cancellation,
            )
            .map_err(map_private_read_error)?;
        let manifest: DurableGenerationManifest =
            serde_json::from_slice(&bytes).map_err(|_| FirstSliceError::CatalogCorrupt)?;
        manifest_source_layout(&manifest)?;
        if manifest.source_authority.is_some() {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        if manifest.receipt.repository != repository || manifest.receipt.generation != generation {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        Ok(manifest.receipt.parent)
    }
    pub(crate) fn source_authority_owner(
        &self,
        generation: GenerationId,
        cancellation: &Cancellation,
    ) -> Result<Option<RepositoryId>, FirstSliceError> {
        for name in private_entry_names(&self.repositories)? {
            check_cancellation(cancellation)?;
            let text = name.to_str().ok_or(FirstSliceError::CatalogCorrupt)?;
            let repository =
                RepositoryId::from_str(text).map_err(|_| FirstSliceError::CatalogCorrupt)?;
            if self
                .source_authority_descriptor(repository, generation, cancellation)?
                .is_some()
            {
                return Ok(Some(repository));
            }
        }
        Ok(None)
    }

    pub(crate) fn source_authority_descriptor(
        &self,
        repository: RepositoryId,
        generation: GenerationId,
        cancellation: &Cancellation,
    ) -> Result<Option<(u64, FirstSliceIndexReceipt)>, FirstSliceError> {
        let repository_dir = PrivateDirectory::open(
            self.repositories.capability(),
            OsStr::new(&repository.to_string()),
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let mut selected = false;
        for name in private_entry_names(&repository_dir)? {
            check_cancellation(cancellation)?;
            let text = name.to_str().ok_or(FirstSliceError::CatalogCorrupt)?;
            if let Some((sequence, candidate)) = parse_activation_name(text)
                && candidate == generation
            {
                read_activation_marker(&repository_dir, name, sequence, candidate)?;
                selected = true;
            }
        }
        if !selected {
            return Ok(None);
        }
        let directory = PrivateDirectory::open(
            repository_dir.capability(),
            OsStr::new(&generation.to_string()),
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let bytes = directory
            .read_file_bounded_cancellable(
                OsStr::new(MANIFEST_FILENAME),
                MAX_MANIFEST_BYTES,
                cancellation,
            )
            .map_err(map_private_read_error)?;
        let manifest: DurableGenerationManifest =
            serde_json::from_slice(&bytes).map_err(|_| FirstSliceError::CatalogCorrupt)?;
        manifest_source_layout(&manifest)?;
        if manifest.receipt.repository != repository || manifest.receipt.generation != generation {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        Ok(manifest
            .source_authority
            .map(|value| (value.bytes, manifest.receipt)))
    }

    pub(crate) fn restore_source_authority(
        &self,
        repository: RepositoryId,
        generation: GenerationId,
        cancellation: &Cancellation,
    ) -> Result<VerifiedSourceAuthority, FirstSliceError> {
        let repository_dir = PrivateDirectory::open(
            self.repositories.capability(),
            OsStr::new(&repository.to_string()),
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let directory = PrivateDirectory::open(
            repository_dir.capability(),
            OsStr::new(&generation.to_string()),
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let encoded = directory
            .read_file_bounded_cancellable(
                OsStr::new(MANIFEST_FILENAME),
                MAX_MANIFEST_BYTES,
                cancellation,
            )
            .map_err(map_private_read_error)?;
        let manifest: DurableGenerationManifest =
            serde_json::from_slice(&encoded).map_err(|_| FirstSliceError::CatalogCorrupt)?;
        manifest_source_layout(&manifest)?;
        let descriptor = manifest
            .source_authority
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        if manifest.receipt.repository != repository || manifest.receipt.generation != generation {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let bytes = directory
            .read_file_bounded_cancellable(OsStr::new(FILENAME), descriptor.bytes, cancellation)
            .map_err(map_private_read_error)?;
        if bytes.len() as u64 != descriptor.bytes || content_hash_bytes(&bytes) != descriptor.digest
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let context = GenerationContext::new(cancellation, GenerationBudget::default());
        let authority = restore_authority(
            &bytes,
            repository,
            generation,
            manifest.receipt.parent,
            &context,
        )
        .map_err(|error| authority_error(error, cancellation))?;
        if authority.file_count() as u64 != manifest.receipt.indexed_files
            || authority.origin_count() as u64 != manifest.receipt.extracted_files
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        Ok(authority)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    contract_major: u16,
    contract_minor: u16,
    repository: RepositoryId,
    generation: GenerationId,
    #[serde(deserialize_with = "required_nullable")]
    parent: Option<GenerationId>,
    manifest_hash: ContentHash,
    configuration_hash: ContentHash,
    provider_set_hash: ContentHash,
}

impl From<GenerationMetadata> for Metadata {
    fn from(value: GenerationMetadata) -> Self {
        Self {
            contract_major: value.contract_version().major(),
            contract_minor: value.contract_version().minor(),
            repository: value.repository(),
            generation: value.generation(),
            parent: value.parent(),
            manifest_hash: value.manifest_hash(),
            configuration_hash: value.configuration_hash(),
            provider_set_hash: value.provider_set_hash(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    claim: FileIdentityClaim,
    language: String,
    encoding: String,
    generated: bool,
    provenance: FactId,
    source: SourceRef,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    schema: String,
    metadata: Metadata,
    files: Vec<Entry>,
    origins: Vec<(SourceRef, SourceRef)>,
    #[serde(deserialize_with = "required_nullable")]
    recovery: Option<rootlight_ir::AggregateSourceRecovery>,
}

fn restore_authority(
    encoded: &[u8],
    repository: RepositoryId,
    generation: GenerationId,
    parent: Option<GenerationId>,
    context: &GenerationContext<'_>,
) -> Result<VerifiedSourceAuthority, SourceAuthorityError> {
    context.check()?;
    let decoded = serde_json::from_slice::<Wire>(encoded);
    context.check()?;
    let wire = decoded.map_err(|_| SourceAuthorityError::Invalid)?;
    if wire.schema != SCHEMA
        || wire.metadata.repository != repository
        || wire.metadata.generation != generation
        || wire.metadata.parent != parent
    {
        return Err(SourceAuthorityError::Invalid);
    }
    let m = wire.metadata;
    let metadata = GenerationMetadata::new_for_contract(
        GenerationContractVersion::new(m.contract_major, m.contract_minor),
        repository,
        generation,
        parent,
        m.manifest_hash,
        m.configuration_hash,
        m.provider_set_hash,
    )
    .map_err(|_| SourceAuthorityError::Invalid)?;
    context.require(
        GenerationResource::Rows,
        u64::try_from(wire.files.len()).map_err(|_| SourceAuthorityError::Invalid)?,
    )?;
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(wire.files.len())
        .map_err(|_| SourceAuthorityError::Memory)?;
    for entry in wire.files {
        context.check()?;
        let relative = RelativePath::from_identity_bytes(&entry.claim.path_identity)
            .map_err(|_| SourceAuthorityError::Invalid)?;
        if relative.as_str() != entry.claim.path || entry.claim.repository != repository {
            return Err(SourceAuthorityError::Invalid);
        }
        let file = FileRecord {
            id: entry.claim.file,
            repository,
            generation,
            path: entry.claim.path.clone(),
            path_locator: Some(relative.to_locator()),
            content_hash: entry.claim.content_hash,
            byte_length: entry.claim.byte_length,
            language: entry.language,
            encoding: entry.encoding,
            generated: entry.generated,
            provenance: entry.provenance,
            evidence: FactEvidence {
                source: Some(entry.source),
                derivation: Vec::new(),
            },
        };
        entries.push(
            SourceFileCatalogEntry::new(file, entry.claim)
                .map_err(|_| SourceAuthorityError::Invalid)?,
        );
    }
    VerifiedSourceAuthority::verify(metadata, entries, wire.origins, wire.recovery, context)
}

/// Streams a compact source artifact exclusively from a verified full generation.
///
/// Serialization borrows the semantic generation and materializes only one file
/// claim at a time. The destination must be a privately staged generation file.
///
/// # Errors
/// Preserves control failure and rejects incomplete metadata or output failure.
pub(super) fn export_source_authority(
    generation: &IdentityVerifiedGeneration,
    writer: &mut impl Write,
    context: &GenerationContext<'_>,
) -> Result<(), SourceAuthorityError> {
    struct Export<'a, 'c> {
        generation: &'a IdentityVerifiedGeneration,
        context: &'a GenerationContext<'c>,
    }
    struct Files<'a, 'c>(&'a Export<'a, 'c>);
    impl Serialize for Files<'_, '_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let generation = self.0.generation;
            let mut seq = serializer.serialize_seq(Some(generation.snapshot().file_count()))?;
            for extension in &generation.document().extensions {
                self.0.context.check().map_err(serde::ser::Error::custom)?;
                if extension.namespace != rootlight_ir::FILE_IDENTITY_CLAIM_NAMESPACE {
                    continue;
                }
                let claim = rootlight_ir::decode_file_identity_claim_envelope(extension)
                    .map_err(serde::ser::Error::custom)?;
                let file = generation
                    .snapshot()
                    .find_file(claim.file)
                    .ok_or_else(|| serde::ser::Error::custom("missing source file"))?;
                serialize_entry(&mut seq, file, claim)?;
            }
            for entry in generation.snapshot().source_files().entries() {
                self.0.context.check().map_err(serde::ser::Error::custom)?;
                serialize_entry(&mut seq, entry.file(), entry.identity_claim())?;
            }
            seq.end()
        }
    }
    fn serialize_entry<S: SerializeSeq>(
        seq: &mut S,
        file: &FileRecord,
        claim: FileIdentityClaim,
    ) -> Result<(), S::Error> {
        let source = file
            .evidence
            .source
            .clone()
            .ok_or_else(|| serde::ser::Error::custom("missing source evidence"))?;
        let relative = RelativePath::from_identity_bytes(&claim.path_identity)
            .map_err(serde::ser::Error::custom)?;
        if file.path_locator.as_ref() != Some(&relative.to_locator())
            || file.path != relative.as_str()
            || !file.evidence.derivation.is_empty()
        {
            return Err(serde::ser::Error::custom(
                "inconsistent source path or evidence",
            ));
        }
        seq.serialize_element(&Entry {
            claim,
            language: file.language.clone(),
            encoding: file.encoding.clone(),
            generated: file.generated,
            provenance: file.provenance,
            source,
        })
    }
    struct Origins<'a, 'c>(&'a Export<'a, 'c>);
    impl Serialize for Origins<'_, '_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut seq = serializer.serialize_seq(None)?;
            for mapping in &self.0.generation.document().source_mappings {
                self.0.context.check().map_err(serde::ser::Error::custom)?;
                if mapping.kind == SourceMappingKind::ExtractedFromContainer {
                    seq.serialize_element(&(&mapping.from, &mapping.to))?;
                }
            }
            for extension in &self.0.generation.document().extensions {
                self.0.context.check().map_err(serde::ser::Error::custom)?;
                if extension.namespace == rootlight_ir::AGGREGATE_SOURCE_ORIGIN_NAMESPACE {
                    seq.serialize_element(
                        &rootlight_ir::decode_aggregate_source_origin_envelope(extension)
                            .map_err(serde::ser::Error::custom)?,
                    )?;
                }
            }
            seq.end()
        }
    }
    impl Serialize for Export<'_, '_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut row = serializer.serialize_struct("SourceAuthority", 5)?;
            row.serialize_field("schema", SCHEMA)?;
            row.serialize_field("metadata", &Metadata::from(self.generation.metadata()))?;
            row.serialize_field("files", &Files(self))?;
            row.serialize_field("origins", &Origins(self))?;
            let mut envelopes = self
                .generation
                .document()
                .extensions
                .iter()
                .filter(|value| {
                    value.namespace == rootlight_ir::AGGREGATE_SOURCE_RECOVERY_NAMESPACE
                });
            let recovery = envelopes
                .next()
                .map(rootlight_ir::decode_aggregate_source_recovery_envelope)
                .transpose()
                .map_err(serde::ser::Error::custom)?;
            if envelopes.next().is_some() {
                return Err(serde::ser::Error::custom("duplicate source recovery cause"));
            }
            row.serialize_field("recovery", &recovery)?;
            row.end()
        }
    }
    context.check()?;
    let result = serde_json::to_writer(
        writer,
        &Export {
            generation,
            context,
        },
    );
    context.check()?;
    result.map_err(|_| SourceAuthorityError::Encoding)
}

fn required_nullable<'de, D: serde::Deserializer<'de>, T: serde::Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_codec_rejects_missing_nullable_identity_and_impossible_origin_census() {
        let (_storage, _fixture, paths, receipt, reference, _) =
            super::super::source_only_tests::source_only_reopen_fixture();
        let path = paths
            .state_dir()
            .join("first-slice/repositories")
            .join(receipt.repository.to_string())
            .join(receipt.generation.to_string())
            .join(FILENAME);
        let original: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        for field in ["parent", "recovery", "schema", "files", "origins"] {
            let mut value = original.clone();
            if field == "parent" {
                value["metadata"].as_object_mut().unwrap().remove(field);
            } else {
                value.as_object_mut().unwrap().remove(field);
            }
            assert!(
                restore_authority(
                    &serde_json::to_vec(&value).unwrap(),
                    receipt.repository,
                    receipt.generation,
                    receipt.parent,
                    &context
                )
                .is_err(),
                "missing {field}"
            );
        }
        let mut value = original.clone();
        value["schema"] = "rootlight.source-authority/2".into();
        assert!(
            restore_authority(
                &serde_json::to_vec(&value).unwrap(),
                receipt.repository,
                receipt.generation,
                receipt.parent,
                &context
            )
            .is_err()
        );
        let mut value = original.clone();
        value["origins"] =
            serde_json::to_value(vec![(reference.clone(), reference.clone()); 2]).unwrap();
        assert!(matches!(
            restore_authority(
                &serde_json::to_vec(&value).unwrap(),
                receipt.repository,
                receipt.generation,
                receipt.parent,
                &context
            ),
            Err(SourceAuthorityError::Invalid)
        ));
        let mut value = original.clone();
        value["metadata"]["generation"] =
            serde_json::to_value(GenerationId::from_bytes([99; 20])).unwrap();
        assert!(
            restore_authority(
                &serde_json::to_vec(&value).unwrap(),
                receipt.repository,
                receipt.generation,
                receipt.parent,
                &context
            )
            .is_err()
        );
        let mut value = original;
        value["files"][0]["claim"]["path"] = "foreign.rs".into();
        assert!(
            restore_authority(
                &serde_json::to_vec(&value).unwrap(),
                receipt.repository,
                receipt.generation,
                receipt.parent,
                &context
            )
            .is_err()
        );
    }
}
