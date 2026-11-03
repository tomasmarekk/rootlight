//! Native Gitignore parsing into source-owned ordered rule certificates.
//! The compiled program validates real pattern semantics; common software
//! declarations and repository-wide ignore application are not inferred.

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, CoverageReport, DescriptorError, DiagnosticCode,
    DomainCoverage, EncodingId, LanguageId, MemoryEnforcement, ParseCapabilities, ParseProvider,
    ParseReport, ParseRequest, ResourceUsage, SinkError, StreamEnd, SyntaxFact, SyntaxFactBatch,
    SyntaxFactKind, SyntaxFactSink, SyntaxKindLabel,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::ContentHash;
use rootlight_ir::{
    AnalysisTier, CoverageStatus, DiagnosticSeverity, FactDomain, MAX_GITIGNORE_SOURCE_BYTES,
};

use crate::gitignore_program::{GitignoreMatcher, failure};

/// Bounded native provider for ordered, source-backed Gitignore rule programs.
pub struct GitignoreParseProvider {
    capabilities: ParseCapabilities,
}

impl GitignoreParseProvider {
    /// Returns a content identity for the provider, native program and pinned grammar.
    #[must_use]
    pub fn binary_digest() -> ContentHash {
        let mut hasher = blake3::Hasher::new();
        for source in [
            include_bytes!("gitignore.rs").as_slice(),
            include_bytes!("gitignore_program.rs").as_slice(),
            include_bytes!("../../rootlight-ir/src/gitignore_program.rs").as_slice(),
            include_bytes!("../../../source-inputs/cargo/ignore-0.4.29/src/gitignore.rs")
                .as_slice(),
        ] {
            hasher.update(source);
        }
        ContentHash::from_bytes(*hasher.finalize().as_bytes())
    }

