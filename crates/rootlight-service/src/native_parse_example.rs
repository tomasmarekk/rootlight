//! Reads one UTF-8 snapshot and exposes committed syntax, with optional diagnostic lowering.
//! CLI: native_parse REPOSITORY_DIRECTORY REPO_ID GEN_ID LANGUAGE RELATIVE_PATH [--analyze].
//! Caller-supplied identities are diagnostic source coordinates, not an indexed generation;
//! the complete report is buffered under a diagnostic output bound before stdout is written.
//! Optional lowering assumes authored input; it does not classify project-generated sources.

use std::{
    error::Error,
    ffi::OsString,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AnalysisRequest, EncodingId, GenerationBoundSnapshot, LanguageId,
    MemoryAdmissionPolicy, ParseProvider, ParseReport, ParseRequest, StreamUsage, SyntaxFact,
    execute_analysis, execute_parse,
};
use rootlight_adapter_treesitter::{TreeSitterAnalyzer, TreeSitterProvider};
use rootlight_cancel::Cancellation;
use rootlight_config::ConfigSnapshot;
use rootlight_ids::{ContentHash, GenerationId, RepositoryId, content_hash};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ExtensionSupport, NormalizedIrDocument, ProducerIdentity,
    SourceRef, SourceSpan,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use serde::{Serialize, Serializer, ser::SerializeSeq};
use serde_json::{Value, json};

#[path = "source_inventory_example/admission_profile.rs"]
mod profile;

type DiagnosticResult<T> = Result<T, Box<dyn Error>>;

const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const DIAGNOSTIC_DEADLINE_SECONDS: u64 = 3_600;

struct Arguments {
    directory: PathBuf,
    repository: RepositoryId,
    generation: GenerationId,
    language: LanguageId,
    path: RelativePath,
    analyze: bool,
}

impl Arguments {
    fn parse(mut args: impl Iterator<Item = OsString>) -> DiagnosticResult<Self> {
        let directory = PathBuf::from(args.next().ok_or("missing repository directory")?);
        let repository = args
            .next()
            .ok_or("missing repository id")?
            .to_str()
            .ok_or("repository id is not utf-8")?
            .parse()?;
        let generation = args
            .next()
            .ok_or("missing generation id")?
            .to_str()
            .ok_or("generation id is not utf-8")?
            .parse()?;
        let language = args.next().ok_or("missing language")?;
        let language = LanguageId::new(language.to_str().ok_or("language is not utf-8")?)?;
        let path = args.next().ok_or("missing relative path")?;
        let path = RelativePath::parse(Path::new(&path))?;
        let analyze = match args.next() {
            None => false,
            Some(flag) if flag == "--analyze" => true,
            Some(_) => return Err("unexpected argument".into()),
        };
        if args.next().is_some() {
            return Err("unexpected argument".into());
        }
        Ok(Self {
            directory,
            repository,
            generation,
            language,
            path,
            analyze,
        })
    }
}

#[derive(Serialize)]
struct FactView<'a> {
    local_id: u64,
    parent: Option<u64>,
    kind: String,
    span: SourceSpan,
    depth: usize,
    syntax_kind: &'a str,
}

struct Facts<'a> {
    facts: &'a [SyntaxFact],
    cancellation: &'a Cancellation,
}

impl Serialize for Facts<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.facts.len()))?;
        for fact in self.facts {
            self.cancellation
                .check()
                .map_err(serde::ser::Error::custom)?;
            sequence.serialize_element(&FactView {
                local_id: fact.local_id(),
                parent: fact.parent(),
                kind: format!("{:?}", fact.kind()),
                span: fact.span(),
                depth: fact.depth(),
                syntax_kind: fact.syntax_kind().as_str(),
            })?;
        }
        sequence.end()
    }
}

#[derive(Serialize)]
struct DiagnosticView<'a> {
    code: &'a str,
    severity: rootlight_ir::DiagnosticSeverity,
    source: Option<&'a SourceRef>,
    coverage_effect: rootlight_ir::CoverageStatus,
}

struct Diagnostics<'a> {
    diagnostics: &'a [AdapterDiagnostic],
    cancellation: &'a Cancellation,
}

impl Serialize for Diagnostics<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.diagnostics.len()))?;
        for diagnostic in self.diagnostics {
            self.cancellation
                .check()
                .map_err(serde::ser::Error::custom)?;
            sequence.serialize_element(&DiagnosticView {
                code: diagnostic.code().as_str(),
                severity: diagnostic.severity(),
                source: diagnostic.source(),
                coverage_effect: diagnostic.coverage_effect(),
            })?;
        }
        sequence.end()
    }
}

