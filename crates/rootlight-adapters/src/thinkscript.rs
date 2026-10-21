//! Emit a closed required ThinkScript script/input source plan.
//! The shared finite model authenticates physical positions; evaluation and
//! omitted expression syntax remain explicit partial coverage.

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, CoverageReport, DescriptorError, DiagnosticCode,
    DomainCoverage, EncodingId, LanguageId, MemoryEnforcement, ParseCapabilities, ParseProvider,
    ParseReport, ParseRequest, ResourceUsage, SinkError, StreamEnd, StreamUsage, SyntaxFact,
    SyntaxFactBatch, SyntaxFactKind, SyntaxFactSink, SyntaxKindLabel,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::ContentHash;
use rootlight_ir::{AnalysisTier, CoverageStatus, DiagnosticSeverity, FactDomain, SourceSpan};
use rootlight_thinkscript::{Plan, Role};

/// Finite physical-source parser for written ThinkScript scripts and inputs.
pub struct ThinkScriptParseProvider {
    capabilities: ParseCapabilities,
}

impl ThinkScriptParseProvider {
    /// Creates a provider under caller-owned source and required-node ceilings.
    ///
    /// # Errors
    /// Returns an invalid fixed descriptor or a zero ceiling error.
    ///
    /// # Panics
    /// Panics only if the fixed language label violates the SDK contract.
    pub fn new(max_source_bytes: usize, max_syntax_nodes: usize) -> Result<Self, DescriptorError> {
        Ok(Self {
            capabilities: ParseCapabilities::new(
                vec![LanguageId::new("thinkscript").expect("fixed ThinkScript label")],
                vec![EncodingId::utf8()],
                max_source_bytes,
                max_syntax_nodes,
                128,
                0,
                false,
                false,
                true,
                1,
                MemoryEnforcement::Unavailable,
            )?,
        })
    }

    /// Binds both the frontend wrapper and complete shared syntax model.
    #[must_use]
    pub fn binary_digest() -> ContentHash {
        let mut hash = blake3::Hasher::new();
        hash.update(b"rootlight-thinkscript-written-inputs/1\0");
        hash.update(include_bytes!("thinkscript.rs"));
        hash.update(&rootlight_thinkscript::source_digest());
        ContentHash::from_bytes(*hash.finalize().as_bytes())
    }

    fn plan(
        &self,
        request: &ParseRequest<'_>,
        cancel: &Cancellation,
    ) -> Result<Plan, AdapterError> {
        cancel.check()?;
        if request.source().bytes().len()
            > self
                .capabilities
                .max_source_bytes()
                .min(request.limits().max_source_bytes())
        {
            return Err(AdapterError::ProviderFailed {
                code: DiagnosticCode::new("thinkscript-source-byte-limit").expect("fixed refusal"),
            });
        }
        let maximum = request
            .limits()
            .max_syntax_nodes()
            .min(self.capabilities.max_syntax_nodes());
        rootlight_thinkscript::scan(request.source().bytes(), maximum, maximum, cancel)
            .map_err(model_error)
    }

    /// Returns exact required raw record demand, including every Read witness.
    ///
    /// # Errors
    /// Returns typed cancellation, source-model or allocation refusal.
    pub fn required_syntax_fact_count(
        &self,
        request: &ParseRequest<'_>,
        cancel: &Cancellation,
    ) -> Result<usize, AdapterError> {
        Ok(self.plan(request, cancel)?.records.len())
    }
}

impl ParseProvider for ThinkScriptParseProvider {
    fn capabilities(&self) -> &ParseCapabilities {
        &self.capabilities
    }

