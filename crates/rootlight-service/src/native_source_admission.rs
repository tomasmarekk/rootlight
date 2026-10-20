//! Native-origin work refusals preserve a complete file without syntax claims.
//! Certificates are admitted only for the service's default Structural policy;
//! source owners and all authenticated origin mappings survive normal sealing.

use super::*;
use rootlight_adapter_treesitter::NativeIdentityWorkLimit;

pub(super) fn certificate_entry_bytes() -> Result<u64, FirstSliceError> {
    // Reserve key/value storage plus a conservative per-entry B-tree allowance.
    u64::try_from(size_of::<(FileId, NativeIdentityWorkLimit)>() + size_of::<[usize; 8]>())
        .map_err(|_| FirstSliceError::Limits)
}

pub(super) fn certify(
    limits: &AnalysisLimits,
    mode: FirstSliceIndexMode,
    analyzer: &TreeSitterAnalyzer,
    parser: &dyn StructuralPreflight,
    request: &rootlight_adapter_sdk::ParseRequest<'_>,
    original: &AdapterError,
    cancellation: &Cancellation,
) -> Result<Option<NativeIdentityWorkLimit>, AdapterError> {
    cancellation.check()?;
    if mode != FirstSliceIndexMode::Structural
        || !matches!(
            original,
            AdapterError::Sink(rootlight_adapter_sdk::SinkError::StreamLimit {
                resource: rootlight_adapter_sdk::ResourceKind::Records,
                ..
            })
        )
        || !analyzer.uses_parse_provider(parser)
    {
        return Ok(None);
    }
    let Ok(defaults) = analysis_limits(limits.max_source_bytes()) else {
        return Ok(None);
    };
    if *limits != defaults {
        return Ok(None);
    }
    let proof = parser.certify_intrinsic_identity_limit(request, cancellation)?;
    cancellation.check()?;
    match proof {
        Some(proof)
            if parser.owns_native_identity_limit(&proof)
                && proof.matches_request(request)
                && proof.matches_error(original) =>
        {
            Ok(Some(proof))
        }
        Some(_) => Err(AdapterError::ProviderFailed {
            code: rootlight_adapter_sdk::DiagnosticCode::new("native-limit-certificate-mismatch")
                .expect("static diagnostic is valid"),
        }),
        None => Ok(None),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "captured source, certificate and origin owners must be independently joined"
)]
pub(super) fn source_document(
    repository: RepositoryId,
    generation: GenerationId,
    input: &RustSourceInput,
    language: &str,
    proof: &NativeIdentityWorkLimit,
    sources: &[RustSourceInput],
    limits: &rootlight_ir::IrLimits,
    cancellation: &Cancellation,
) -> Result<NormalizedIrDocument, FirstSliceError> {
    check_cancellation(cancellation)?;
    let source = whole_source(repository, generation, &input.snapshot)?;
    if !proof.matches_source(&source, language, &input.encoding) {
        return Err(FirstSliceError::Identity);
    }
    let path = input.snapshot.path();
    let claim = FileIdentityClaim {
        file: input.snapshot.file(),
        repository,
        path: fallible_copy_string(path.as_str())?,
        path_identity: fallible_copy_bytes(path.identity_bytes())?,
        content_hash: input.snapshot.content_hash(),
        byte_length: source.span().end_byte(),
    };
    let disposition = UnsupportedSourceInput {
        claim,
        language: fallible_copy_string(language)?,
        encoding: fallible_copy_string(&input.encoding)?,
        generated: input.generated,
        disposition: UnsupportedSourceDisposition::IntrinsicIdentityWorkLimit {
            observed: proof.observed(),
            limit: proof.limit(),
        },
    };
    let mut document = unsupported_source_document(repository, generation, &disposition)?;
    // A file-only disposition must not erase already authenticated generation
    // origins. Container member mappings are still appended by PreparedSources.
    for mapping in &input.origins {
        check_cancellation(cancellation)?;
        let mut origin = None;
        for candidate in sources {
            check_cancellation(cancellation)?;
            if candidate.snapshot.path() == mapping.origin_path()
                && origin.replace(candidate).is_some()
            {
                return Err(FirstSliceError::Identity);
            }
        }
        let origin = origin.ok_or(FirstSliceError::Identity)?;
        let origin_source = whole_source(repository, generation, &origin.snapshot)?;
        if mapping.generated().file() != source.span().file()
            || mapping.generated().end_byte() > source.span().end_byte()
            || mapping.origin().file() != origin_source.span().file()
            || mapping.origin().end_byte() > origin_source.span().end_byte()
        {
            return Err(FirstSliceError::Identity);
        }
        let from = SourceRef::new(
            repository,
            generation,
            mapping.generated(),
            source.content_hash(),
            None,
        );
        let to = SourceRef::new(
            repository,
            generation,
            mapping.origin(),
            origin_source.content_hash(),
            None,
        );
        let base = document
            .provenance
            .first()
            .ok_or(FirstSliceError::Identity)?;
        let mut provenance = ProvenanceRecord {
            id: FactId::from_bytes([0; 20]),
            repository,
            generation,
            producer_kind: ProducerKind::Derivation,
            producer: base.producer.clone(),
            binary_digest: base.binary_digest,
            frontend_version: base.frontend_version.clone(),
            language: fallible_copy_string(language)?,
            tier: AnalysisTier::TierD,
            build_context: base.build_context,
            input_sources: vec![source.clone(), origin_source],
            evidence_sources: vec![from.clone(), to.clone()],
            derivation_parents: vec![FactRef::Fact(base.id)],
            rule: Some(mapping.provenance_rule()),
        };
        // Identity recipes retain vector order; match IR normalization first.
        provenance.input_sources.sort_unstable();
        provenance.input_sources.dedup();
        provenance.evidence_sources.sort_unstable();
        provenance.evidence_sources.dedup();
        provenance.id =
            derive_provenance_record_id(&provenance).map_err(|_| FirstSliceError::Identity)?;
        let mut record = SourceMappingRecord {
            id: FactId::from_bytes([0; 20]),
            repository,
            generation,
            from: from.clone(),
            to,
            kind: SourceMappingKind::GeneratedToOrigin,
            provenance: provenance.id,
            evidence: FactEvidence {
                source: Some(from),
                derivation: vec![FactRef::File(origin.snapshot.file())],
            },
        };
        record.id = rootlight_ir::derive_source_mapping_record_id(&record)
            .map_err(|_| FirstSliceError::Identity)?;
        reserve_records(&mut document.provenance, 1, limits.max_provenance_records)?;
        reserve_records(&mut document.source_mappings, 1, limits.max_source_mappings)?;
        document.provenance.push(provenance);
        document.source_mappings.push(record);
    }
    for coverage in &mut document.coverage_records {
        check_cancellation(cancellation)?;
        let count = match coverage.domain {
            IrFactDomain::Provenance => Some(document.provenance.len()),
            IrFactDomain::SourceMappings => Some(document.source_mappings.len()),
            _ => None,
        };
        if let Some(count) = count {
            let count = u64::try_from(count).map_err(|_| FirstSliceError::Limits)?;
            coverage.discovered = count;
            coverage.indexed = count;
            coverage.id =
                derive_coverage_record_id(coverage).map_err(|_| FirstSliceError::Identity)?;
        }
    }
    Ok(document)
}

fn whole_source(
    repository: RepositoryId,
    generation: GenerationId,
    snapshot: &SourceSnapshot,
) -> Result<SourceRef, FirstSliceError> {
    let length = u64::try_from(snapshot.content().len()).map_err(|_| FirstSliceError::Limits)?;
    let span =
        SourceSpan::new(snapshot.file(), 0, length).map_err(|_| FirstSliceError::Identity)?;
    Ok(SourceRef::new(
        repository,
        generation,
        span,
        snapshot.content_hash(),
        None,
    ))
}