#[derive(Serialize)]
struct NativeReport<'a> {
    schema: &'static str,
    relative_path: &'a str,
    language: &'a str,
    encoding: &'static str,
    source: &'a SourceRef,
    configuration_hash: ContentHash,
    profile_source_hash: ContentHash,
    profile: Value,
    coverage: Value,
    resources: Value,
    end: Value,
    memory_admission: String,
    canonical_fact_count: usize,
    diagnostic_count: usize,
    facts: Facts<'a>,
    diagnostics: Diagnostics<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    analysis: Option<AnalysisView<'a>>,
    qualification: Value,
}

#[derive(Serialize)]
struct AnalysisView<'a> {
    identity: Value,
    document: &'a NormalizedIrDocument,
    coverage: Value,
    resources: Value,
    end: Value,
    memory_admission: String,
}

fn stream_usage(usage: StreamUsage) -> Value {
    json!({
        "batches": usage.batches(),
        "records": usage.records(),
        "output_bytes": usage.output_bytes(),
        "diagnostics": usage.diagnostics(),
        "diagnostic_bytes": usage.diagnostic_bytes(),
        "string_bytes": usage.string_bytes(),
    })
}

fn report_values(report: &ParseReport) -> (Value, Value, Value) {
    let coverage = report.coverage();
    let domains: Vec<_> = coverage
        .domains()
        .iter()
        .map(|domain| {
            json!({
                "domain": domain.domain(),
                "status": domain.status(),
                "discovered": domain.discovered(),
                "indexed": domain.indexed(),
                "skipped": domain.skipped(),
            })
        })
        .collect();
    let coverage = json!({
        "tier": coverage.tier(),
        "status": coverage.status(),
        "total_source_bytes": coverage.total_source_bytes(),
        "covered_source_bytes": coverage.covered_source_bytes(),
        "skipped_regions": coverage.skipped_regions(),
        "domains": domains,
    });
    let resources = report.resources();
    let resources = json!({
        "source_bytes": resources.source_bytes(),
        "output_records": resources.output_records(),
        "syntax_nodes": resources.syntax_nodes(),
        "max_syntax_depth": resources.max_syntax_depth(),
        "reported_memory_bytes": resources.reported_memory_bytes(),
        "stream": stream_usage(resources.stream()),
    });
    let end = report.end();
    (
        coverage,
        resources,
        json!({
            "next_sequence": end.next_sequence(),
            "usage": stream_usage(end.usage()),
        }),
    )
}

struct OutputWriter<'a> {
    bytes: Vec<u8>,
    maximum: usize,
    cancellation: &'a Cancellation,
}

impl Write for OutputWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.cancellation.check().map_err(io::Error::other)?;
        let length = self
            .bytes
            .len()
            .checked_add(buffer.len())
            .ok_or_else(|| io::Error::other("diagnostic output length overflow"))?;
        if length > self.maximum {
            return Err(io::Error::other("diagnostic output bound exceeded"));
        }
        self.bytes
            .try_reserve(buffer.len())
            .map_err(io::Error::other)?;
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn buffer_report(
    report: &impl Serialize,
    maximum: usize,
    cancellation: &Cancellation,
) -> DiagnosticResult<Vec<u8>> {
    let mut writer = OutputWriter {
        bytes: Vec::new(),
        maximum,
        cancellation,
    };
    serde_json::to_writer(&mut writer, report)?;
    writer.write_all(b"\n")?;
    cancellation.check()?;
    Ok(writer.bytes)
}

