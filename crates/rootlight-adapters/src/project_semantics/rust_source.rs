//! Text access for physical Rust project syntax facts.
//! UTF-16 decoding reuses the shared validated view; only captured endpoints are
//! projected, while committed source references keep original encoded bytes.

use std::{borrow::Cow, mem::size_of};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, ResourceKind, SinkError, SourceText, SyntaxFact,
};
use rootlight_cancel::Cancellation;
use rootlight_ir::SourceSpan;

pub(super) struct RustSourceText<'a> {
    text: Cow<'a, str>,
    boundaries: Vec<(u64, usize)>,
}

impl<'a> RustSourceText<'a> {
    pub(super) fn new(
        bytes: &'a [u8],
        encoding: &str,
        facts: &[SyntaxFact],
        limits: &AnalysisLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let source = SourceText::parse(bytes, cancellation)?
            .filter(|source| source.encoding().as_str() == encoding)
            .ok_or_else(|| super::provider_failure("rust-project-source-encoding"))?;
        if let Some(text) = source.utf8() {
            return Ok(Self {
                text: Cow::Borrowed(text),
                boundaries: Vec::new(),
            });
        }

        let count = facts
            .len()
            .checked_mul(2)
            .ok_or(SinkError::AccountingOverflow)?;
        let mut boundaries = Vec::new();
        require_memory(
            count
                .checked_mul(size_of::<(u64, usize)>())
                .ok_or(SinkError::AccountingOverflow)?,
            limits,
        )?;
        boundaries
            .try_reserve_exact(count)
            .map_err(|_| SinkError::AllocationFailed)?;
        for (index, fact) in facts.iter().enumerate() {
            super::check_periodically(index, cancellation)?;
            boundaries.extend([(fact.span().start_byte(), 0), (fact.span().end_byte(), 0)]);
        }
        boundaries.sort_unstable_by_key(|entry| entry.0);
        boundaries.dedup_by_key(|entry| entry.0);
        let map_bytes = boundaries
            .capacity()
            .checked_mul(size_of::<(u64, usize)>())
            .ok_or(SinkError::AccountingOverflow)?;
        let mut projected_bytes = 0usize;
        for (index, character) in source.chars().enumerate() {
            super::check_periodically(index, cancellation)?;
            projected_bytes = projected_bytes
                .checked_add(character.len_utf8())
                .ok_or(SinkError::AccountingOverflow)?;
        }
        if projected_bytes > limits.max_source_bytes() {
            return Err(SinkError::StreamLimit {
                resource: ResourceKind::SourceBytes,
                observed: projected_bytes,
                limit: limits.max_source_bytes(),
            }
            .into());
        }
        require_memory(
            map_bytes
                .checked_add(projected_bytes)
                .ok_or(SinkError::AccountingOverflow)?,
            limits,
        )?;
        let mut text = String::new();
        text.try_reserve_exact(projected_bytes)
            .map_err(|_| SinkError::AllocationFailed)?;
        require_memory(
            map_bytes
                .checked_add(text.capacity())
                .ok_or(SinkError::AccountingOverflow)?,
            limits,
        )?;

        let mut next = 0;
        if boundaries.first().is_some_and(|entry| entry.0 == 0) {
            next = 1;
        }
        let mut physical = 2u64;
        for (index, character) in source.chars().enumerate() {
            super::check_periodically(index, cancellation)?;
            if let Some(entry) = boundaries.get_mut(next) {
                if entry.0 < physical {
                    return Err(super::provider_failure("rust-project-source-boundary"));
                }
                if entry.0 == physical {
                    entry.1 = text.len();
                    next += 1;
                }
            }
            text.push(character);
            physical = physical
                .checked_add(
                    u64::try_from(source.encoding().character_bytes(character))
                        .map_err(|_| SinkError::AccountingOverflow)?,
                )
                .ok_or(SinkError::AccountingOverflow)?;
        }
        if let Some(entry) = boundaries.get_mut(next)
            && entry.0 == physical
        {
            entry.1 = text.len();
            next += 1;
        }
        if next != boundaries.len() {
            return Err(super::provider_failure("rust-project-source-boundary"));
        }
        cancellation.check()?;
        Ok(Self {
            text: Cow::Owned(text),
            boundaries,
        })
    }

    pub(super) fn as_str(&self) -> &str {
        &self.text
    }

