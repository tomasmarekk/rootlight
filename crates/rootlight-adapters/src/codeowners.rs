//! Native CODEOWNERS source inventories and mandatory parser certificates.
//! Invalid lines remain scoped evidence while valid ordered owner intent stays
//! available independently of repository policy and external account checks.

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, CoverageReport, DescriptorError, DiagnosticCode,
    DomainCoverage, EncodingId, LanguageId, MemoryEnforcement, ParseCapabilities, ParseProvider,
    ParseReport, ParseRequest, ResourceUsage, SinkError, StreamEnd, SyntaxFact, SyntaxFactBatch,
    SyntaxFactKind, SyntaxFactSink, SyntaxKindLabel,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::ContentHash;
use rootlight_ir::{
    AnalysisTier, CodeownersProgram, CoverageStatus, DiagnosticSeverity, FactDomain,
    MAX_CODEOWNERS_SOURCE_BYTES, SourceRef,
};

use crate::codeowners_program::{ensure_matching_syntax, failure};

/// Bounded native provider for authenticated ordered CODEOWNERS programs.
pub struct CodeownersParseProvider {
    capabilities: ParseCapabilities,
}

impl CodeownersParseProvider {
    /// Returns an identity covering native source, IR and pinned matcher inputs.
    #[must_use]
    pub fn binary_digest() -> ContentHash {
        let mut hasher = blake3::Hasher::new();
        for source in [
            include_bytes!("codeowners.rs").as_slice(),
            include_bytes!("codeowners_program.rs").as_slice(),
            include_bytes!("../../rootlight-ir/src/codeowners_program.rs").as_slice(),
            include_bytes!("../../../source-inputs/cargo/ignore-0.4.29/src/gitignore.rs")
                .as_slice(),
            include_bytes!("../../../source-inputs/cargo/globset-0.4.19/src/glob.rs").as_slice(),
        ] {
            hasher.update(source);
        }
        ContentHash::from_bytes(*hasher.finalize().as_bytes())
    }

