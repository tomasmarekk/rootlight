//! Read-only native admission observations joined to exact discovery inputs.
//! Diagnostic generations identify these probes, not published service indexes;
//! accepted syntax is neither structural coverage nor semantic completeness.

use std::{collections::BTreeMap, error::Error, path::Path, time::Instant};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, EncodingId, GenerationBoundSnapshot, LanguageId, ParseProvider,
    ParseRequest, ReportError, RequestError, ResourceKind, SinkError,
};
use rootlight_adapter_treesitter::{
    ADAPTER_VERSION, FSharpAdmissionKind, FSharpAdmissionWitness, GrammarFamily, GrammarRegistry,
    RuntimeConfig, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_config::ConfigSnapshot;
use rootlight_discovery::{
    DISCOVERY_MANIFEST_VERSION, DiscoveryManifest, ManifestInput, fsharp_native_probe_allowed,
};
use rootlight_ids::{
    ContentHash, FileId, GenerationId, GenerationIdentity, content_hash, derive_generation,
};
use rootlight_ir::{SourceRef, SourceSpan};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use serde::Serialize;

#[path = "admission_profile.rs"]
mod profile;

#[derive(Debug, Serialize)]
struct Profile {
    schema: &'static str,
    adapter_version: &'static str,
    discovery_version: &'static str,
    grammar_version: &'static str,
    grammar_source_sha256: &'static str,
    parser_sha256: &'static str,
    scanner_sha256: Option<&'static str>,
    abi_version: usize,
    profile_source_hash: ContentHash,
    admission_source_hash: ContentHash,
    lexical_guard_source_hash: ContentHash,
    limits: BTreeMap<&'static str, usize>,
}

impl Profile {
    fn new(analysis: &AnalysisLimits, runtime: &RuntimeConfig) -> Result<Self, Box<dyn Error>> {
        let registry = GrammarRegistry::audited()?;
        let grammar = registry
            .get(GrammarFamily::FSharp)
            .ok_or("fsharp grammar unavailable")?;
        let stream = analysis.syntax_stream();
        let batch = stream.batch();
        let limits = BTreeMap::from([
            ("max_source_bytes", analysis.max_source_bytes()),
            ("max_syntax_nodes", analysis.max_syntax_nodes()),
            ("max_syntax_depth", analysis.max_syntax_depth()),
            ("max_embedded_ranges", analysis.max_embedded_ranges()),
            (
                "max_reported_memory_bytes",
                analysis.max_reported_memory_bytes(),
            ),
            ("stream_max_batches", stream.max_batches()),
            ("stream_max_records", stream.max_records()),
            ("stream_max_output_bytes", stream.max_output_bytes()),
            ("stream_max_diagnostics", stream.max_diagnostics()),
            ("stream_max_diagnostic_bytes", stream.max_diagnostic_bytes()),
            ("stream_max_string_bytes", stream.max_string_bytes()),
            ("batch_max_records", batch.max_records()),
            ("batch_max_output_bytes", batch.max_output_bytes()),
            ("batch_max_diagnostics", batch.max_diagnostics()),
            ("batch_max_diagnostic_bytes", batch.max_diagnostic_bytes()),
            ("runtime_max_source_bytes", runtime.max_source_bytes()),
            ("runtime_max_syntax_nodes", runtime.max_syntax_nodes()),
            ("runtime_max_syntax_depth", runtime.max_syntax_depth()),
            ("runtime_max_included_ranges", runtime.max_included_ranges()),
            (
                "runtime_max_incremental_edits",
                runtime.max_incremental_edits(),
            ),
            (
                "runtime_max_concurrent_parses",
                runtime.max_concurrent_parses(),
            ),
            ("runtime_max_cache_bytes", runtime.max_cache_bytes()),
            (
                "runtime_input_chunk_bytes",
                runtime.default_settings().input_chunk_bytes(),
            ),
        ]);
        Ok(Self {
            schema: "rootlight-source-inventory-admission-profile/1",
            adapter_version: ADAPTER_VERSION,
            discovery_version: DISCOVERY_MANIFEST_VERSION,
            grammar_version: grammar.grammar_version(),
            grammar_source_sha256: grammar.grammar_source_sha256(),
            parser_sha256: grammar.parser_sha256(),
            scanner_sha256: grammar.scanner_sha256(),
            abi_version: grammar.abi_version(),
            profile_source_hash: content_hash(include_bytes!("admission_profile.rs")),
            admission_source_hash: content_hash(include_bytes!(
                "../../../rootlight-adapter-treesitter/src/runtime/fsharp_admission.rs"
            )),
            lexical_guard_source_hash: content_hash(include_bytes!(
                "../../../rootlight-discovery/src/fsharp.rs"
            )),
            limits,
        })
    }
}

#[derive(Debug, Serialize)]
struct AcceptedEvidence {
    witness_kind: &'static str,
    declaration: SourceRef,
    sample: SourceRef,
}

#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum Outcome {
    Guarded {
        reason: &'static str,
    },
    Accepted {
        #[serde(flatten)]
        witness: Box<AcceptedEvidence>,
    },
    NoEvidence,
    ResourceBounded {
        resource: &'static str,
        observed: Option<usize>,
        limit: Option<usize>,
        code: Option<String>,
    },
    InvalidEncoding {
        code: String,
    },
}

