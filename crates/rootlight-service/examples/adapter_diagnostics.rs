//! Read-only, metadata-only SDK diagnostics over an authenticated saved inventory.
//! Full service-preflight limits isolate adapter errors; repository-partitioned
//! lowering, extracted sources, publication, and semantic completeness are not tested.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisRequest, EncodingId, GenerationBoundSnapshot, LanguageId,
    MemoryAdmissionPolicy, ParseProvider, ParseRequest, ReportError, RequestError, SinkError,
};
use rootlight_adapter_treesitter::{
    GrammarDescriptor, GrammarFamily, GrammarRegistry, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_adapters::{
    AgdaParseProvider, AwkParseProvider, ElmParseProvider, ErlangParseProvider, IniParseProvider,
    MakeParseProvider, PrologParseProvider, ScssParseProvider, TclParseProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_config::ConfigSnapshot;
use rootlight_discovery::{
    DISCOVERY_MANIFEST_VERSION, DiscoveryManifest, InputClass, LanguageEvidence, ManifestInput,
    fsharp_native_probe_allowed,
};
use rootlight_ids::{ContentHash, GenerationIdentity, content_hash, derive_generation};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ExtensionSupport, ProducerIdentity, SourceRef, SourceSpan,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[path = "source_inventory/admission_profile.rs"]
mod profile;

const SCHEMA: &str = "rootlight-adapter-diagnostics/1";
const MAX_INVENTORY_BYTES: u64 = 256 * 1024 * 1024;
const ANALYZER_BINARY_SEED: &[u8] = b"rootlight.first-slice.treesitter-structural/229";
const USAGE: &str =
    "usage: adapter_diagnostics REPOSITORY SAVED_INVENTORY_JSON [--max-fatal-errors COUNT]";

#[derive(Deserialize)]
struct SavedInventory {
    schema: String,
    explicit_configuration: bool,
    manifest_hash: ContentHash,
    manifest: DiscoveryManifest,
}

trait DiagnosticParser: ParseProvider {
    fn required_count(
        &self,
        request: &ParseRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<usize, AdapterError>;
}

macro_rules! diagnostic_parser {
    ($($provider:ty),+ $(,)?) => {
        $(impl DiagnosticParser for $provider {
            fn required_count(
                &self,
                request: &ParseRequest<'_>,
                cancellation: &Cancellation,
            ) -> Result<usize, AdapterError> {
                self.required_syntax_fact_count(request, cancellation)
            }
        })+
    };
}

diagnostic_parser!(
    TreeSitterProvider,
    AgdaParseProvider,
    AwkParseProvider,
    ElmParseProvider,
    ErlangParseProvider,
    IniParseProvider,
    MakeParseProvider,
    PrologParseProvider,
    ScssParseProvider,
    TclParseProvider,
);

struct NativeAnalyzer {
    analyzer: TreeSitterAnalyzer,
    parser: Arc<dyn DiagnosticParser>,
}

struct NativeAnalyzers {
    by_language: BTreeMap<String, NativeAnalyzer>,
    tree_parser: Arc<TreeSitterProvider>,
}

#[derive(Default, Serialize)]
struct Counts {
    physical_inputs: usize,
    selected_inputs: usize,
    passed_inputs: usize,
    known_source_fallbacks: usize,
    known_admission_fallbacks: usize,
    failed_inputs: usize,
    skipped_inputs: usize,
}

fn increment(value: &mut usize) -> Result<(), Box<dyn Error>> {
    *value = value.checked_add(1).ok_or("diagnostic count overflow")?;
    Ok(())
}

fn emit(output: &mut impl Write, value: Value) -> Result<(), Box<dyn Error>> {
    serde_json::to_writer(&mut *output, &value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn deadline() -> Result<Cancellation, Box<dyn Error>> {
    let end = Instant::now()
        .checked_add(Duration::from_secs(3600))
        .ok_or("diagnostic deadline is not representable")?;
    Ok(Cancellation::with_deadline(end))
}

fn grammar_digest(descriptor: &GrammarDescriptor) -> Result<ContentHash, Box<dyn Error>> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rootlight.first-slice.static-components/1\0");
    for component in [
        ANALYZER_BINARY_SEED,
        descriptor.language().as_str().as_bytes(),
        descriptor.grammar_version().as_bytes(),
        descriptor.grammar_source_sha256().as_bytes(),
        descriptor.parser_sha256().as_bytes(),
        descriptor
            .scanner_sha256()
            .unwrap_or("no-scanner")
            .as_bytes(),
    ] {
        hasher.update(&u64::try_from(component.len())?.to_be_bytes());
        hasher.update(component);
    }
    Ok(ContentHash::from_bytes(*hasher.finalize().as_bytes()))
}

fn frontend(descriptor: &GrammarDescriptor) -> String {
    let name = match descriptor.family() {
        GrammarFamily::JavaScript => "typescript-tsx",
        GrammarFamily::Sql => "sequel",
        GrammarFamily::Vba | GrammarFamily::VisualBasicClassic => "vba",
        _ => descriptor.language().as_str(),
    };
    format!("tree-sitter-{name}-{}", descriptor.grammar_version())
}

fn analyzers(
    config: &ConfigSnapshot,
    limits: &AnalysisLimits,
) -> Result<NativeAnalyzers, Box<dyn Error>> {
    let parser = Arc::new(TreeSitterProvider::new(profile::parser_config(
        limits.max_source_bytes(),
    )?)?);
    let producer = ProducerIdentity::new("rootlight-first-slice-treesitter", "1.0", config.hash())?;
    let mut result = BTreeMap::new();
    for descriptor in GrammarRegistry::audited()?.descriptors() {
        let provider: Arc<dyn ParseProvider> = parser.clone();
        let language = descriptor.language().clone();
        let analyzer = if language.as_str() == "rust" {
            TreeSitterAnalyzer::new_rust_structural(
                provider,
                producer.clone(),
                language,
                &frontend(descriptor),
                grammar_digest(descriptor)?,
            )?
        } else {
            TreeSitterAnalyzer::new(
                provider,
                producer.clone(),
                language,
                &frontend(descriptor),
                grammar_digest(descriptor)?,
            )?
        };
        if result
            .insert(
                descriptor.language().as_str().to_owned(),
                NativeAnalyzer {
                    analyzer,
                    parser: parser.clone(),
                },
            )
            .is_some()
        {
            return Err("native analyzer language repeats".into());
        }
    }
    macro_rules! register_native {
        ($provider:ty, $language:literal) => {{
            let parser = Arc::new(<$provider>::new(
                limits.max_source_bytes(),
                limits.max_syntax_nodes(),
            )?);
            let provider: Arc<dyn ParseProvider> = parser.clone();
            let analyzer = TreeSitterAnalyzer::new(
                provider,
                ProducerIdentity::new(
                    concat!("rootlight-first-slice-", $language, "-native"),
                    "1.0",
                    config.hash(),
                )?,
                LanguageId::new($language)?,
                concat!($language, "-native-structural-1"),
                <$provider>::binary_digest(),
            )?;
            if result
                .insert($language.to_owned(), NativeAnalyzer { analyzer, parser })
                .is_some()
            {
                return Err("native analyzer language repeats".into());
            }
        }};
    }
    register_native!(AgdaParseProvider, "agda");
    register_native!(AwkParseProvider, "awk");
    register_native!(ElmParseProvider, "elm");
    register_native!(ErlangParseProvider, "erlang");
    register_native!(IniParseProvider, "ini");
    register_native!(MakeParseProvider, "make");
    register_native!(PrologParseProvider, "prolog");
    register_native!(ScssParseProvider, "scss");
    register_native!(TclParseProvider, "tcl");
    Ok(NativeAnalyzers {
        by_language: result,
        tree_parser: parser,
    })
}

fn saved_language(input: &ManifestInput) -> Option<&str> {
    // Manifest/toolchain names do not classify the source syntax of that file.
    for evidence in [
        LanguageEvidence::Configuration,
        LanguageEvidence::Filename,
        LanguageEvidence::Content,
        LanguageEvidence::Context,
        LanguageEvidence::Project,
        LanguageEvidence::Extension,
        LanguageEvidence::Shebang,
    ] {
        let mut signals = input
            .language_signals
            .iter()
            .filter(|signal| signal.evidence == evidence);
        if let Some(first) = signals.next() {
            return (!signals.any(|signal| signal.language != first.language))
                .then_some(first.language.as_str());
        }
    }
    None
}

fn invalid_utf8(error: &AdapterError) -> bool {
    matches!(error, AdapterError::ProviderFailed { code }
        if matches!(code.as_str(), "invalid-utf8" | "treesitter-lowering-invalid-utf8"))
}

fn error_row(phase: &str, error: &AdapterError) -> Value {
    json!({"phase":phase, "outcome":if invalid_utf8(error) {"known_source_fallback"} else {"adapter_failure"},
        "error_debug":format!("{error:?}"), "error_display":error.to_string()})
}

fn bounded_admission(error: &AdapterError) -> bool {
    matches!(error,
        AdapterError::ProviderFailed { code }
            if matches!(code.as_str(), "syntax-node-parse-work-limit" | "syntax-depth-parse-work-limit"))
        || matches!(
            error,
            AdapterError::RejectedRequest(RequestError::ProviderLimit { .. })
                | AdapterError::Sink(SinkError::BatchLimit { .. } | SinkError::StreamLimit { .. })
                | AdapterError::InvalidReport(ReportError::ResourceLimit { .. })
        )
}

fn diagnose(
    native: &NativeAnalyzer,
    request: &AnalysisRequest<'_>,
    cancellation: &Cancellation,
    output: &mut impl Write,
    ordinal: usize,
) -> Result<Value, Box<dyn Error>> {
    let artifact = match native.analyzer.prepare_artifact(request, cancellation) {
        Ok(artifact) => artifact,
        Err(error) => return Ok(error_row("prepare_artifact", &error)),
    };
    emit(
        output,
        json!({"schema":SCHEMA, "kind":"stage", "ordinal":ordinal,
        "phase":"prepare_artifact", "outcome":"passed", "retained_syntax_facts":artifact.syntax_fact_count(),
        "syntax_partition_complete":artifact.syntax_partition_complete()}),
    )?;
    let required = match native
        .parser
        .required_count(&request.to_parse_request(), cancellation)
    {
        Ok(required) => required,
        Err(error) => return Ok(error_row("required_syntax_fact_count", &error)),
    };
    emit(
        output,
        json!({"schema":SCHEMA, "kind":"stage", "ordinal":ordinal,
        "phase":"required_syntax_fact_count", "outcome":"passed", "required_syntax_facts":required}),
    )?;
    Ok(
        match native.analyzer.analyze_prepared_artifact(
            request,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            cancellation,
        ) {
            Ok(output) => json!({"phase":"analyze_prepared_artifact", "outcome":"passed",
            "required_syntax_facts":required, "retained_syntax_facts":artifact.syntax_fact_count(),
            "syntax_partition_complete":artifact.syntax_partition_complete(),
            "normalized_entities":output.document().entities.len(),
            "retained_gaps":output.document().skipped_regions.len()}),
            Err(error) => error_row("analyze_prepared_artifact", &error),
        },
    )
}

fn arguments() -> Result<(PathBuf, PathBuf, usize), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let repository = args.next().ok_or(USAGE)?;
    let inventory = args.next().ok_or(USAGE)?;
    let maximum_fatal_errors = if let Some(option) = args.next() {
        if option != "--max-fatal-errors" {
            return Err(USAGE.into());
        }
        let value = args.next().ok_or(USAGE)?;
        let count: usize = value.to_str().ok_or(USAGE)?.parse()?;
        if count == 0 {
            return Err("maximum fatal errors must be positive".into());
        }
        count
    } else {
        1
    };
    if args.next().is_some() {
        return Err(USAGE.into());
    }
    Ok((
        PathBuf::from(repository),
        PathBuf::from(inventory),
        maximum_fatal_errors,
    ))
}

fn main() -> Result<(), Box<dyn Error>> {
    let (repository, saved_path, maximum_fatal_errors) = arguments()?;
    let mut bytes = Vec::new();
    File::open(saved_path)?
        .take(MAX_INVENTORY_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len())? > MAX_INVENTORY_BYTES {
        return Err("saved inventory exceeds diagnostic read bound".into());
    }
    let saved: SavedInventory = serde_json::from_slice(&bytes)?;
    let config = ConfigSnapshot::resolve(&[])?;
    if saved.schema != "rootlight-source-inventory-diagnostic/1"
        || saved.explicit_configuration
        || saved.manifest.version != DISCOVERY_MANIFEST_VERSION
        || saved.manifest.configuration_hash != config.hash()
        || saved.manifest.hash()? != saved.manifest_hash
        || saved.manifest.coverage.included != u64::try_from(saved.manifest.inputs.len())?
    {
        return Err("saved inventory schema, configuration, or manifest identity differs".into());
    }
    let maximum = usize::try_from(config.analysis().max_source_file_bytes)?;
    let limits = profile::analysis_limits(maximum)?;
    let profile_hash = content_hash(include_bytes!("source_inventory/admission_profile.rs"));
    let generation = derive_generation(GenerationIdentity {
        repository: saved.manifest.repository,
        parent: None,
        manifest_hash: saved.manifest_hash,
        config_hash: config.hash(),
        provider_set_hash: profile_hash,
        format_version: 1,
    })
    .id();
    let root = RepositoryRoot::open(saved.manifest.repository, &repository)?;
    let NativeAnalyzers {
        by_language: analyzers,
        tree_parser,
    } = analyzers(&config, &limits)?;
    let mut output = io::BufWriter::new(io::stdout().lock());
    emit(
        &mut output,
        json!({"schema":SCHEMA, "kind":"scope", "saved_manifest_hash":saved.manifest_hash,
        "saved_inventory_bytes_hash":content_hash(&bytes), "profile_source_hash":profile_hash,
        "analyzer_binary_seed":std::str::from_utf8(ANALYZER_BINARY_SEED)?,
        "configuration_hash":config.hash(), "repository":root.repository(), "diagnostic_generation":generation,
        "input_scope":"saved_physical_inventory_only", "routing_scope":"saved_source_language_evidence_and_guarded_native_fsharp_admission",
        "limit_scope":"full_service_preflight_limits_not_repository_lowering_partitions",
        "extracted_sources_tested":false, "verified_vba_host_context_tested":false,
        "maximum_fatal_errors":maximum_fatal_errors,
        "full_semantic_completeness_proven":false, "installed_mcp_qualified":false,
        "read_only":true, "source_text_emitted":false, "analyzer_languages":analyzers.keys().collect::<Vec<_>>()}),
    )?;
    let mut counts = Counts::default();
    let mut seen = BTreeSet::new();
    for (ordinal, input) in saved.manifest.inputs.iter().enumerate() {
        increment(&mut counts.physical_inputs)?;
        if !seen.insert(input.path.as_str()) {
            return Err("saved inventory repeats a physical path".into());
        }
        let cancellation = deadline()?;
        let generated = matches!(input.class, InputClass::Generated);
        let mut row = json!({"schema":SCHEMA, "kind":"input", "ordinal":ordinal, "path":input.path,
            "file":input.file, "bytes":input.bytes, "content_hash":input.content_hash,
            "encoding":input.encoding, "generated":generated});
        emit(
            &mut output,
            json!({"schema":SCHEMA, "kind":"input_started", "ordinal":ordinal, "path":input.path}),
        )?;
        let path = RelativePath::parse(Path::new(&input.path))?;
        if path.as_str() != input.path {
            return Err("saved source path is not canonical".into());
        }
        let snapshot = root.snapshot_with_cancellation(
            &path,
            config.analysis().max_source_file_bytes,
            &cancellation,
        )?;
        if snapshot.file() != input.file
            || snapshot.content_hash() != input.content_hash
            || snapshot.metadata().length != input.bytes
        {
            return Err("live source differs from authenticated saved inventory".into());
        }
        let source = SourceRef::new(
            root.repository(),
            generation,
            SourceSpan::new(input.file, 0, input.bytes)?,
            input.content_hash,
            None,
        );
        let mut language = saved_language(input).map(str::to_owned);
        if language.is_none()
            && input.language_signals.is_empty()
            && input.project_language_witnesses.is_empty()
            && Path::new(&input.path)
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("fs"))
            && tree_parser
                .capabilities()
                .encodings()
                .contains(&EncodingId::new(&input.encoding)?)
            && fsharp_native_probe_allowed(snapshot.content(), &cancellation)?
        {
            let request = AnalysisRequest::new_with_parse_context(
                GenerationBoundSnapshot::new(&snapshot, &source)?,
                LanguageId::new("fsharp")?,
                EncodingId::new(&input.encoding)?,
                Vec::new(),
                AnalysisTier::TierD,
                BuildContextIdentity::new(content_hash(b"rootlight.first-slice.build-context/1")),
                &limits,
            )?
            .with_generated_status(generated);
            match tree_parser.probe_fsharp_admission(&request.to_parse_request(), &cancellation) {
                Ok(Some(_)) => language = Some("fsharp".to_owned()),
                Ok(None) => {}
                Err(error) => {
                    row["result"] = error_row("native_fsharp_admission", &error);
                    if invalid_utf8(&error) {
                        increment(&mut counts.known_source_fallbacks)?;
                    } else if bounded_admission(&error) {
                        row["result"]["outcome"] = json!("known_admission_fallback");
                        increment(&mut counts.known_admission_fallbacks)?;
                    } else {
                        increment(&mut counts.failed_inputs)?;
                    }
                    increment(&mut counts.selected_inputs)?;
                    emit(&mut output, row)?;
                    if counts.failed_inputs >= maximum_fatal_errors {
                        break;
                    }
                    continue;
                }
            }
        }
        let Some(language) = language else {
            row["result"] =
                json!({"outcome":"skipped", "reason":"no_unambiguous_saved_native_language"});
            increment(&mut counts.skipped_inputs)?;
            emit(&mut output, row)?;
            continue;
        };
        row["language"] = json!(language);
        let Some(native) = analyzers.get(&language) else {
            row["result"] = json!({"outcome":"skipped", "reason":"unsupported_native_language"});
            increment(&mut counts.skipped_inputs)?;
            emit(&mut output, row)?;
            continue;
        };
        let encoding = EncodingId::new(&input.encoding)?;
        if !native.analyzer.supports_encoding(&encoding) {
            row["result"] = json!({"outcome":"skipped", "reason":"unsupported_native_encoding"});
            increment(&mut counts.skipped_inputs)?;
            emit(&mut output, row)?;
            continue;
        }
        increment(&mut counts.selected_inputs)?;
        row["source_ref"] = serde_json::to_value(&source)?;
        let tier = if language == "rust" {
            AnalysisTier::TierB
        } else {
            AnalysisTier::TierD
        };
        row["result"] = match AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&snapshot, &source)?,
            LanguageId::new(&language)?,
            encoding,
            Vec::new(),
            tier,
            BuildContextIdentity::new(content_hash(b"rootlight.first-slice.build-context/1")),
            &limits,
        ) {
            Ok(request) => diagnose(
                native,
                &request.with_generated_status(generated),
                &cancellation,
                &mut output,
                ordinal,
            )?,
            Err(error) => error_row("analysis_request", &AdapterError::RejectedRequest(error)),
        };
        match row["result"]["outcome"].as_str() {
            Some("passed") => increment(&mut counts.passed_inputs)?,
            Some("known_source_fallback") => increment(&mut counts.known_source_fallbacks)?,
            _ => increment(&mut counts.failed_inputs)?,
        }
        emit(&mut output, row)?;
        if counts.failed_inputs >= maximum_fatal_errors {
            break;
        }
    }
    emit(
        &mut output,
        json!({"schema":SCHEMA, "kind":"summary", "counts":counts,
        "incomplete_scan":counts.physical_inputs != saved.manifest.inputs.len(),
        "saved_inventory_physical_inputs":saved.manifest.inputs.len(),
        "full_semantic_completeness_proven":false, "installed_mcp_qualified":false,
        "limit_scope":"full_service_preflight_limits_not_repository_lowering_partitions"}),
    )?;
    if counts.failed_inputs > 0 {
        return Err("adapter diagnostics retained one or more typed failures".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::DiagnosticCode;

    #[test]
    fn errors_retain_the_original_variant_and_phase() {
        let error = AdapterError::RejectedRequest(RequestError::DeadlineRequired);
        let row = error_row("prepare_artifact", &error);
        assert_eq!(row["phase"], "prepare_artifact");
        assert_eq!(row["outcome"], "adapter_failure");
        assert_eq!(row["error_debug"], "RejectedRequest(DeadlineRequired)");
    }

    #[test]
    fn invalid_utf8_is_known_source_fallback_not_a_pass() {
        let error = AdapterError::ProviderFailed {
            code: DiagnosticCode::new("treesitter-lowering-invalid-utf8").unwrap(),
        };
        assert_eq!(
            error_row("analyze_prepared_artifact", &error)["outcome"],
            "known_source_fallback"
        );
        assert!(!bounded_admission(&error));
    }

    #[test]
    fn bounded_admission_is_separate_from_lowering_failure() {
        let error = AdapterError::ProviderFailed {
            code: DiagnosticCode::new("syntax-depth-parse-work-limit").unwrap(),
        };
        assert!(bounded_admission(&error));
        assert_eq!(
            error_row("analyze_prepared_artifact", &error)["outcome"],
            "adapter_failure"
        );
    }

    #[test]
    fn counters_reject_overflow() {
        let mut count = usize::MAX;
        assert!(increment(&mut count).is_err());
        assert_eq!(count, usize::MAX);
    }

    #[test]
    fn analyzer_binary_seed_matches_the_service_recipe() {
        let seed = std::str::from_utf8(ANALYZER_BINARY_SEED).unwrap();
        let declaration = format!("const ANALYZER_BINARY_SEED: &[u8] = b\"{seed}\";");
        assert!(include_str!("../src/lib.rs").contains(&declaration));
    }
}
