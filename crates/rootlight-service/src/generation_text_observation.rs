//! Test-only decoded-text census of already borrowed generation inputs.
//! Traversal follows storage's TextBytes order; fixed numeric rows never retain
//! names, paths or payloads, and terminal-error reconciliation is explicit.

#![cfg(test)]

use rootlight_cancel::Cancellation;
use rootlight_ir::NormalizedIrDocument;
use rootlight_storage::{HARD_MAX_GENERATION_TEXT_BYTES, SourceFileCatalog};
use serde::Serialize;

use super::{FirstSliceError, FirstSliceResource, check_cancellation};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum FieldClass {
    FilePath,
    FileLocatorComponent,
    FileMetadata,
    EntityLanguage,
    EntityCanonicalName,
    EntityDisplayName,
    EntityQualifiedName,
    OccurrenceSyntax,
    ProvenanceProducer,
    ProvenanceMetadata,
    SkippedDetail,
    DiagnosticText,
    ExtensionMetadata,
    ExtensionPayload,
}

const CLASSES: [FieldClass; 14] = [
    FieldClass::FilePath,
    FieldClass::FileLocatorComponent,
    FieldClass::FileMetadata,
    FieldClass::EntityLanguage,
    FieldClass::EntityCanonicalName,
    FieldClass::EntityDisplayName,
    FieldClass::EntityQualifiedName,
    FieldClass::OccurrenceSyntax,
    FieldClass::ProvenanceProducer,
    FieldClass::ProvenanceMetadata,
    FieldClass::SkippedDetail,
    FieldClass::DiagnosticText,
    FieldClass::ExtensionMetadata,
    FieldClass::ExtensionPayload,
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
struct Usage {
    string_count: u64,
    utf8_bytes: u64,
}

impl Usage {
    fn added(self, bytes: u64) -> Result<Self, FirstSliceError> {
        Ok(Self {
            string_count: self
                .string_count
                .checked_add(1)
                .ok_or(FirstSliceError::Limits)?,
            utf8_bytes: self
                .utf8_bytes
                .checked_add(bytes)
                .ok_or(FirstSliceError::Limits)?,
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
struct ClassRow {
    class: FieldClass,
    complete_borrowed_input: Usage,
    accepted_prefix: Usage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
struct RejectedField {
    class: FieldClass,
    string_utf8_bytes: u64,
    observed: u64,
}

#[derive(Debug, Serialize)]
struct TerminalError {
    resource: &'static str,
    observed: u64,
    limit: u64,
}

/// Fixed-shape TextBytes accounting, independent of the caller's service result.
#[derive(Debug, Serialize)]
pub(super) struct TextObservation {
    schema: &'static str,
    scope: &'static str,
    limit: u64,
    complete_borrowed_input: Usage,
    accepted_prefix: Usage,
    rejected_field: Option<RejectedField>,
    unvisited_after_rejected_field: Option<Usage>,
    rows: [ClassRow; 14],
    reconciled_terminal_error: Option<TerminalError>,
    source_text_included: bool,
    names_paths_payloads_included: bool,
    other_storage_resources_validated: bool,
    whole_corpus_observed: bool,
    product_success_qualified: bool,
    benchmark_qualified: bool,
}

impl TextObservation {
    fn new(limit: u64) -> Self {
        Self {
            schema: "rootlight.generation-text-observation/1",
            scope: "borrowed_generation_document_and_source_catalog_text_only",
            limit,
            complete_borrowed_input: Usage::default(),
            accepted_prefix: Usage::default(),
            rejected_field: None,
            unvisited_after_rejected_field: None,
            rows: CLASSES.map(|class| ClassRow {
                class,
                complete_borrowed_input: Usage::default(),
                accepted_prefix: Usage::default(),
            }),
            reconciled_terminal_error: None,
            source_text_included: false,
            names_paths_payloads_included: false,
            other_storage_resources_validated: false,
            whole_corpus_observed: false,
            product_success_qualified: false,
            benchmark_qualified: false,
        }
    }

    fn add(
        &mut self,
        class: FieldClass,
        value: &str,
        cancellation: &Cancellation,
    ) -> Result<(), FirstSliceError> {
        check_cancellation(cancellation)?;
        let bytes = u64::try_from(value.len()).map_err(|_| FirstSliceError::Limits)?;
        let complete = self.complete_borrowed_input.added(bytes)?;
        let candidate = if self.rejected_field.is_none() {
            Some(self.accepted_prefix.added(bytes)?)
        } else {
            None
        };
        let accepted = candidate.filter(|usage| usage.utf8_bytes <= self.limit);
        let row = self
            .rows
            .iter_mut()
            .find(|row| row.class == class)
            .ok_or(FirstSliceError::Identity)?;
        let complete_row = row.complete_borrowed_input.added(bytes)?;
        let prefix_row = if accepted.is_some() {
            row.accepted_prefix.added(bytes)?
        } else {
            row.accepted_prefix
        };
        // Every fallible calculation precedes commits, including diagnostic counters.
        row.complete_borrowed_input = complete_row;
        row.accepted_prefix = prefix_row;
        self.complete_borrowed_input = complete;
        if let Some(accepted) = accepted {
            self.accepted_prefix = accepted;
        } else if let Some(candidate) = candidate {
            self.rejected_field = Some(RejectedField {
                class,
                string_utf8_bytes: bytes,
                observed: candidate.utf8_bytes,
            });
        }
        Ok(())
    }

    /// Binds this prediction only to explicitly supplied terminal TextBytes values.
    ///
    /// # Errors
    /// Rejects other resources, non-failures, different limits or different boundaries.
    /// The caller retains and returns its original service error independently.
    pub(super) fn reconcile_failure(
        mut self,
        resource: FirstSliceResource,
        observed: u64,
        limit: u64,
    ) -> Result<Self, FirstSliceError> {
        if resource != FirstSliceResource::TextBytes
            || observed <= limit
            || self.limit != limit
            || self
                .rejected_field
                .is_none_or(|field| field.observed != observed)
        {
            return Err(FirstSliceError::Identity);
        }
        self.reconciled_terminal_error = Some(TerminalError {
            resource: "text_bytes",
            observed,
            limit,
        });
        Ok(self)
    }
}

/// Counts borrowed input completely while preserving storage's first rejected prefix.
///
/// No source is reparsed or cloned. The complete total describes only these borrowed
/// records, not corpus completeness or successful indexing. Counting continues after
/// the predicted boundary to distinguish the remainder from the encountered deficit.
///
/// # Errors
/// Returns independent diagnostic cancellation, overflow or invalid-limit failures.
/// Other storage resource checks and fact validation are deliberately not reproduced.
pub(super) fn measure(
    document: &NormalizedIrDocument,
    source_files: &SourceFileCatalog,
    limit: u64,
    cancellation: &Cancellation,
) -> Result<TextObservation, FirstSliceError> {
    check_cancellation(cancellation)?;
    if limit == 0 || limit > HARD_MAX_GENERATION_TEXT_BYTES {
        return Err(FirstSliceError::Identity);
    }
    let mut result = TextObservation::new(limit);
    for file in &document.files {
        result.add(FieldClass::FilePath, &file.path, cancellation)?;
        if let Some(locator) = &file.path_locator {
            for component in locator.components() {
                result.add(FieldClass::FileLocatorComponent, component, cancellation)?;
            }
        }
        result.add(FieldClass::FileMetadata, &file.language, cancellation)?;
        result.add(FieldClass::FileMetadata, &file.encoding, cancellation)?;
    }
    for entry in source_files.entries() {
        let file = entry.file();
        result.add(FieldClass::FilePath, &file.path, cancellation)?;
        // File-only locator components and path identity belong to EncodedTextBytes.
        result.add(FieldClass::FileMetadata, &file.language, cancellation)?;
        result.add(FieldClass::FileMetadata, &file.encoding, cancellation)?;
    }
    for entity in &document.entities {
        result.add(FieldClass::EntityLanguage, &entity.language, cancellation)?;
        result.add(
            FieldClass::EntityCanonicalName,
            &entity.canonical_name,
            cancellation,
        )?;
        result.add(
            FieldClass::EntityDisplayName,
            &entity.display_name,
            cancellation,
        )?;
        result.add(
            FieldClass::EntityQualifiedName,
            &entity.qualified_name,
            cancellation,
        )?;
    }
    for occurrence in &document.occurrences {
        result.add(
            FieldClass::OccurrenceSyntax,
            &occurrence.syntax_kind,
            cancellation,
        )?;
    }
    for provenance in &document.provenance {
        result.add(
            FieldClass::ProvenanceProducer,
            provenance.producer.name(),
            cancellation,
        )?;
        result.add(
            FieldClass::ProvenanceProducer,
            provenance.producer.version(),
            cancellation,
        )?;
        if let Some(frontend) = &provenance.frontend_version {
            result.add(FieldClass::ProvenanceMetadata, frontend, cancellation)?;
        }
        result.add(
            FieldClass::ProvenanceMetadata,
            &provenance.language,
            cancellation,
        )?;
        if let Some(rule) = &provenance.rule {
            result.add(FieldClass::ProvenanceMetadata, rule, cancellation)?;
        }
    }
    for region in &document.skipped_regions {
        result.add(FieldClass::SkippedDetail, &region.detail, cancellation)?;
    }
    for diagnostic in &document.diagnostics {
        result.add(FieldClass::DiagnosticText, &diagnostic.code, cancellation)?;
        result.add(
            FieldClass::DiagnosticText,
            &diagnostic.message,
            cancellation,
        )?;
    }
    for extension in &document.extensions {
        result.add(
            FieldClass::ExtensionMetadata,
            &extension.namespace,
            cancellation,
        )?;
        result.add(
            FieldClass::ExtensionMetadata,
            &extension.version,
            cancellation,
        )?;
        result.add(
            FieldClass::ExtensionPayload,
            &extension.payload,
            cancellation,
        )?;
    }
    if let Some(rejected) = result.rejected_field {
        result.unvisited_after_rejected_field = Some(Usage {
            string_count: result
                .complete_borrowed_input
                .string_count
                .checked_sub(
                    result
                        .accepted_prefix
                        .string_count
                        .checked_add(1)
                        .ok_or(FirstSliceError::Limits)?,
                )
                .ok_or(FirstSliceError::Identity)?,
            utf8_bytes: result
                .complete_borrowed_input
                .utf8_bytes
                .checked_sub(rejected.observed)
                .ok_or(FirstSliceError::Identity)?,
        });
    }
    check_cancellation(cancellation)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::{
        FactId, FileIdentity, GenerationIdentity, RepositoryId, SymbolId, content_hash,
        derive_file, derive_generation,
    };
    use rootlight_ir::{
        AnalysisTier, BuildContextIdentity, Confidence, CoverageStatus, DiagnosticRecord,
        DiagnosticSeverity, EntityKind, EntityRecord, EntityVisibility, ExtensionCriticality,
        ExtensionEnvelope, ExtensionSupport, FactDomain, FactEvidence, FileIdentityClaim,
        FilePathLocator, FilePathLocatorEncoding, FileRecord, IrLimits, OccurrenceRecord,
        OccurrenceRole, OccurrenceTarget, ProducerIdentity, ProducerKind, ProvenanceRecord,
        SkippedRegion, SkippedRegionReason, SourceRef, SourceSpan,
    };
    use rootlight_storage::{
        GENERATION_CONTRACT_VERSION, GenerationBudget, GenerationContext, GenerationControlError,
        GenerationMetadata, GenerationResource, GenerationSnapshot, GenerationSnapshotError,
        HARD_MAX_GENERATION_ROWS, HARD_MAX_GENERATION_SOURCE_REFS, SourceFileCatalogEntry,
    };

    fn metadata() -> GenerationMetadata {
        let repository = RepositoryId::from_bytes([1; 16]);
        let hash = content_hash(b"synthetic");
        let version = (u32::from(GENERATION_CONTRACT_VERSION.major()) << 16)
            | u32::from(GENERATION_CONTRACT_VERSION.minor());
        let generation = derive_generation(GenerationIdentity {
            repository,
            parent: None,
            manifest_hash: hash,
            config_hash: hash,
            provider_set_hash: hash,
            format_version: version,
        })
        .id();
        GenerationMetadata::new(repository, generation, None, hash, hash, hash).unwrap()
    }

    fn file(metadata: &GenerationMetadata, path: &str, locator: &str) -> FileRecord {
        let id = derive_file(FileIdentity {
            repository: metadata.repository(),
            path_identity: path.as_bytes(),
        })
        .id();
        let hash = content_hash(b"synthetic source");
        FileRecord {
            id,
            repository: metadata.repository(),
            generation: metadata.generation(),
            path: path.to_owned(),
            path_locator: Some(
                FilePathLocator::new(
                    FilePathLocatorEncoding::UnixBytesV1,
                    vec![locator.to_owned()],
                )
                .unwrap(),
            ),
            content_hash: hash,
            byte_length: 9_000_000,
            language: "xml".to_owned(),
            encoding: "utf-8".to_owned(),
            generated: false,
            provenance: FactId::from_bytes([2; 20]),
            evidence: FactEvidence {
                source: Some(SourceRef::new(
                    metadata.repository(),
                    metadata.generation(),
                    SourceSpan::new(id, 0, 9_000_000).unwrap(),
                    hash,
                    None,
                )),
                derivation: vec![],
            },
        }
    }

    fn fixture(metadata: &GenerationMetadata) -> NormalizedIrDocument {
        let mut document =
            NormalizedIrDocument::empty(metadata.repository(), metadata.generation());
        document.files.push(file(metadata, "δ", "ceb4"));
        document.entities.push(EntityRecord {
            id: SymbolId::from_bytes([3; 20]),
            repository: metadata.repository(),
            generation: metadata.generation(),
            kind: EntityKind::Function,
            language: "xml".to_owned(),
            tier: AnalysisTier::TierB,
            canonical_name: "π".to_owned(),
            display_name: "π".to_owned(),
            qualified_name: "ns::π".to_owned(),
            container: None,
            visibility: EntityVisibility::Unknown,
            flags: vec![],
            provenance: FactId::from_bytes([2; 20]),
            evidence: FactEvidence {
                source: None,
                derivation: vec![],
            },
        });
        document.extensions.push(ExtensionEnvelope {
            id: FactId::from_bytes([4; 20]),
            repository: metadata.repository(),
            generation: metadata.generation(),
            namespace: "test.bytes".to_owned(),
            version: "1".to_owned(),
            criticality: ExtensionCriticality::Noncritical,
            payload: "λ🙂".to_owned(),
            provenance: FactId::from_bytes([2; 20]),
            evidence: FactEvidence {
                source: None,
                derivation: vec![],
            },
        });
        document
    }

    fn compare_storage(
        metadata: &GenerationMetadata,
        document: &NormalizedIrDocument,
        catalog: &SourceFileCatalog,
    ) {
        let total = measure(
            document,
            catalog,
            HARD_MAX_GENERATION_TEXT_BYTES,
            &Cancellation::new(),
        )
        .unwrap()
        .complete_borrowed_input
        .utf8_bytes;
        // These fixtures exercise storage's budget-first pass, not fact identities.
        for limit in 1..total {
            let cancellation = Cancellation::new();
            let budget = GenerationBudget::new(
                HARD_MAX_GENERATION_ROWS,
                HARD_MAX_GENERATION_SOURCE_REFS,
                limit,
            )
            .unwrap();
            let context = GenerationContext::new(&cancellation, budget);
            let error = GenerationSnapshot::new_with_source_files_and_context(
                *metadata,
                document.clone(),
                catalog.clone(),
                &IrLimits::default(),
                &ExtensionSupport::default(),
                &context,
            )
            .unwrap_err();
            let GenerationSnapshotError::Control(GenerationControlError::BudgetExceeded {
                resource: GenerationResource::TextBytes,
                observed,
                limit: actual_limit,
            }) = error
            else {
                panic!("expected storage TextBytes boundary, got {error:?}");
            };
            let measured = measure(document, catalog, limit, &cancellation).unwrap();
            let rejected = measured.rejected_field.unwrap();
            assert_eq!(rejected.observed, observed);
            assert_eq!(actual_limit, limit);
            assert_eq!(
                measured
                    .accepted_prefix
                    .utf8_bytes
                    .checked_add(rejected.string_utf8_bytes),
                Some(observed)
            );
            measured
                .reconcile_failure(FirstSliceResource::TextBytes, observed, limit)
                .unwrap();
        }
    }

    #[test]
    fn storage_oracle_and_unicode_metadata_payload_composition_agree() {
        let metadata = metadata();
        let document = fixture(&metadata);
        let catalog = SourceFileCatalog::default();
        let result = measure(&document, &catalog, 28, &Cancellation::new()).unwrap();
        assert_eq!(
            result.complete_borrowed_input,
            Usage {
                string_count: 11,
                utf8_bytes: 44
            }
        );
        assert_eq!(result.accepted_prefix.utf8_bytes, 27);
        assert_eq!(
            result.rejected_field,
            Some(RejectedField {
                class: FieldClass::ExtensionMetadata,
                string_utf8_bytes: 10,
                observed: 37
            })
        );
        assert_eq!(
            result.unvisited_after_rejected_field,
            Some(Usage {
                string_count: 2,
                utf8_bytes: 7
            })
        );
        let row = result
            .rows
            .iter()
            .find(|row| row.class == FieldClass::ExtensionPayload)
            .unwrap();
        assert_eq!(row.complete_borrowed_input.utf8_bytes, 6);
        assert_eq!(row.accepted_prefix.utf8_bytes, 0);
        compare_storage(&metadata, &document, &catalog);
    }

    #[test]
    fn storage_oracle_preserves_occurrence_provenance_skip_and_diagnostic_order() {
        let metadata = metadata();
        let mut document = fixture(&metadata);
        let source = document.files[0].evidence.source.clone().unwrap();
        let hash = content_hash(b"synthetic");
        let evidence = FactEvidence {
            source: None,
            derivation: vec![],
        };
        document.occurrences.push(OccurrenceRecord {
            id: FactId::from_bytes([5; 20]),
            repository: metadata.repository(),
            generation: metadata.generation(),
            file: source.span().file(),
            source: source.clone(),
            role: OccurrenceRole::Reference,
            enclosing: None,
            target: OccurrenceTarget::Unresolved { text_hash: hash },
            syntactic_text_hash: hash,
            syntax_kind: "node".to_owned(),
            provenance: FactId::from_bytes([2; 20]),
            confidence: Confidence::new(1000).unwrap(),
            evidence: evidence.clone(),
        });
        for (id, name, version, frontend, language, rule) in [
            (6, "p", "2", Some("λ"), "xml", Some("r🙂")),
            (7, "q", "3", None, "z", None),
        ] {
            document.provenance.push(ProvenanceRecord {
                id: FactId::from_bytes([id; 20]),
                repository: metadata.repository(),
                generation: metadata.generation(),
                producer_kind: ProducerKind::Parser,
                producer: ProducerIdentity::new(name, version, hash).unwrap(),
                binary_digest: hash,
                frontend_version: frontend.map(str::to_owned),
                language: language.to_owned(),
                tier: AnalysisTier::TierB,
                build_context: BuildContextIdentity::new(hash),
                input_sources: vec![],
                evidence_sources: vec![],
                derivation_parents: vec![],
                rule: rule.map(str::to_owned),
            });
        }
        document.skipped_regions.push(SkippedRegion {
            id: FactId::from_bytes([8; 20]),
            repository: metadata.repository(),
            generation: metadata.generation(),
            source,
            domain: FactDomain::Occurrences,
            reason: SkippedRegionReason::UnsupportedConstruct,
            detail: "skip".to_owned(),
            provenance: FactId::from_bytes([2; 20]),
            evidence: evidence.clone(),
        });
        document.diagnostics.push(DiagnosticRecord {
            id: FactId::from_bytes([9; 20]),
            repository: metadata.repository(),
            generation: metadata.generation(),
            code: "D".to_owned(),
            message: "⚑".to_owned(),
            severity: DiagnosticSeverity::Warning,
            source: None,
            coverage_effect: CoverageStatus::Bounded,
            provenance: FactId::from_bytes([2; 20]),
            evidence,
        });
        let catalog = SourceFileCatalog::default();
        // Each boundary names the next string in storage's exact traversal order.
        for (limit, class, bytes) in [
            (27, FieldClass::OccurrenceSyntax, 4),
            (31, FieldClass::ProvenanceProducer, 1),
            (32, FieldClass::ProvenanceProducer, 1),
            (33, FieldClass::ProvenanceMetadata, 2),
            (35, FieldClass::ProvenanceMetadata, 3),
            (38, FieldClass::ProvenanceMetadata, 5),
            (43, FieldClass::ProvenanceProducer, 1),
            (44, FieldClass::ProvenanceProducer, 1),
            (45, FieldClass::ProvenanceMetadata, 1),
            (46, FieldClass::SkippedDetail, 4),
            (50, FieldClass::DiagnosticText, 1),
            (51, FieldClass::DiagnosticText, 3),
            (54, FieldClass::ExtensionMetadata, 10),
        ] {
            let result = measure(&document, &catalog, limit, &Cancellation::new()).unwrap();
            assert_eq!(result.accepted_prefix.utf8_bytes, limit);
            assert_eq!(
                result.rejected_field,
                Some(RejectedField {
                    class,
                    string_utf8_bytes: bytes,
                    observed: limit + bytes,
                })
            );
            assert_eq!(
                result.complete_borrowed_input,
                Usage {
                    string_count: 23,
                    utf8_bytes: 71,
                }
            );
        }
        compare_storage(&metadata, &document, &catalog);
    }

    #[test]
    fn catalog_locators_and_raw_source_lengths_are_not_decoded_text() {
        let metadata = metadata();
        let document = NormalizedIrDocument::empty(metadata.repository(), metadata.generation());
        let file = file(&metadata, "catalog.txt", "636174616c6f672e747874");
        let claim = FileIdentityClaim {
            file: file.id,
            repository: file.repository,
            path: file.path.clone(),
            path_identity: file.path.as_bytes().to_vec(),
            content_hash: file.content_hash,
            byte_length: file.byte_length,
        };
        let catalog =
            SourceFileCatalog::new(vec![SourceFileCatalogEntry::new(file, claim).unwrap()])
                .unwrap();
        let result = measure(&document, &catalog, 19, &Cancellation::new()).unwrap();
        assert_eq!(
            result.complete_borrowed_input,
            Usage {
                string_count: 3,
                utf8_bytes: 19
            }
        );
        assert!(result.rejected_field.is_none());
        assert_eq!(
            result
                .rows
                .iter()
                .find(|row| row.class == FieldClass::FileLocatorComponent)
                .unwrap()
                .complete_borrowed_input,
            Usage::default()
        );
        compare_storage(&metadata, &document, &catalog);
    }

    #[test]
    fn explicit_terminal_reconciliation_rejects_other_resources_and_boundaries() {
        let metadata = metadata();
        let document = fixture(&metadata);
        for (resource, observed, limit) in [
            (FirstSliceResource::StringBytes, 37, 28),
            (FirstSliceResource::TextBytes, 38, 28),
            (FirstSliceResource::TextBytes, 37, 29),
            (FirstSliceResource::TextBytes, 28, 28),
        ] {
            assert!(matches!(
                measure(
                    &document,
                    &SourceFileCatalog::default(),
                    28,
                    &Cancellation::new()
                )
                .unwrap()
                .reconcile_failure(resource, observed, limit),
                Err(FirstSliceError::Identity)
            ));
        }
        let original = FirstSliceError::ResourceLimit {
            resource: FirstSliceResource::TextBytes,
            observed: 37,
            limit: 28,
        };
        let report = measure(
            &document,
            &SourceFileCatalog::default(),
            28,
            &Cancellation::new(),
        )
        .unwrap()
        .reconcile_failure(FirstSliceResource::TextBytes, 37, 28)
        .unwrap();
        let json = serde_json::to_value(report).unwrap();
        let encoded = serde_json::to_string(&json).unwrap();
        for private_value in ["test.bytes", "λ🙂", "ns::π", "δ"] {
            assert!(!encoded.contains(private_value));
        }
        assert_eq!(json["reconciled_terminal_error"]["observed"], 37);
        for key in [
            "source_text_included",
            "names_paths_payloads_included",
            "other_storage_resources_validated",
            "whole_corpus_observed",
            "product_success_qualified",
            "benchmark_qualified",
        ] {
            assert_eq!(json[key], false);
        }
        assert_eq!(
            original,
            FirstSliceError::ResourceLimit {
                resource: FirstSliceResource::TextBytes,
                observed: 37,
                limit: 28
            }
        );
    }

    #[test]
    fn empty_and_exact_limit_inputs_do_not_predict_a_failure() {
        let metadata = metadata();
        let empty = NormalizedIrDocument::empty(metadata.repository(), metadata.generation());
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(
            &cancellation,
            GenerationBudget::new(HARD_MAX_GENERATION_ROWS, HARD_MAX_GENERATION_SOURCE_REFS, 1)
                .unwrap(),
        );
        GenerationSnapshot::new_with_context(
            metadata,
            empty.clone(),
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &context,
        )
        .unwrap();
        let result = measure(
            &empty,
            &SourceFileCatalog::default(),
            1,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(result.complete_borrowed_input, Usage::default());
        assert!(result.unvisited_after_rejected_field.is_none());
        assert!(matches!(
            result.reconcile_failure(FirstSliceResource::TextBytes, 2, 1),
            Err(FirstSliceError::Identity)
        ));
        let result = measure(
            &fixture(&metadata),
            &SourceFileCatalog::default(),
            44,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(result.accepted_prefix, result.complete_borrowed_input);
        assert!(result.rejected_field.is_none());
        for limit in [0, HARD_MAX_GENERATION_TEXT_BYTES + 1] {
            assert!(matches!(
                measure(
                    &empty,
                    &SourceFileCatalog::default(),
                    limit,
                    &Cancellation::new()
                ),
                Err(FirstSliceError::Identity)
            ));
        }
    }

    #[test]
    fn overflow_and_cancellation_leave_counter_commits_untouched() {
        for count_overflow in [false, true] {
            let mut result = TextObservation::new(1);
            result.complete_borrowed_input = if count_overflow {
                Usage {
                    string_count: u64::MAX,
                    utf8_bytes: 0,
                }
            } else {
                Usage {
                    string_count: 0,
                    utf8_bytes: u64::MAX,
                }
            };
            let before = result.complete_borrowed_input;
            assert_eq!(
                result.add(FieldClass::FilePath, "x", &Cancellation::new()),
                Err(FirstSliceError::Limits)
            );
            assert_eq!(result.complete_borrowed_input, before);
            assert_eq!(
                result.rows.first().unwrap().complete_borrowed_input,
                Usage::default()
            );
        }
        let mut result = TextObservation::new(1);
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert_eq!(
            result.add(FieldClass::FilePath, "x", &cancellation),
            Err(FirstSliceError::Cancelled(
                CancellationReason::ClientRequest
            ))
        );
        assert_eq!(result.complete_borrowed_input, Usage::default());
    }
}