impl Outcome {
    const fn category(&self) -> &'static str {
        match self {
            Self::Guarded { .. } => "guarded",
            Self::Accepted { .. } => "accepted",
            Self::NoEvidence => "no_evidence",
            Self::ResourceBounded { .. } => "resource_bounded",
            Self::InvalidEncoding { .. } => "invalid_encoding",
        }
    }
}

#[derive(Debug, Serialize)]
struct Row {
    manifest_ordinal: usize,
    path: String,
    file: FileId,
    content_hash: ContentHash,
    bytes: u64,
    encoding: String,
    #[serde(flatten)]
    outcome: Outcome,
}

/// Complete observations for all shared F# suffixes in one immutable manifest.
#[derive(Debug, Serialize)]
pub(super) struct AdmissionReport {
    schema: &'static str,
    identity_scope: &'static str,
    ordinal_scope: &'static str,
    generation: GenerationId,
    manifest_hash: ContentHash,
    config_hash: ContentHash,
    profile_hash: ContentHash,
    profile: Profile,
    rows: Vec<Row>,
    totals: BTreeMap<&'static str, usize>,
    elapsed_us: u128,
    full_semantic_completeness_proven: bool,
    installed_mcp_qualified: bool,
}

/// Observes bounded admission without changing the discovery manifest or repository.
///
/// # Errors
/// Returns snapshot drift, cancellation, or unexpected parser failures instead
/// of converting them into negative language evidence.
pub(super) fn observe(
    root: &RepositoryRoot,
    manifest: &DiscoveryManifest,
    config: &ConfigSnapshot,
    cancellation: &Cancellation,
) -> Result<AdmissionReport, Box<dyn Error>> {
    let started = Instant::now();
    let maximum = usize::try_from(config.analysis().max_source_file_bytes)?;
    let limits = profile::analysis_limits(maximum)?;
    let runtime = profile::parser_config(maximum)?;
    let profile = Profile::new(&limits, &runtime)?;
    let profile_hash = content_hash(&serde_json::to_vec(&profile)?);
    let manifest_hash = manifest.hash()?;
    let generation = derive_generation(GenerationIdentity {
        repository: root.repository(),
        parent: None,
        manifest_hash,
        config_hash: config.hash(),
        provider_set_hash: profile_hash,
        format_version: 1,
    })
    .id();
    let provider = TreeSitterProvider::new(runtime)?;
    let mut rows = Vec::new();
    rows.try_reserve_exact(
        manifest
            .inputs
            .iter()
            .filter(|input| shared_suffix(&input.path))
            .count(),
    )?;
    let mut totals = BTreeMap::from([
        ("guarded", 0usize),
        ("accepted", 0),
        ("no_evidence", 0),
        ("resource_bounded", 0),
        ("invalid_encoding", 0),
    ]);
    for (manifest_ordinal, input) in manifest.inputs.iter().enumerate() {
        cancellation.check()?;
        if !shared_suffix(&input.path) {
            continue;
        }
        let path = RelativePath::parse(Path::new(&input.path))?;
        let snapshot = root.snapshot_with_cancellation(
            &path,
            config.analysis().max_source_file_bytes,
            cancellation,
        )?;
        verify_snapshot(input, &snapshot)?;
        let outcome = observe_input(
            root,
            generation,
            input,
            &snapshot,
            &provider,
            &limits,
            cancellation,
        )?;
        let count = totals
            .get_mut(outcome.category())
            .ok_or("unknown admission outcome")?;
        *count = count.checked_add(1).ok_or("admission count overflow")?;
        rows.push(Row {
            manifest_ordinal,
            path: input.path.clone(),
            file: input.file,
            content_hash: input.content_hash,
            bytes: input.bytes,
            encoding: input.encoding.clone(),
            outcome,
        });
    }
    cancellation.check()?;
    Ok(AdmissionReport {
        schema: "rootlight-fsharp-admission-diagnostic/1",
        identity_scope: "diagnostic generation only; not a published service generation",
        ordinal_scope: "zero-based index in the complete discovery manifest inputs",
        generation,
        manifest_hash,
        config_hash: config.hash(),
        profile_hash,
        profile,
        rows,
        totals,
        elapsed_us: started.elapsed().as_micros(),
        full_semantic_completeness_proven: false,
        installed_mcp_qualified: false,
    })
}