    pub(super) fn text(&self, span: SourceSpan) -> Option<&str> {
        let endpoint = |physical| {
            if self.boundaries.is_empty() {
                usize::try_from(physical).ok()
            } else {
                self.boundaries
                    .binary_search_by_key(&physical, |entry| entry.0)
                    .ok()
                    .map(|index| self.boundaries[index].1)
            }
        };
        self.text
            .get(endpoint(span.start_byte())?..endpoint(span.end_byte())?)
    }
}

fn require_memory(observed: usize, limits: &AnalysisLimits) -> Result<(), AdapterError> {
    if observed > limits.max_reported_memory_bytes() {
        Err(SinkError::StreamLimit {
            resource: ResourceKind::ReportedMemoryBytes,
            observed,
            limit: limits.max_reported_memory_bytes(),
        }
        .into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rootlight_adapter_sdk::{BatchThresholds, StreamLimits, SyntaxFactKind, SyntaxKindLabel};
    use rootlight_ids::FileId;
    use rootlight_ir::IrLimits;

    use super::*;

    fn limits(memory: usize) -> AnalysisLimits {
        let stream = StreamLimits::new(
            32,
            256,
            16_384,
            16,
            4096,
            4096,
            BatchThresholds::new(32, 4096, 4, 1024).unwrap(),
        )
        .unwrap();
        AnalysisLimits::new(
            4096,
            256,
            128,
            0,
            memory,
            stream.clone(),
            stream,
            IrLimits::default(),
        )
        .unwrap()
    }

    fn fact(start: u64, end: u64) -> SyntaxFact {
        SyntaxFact::new(
            start + 1,
            None,
            SyntaxFactKind::Occurrence,
            SourceSpan::new(FileId::from_bytes([1; 20]), start, end).unwrap(),
            0,
            SyntaxKindLabel::new("rust.value.reference").unwrap(),
        )
    }

    #[test]
    fn encoded_endpoints_retain_whole_scalar_spellings_and_bom_ownership() {
        let text = "λ😀x";
        let facts = [fact(0, 10), fact(2, 4), fact(4, 8), fact(8, 10)];
        for (encoding, bytes) in [
            (
                "utf-16le",
                [0xff, 0xfe]
                    .into_iter()
                    .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
                    .collect::<Vec<_>>(),
            ),
            (
                "utf-16be",
                [0xfe, 0xff]
                    .into_iter()
                    .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
                    .collect(),
            ),
        ] {
            let view = RustSourceText::new(
                &bytes,
                encoding,
                &facts,
                &limits(4096),
                &Cancellation::new(),
            )
            .unwrap();
            for (fact, expected) in facts.iter().zip([text, "λ", "😀", "x"]) {
                assert_eq!(view.text(fact.span()), Some(expected));
            }
        }
        let utf8 = "\u{feff}λ😀x";
        let view = RustSourceText::new(
            utf8.as_bytes(),
            "utf-8",
            &[],
            &limits(4096),
            &Cancellation::new(),
        )
        .unwrap();
        assert!(matches!(view.text, Cow::Borrowed(_)));
        assert_eq!(
            view.text(fact(0, u64::try_from(utf8.len()).unwrap()).span()),
            Some(utf8)
        );
    }

    #[test]
    fn encoded_endpoint_admission_rejects_partial_scalars_and_memory_exhaustion() {
        let bytes: Vec<_> = [0xff, 0xfe]
            .into_iter()
            .chain("😀".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert!(matches!(
            RustSourceText::new(
                &bytes,
                "utf-16le",
                &[fact(2, 4)],
                &limits(4096),
                &Cancellation::new()
            ),
            Err(AdapterError::ProviderFailed { .. })
        ));
        assert!(matches!(
            RustSourceText::new(
                &bytes,
                "utf-16le",
                &[fact(2, 6)],
                &limits(1),
                &Cancellation::new()
            ),
            Err(AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::ReportedMemoryBytes,
                ..
            }))
        ));
    }

    #[test]
    fn encoded_source_validation_preserves_cancellation_and_encoding_errors() {
        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            RustSourceText::new(b"source", "utf-8", &[], &limits(4096), &cancelled),
            Err(AdapterError::Cancelled { .. })
        ));
        for bytes in [
            vec![0xff, 0xfe, 0x00],
            vec![0xff, 0xfe, 0x00, 0xd8],
            vec![0x61, 0x00],
        ] {
            assert!(matches!(
                RustSourceText::new(&bytes, "utf-16le", &[], &limits(4096), &Cancellation::new()),
                Err(AdapterError::ProviderFailed { .. })
            ));
        }
    }
}