    /// Creates a provider under caller source and syntax-node ceilings.
    ///
    /// # Errors
    /// Returns [`DescriptorError`] when a ceiling is zero.
    ///
    /// # Panics
    /// Panics only if the fixed language label violates the SDK contract.
    pub fn new(max_source_bytes: usize, max_syntax_nodes: usize) -> Result<Self, DescriptorError> {
        Ok(Self {
            capabilities: ParseCapabilities::new(
                vec![LanguageId::new("gitignore").expect("fixed Gitignore language label")],
                vec![EncodingId::utf8()],
                max_source_bytes.min(MAX_GITIGNORE_SOURCE_BYTES),
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

    /// Counts mandatory native certificates before syntax-stream admission.
    ///
    /// # Errors
    /// Returns an error for invalid source, cancellation or limits. A refused
    /// native program needs only a source root and a scoped parser diagnostic.
    pub fn required_syntax_fact_count(
        &self,
        request: &ParseRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<usize, AdapterError> {
        match self.compile(request, cancellation) {
            Ok(program) => self.fact_count(request, &program),
            Err(error) if syntax_refusal(&error).is_some() => Ok(1),
            Err(error) => Err(error),
        }
    }

    fn compile(
        &self,
        request: &ParseRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<GitignoreMatcher, AdapterError> {
        let bytes = request.source().bytes();
        if bytes.len() > self.capabilities.max_source_bytes() {
            return Err(failure("gitignore-source-byte-limit"));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| failure("invalid-utf8"))?;
        GitignoreMatcher::parse(request.source().source_ref().clone(), text, cancellation)
    }

    fn fact_count(
        &self,
        request: &ParseRequest<'_>,
        program: &GitignoreMatcher,
    ) -> Result<usize, AdapterError> {
        program
            .program()
            .rules()
            .len()
            .checked_add(2)
            .filter(|count| {
                *count
                    <= request
                        .limits()
                        .max_syntax_nodes()
                        .min(self.capabilities.max_syntax_nodes())
            })
            .ok_or_else(|| failure("gitignore-syntax-node-limit"))
    }
}

impl ParseProvider for GitignoreParseProvider {
    fn capabilities(&self) -> &ParseCapabilities {
        &self.capabilities
    }

    fn parse(
        &self,
        request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancellation: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        let program = match self.compile(request, cancellation) {
            Ok(program) => program,
            Err(error) => {
                let Some(code) = syntax_refusal(&error) else {
                    return Err(error);
                };
                return refused_parse(request, sink, cancellation, code.clone());
            }
        };
        let count = self.fact_count(request, &program)?;
        let source = request.source();
        let mut facts = Vec::new();
        facts
            .try_reserve_exact(count)
            .map_err(|_| SinkError::AllocationFailed)?;
        facts.push(SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Root,
            source.source_ref().span(),
            0,
            label("gitignore.source_file"),
        ));
        facts.push(SyntaxFact::new(
            2,
            Some(1),
            SyntaxFactKind::Signature,
            source.source_ref().span(),
            1,
            label("gitignore.program.expression"),
        ));
        for (index, rule) in program.program().rules().iter().enumerate() {
            cancellation.check()?;
            let id = u64::try_from(index)
                .ok()
                .and_then(|id| id.checked_add(3))
                .ok_or_else(|| failure("gitignore-syntax-node-limit"))?;
            facts.push(SyntaxFact::new(
                id,
                Some(2),
                SyntaxFactKind::Signature,
                rule.line,
                2,
                label("gitignore.rule.expression"),
            ));
        }
        for chunk in facts.chunks(32) {
            cancellation.check()?;
            let mut batch = Vec::new();
            batch
                .try_reserve_exact(chunk.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            batch.extend_from_slice(chunk);
            sink.push_cancellable(
                SyntaxFactBatch::new(sink.next_sequence(), batch, Vec::new()),
                cancellation,
            )?;
        }
        let usage = sink.staged_usage();
        let source_bytes = source.bytes().len();
        Ok(ParseReport::new(
            CoverageReport::new(
                AnalysisTier::TierD,
                CoverageStatus::Bounded,
                source_bytes,
                source_bytes,
                0,
                vec![
                    DomainCoverage::new(FactDomain::Files, CoverageStatus::Complete, 1, 1, 0)?,
                    DomainCoverage::new(FactDomain::Entities, CoverageStatus::Bounded, 0, 0, 0)?,
                    DomainCoverage::new(FactDomain::Relations, CoverageStatus::Bounded, 0, 0, 0)?,
                ],
            )?,
            ResourceUsage::new(source_bytes, count, count, 2, None, usage),
            StreamEnd::new(sink.next_sequence(), usage),
        )?)
    }
}

fn label(value: &str) -> SyntaxKindLabel {
    SyntaxKindLabel::new(value).expect("fixed Gitignore syntax label")
}

fn syntax_refusal(error: &AdapterError) -> Option<&DiagnosticCode> {
    match error {
        AdapterError::ProviderFailed { code }
            if matches!(
                code.as_str(),
                "gitignore-pattern-parse-error" | "gitignore-matcher-build-error"
            ) =>
        {
            Some(code)
        }
        _ => None,
    }
}

fn refused_parse(
    request: &ParseRequest<'_>,
    sink: &mut dyn SyntaxFactSink,
    cancellation: &Cancellation,
    code: DiagnosticCode,
) -> Result<ParseReport, AdapterError> {
    let source = request.source();
    // A partially valid rule list cannot certify last-match semantics for the
    // physical file. Keep its exact source and refuse the native program atomically.
    sink.push_cancellable(
        SyntaxFactBatch::new(
            sink.next_sequence(),
            vec![SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Root,
                source.source_ref().span(),
                0,
                label("gitignore.source_file"),
            )],
            vec![AdapterDiagnostic::new(
                code,
                DiagnosticSeverity::Error,
                Some(source.source_ref().clone()),
                CoverageStatus::Unknown,
            )],
        ),
        cancellation,
    )?;
    let usage = sink.staged_usage();
    let source_bytes = source.bytes().len();
    Ok(ParseReport::new(
        CoverageReport::new(
            AnalysisTier::TierD,
            CoverageStatus::Unknown,
            source_bytes,
            0,
            1,
            vec![
                DomainCoverage::new(FactDomain::Files, CoverageStatus::Complete, 1, 1, 0)?,
                DomainCoverage::new(FactDomain::Entities, CoverageStatus::Unknown, 0, 0, 0)?,
                DomainCoverage::new(FactDomain::Relations, CoverageStatus::Unknown, 0, 0, 0)?,
            ],
        )?,
        ResourceUsage::new(source_bytes, 1, 1, 0, None, usage),
        StreamEnd::new(sink.next_sequence(), usage),
    )?)
}
