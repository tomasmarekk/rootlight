//! Admit one complete ordered native rule program from mandatory parser proofs.
//! Rule metadata remains source-owned native IR; filesystem application and
//! repository policy and external permissions stay explicit gaps.

use std::{io, mem::size_of};

use super::*;
use rootlight_ir::{
    CODEOWNERS_PROGRAM_NAMESPACE, CODEOWNERS_PROGRAM_VERSION, CodeownersProgram, CodeownersRule,
    MAX_CODEOWNERS_ACTIVE_LINES, MAX_CODEOWNERS_PROGRAM_PAYLOAD_BYTES, MAX_CODEOWNERS_SOURCE_BYTES,
    new_codeowners_program_envelope,
};

pub(super) struct Gap {
    pub(super) span: SourceSpan,
    pub(super) detail: &'static str,
}

#[derive(Default)]
pub(super) struct Plan {
    program: Option<CodeownersProgram>,
    payload_bytes: usize,
    pub(super) gap: Option<Gap>,
    pub(super) modeled_peak: usize,
}

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "codeowners.program.expression"
                | "codeowners.rule.expression"
                | "codeowners.invalid.expression"
        )
}

impl Plan {
    pub(super) fn len(&self) -> usize {
        usize::from(self.program.is_some())
    }

    fn refuse(span: SourceSpan, detail: &'static str) -> Self {
        Self {
            gap: Some(Gap { span, detail }),
            ..Self::default()
        }
    }

    pub(super) fn new(
        request: &AnalysisRequest<'_>,
        output: &ParseOutput,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        if request.language().as_str() != "codeowners" {
            return Ok(Self::default());
        }
        let source = request.source().source_ref();
        let bytes = request.source().bytes();
        let Ok(text) = std::str::from_utf8(bytes) else {
            return Ok(Self::refuse(
                source.span(),
                "codeowners-whole-utf8-source-required",
            ));
        };
        if request.encoding().as_str() != "utf-8"
            || !request.included_ranges().is_empty()
            || source.span().start_byte() != 0
            || usize::try_from(source.span().end_byte()).ok() != Some(bytes.len())
        {
            return Ok(Self::refuse(
                source.span(),
                "codeowners-whole-utf8-source-required",
            ));
        }
        require_resource_limit(
            ResourceKind::ExtensionBytes,
            bytes.len(),
            MAX_CODEOWNERS_SOURCE_BYTES,
        )?;
        let mut markers = output.facts().iter().filter(|fact| {
            fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "codeowners.program.expression"
        });
        let Some(marker) = markers.next() else {
            return Ok(Self::refuse(
                source.span(),
                "codeowners-native-program-proof-unavailable",
            ));
        };
        if markers.next().is_some() || marker.span() != source.span() {
            return Ok(Self::refuse(
                source.span(),
                "codeowners-native-program-proof-unavailable",
            ));
        }
        let rule_count = output
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == SyntaxFactKind::Signature
                    && matches!(
                        fact.syntax_kind().as_str(),
                        "codeowners.rule.expression" | "codeowners.invalid.expression"
                    )
            })
            .count();
        require_resource_limit(
            ResourceKind::Records,
            rule_count,
            MAX_CODEOWNERS_ACTIVE_LINES,
        )?;
        let base = output
            .report()
            .resources()
            .reported_memory_bytes()
            .unwrap_or(0);
        let mut peak = checked_add(
            base,
            checked_add(
                bytes
                    .len()
                    .checked_mul(32)
                    .ok_or(SinkError::AccountingOverflow)?,
                rule_count
                    .checked_mul(size_of::<CodeownersRule>() * 8)
                    .ok_or(SinkError::AccountingOverflow)?,
            )?,
        )?;
        require_resource_limit(
            ResourceKind::ReportedMemoryBytes,
            peak,
            request.limits().max_reported_memory_bytes(),
        )?;
        let Ok(program) = CodeownersProgram::from_source(source.clone(), text) else {
            return Ok(Self::refuse(
                source.span(),
                "codeowners-native-rule-inventory-unavailable",
            ));
        };
        for (kind, expected) in [
            (
                "codeowners.rule.expression",
                program
                    .rules()
                    .iter()
                    .map(|rule| rule.line)
                    .collect::<Vec<_>>(),
            ),
            (
                "codeowners.invalid.expression",
                program
                    .invalid_lines()
                    .iter()
                    .map(|line| line.line)
                    .collect::<Vec<_>>(),
            ),
        ] {
            let mut observed = Vec::new();
            observed
                .try_reserve_exact(expected.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            for fact in output.facts() {
                cancellation.check()?;
                if fact.kind() == SyntaxFactKind::Signature && fact.syntax_kind().as_str() == kind {
                    if fact.parent() != Some(marker.local_id()) {
                        return Ok(Self::refuse(
                            fact.span(),
                            "codeowners-native-rule-proof-unavailable",
                        ));
                    }
                    observed.push(fact.span());
                }
            }
            observed.sort_unstable_by_key(|span| (span.start_byte(), span.end_byte()));
            if observed != expected {
                return Ok(Self::refuse(
                    source.span(),
                    "codeowners-native-rule-inventory-unavailable",
                ));
            }
        }
        let mut payload = PayloadBytes(0);
        serde_json::to_writer(&mut payload, &program)
            .map_err(|_| provider_failure("codeowners-payload-accounting"))?;
        require_resource_limit(
            ResourceKind::ExtensionBytes,
            payload.0,
            request
                .limits()
                .ir()
                .max_extension_payload_bytes
                .min(MAX_CODEOWNERS_PROGRAM_PAYLOAD_BYTES),
        )?;
        require_resource_limit(
            ResourceKind::ExtensionBytes,
            payload.0,
            request.limits().ir().max_total_extension_bytes,
        )?;
        peak = checked_add(
            peak,
            payload
                .0
                .checked_mul(8)
                .ok_or(SinkError::AccountingOverflow)?,
        )?;
        require_resource_limit(
            ResourceKind::ReportedMemoryBytes,
            peak,
            request.limits().max_reported_memory_bytes(),
        )?;
        cancellation.check()?;
        Ok(Self {
            program: Some(program),
            payload_bytes: payload.0,
            gap: Some(Gap {
                span: source.span(),
                detail: "codeowners-repository-application-and-owner-permissions-unverified",
            }),
            modeled_peak: peak,
        })
    }

    pub(super) fn account_strings(
        &self,
        total: &mut usize,
        limits: &IrLimits,
    ) -> Result<(), AdapterError> {
        if self.program.is_some() {
            for length in [
                CODEOWNERS_PROGRAM_NAMESPACE.len(),
                CODEOWNERS_PROGRAM_VERSION.len(),
            ] {
                account_string(total, length, limits)?;
            }
        }
        if let Some(gap) = &self.gap {
            account_string(total, gap.detail.len(), limits)?;
        }
        Ok(())
    }

    pub(super) fn envelope(
        &mut self,
        provenance: FactId,
    ) -> Result<Option<ExtensionEnvelope>, AdapterError> {
        let Some(program) = self.program.take() else {
            return Ok(None);
        };
        let envelope = new_codeowners_program_envelope(provenance, &program)
            .map_err(|_| provider_failure("codeowners-program-envelope"))?;
        if envelope.payload.len() != self.payload_bytes {
            return Err(provider_failure("codeowners-payload-accounting"));
        }
        Ok(Some(envelope))
    }
}

struct PayloadBytes(usize);
impl io::Write for PayloadBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("Codeowners payload size overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