    fn parse(
        &self,
        request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancel: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        let plan = self.plan(request, cancel)?;
        let source = request.source().source_ref();
        let mut facts = Vec::new();
        facts
            .try_reserve_exact(plan.records.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut entities = 0;
        for record in &plan.records {
            cancel.check()?;
            entities += usize::from(matches!(record.role, Role::Script | Role::Input));
            facts.push(SyntaxFact::new(
                record.id,
                record.parent,
                role_kind(record.role),
                SourceSpan::new(source.span().file(), record.start as u64, record.end as u64)
                    .map_err(|_| SinkError::InvalidIr)?,
                record.depth,
                SyntaxKindLabel::new(record.role.label()).expect("closed ThinkScript role"),
            ));
        }
        for batch in facts.chunks(32) {
            cancel.check()?;
            let mut emitted = Vec::new();
            emitted
                .try_reserve_exact(batch.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            emitted.extend_from_slice(batch);
            sink.push_cancellable(
                SyntaxFactBatch::new(sink.next_sequence(), emitted, Vec::new()),
                cancel,
            )?;
        }
        if can_emit_gap(request, sink.staged_usage()) {
            let code = if plan.lexical_complete {
                "thinkscript-semantics-partial"
            } else {
                "thinkscript-lexical-structure-partial"
            };
            sink.push_cancellable(
                SyntaxFactBatch::new(
                    sink.next_sequence(),
                    Vec::new(),
                    vec![AdapterDiagnostic::new(
                        DiagnosticCode::new(code).expect("fixed gap"),
                        DiagnosticSeverity::Warning,
                        Some(source.clone()),
                        CoverageStatus::Bounded,
                    )],
                ),
                cancel,
            )?;
        }
        let usage = sink.staged_usage();
        let bytes = request.source().bytes().len();
        let status = if plan.lexical_complete {
            CoverageStatus::Bounded
        } else {
            CoverageStatus::Unknown
        };
        Ok(ParseReport::new(
            CoverageReport::new(
                AnalysisTier::TierD,
                status,
                bytes,
                bytes,
                usize::from(bytes > 0),
                vec![
                    DomainCoverage::new(FactDomain::Files, CoverageStatus::Complete, 1, 1, 0)?,
                    DomainCoverage::new(FactDomain::Entities, status, entities, entities, 0)?,
                ],
            )?,
            ResourceUsage::new(
                bytes,
                facts.len(),
                plan.syntax_nodes,
                plan.max_depth,
                None,
                usage,
            ),
            StreamEnd::new(sink.next_sequence(), usage),
        )?)
    }
}

fn role_kind(role: Role) -> SyntaxFactKind {
    match role {
        Role::Root => SyntaxFactKind::Root,
        Role::Environment | Role::Header | Role::ReadContext => SyntaxFactKind::Signature,
        Role::Script | Role::Input => SyntaxFactKind::Declaration,
        Role::ScriptName | Role::InputName | Role::Read => SyntaxFactKind::Occurrence,
    }
}

fn model_error(error: rootlight_thinkscript::Error) -> AdapterError {
    match error {
        rootlight_thinkscript::Error::Cancelled(error) => error.into(),
        rootlight_thinkscript::Error::Allocation => SinkError::AllocationFailed.into(),
        rootlight_thinkscript::Error::Limit => AdapterError::ProviderFailed {
            code: DiagnosticCode::new("thinkscript-syntax-node-limit").expect("fixed refusal"),
        },
    }
}

fn can_emit_gap(request: &ParseRequest<'_>, used: StreamUsage) -> bool {
    let syntax = request.limits().syntax_stream();
    let batch = syntax.batch();
    let ir = request.limits().ir();
    used.batches() < syntax.max_batches()
        && used.diagnostics() < syntax.max_diagnostics()
        && syntax
            .max_diagnostic_bytes()
            .saturating_sub(used.diagnostic_bytes())
            >= 48
        && syntax
            .max_string_bytes()
            .saturating_sub(used.string_bytes())
            >= 48
        && syntax
            .max_output_bytes()
            .saturating_sub(used.output_bytes())
            >= 256
        && batch.max_diagnostics() > 0
        && batch.max_diagnostic_bytes() >= 48
        && batch.max_output_bytes() >= 256
        && ir.max_diagnostics > 0
        && ir.max_skipped_regions > 0
        && ir.max_total_diagnostic_bytes >= 128
}