fn main() -> DiagnosticResult<()> {
    let args = Arguments::parse(std::env::args_os().skip(1))?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(DIAGNOSTIC_DEADLINE_SECONDS))
        .ok_or("diagnostic deadline overflow")?;
    let cancellation = Cancellation::with_deadline(deadline);
    let config = ConfigSnapshot::resolve(&[])?;
    let source_capacity = config.analysis().max_source_file_bytes;
    let maximum_source_bytes = usize::try_from(source_capacity)?;
    let analysis = profile::analysis_limits(maximum_source_bytes)?;
    let runtime = profile::parser_config(maximum_source_bytes)?;
    let root = RepositoryRoot::open(args.repository, &args.directory)?;
    let snapshot = root.snapshot_with_cancellation(&args.path, source_capacity, &cancellation)?;
    std::str::from_utf8(snapshot.content())?;
    let source = SourceRef::new(
        args.repository,
        args.generation,
        SourceSpan::new(snapshot.file(), 0, snapshot.metadata().length)?,
        snapshot.content_hash(),
        None,
    );
    let request = ParseRequest::new(
        GenerationBoundSnapshot::new(&snapshot, &source)?,
        args.language.clone(),
        EncodingId::utf8(),
        Vec::new(),
        &analysis,
    )?;
    let stream = analysis.syntax_stream();
    let batch = stream.batch();
    let profile = json!({
        "max_source_bytes": analysis.max_source_bytes(),
        "max_syntax_nodes": analysis.max_syntax_nodes(),
        "max_syntax_depth": analysis.max_syntax_depth(),
        "max_embedded_ranges": analysis.max_embedded_ranges(),
        "max_reported_memory_bytes": analysis.max_reported_memory_bytes(),
        "syntax_stream": {
            "max_batches": stream.max_batches(),
            "max_records": stream.max_records(),
            "max_output_bytes": stream.max_output_bytes(),
            "max_diagnostics": stream.max_diagnostics(),
            "max_diagnostic_bytes": stream.max_diagnostic_bytes(),
            "max_string_bytes": stream.max_string_bytes(),
            "batch": {
                "max_records": batch.max_records(),
                "max_output_bytes": batch.max_output_bytes(),
                "max_diagnostics": batch.max_diagnostics(),
                "max_diagnostic_bytes": batch.max_diagnostic_bytes(),
            },
        },
        "runtime": {
            "max_source_bytes": runtime.max_source_bytes(),
            "max_syntax_nodes": runtime.max_syntax_nodes(),
            "max_syntax_depth": runtime.max_syntax_depth(),
            "max_included_ranges": runtime.max_included_ranges(),
            "max_incremental_edits": runtime.max_incremental_edits(),
            "max_concurrent_parses": runtime.max_concurrent_parses(),
            "max_cache_bytes": runtime.max_cache_bytes(),
            "input_chunk_bytes": runtime.default_settings().input_chunk_bytes(),
        },
        "diagnostic_deadline_seconds": DIAGNOSTIC_DEADLINE_SECONDS,
        "diagnostic_max_output_bytes": MAX_OUTPUT_BYTES,
    });
    let provider = Arc::new(TreeSitterProvider::new(runtime)?);
    let output = execute_parse(
        provider.as_ref(),
        &request,
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &cancellation,
    )?;
    // These source fingerprints label a diagnostic frontend, not an authenticated
    // installed binary or the provider identity used by a published service index.
    let frontend_version = "rootlight-native-parse-diagnostic-1.0";
    let frontend_digest = content_hash(include_bytes!("native_parse_example.rs"));
    let build_context = BuildContextIdentity::new(content_hash(
        b"rootlight.native-parse.diagnostic-build-context/1",
    ));
    let producer =
        ProducerIdentity::new("rootlight-native-parse-diagnostic", "1.0", config.hash())?;
    let analyzed = if args.analyze {
        let parse_provider: Arc<dyn ParseProvider> =
            Arc::clone(&provider) as Arc<dyn ParseProvider>;
        let analyzer = TreeSitterAnalyzer::new(
            parse_provider,
            producer.clone(),
            args.language.clone(),
            frontend_version,
            frontend_digest,
        )?;
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&snapshot, &source)?,
            args.language.clone(),
            AnalysisTier::TierD,
            build_context,
            &analysis,
        )?
        .with_generated_status(false);
        Some(execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancellation,
        )?)
    } else {
        None
    };
    let analysis_view = analyzed.as_ref().map(|analyzed| {
        let (coverage, resources, end) = report_values(analyzed.report());
        AnalysisView {
            identity: json!({
                "producer": producer,
                "frontend_version": frontend_version,
                "frontend_digest": frontend_digest,
                "frontend_digest_scope": "diagnostic example source fingerprint, not an executable hash",
                "build_context": build_context,
                "generated_source_status": "diagnostic authored-input assumption; not project classified",
                "published_service_provider_identity": false,
            }),
            document: analyzed.document(),
            coverage,
            resources,
            end,
            memory_admission: format!("{:?}", analyzed.memory_admission()),
        }
    });
    let (coverage, resources, end) = report_values(output.report());
    let report = NativeReport {
        schema: "rootlight.native-parse-diagnostic/1",
        relative_path: snapshot.path().as_str(),
        language: args.language.as_str(),
        encoding: "utf-8",
        source: &source,
        configuration_hash: config.hash(),
        profile_source_hash: content_hash(include_bytes!(
            "source_inventory_example/admission_profile.rs"
        )),
        profile,
        coverage,
        resources,
        end,
        memory_admission: format!("{:?}", output.memory_admission()),
        canonical_fact_count: output.facts().len(),
        diagnostic_count: output.diagnostics().len(),
        facts: Facts {
            facts: output.facts(),
            cancellation: &cancellation,
        },
        diagnostics: Diagnostics {
            diagnostics: output.diagnostics(),
            cancellation: &cancellation,
        },
        analysis: analysis_view,
        qualification: json!({
            "caller_supplied_repository_and_generation": true,
            "whole_source_parse": true,
            "source_text_included": args.analyze,
            "raw_source_text_included": false,
            "whole_source_file_payload_included": false,
            "normalized_ir_payload_text_may_be_included": args.analyze,
            "normalized_ir_may_include_bounded_lexical_text": args.analyze,
            "lowering_executed": args.analyze,
            "diagnostic_lowering": args.analyze,
            "parser_invocations": if args.analyze { 2 } else { 1 },
            "service_index_executed": false,
            "installed_mcp_qualified": false,
            "semantic_completeness_proven": false,
            "performance_acceptance_proven": false,
            "memory_enforcement_scope": "committed SDK admission; not independent OS enforcement",
        }),
    };
    let bytes = buffer_report(&report, MAX_OUTPUT_BYTES, &cancellation)?;
    io::stdout().lock().write_all(&bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments() -> Vec<OsString> {
        vec![
            "repository".into(),
            RepositoryId::from_bytes([1; 16]).to_string().into(),
            GenerationId::from_bytes([2; 20]).to_string().into(),
            "fsharp".into(),
            "src/Example.fs".into(),
        ]
    }

    #[test]
    fn arguments_require_exact_explicit_source_coordinates() {
        let parsed = Arguments::parse(arguments().into_iter()).unwrap();
        assert_eq!(parsed.language.as_str(), "fsharp");
        assert_eq!(parsed.path.as_str(), "src/Example.fs");
        assert!(!parsed.analyze);
        let mut extra = arguments();
        extra.push("extra".into());
        assert!(Arguments::parse(extra.into_iter()).is_err());
        let mut missing = arguments();
        missing.pop();
        assert!(Arguments::parse(missing.into_iter()).is_err());
    }

    #[test]
    fn arguments_accept_only_one_trailing_analyze_flag() {
        let mut args = arguments();
        args.push("--analyze".into());
        assert!(Arguments::parse(args.clone().into_iter()).unwrap().analyze);
        args.push("--analyze".into());
        assert!(Arguments::parse(args.into_iter()).is_err());
        let mut args = arguments();
        args.push("--unknown".into());
        assert!(Arguments::parse(args.into_iter()).is_err());
    }

    #[test]
    fn arguments_reject_invalid_ids_languages_and_source_paths() {
        for (index, replacement) in [
            (1, "invalid"),
            (2, "invalid"),
            (3, "f sharp"),
            (4, "../Example.fs"),
        ] {
            let mut args = arguments();
            *args.get_mut(index).unwrap() = replacement.into();
            assert!(Arguments::parse(args.into_iter()).is_err());
        }
    }

    #[test]
    fn output_bound_includes_the_complete_json_and_newline() {
        let cancellation = Cancellation::new();
        let value = json!({"count": 2});
        let expected = b"{\"count\":2}\n";
        assert_eq!(
            buffer_report(&value, expected.len(), &cancellation).unwrap(),
            expected
        );
        assert!(buffer_report(&value, expected.len() - 1, &cancellation).is_err());
    }

    #[test]
    fn rejected_output_write_does_not_append_partial_bytes() {
        let cancellation = Cancellation::new();
        let mut writer = OutputWriter {
            bytes: Vec::new(),
            maximum: 3,
            cancellation: &cancellation,
        };
        writer.write_all(b"abc").unwrap();
        assert!(writer.write_all(b"d").is_err());
        assert_eq!(writer.bytes, b"abc");
    }

    #[test]
    fn cancelled_output_is_not_returned_as_a_complete_report() {
        let cancellation = Cancellation::with_deadline(Instant::now());
        assert!(buffer_report(&json!({"count": 0}), 64, &cancellation).is_err());
    }
}
