//! Admit one complete ordered native rule program from mandatory parser proofs.
//! Rule metadata remains source-owned native IR; filesystem application and
//! nested/global Git context stay explicit gaps rather than inferred targets.

use std::{io, mem::size_of};

use super::*;
use rootlight_ir::{
    GITIGNORE_PROGRAM_NAMESPACE, GITIGNORE_PROGRAM_VERSION, GitignoreProgram, GitignoreRule,
    MAX_GITIGNORE_PROGRAM_PAYLOAD_BYTES, MAX_GITIGNORE_RULES, MAX_GITIGNORE_SOURCE_BYTES,
    new_gitignore_program_envelope,
};

pub(super) struct Gap {
    pub(super) span: SourceSpan,
    pub(super) detail: &'static str,
}

#[derive(Default)]
pub(super) struct Plan {
    program: Option<GitignoreProgram>,
    payload_bytes: usize,
    pub(super) gap: Option<Gap>,
    pub(super) modeled_peak: usize,
}

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "gitignore.program.expression" | "gitignore.rule.expression"
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
        if request.language().as_str() != "gitignore" {
            return Ok(Self::default());
        }
        let source = request.source().source_ref();
        let bytes = request.source().bytes();
        let Ok(text) = std::str::from_utf8(bytes) else {
            return Ok(Self::refuse(
                source.span(),
                "gitignore-whole-utf8-source-required",
            ));
        };
        if request.encoding().as_str() != "utf-8"
            || !request.included_ranges().is_empty()
            || source.span().start_byte() != 0
            || usize::try_from(source.span().end_byte()).ok() != Some(bytes.len())
        {
            return Ok(Self::refuse(
                source.span(),
                "gitignore-whole-utf8-source-required",
            ));
        }
        require_resource_limit(
            ResourceKind::ExtensionBytes,
            bytes.len(),
            MAX_GITIGNORE_SOURCE_BYTES,
        )?;
        let mut markers = output.facts().iter().filter(|fact| {
            fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "gitignore.program.expression"
        });
        let Some(marker) = markers.next() else {
            return Ok(Self::refuse(
                source.span(),
                "gitignore-native-program-proof-unavailable",
            ));
        };
        if markers.next().is_some() || marker.span() != source.span() {
            return Ok(Self::refuse(
                source.span(),
                "gitignore-native-program-proof-unavailable",
            ));
        }
        let rule_count = output
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == SyntaxFactKind::Signature
                    && fact.syntax_kind().as_str() == "gitignore.rule.expression"
            })
            .count();
        require_resource_limit(ResourceKind::Records, rule_count, MAX_GITIGNORE_RULES)?;
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
                    .checked_mul(size_of::<GitignoreRule>() * 8)
                    .ok_or(SinkError::AccountingOverflow)?,
            )?,
        )?;
        require_resource_limit(
            ResourceKind::ReportedMemoryBytes,
            peak,
            request.limits().max_reported_memory_bytes(),
        )?;
        let mut lines = Vec::new();
        lines
            .try_reserve_exact(rule_count)
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in output.facts() {
            cancellation.check()?;
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "gitignore.rule.expression"
            {
                if fact.parent() != Some(marker.local_id()) {
                    return Ok(Self::refuse(
                        fact.span(),
                        "gitignore-native-rule-proof-unavailable",
                    ));
                }
                lines.push(fact.span());
            }
        }
        lines.sort_unstable_by_key(|span| (span.start_byte(), span.end_byte()));
        let Ok(program) = GitignoreProgram::from_evidence(source.clone(), text, lines) else {
            return Ok(Self::refuse(
                source.span(),
                "gitignore-native-rule-inventory-unavailable",
            ));
        };
        let mut payload = PayloadBytes(0);
        serde_json::to_writer(&mut payload, &program)
            .map_err(|_| provider_failure("gitignore-payload-accounting"))?;
        require_resource_limit(
            ResourceKind::ExtensionBytes,
            payload.0,
            request
                .limits()
                .ir()
                .max_extension_payload_bytes
                .min(MAX_GITIGNORE_PROGRAM_PAYLOAD_BYTES),
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
                detail: "gitignore-repository-context-not-analyzed",
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
                GITIGNORE_PROGRAM_NAMESPACE.len(),
                GITIGNORE_PROGRAM_VERSION.len(),
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
        let envelope = new_gitignore_program_envelope(provenance, &program)
            .map_err(|_| provider_failure("gitignore-program-envelope"))?;
        if envelope.payload.len() != self.payload_bytes {
            return Err(provider_failure("gitignore-payload-accounting"));
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
            .ok_or_else(|| io::Error::other("Gitignore payload size overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
