//! Reconcile native line-based RDF evidence with every current physical source line.
//! Required typed statements are admitted together, independently of optional text.

use std::{io, mem::size_of};

use super::*;
use rootlight_ir::{MAX_RDF_STATEMENT_PAYLOAD_BYTES, RdfStatement, new_rdf_statement_envelope};

#[derive(Clone, Copy, Default)]
enum Format {
    #[default]
    NTriples,
    NQuads,
    Dgraph,
}

impl Format {
    fn for_language(language: &str) -> Option<Self> {
        match language {
            "ntriples" => Some(Self::NTriples),
            "nquads" => Some(Self::NQuads),
            "nquads-dgraph" => Some(Self::Dgraph),
            _ => None,
        }
    }

    const fn gap(self) -> &'static str {
        match self {
            Self::NTriples => "ntriples-native-statement-evidence-unavailable",
            Self::NQuads => "nquads-native-statement-evidence-unavailable",
            Self::Dgraph => "nquads-dgraph-native-statement-evidence-unavailable",
        }
    }

    const fn projection_gap(self) -> &'static str {
        match self {
            Self::NTriples => "ntriples-whole-physical-utf8-source-required",
            Self::NQuads => "nquads-whole-physical-utf8-source-required",
            Self::Dgraph => "nquads-dgraph-whole-physical-utf8-source-required",
        }
    }

    fn role(self, kind: &str) -> Option<usize> {
        match (self, kind) {
            (Self::NTriples, "ntriples.statement.expression")
            | (Self::NQuads, "nquads.statement.expression")
            | (Self::Dgraph, "nquads-dgraph.statement.expression") => Some(0),
            (Self::NTriples, "ntriples.subject.expression")
            | (Self::NQuads, "nquads.subject.expression")
            | (Self::Dgraph, "nquads-dgraph.subject.expression") => Some(1),
            (Self::NTriples, "ntriples.predicate.expression")
            | (Self::NQuads, "nquads.predicate.expression")
            | (Self::Dgraph, "nquads-dgraph.predicate.expression") => Some(2),
            (Self::NTriples, "ntriples.object.expression")
            | (Self::NQuads, "nquads.object.expression")
            | (Self::Dgraph, "nquads-dgraph.object.expression") => Some(3),
            (Self::NQuads, "nquads.graph.expression") => Some(4),
            _ => None,
        }
    }

    fn metadata(self, fact: &SyntaxFact) -> bool {
        fact.kind() == SyntaxFactKind::Signature
            && fact.syntax_kind().as_str().starts_with(match self {
                Self::NTriples => "ntriples.",
                Self::NQuads => "nquads.",
                Self::Dgraph => "nquads-dgraph.",
            })
    }

    fn decode(
        self,
        source: SourceRef,
        text: &str,
        proof: Statement,
    ) -> Result<RdfStatement, &'static str> {
        match self {
            Self::NTriples => {
                RdfStatement::from_ntriples(source, text, proof.terms).map_err(|_| self.gap())
            }
            Self::NQuads => RdfStatement::from_nquads(source, text, proof.terms, proof.graph)
                .map_err(|_| self.gap()),
            Self::Dgraph => {
                RdfStatement::from_nquads_dgraph(source, text, proof.terms).map_err(|error| {
                    match error {
                        rootlight_ir::DgraphStatementError::InvalidEvidence => self.gap(),
                        rootlight_ir::DgraphStatementError::UnsupportedConstruct => {
                            "nquads-dgraph-native-construct-unmodeled"
                        }
                        rootlight_ir::DgraphStatementError::UnsupportedIdentifierCategory => {
                            "nquads-dgraph-identifier-category-evidence-unavailable"
                        }
                    }
                })
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Statement {
    span: SourceSpan,
    terms: [SourceSpan; 3],
    graph: Option<SourceSpan>,
    payload_bytes: usize,
}

pub(super) struct Gap {
    pub(super) span: SourceSpan,
    pub(super) detail: &'static str,
}

#[derive(Default)]
pub(super) struct Plan {
    format: Format,
    pub(super) statements: Vec<Statement>,
    pub(super) gaps: Vec<Gap>,
    pub(super) modeled_peak: usize,
    base_bytes: usize,
    live_bytes: usize,
    memory_limit: usize,
}

struct PayloadBytes(usize);

impl io::Write for PayloadBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("rdf payload size overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Plan {
    fn peak(&mut self, temporary: usize) -> Result<(), AdapterError> {
        let bytes = checked_add(checked_add(self.base_bytes, self.live_bytes)?, temporary)?;
        require_resource_limit(ResourceKind::ReportedMemoryBytes, bytes, self.memory_limit)?;
        self.modeled_peak = self.modeled_peak.max(bytes);
        Ok(())
    }

    fn gap(
        &mut self,
        span: SourceSpan,
        detail: &'static str,
        temporary: usize,
    ) -> Result<(), AdapterError> {
        if self.gaps.len() == self.gaps.capacity() {
            let extra = self.gaps.len().max(1);
            let extra_bytes = extra
                .checked_mul(size_of::<Gap>())
                .ok_or(SinkError::AccountingOverflow)?;
            self.live_bytes = checked_add(self.live_bytes, extra_bytes)?;
            self.peak(temporary)?;
            self.gaps
                .try_reserve_exact(extra)
                .map_err(|_| SinkError::AllocationFailed)?;
        }
        self.gaps.push(Gap { span, detail });
        Ok(())
    }

    pub(super) fn new(
        request: &AnalysisRequest<'_>,
        output: &ParseOutput,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let Some(format) = Format::for_language(request.language().as_str()) else {
            return Ok(Self::default());
        };
        let mut plan = Self {
            format,
            base_bytes: output
                .report()
                .resources()
                .reported_memory_bytes()
                .unwrap_or(0),
            memory_limit: request.limits().max_reported_memory_bytes(),
            ..Self::default()
        };
        let source = request.source().source_ref();
        let bytes = request.source().bytes();
        if request.encoding().as_str() != "utf-8"
            || !request.included_ranges().is_empty()
            || source.span().start_byte() != 0
            || source.span().end_byte()
                != u64::try_from(bytes.len()).map_err(|_| SinkError::AccountingOverflow)?
            || std::str::from_utf8(bytes).is_err()
        {
            plan.gap(source.span(), format.projection_gap(), 0)?;
            return Ok(plan);
        }
        let mut count = 0usize;
        for (index, fact) in output.facts().iter().enumerate() {
            check_periodically(index, cancellation)?;
            if format.metadata(fact) {
                count = checked_add(count, 1)?;
            }
        }
        let metadata_bytes = count
            .checked_mul(size_of::<&SyntaxFact>())
            .ok_or(SinkError::AccountingOverflow)?;
        let capacity = count / 4;
        plan.live_bytes = capacity
            .checked_mul(size_of::<Statement>())
            .ok_or(SinkError::AccountingOverflow)?;
        plan.peak(metadata_bytes)?;
        plan.statements
            .try_reserve_exact(capacity)
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut facts = Vec::new();
        facts
            .try_reserve_exact(count)
            .map_err(|_| SinkError::AllocationFailed)?;
        for (index, fact) in output.facts().iter().enumerate() {
            check_periodically(index, cancellation)?;
            if format.metadata(fact) {
                facts.push(fact);
            }
        }
        plan.peak(
            metadata_bytes
                .checked_mul(3)
                .ok_or(SinkError::AccountingOverflow)?,
        )?;
        sort_owned_cancellable_by(&mut facts, cancellation, |left, right| {
            (
                left.span().start_byte(),
                left.span().end_byte(),
                left.local_id(),
            )
                .cmp(&(
                    right.span().start_byte(),
                    right.span().end_byte(),
                    right.local_id(),
                ))
        })?;
        let mut offset = 0usize;
        let mut next = 0usize;
        let mut payload_total = 0usize;
        for line in bytes.split_inclusive(|byte| matches!(byte, b'\r' | b'\n')) {
            cancellation.check()?;
            let full_end = checked_add(offset, line.len())?;
            let full_end_byte =
                u64::try_from(full_end).map_err(|_| SinkError::AccountingOverflow)?;
            let end = full_end
                - usize::from(
                    line.last()
                        .is_some_and(|byte| matches!(byte, b'\r' | b'\n')),
                );
            let mut start =
                offset + usize::from(offset == 0 && line.starts_with(b"\xef\xbb\xbf")) * 3;
            while start < end
                && bytes
                    .get(start)
                    .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
            {
                check_periodically(start - offset, cancellation)?;
                start += 1;
            }
            let first = next;
            while facts
                .get(next)
                .is_some_and(|fact| fact.span().start_byte() < full_end_byte)
            {
                check_periodically(next, cancellation)?;
                next += 1;
            }
            let group = &facts[first..next];
            let empty = start == end || bytes.get(start) == Some(&b'#');
            if empty && group.is_empty() {
                offset = full_end;
                continue;
            }
            let line_span = SourceSpan::new(
                source.span().file(),
                u64::try_from(offset).map_err(|_| SinkError::AccountingOverflow)?,
                u64::try_from(end).map_err(|_| SinkError::AccountingOverflow)?,
            )
            .map_err(|_| provider_failure("ntriples-line-span"))?;
            let Some(mut statement) = (!empty)
                .then(|| prove(format, group, bytes, start, end))
                .flatten()
            else {
                plan.gap(line_span, format.gap(), metadata_bytes)?;
                offset = full_end;
                continue;
            };
            let text = physical_text(bytes, statement.span)?;
            let temporary = checked_add(
                metadata_bytes,
                checked_add(
                    text.len()
                        .checked_mul(2)
                        .ok_or(SinkError::AccountingOverflow)?,
                    size_of::<RdfStatement>(),
                )?,
            )?;
            plan.peak(temporary)?;
            let decoded =
                match format.decode(source_for_span(source, statement.span), text, statement) {
                    Ok(decoded) => decoded,
                    Err(detail) => {
                        plan.gap(line_span, detail, metadata_bytes)?;
                        offset = full_end;
                        continue;
                    }
                };
            let mut payload = PayloadBytes(0);
            serde_json::to_writer(&mut payload, &decoded)
                .map_err(|_| provider_failure("ntriples-payload-accounting"))?;
            statement.payload_bytes = payload.0;
            require_resource_limit(
                ResourceKind::ExtensionBytes,
                payload.0,
                request
                    .limits()
                    .ir()
                    .max_extension_payload_bytes
                    .min(MAX_RDF_STATEMENT_PAYLOAD_BYTES),
            )?;
            payload_total = checked_add(payload_total, payload.0)?;
            require_resource_limit(
                ResourceKind::ExtensionBytes,
                payload_total,
                request.limits().ir().max_total_extension_bytes,
            )?;
            plan.statements.push(statement);
            offset = full_end;
        }
        if next != facts.len() {
            plan.gap(source.span(), format.gap(), metadata_bytes)?;
        }
        cancellation.check()?;
        Ok(plan)
    }

    pub(super) fn envelope(
        &mut self,
        proof: Statement,
        request: &AnalysisRequest<'_>,
        provenance: FactId,
    ) -> Result<ExtensionEnvelope, AdapterError> {
        let text = physical_text(request.source().bytes(), proof.span)?;
        let temporary = checked_add(
            proof
                .payload_bytes
                .checked_mul(8)
                .ok_or(SinkError::AccountingOverflow)?,
            checked_add(
                text.len()
                    .checked_mul(2)
                    .ok_or(SinkError::AccountingOverflow)?,
                size_of::<RdfStatement>(),
            )?,
        )?;
        self.peak(temporary)?;
        let statement = self
            .format
            .decode(
                source_for_span(request.source().source_ref(), proof.span),
                text,
                proof,
            )
            .map_err(|_| provider_failure("ntriples-current-statement"))?;
        let envelope = new_rdf_statement_envelope(provenance, &statement)
            .map_err(|_| provider_failure("ntriples-statement-envelope"))?;
        if envelope.payload.len() != proof.payload_bytes {
            return Err(provider_failure("ntriples-payload-accounting"));
        }
        let owned = [
            envelope.payload.capacity(),
            envelope.namespace.capacity(),
            envelope.version.capacity(),
            envelope
                .evidence
                .derivation
                .capacity()
                .checked_mul(size_of::<FactRef>())
                .ok_or(SinkError::AccountingOverflow)?,
            size_of::<ExtensionEnvelope>()
                .checked_mul(2)
                .ok_or(SinkError::AccountingOverflow)?,
            size_of::<FactId>(),
            size_of::<[usize; 8]>(),
        ]
        .into_iter()
        .try_fold(0usize, checked_add)?;
        self.live_bytes = checked_add(self.live_bytes, owned)?;
        self.peak(temporary)?;
        Ok(envelope)
    }

    pub(super) fn account_strings(
        &self,
        total: &mut usize,
        limits: &IrLimits,
    ) -> Result<(), AdapterError> {
        for statement in &self.statements {
            let length = usize::try_from(statement.span.end_byte() - statement.span.start_byte())
                .map_err(|_| SinkError::AccountingOverflow)?;
            account_string(total, length, limits)?;
            account_string(total, length, limits)?;
            account_string(total, rootlight_ir::RDF_STATEMENT_NAMESPACE.len(), limits)?;
            account_string(total, 1, limits)?;
        }
        Ok(())
    }
}

fn physical_text(bytes: &[u8], span: SourceSpan) -> Result<&str, AdapterError> {
    let start = usize::try_from(span.start_byte()).map_err(|_| SinkError::AccountingOverflow)?;
    let end = usize::try_from(span.end_byte()).map_err(|_| SinkError::AccountingOverflow)?;
    std::str::from_utf8(
        bytes
            .get(start..end)
            .ok_or_else(|| provider_failure("ntriples-source-span"))?,
    )
    .map_err(|_| provider_failure("ntriples-source-utf8"))
}

fn prove(
    format: Format,
    facts: &[&SyntaxFact],
    bytes: &[u8],
    start: usize,
    end: usize,
) -> Option<Statement> {
    if !matches!(
        (format, facts.len()),
        (Format::NTriples | Format::Dgraph, 4) | (Format::NQuads, 4 | 5)
    ) {
        return None;
    }
    let mut roles = [None; 5];
    for fact in facts {
        let slot = roles.get_mut(format.role(fact.syntax_kind().as_str())?)?;
        if slot.replace(fact.span()).is_some() {
            return None;
        }
    }
    let statement = roles[0];
    let span = statement?;
    if span.start_byte() != u64::try_from(start).ok()?
        || span.end_byte() > u64::try_from(end).ok()?
    {
        return None;
    }
    let tail = bytes.get(usize::try_from(span.end_byte()).ok()?..end)?;
    let first = tail.iter().position(|byte| !matches!(byte, b' ' | b'\t'));
    if first.is_some_and(|index| tail.get(index) != Some(&b'#')) {
        return None;
    }
    let terms = [roles[1]?, roles[2]?, roles[3]?];
    let mut previous = span.start_byte();
    for term in terms.into_iter().chain(roles[4]) {
        if term.file() != span.file()
            || term.start_byte() < previous
            || term.start_byte() >= term.end_byte()
            || term.end_byte() > span.end_byte()
        {
            return None;
        }
        previous = term.end_byte();
    }
    Some(Statement {
        span,
        terms,
        graph: roles[4],
        payload_bytes: 0,
    })
}

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "ntriples.statement.expression"
                | "ntriples.subject.expression"
                | "ntriples.predicate.expression"
                | "ntriples.object.expression"
                | "nquads.statement.expression"
                | "nquads.subject.expression"
                | "nquads.predicate.expression"
                | "nquads.object.expression"
                | "nquads.graph.expression"
                | "nquads-dgraph.statement.expression"
                | "nquads-dgraph.subject.expression"
                | "nquads-dgraph.predicate.expression"
                | "nquads-dgraph.object.expression"
        )
}