fn shared_suffix(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("fs"))
}

fn verify_snapshot(input: &ManifestInput, snapshot: &SourceSnapshot) -> Result<(), Box<dyn Error>> {
    if snapshot.file() != input.file
        || snapshot.path().as_str() != input.path
        || snapshot.content_hash() != input.content_hash
        || snapshot.metadata().length != input.bytes
        || u64::try_from(snapshot.content().len())? != input.bytes
    {
        return Err("diagnostic snapshot differs from discovery input".into());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn observe_input(
    root: &RepositoryRoot,
    generation: GenerationId,
    input: &ManifestInput,
    snapshot: &SourceSnapshot,
    provider: &TreeSitterProvider,
    limits: &AnalysisLimits,
    cancellation: &Cancellation,
) -> Result<Outcome, Box<dyn Error>> {
    let reason = if !input.language_signals.is_empty() {
        Some("language_signals")
    } else if !input.project_language_witnesses.is_empty() {
        Some("project_language_witnesses")
    } else {
        None
    };
    if let Some(reason) = reason {
        return Ok(Outcome::Guarded { reason });
    }
    let encoding = EncodingId::new(&input.encoding)?;
    if !provider.capabilities().encodings().contains(&encoding) {
        return Ok(Outcome::Guarded {
            reason: "unsupported_encoding",
        });
    }
    if !fsharp_native_probe_allowed(snapshot.content(), cancellation)? {
        return Ok(Outcome::Guarded {
            reason: "eligibility_veto",
        });
    }
    let source = SourceRef::new(
        root.repository(),
        generation,
        SourceSpan::new(snapshot.file(), 0, snapshot.metadata().length)?,
        snapshot.content_hash(),
        None,
    );
    let request = ParseRequest::new(
        GenerationBoundSnapshot::new(snapshot, &source)?,
        LanguageId::new("fsharp")?,
        encoding,
        Vec::new(),
        limits,
    )?;
    classify_result(
        provider.probe_fsharp_admission(&request, cancellation),
        cancellation,
    )
}

fn classify_result(
    result: Result<Option<FSharpAdmissionWitness>, AdapterError>,
    cancellation: &Cancellation,
) -> Result<Outcome, Box<dyn Error>> {
    cancellation.check()?;
    match result {
        Ok(Some(witness)) => Ok(Outcome::Accepted {
            witness: Box::new(AcceptedEvidence {
                witness_kind: match witness.kind() {
                    FSharpAdmissionKind::NamedFunction => "named_function",
                    FSharpAdmissionKind::BoundLambda => "bound_lambda",
                    FSharpAdmissionKind::RecordType => "record_type",
                    FSharpAdmissionKind::UnionType => "union_type",
                    FSharpAdmissionKind::InterfaceType => "interface_type",
                    FSharpAdmissionKind::ObjectType => "object_type",
                },
                declaration: witness.declaration().clone(),
                sample: witness.sample().clone(),
            }),
        }),
        Ok(None) => Ok(Outcome::NoEvidence),
        Err(AdapterError::ProviderFailed { code })
            if matches!(
                code.as_str(),
                "invalid-utf8" | "treesitter-lowering-invalid-utf8"
            ) =>
        {
            Ok(Outcome::InvalidEncoding {
                code: code.as_str().to_owned(),
            })
        }
        Err(AdapterError::ProviderFailed { code })
            if matches!(
                code.as_str(),
                "syntax-node-parse-work-limit" | "syntax-depth-parse-work-limit"
            ) =>
        {
            Ok(Outcome::ResourceBounded {
                resource: if code.as_str() == "syntax-node-parse-work-limit" {
                    "syntax_nodes"
                } else {
                    "syntax_depth"
                },
                observed: None,
                limit: None,
                code: Some(code.as_str().to_owned()),
            })
        }
        Err(AdapterError::RejectedRequest(RequestError::SourceTooLarge { observed, limit })) => {
            resource_outcome(ResourceKind::SourceBytes, observed, limit)
        }
        Err(AdapterError::RejectedRequest(RequestError::TooManyIncludedRanges {
            observed,
            limit,
        })) => resource_outcome(ResourceKind::IncludedRanges, observed, limit),
        Err(AdapterError::RejectedRequest(
            RequestError::ProviderLimit {
                resource,
                observed,
                limit,
            }
            | RequestError::ProjectLimit {
                resource,
                observed,
                limit,
            },
        ))
        | Err(AdapterError::Sink(
            SinkError::BatchLimit {
                resource,
                observed,
                limit,
            }
            | SinkError::StreamLimit {
                resource,
                observed,
                limit,
            },
        ))
        | Err(AdapterError::InvalidReport(ReportError::ResourceLimit {
            resource,
            observed,
            limit,
        })) => resource_outcome(resource, observed, limit),
        Err(error) => Err(error.into()),
    }
}

fn resource_outcome(
    resource: ResourceKind,
    observed: usize,
    limit: usize,
) -> Result<Outcome, Box<dyn Error>> {
    let resource = match resource {
        ResourceKind::Batches => "batches",
        ResourceKind::Records => "records",
        ResourceKind::RequiredSyntaxFacts => "required_syntax_facts",
        ResourceKind::OutputBytes => "output_bytes",
        ResourceKind::Diagnostics => "diagnostics",
        ResourceKind::DiagnosticBytes => "diagnostic_bytes",
        ResourceKind::StringBytes => "string_bytes",
        ResourceKind::NestedItems => "nested_items",
        ResourceKind::ExtensionBytes => "extension_bytes",
        ResourceKind::SourceBytes => "source_bytes",
        ResourceKind::ProjectFiles => "project_files",
        ResourceKind::ProjectSourceBytes => "project_source_bytes",
        ResourceKind::ProjectContextBytes => "project_context_bytes",
        ResourceKind::GeneratedMappings => "generated_mappings",
        ResourceKind::GeneratedMappingBytes => "generated_mapping_bytes",
        ResourceKind::AnalysisUnitBytes => "analysis_unit_bytes",
        ResourceKind::BuildTargetBytes => "build_target_bytes",
        ResourceKind::IncludedRanges => "included_ranges",
        ResourceKind::SyntaxNodes => "syntax_nodes",
        ResourceKind::SyntaxDepth => "syntax_depth",
        ResourceKind::ReportedMemoryBytes => "reported_memory_bytes",
        _ => return Err("unmapped admission resource error".into()),
    };
    Ok(Outcome::ResourceBounded {
        resource,
        observed: Some(observed),
        limit: Some(limit),
        code: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::DiagnosticCode;
    use rootlight_cancel::CancellationReason;

    #[test]
    fn resource_and_encoding_failures_are_not_negative_syntax_evidence() {
        let cancellation = Cancellation::new();
        assert!(matches!(
            classify_result(Ok(None), &cancellation).unwrap(),
            Outcome::NoEvidence
        ));
        for code in [
            "syntax-node-parse-work-limit",
            "syntax-depth-parse-work-limit",
        ] {
            let error = AdapterError::ProviderFailed {
                code: DiagnosticCode::new(code).unwrap(),
            };
            assert!(matches!(
                classify_result(Err(error), &cancellation).unwrap(),
                Outcome::ResourceBounded { observed: None, .. }
            ));
        }
        let error = AdapterError::RejectedRequest(RequestError::ProviderLimit {
            resource: ResourceKind::ReportedMemoryBytes,
            observed: 2,
            limit: 1,
        });
        assert!(matches!(
            classify_result(Err(error), &cancellation).unwrap(),
            Outcome::ResourceBounded {
                resource: "reported_memory_bytes",
                observed: Some(2),
                limit: Some(1),
                ..
            }
        ));
        let error = AdapterError::ProviderFailed {
            code: DiagnosticCode::new("invalid-utf8").unwrap(),
        };
        assert!(matches!(
            classify_result(Err(error), &cancellation).unwrap(),
            Outcome::InvalidEncoding { .. }
        ));
        let error = AdapterError::ProviderFailed {
            code: DiagnosticCode::new("unexpected-native-failure").unwrap(),
        };
        assert!(classify_result(Err(error), &cancellation).is_err());
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(classify_result(Ok(None), &cancellation).is_err());
    }
}