    /// Creates a provider under caller source and mandatory syntax ceilings.
    ///
    /// # Errors
    /// Returns [`DescriptorError`] for a zero resource ceiling.
    ///
    /// # Panics
    /// Panics only if a fixed native language identifier is invalid.
    pub fn new(max_source_bytes: usize, max_syntax_nodes: usize) -> Result<Self, DescriptorError> {
        Ok(Self {
            capabilities: ParseCapabilities::new(
                vec![LanguageId::new("codeowners").expect("fixed CODEOWNERS language label")],
                vec![EncodingId::utf8()],
                max_source_bytes.min(MAX_CODEOWNERS_SOURCE_BYTES),
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

    /// Counts complete valid and invalid inventories before stream admission.
    ///
    /// # Errors
    /// Returns source identity, UTF-8, hard-limit, cancellation or allocation errors.
    pub fn required_syntax_fact_count(
        &self,
        request: &ParseRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<usize, AdapterError> {
        let program = self.program(request, cancellation)?;
        self.fact_count(request, &program)
    }

    fn program(
        &self,
        request: &ParseRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<CodeownersProgram, AdapterError> {
        cancellation.check()?;
        let source = request.source();
        if source.bytes().len() > self.capabilities.max_source_bytes() {
            return Err(failure("codeowners-source-byte-limit"));
        }
        let text = std::str::from_utf8(source.bytes()).map_err(|_| failure("invalid-utf8"))?;
        let program = CodeownersProgram::from_source(source.source_ref().clone(), text)
            .map_err(|_| failure("codeowners-program-evidence"))?;
        cancellation.check()?;
        Ok(program)
    }

    fn fact_count(
        &self,
        request: &ParseRequest<'_>,
        program: &CodeownersProgram,
    ) -> Result<usize, AdapterError> {
        program
            .rules()
            .len()
            .checked_add(program.invalid_lines().len())
            .and_then(|count| count.checked_add(2))
            .filter(|count| {
                *count
                    <= request
                        .limits()
                        .max_syntax_nodes()
                        .min(self.capabilities.max_syntax_nodes())
            })
            .ok_or_else(|| failure("codeowners-syntax-node-limit"))
    }
}

impl ParseProvider for CodeownersParseProvider {
    fn capabilities(&self) -> &ParseCapabilities {
        &self.capabilities
    }

    fn parse(
        &self,
        request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancellation: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        let program = self.program(request, cancellation)?;
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
            label("codeowners.source_file"),
        ));
        facts.push(SyntaxFact::new(
            2,
            Some(1),
            SyntaxFactKind::Signature,
            source.source_ref().span(),
            1,
            label("codeowners.program.expression"),
        ));
        for (index, (span, kind)) in program
            .rules()
            .iter()
            .map(|rule| (rule.line, "codeowners.rule.expression"))
            .chain(
                program
                    .invalid_lines()
                    .iter()
                    .map(|line| (line.line, "codeowners.invalid.expression")),
            )
            .enumerate()
        {
            cancellation.check()?;
            let id = u64::try_from(index)
                .ok()
                .and_then(|id| id.checked_add(3))
                .ok_or_else(|| failure("codeowners-syntax-node-limit"))?;
            facts.push(SyntaxFact::new(
                id,
                Some(2),
                SyntaxFactKind::Signature,
                span,
                2,
                label(kind),
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
        let mut skipped = program.invalid_lines().len();
        for rule in program.rules() {
            cancellation.check()?;
            let range = usize::try_from(rule.pattern.start_byte())
                .map_err(|_| failure("codeowners-source-offset-limit"))?
                ..usize::try_from(rule.pattern.end_byte())
                    .map_err(|_| failure("codeowners-source-offset-limit"))?;
            let pattern = program
                .text()
                .get(range)
                .ok_or_else(|| failure("codeowners-source-offset-limit"))?;
            if ensure_matching_syntax(pattern).is_err() {
                skipped = skipped
                    .checked_add(1)
                    .ok_or(SinkError::AccountingOverflow)?;
                let owner = source.source_ref();
                sink.push_cancellable(
                    SyntaxFactBatch::new(
                        sink.next_sequence(),
                        Vec::new(),
                        vec![AdapterDiagnostic::new(
                            DiagnosticCode::new("codeowners-pattern-semantics-unavailable")
                                .expect("fixed CODEOWNERS diagnostic code"),
                            DiagnosticSeverity::Warning,
                            Some(SourceRef::new(
                                owner.repository(),
                                owner.generation(),
                                rule.pattern,
                                owner.content_hash(),
                                None,
                            )),
                            CoverageStatus::Bounded,
                        )],
                    ),
                    cancellation,
                )?;
            }
        }
        for line in program.invalid_lines() {
            cancellation.check()?;
            let owner = source.source_ref();
            sink.push_cancellable(
                SyntaxFactBatch::new(
                    sink.next_sequence(),
                    Vec::new(),
                    vec![AdapterDiagnostic::new(
                        DiagnosticCode::new(line.reason.diagnostic_code())
                            .expect("fixed CODEOWNERS diagnostic code"),
                        DiagnosticSeverity::Error,
                        Some(SourceRef::new(
                            owner.repository(),
                            owner.generation(),
                            line.line,
                            owner.content_hash(),
                            None,
                        )),
                        CoverageStatus::Bounded,
                    )],
                ),
                cancellation,
            )?;
        }
        let usage = sink.staged_usage();
        let bytes = source.bytes().len();
        Ok(ParseReport::new(
            CoverageReport::new(
                AnalysisTier::TierD,
                CoverageStatus::Bounded,
                bytes,
                bytes,
                skipped,
                vec![
                    DomainCoverage::new(FactDomain::Files, CoverageStatus::Complete, 1, 1, 0)?,
                    DomainCoverage::new(FactDomain::Entities, CoverageStatus::Bounded, 0, 0, 0)?,
                    DomainCoverage::new(FactDomain::Relations, CoverageStatus::Bounded, 0, 0, 0)?,
                ],
            )?,
            ResourceUsage::new(bytes, count, count, 2, None, usage),
            StreamEnd::new(sink.next_sequence(), usage),
        )?)
    }
}

fn label(value: &str) -> SyntaxKindLabel {
    SyntaxKindLabel::new(value).expect("fixed CODEOWNERS syntax label")
}
