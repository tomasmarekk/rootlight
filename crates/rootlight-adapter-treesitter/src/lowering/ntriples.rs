//! Reconcile native N-Triples evidence with every current physical source line.
//! Required typed statements are admitted together, independently of optional text.

use std::{io, mem::size_of};

use super::*;
use rootlight_ir::{MAX_RDF_STATEMENT_PAYLOAD_BYTES, RdfStatement, new_rdf_statement_envelope};

pub(super) const GAP: &str = "ntriples-native-statement-evidence-unavailable";
pub(super) const PROJECTION_GAP: &str = "ntriples-whole-physical-utf8-source-required";

#[derive(Clone, Copy)]
pub(super) struct Statement {
    span: SourceSpan,
    terms: [SourceSpan; 3],
    payload_bytes: usize,
}

pub(super) struct Gap {
    pub(super) span: SourceSpan,
    pub(super) detail: &'static str,
}

#[derive(Default)]
pub(super) struct Plan {
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
        if request.language().as_str() != "ntriples" {
            return Ok(Self::default());
        }
        let mut plan = Self {
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
            plan.gap(source.span(), PROJECTION_GAP, 0)?;
            return Ok(plan);
        }
        let mut count = 0usize;
        for (index, fact) in output.facts().iter().enumerate() {
            check_periodically(index, cancellation)?;
            if metadata(fact) {
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
            if metadata(fact) {
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
            let Some(mut statement) = (!empty).then(|| prove(group, bytes, start, end)).flatten()
            else {
                plan.gap(line_span, GAP, metadata_bytes)?;
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
            let Ok(decoded) = RdfStatement::from_ntriples(
                source_for_span(source, statement.span),
                text,
                statement.terms,
            ) else {
                plan.gap(line_span, GAP, metadata_bytes)?;
                offset = full_end;
                continue;
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
            plan.gap(source.span(), GAP, metadata_bytes)?;
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
        let statement = RdfStatement::from_ntriples(
            source_for_span(request.source().source_ref(), proof.span),
            text,
            proof.terms,
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

fn metadata(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature && fact.syntax_kind().as_str().starts_with("ntriples.")
}

fn prove(facts: &[&SyntaxFact], bytes: &[u8], start: usize, end: usize) -> Option<Statement> {
    if facts.len() != 4 {
        return None;
    }
    let mut statement = None;
    let mut roles = [None; 3];
    for fact in facts {
        let slot = match fact.syntax_kind().as_str() {
            "ntriples.statement.expression" => &mut statement,
            "ntriples.subject.expression" => &mut roles[0],
            "ntriples.predicate.expression" => &mut roles[1],
            "ntriples.object.expression" => &mut roles[2],
            _ => return None,
        };
        if slot.replace(fact.span()).is_some() {
            return None;
        }
    }
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
    Some(Statement {
        span,
        terms: [roles[0]?, roles[1]?, roles[2]?],
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
        )
}
