//! Private UTF-8 parser input with exact original-byte source coordinates.
//! Encoded input keeps its BOM and maps scalar-width runs; physical snapshots
//! and committed evidence never acquire projected bytes under their hashes.

use std::{borrow::Cow, fmt, mem::size_of};

use rootlight_adapter_sdk::{AdapterError, DiagnosticCode, RequestError, ResourceKind, SinkError};
use rootlight_cancel::Cancellation;
use rootlight_ir::{SourceSpan, SourceTextEncoding};

const CANCELLATION_SCALAR_INTERVAL: usize = 256;

/// Binds retained/native parse identity to the exact coordinate projection.
pub(crate) const NATIVE_SOURCE_PROJECTION_POLICY: &str = "rootlight.treesitter.source-projection/1";

/// A validated contiguous parser view, borrowing the ordinary UTF-8 path.
pub(crate) struct NativeSourceView<'a> {
    text: Cow<'a, str>,
    encoding: Option<SourceTextEncoding>,
    physical_bytes: usize,
    runs: Vec<ScalarWidthRun>,
    owned_bytes: usize,
}

#[derive(Debug)]
struct ScalarWidthRun {
    physical_start: usize,
    projected_start: usize,
    scalar_count: usize,
    projected_width: usize,
}

impl ScalarWidthRun {
    const fn physical_width(&self) -> usize {
        if self.projected_width == 4 { 4 } else { 2 }
    }
}

struct ProjectionPlan {
    projected_bytes: usize,
    runs: usize,
}

impl<'a> NativeSourceView<'a> {
    /// Projects a scalar-complete prefix under both physical and UTF-8 byte caps.
    ///
    /// The borrowed prefix remains part of the original snapshot; no projected
    /// bytes acquire its hash. A budget-split scalar is omitted only when the
    /// original source continues beyond that boundary.
    ///
    /// # Errors
    /// Returns encoding, scalar-boundary, allocation, memory or cancellation
    /// errors without treating malformed true EOF as a sampled scalar split.
    pub(crate) fn bounded_prefix(
        raw: &'a [u8],
        encoding: &str,
        maximum: usize,
        max_owned_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let end = bounded_prefix_end(raw, encoding, maximum, cancellation)?;
        let prefix = raw
            .get(..end)
            .ok_or_else(|| source_failure("native-source-boundary"))?;
        Self::new(prefix, encoding, maximum, max_owned_bytes, cancellation)
    }

    pub(crate) const fn physical_bytes(&self) -> usize {
        self.physical_bytes
    }

    /// Admits strict BOM UTF-16 or borrows validated UTF-8 without copying it.
    ///
    /// The limits bound projected bytes and the combined owned string/map
    /// capacities, not the native parser allocator or the borrowed snapshot.
    /// UTF-16 rejects NUL, malformed scalars and missing/mismatched BOMs.
    ///
    /// # Errors
    /// Returns source-free admission, allocation, accounting or cancellation
    /// errors before retaining an invalid or over-budget projection.
    pub(crate) fn new(
        raw: &'a [u8],
        encoding: &str,
        max_projected_bytes: usize,
        max_owned_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        if encoding == "utf-8" {
            require_limit(ResourceKind::SourceBytes, raw.len(), max_projected_bytes)?;
            let text = std::str::from_utf8(raw).map_err(|_| source_failure("invalid-utf8"))?;
            cancellation.check()?;
            return Ok(Self {
                text: Cow::Borrowed(text),
                encoding: None,
                physical_bytes: raw.len(),
                runs: Vec::new(),
                owned_bytes: 0,
            });
        }
        let encoding = match encoding {
            "utf-16le" => SourceTextEncoding::Utf16Le,
            "utf-16be" => SourceTextEncoding::Utf16Be,
            _ => return Err(RequestError::UnsupportedEncoding.into()),
        };
        let expected_bom = match encoding {
            SourceTextEncoding::Utf16Le => [0xff, 0xfe],
            SourceTextEncoding::Utf16Be => [0xfe, 0xff],
        };
        if !raw.len().is_multiple_of(2) || !raw.starts_with(&expected_bom) {
            return Err(source_failure("native-source-invalid-utf16"));
        }
        let plan = preflight_utf16(
            raw,
            encoding,
            max_projected_bytes,
            max_owned_bytes,
            cancellation,
        )?;
        cancellation.check()?;
        let mut runs = Vec::<ScalarWidthRun>::new();
        runs.try_reserve_exact(plan.runs)
            .map_err(|_| SinkError::AllocationFailed)?;
        // Exact reservation is a request, not an allocator guarantee. Admit
        // the returned capacity before reserving the decoded owner as well.
        require_limit(
            ResourceKind::ReportedMemoryBytes,
            projection_owned_bytes(plan.projected_bytes, runs.capacity())?,
            max_owned_bytes,
        )?;
        cancellation.check()?;
        let mut text = String::new();
        text.try_reserve_exact(plan.projected_bytes)
            .map_err(|_| SinkError::AllocationFailed)?;
        let owned_bytes = projection_owned_bytes(text.capacity(), runs.capacity())?;
        require_limit(
            ResourceKind::ReportedMemoryBytes,
            owned_bytes,
            max_owned_bytes,
        )?;
        cancellation.check()?;
        let mut physical_cursor = 0usize;
        for (index, scalar) in utf16_scalars(raw, encoding).enumerate() {
            check_periodically(index, cancellation)?;
            let scalar = strict_scalar(scalar)?;
            let width = scalar.len_utf8();
            let next_projected = text
                .len()
                .checked_add(width)
                .ok_or(SinkError::AccountingOverflow)?;
            if next_projected > plan.projected_bytes {
                return Err(source_failure("native-source-projection-invariant"));
            }
            if let Some(run) = runs.last_mut().filter(|run| run.projected_width == width) {
                run.scalar_count = run
                    .scalar_count
                    .checked_add(1)
                    .ok_or(SinkError::AccountingOverflow)?;
            } else {
                if runs.len() >= plan.runs {
                    return Err(source_failure("native-source-projection-invariant"));
                }
                runs.push(ScalarWidthRun {
                    physical_start: physical_cursor,
                    projected_start: text.len(),
                    scalar_count: 1,
                    projected_width: width,
                });
            }
            text.push(scalar);
            physical_cursor = physical_cursor
                .checked_add(scalar.len_utf16() * 2)
                .ok_or(SinkError::AccountingOverflow)?;
        }
        cancellation.check()?;
        if physical_cursor != raw.len()
            || text.len() != plan.projected_bytes
            || runs.len() != plan.runs
        {
            return Err(source_failure("native-source-projection-invariant"));
        }
        Ok(Self {
            text: Cow::Owned(text),
            encoding: Some(encoding),
            physical_bytes: raw.len(),
            runs,
            owned_bytes,
        })
    }

    /// Borrows the complete parser/query UTF-8 input, including an admitted BOM.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.text.as_bytes()
    }

    /// Borrows the same complete UTF-8 view used for parser byte coordinates.
    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }

    /// Maps a projected scalar boundary to its exact original byte offset.
    ///
    /// # Errors
    /// Rejects out-of-range offsets and interior UTF-8 scalar bytes.
    pub(crate) fn to_physical_offset(&self, projected: usize) -> Result<usize, AdapterError> {
        if !self.text.is_char_boundary(projected) {
            return Err(source_failure("native-source-boundary"));
        }
        if self.encoding.is_none() {
            return Ok(projected);
        }
        if projected == self.text.len() {
            return Ok(self.physical_bytes);
        }
        let index = self
            .runs
            .partition_point(|run| run.projected_start <= projected)
            .checked_sub(1)
            .ok_or_else(|| source_failure("native-source-boundary"))?;
        let run = self
            .runs
            .get(index)
            .ok_or_else(|| source_failure("native-source-boundary"))?;
        map_run_offset(
            projected,
            run.projected_start,
            run.projected_width,
            run.physical_start,
            run.physical_width(),
            run.scalar_count,
        )
    }

    /// Maps an original scalar boundary to its exact projected UTF-8 offset.
    ///
    /// # Errors
    /// Rejects out-of-range, odd UTF-16 and split-surrogate offsets.
    pub(crate) fn to_projected_offset(&self, physical: usize) -> Result<usize, AdapterError> {
        if self.encoding.is_none() {
            return self
                .text
                .is_char_boundary(physical)
                .then_some(physical)
                .ok_or_else(|| source_failure("native-source-boundary"));
        }
        if physical > self.physical_bytes {
            return Err(source_failure("native-source-boundary"));
        }
        if physical == self.physical_bytes {
            return Ok(self.text.len());
        }
        let index = self
            .runs
            .partition_point(|run| run.physical_start <= physical)
            .checked_sub(1)
            .ok_or_else(|| source_failure("native-source-boundary"))?;
        let run = self
            .runs
            .get(index)
            .ok_or_else(|| source_failure("native-source-boundary"))?;
        map_run_offset(
            physical,
            run.physical_start,
            run.physical_width(),
            run.projected_start,
            run.projected_width,
            run.scalar_count,
        )
    }

    /// Maps a projected span without changing its file identity or endpoints.
    ///
    /// # Errors
    /// Rejects non-scalar endpoints, unrepresentable offsets or invalid spans.
    pub(crate) fn physical_span(&self, span: SourceSpan) -> Result<SourceSpan, AdapterError> {
        let start = usize::try_from(span.start_byte()).map_err(|_| SinkError::SourceMismatch)?;
        let end = usize::try_from(span.end_byte()).map_err(|_| SinkError::SourceMismatch)?;
        mapped_span(
            span,
            self.to_physical_offset(start)?,
            self.to_physical_offset(end)?,
        )
    }

    /// Maps a physical span to the private parser/query coordinate space.
    ///
    /// # Errors
    /// Rejects non-scalar endpoints, unrepresentable offsets or invalid spans.
    pub(crate) fn projected_span(&self, span: SourceSpan) -> Result<SourceSpan, AdapterError> {
        let start = usize::try_from(span.start_byte()).map_err(|_| SinkError::SourceMismatch)?;
        let end = usize::try_from(span.end_byte()).map_err(|_| SinkError::SourceMismatch)?;
        mapped_span(
            span,
            self.to_projected_offset(start)?,
            self.to_projected_offset(end)?,
        )
    }

    /// Identifies the physical encoded path; `None` denotes borrowed UTF-8.
    pub(crate) const fn encoding(&self) -> Option<SourceTextEncoding> {
        self.encoding
    }

    /// Returns the admitted string/map capacity charge, excluding borrowed input.
    pub(crate) const fn owned_bytes(&self) -> usize {
        self.owned_bytes
    }
}

fn bounded_prefix_end(
    raw: &[u8],
    encoding: &str,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    cancellation.check()?;
    let end = raw.len().min(maximum);
    if encoding == "utf-8" {
        let prefix = raw
            .get(..end)
            .ok_or_else(|| source_failure("native-source-boundary"))?;
        return match std::str::from_utf8(prefix) {
            Ok(_) => Ok(end),
            Err(error) if error.error_len().is_none() && end < raw.len() => Ok(error.valid_up_to()),
            Err(_) => Err(source_failure("invalid-utf8")),
        };
    }
    let encoding = match encoding {
        "utf-16le" => SourceTextEncoding::Utf16Le,
        "utf-16be" => SourceTextEncoding::Utf16Be,
        _ => return Err(RequestError::UnsupportedEncoding.into()),
    };
    let bom = match encoding {
        SourceTextEncoding::Utf16Le => [0xff, 0xfe],
        SourceTextEncoding::Utf16Be => [0xfe, 0xff],
    };
    if !raw.starts_with(&bom) || (end == raw.len() && !end.is_multiple_of(2)) {
        return Err(source_failure("native-source-invalid-utf16"));
    }
    let end = end - end % 2;
    let prefix = raw
        .get(..end)
        .ok_or_else(|| source_failure("native-source-boundary"))?;
    let mut physical = 0usize;
    let mut projected = 0usize;
    for (index, scalar) in utf16_scalars(prefix, encoding).enumerate() {
        check_periodically(index, cancellation)?;
        let scalar = match scalar {
            Ok(scalar) => strict_scalar(Ok(scalar))?,
            Err(error)
                if physical.checked_add(2) == Some(end)
                    && end < raw.len()
                    && (0xd800..=0xdbff).contains(&error.unpaired_surrogate()) =>
            {
                break;
            }
            Err(_) => return Err(source_failure("native-source-invalid-utf16")),
        };
        let next = projected
            .checked_add(scalar.len_utf8())
            .ok_or(SinkError::AccountingOverflow)?;
        if next > maximum {
            break;
        }
        projected = next;
        physical = physical
            .checked_add(scalar.len_utf16() * 2)
            .ok_or(SinkError::AccountingOverflow)?;
    }
    cancellation.check()?;
    Ok(physical)
}

impl fmt::Debug for NativeSourceView<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeSourceView")
            .field("encoding", &self.encoding)
            .field("physical_bytes", &self.physical_bytes)
            .field("projected_bytes", &self.text.len())
            .field("mapping_runs", &self.runs.len())
            .field("owned_bytes", &self.owned_bytes)
            .finish()
    }
}

fn preflight_utf16(
    raw: &[u8],
    encoding: SourceTextEncoding,
    max_projected_bytes: usize,
    max_owned_bytes: usize,
    cancellation: &Cancellation,
) -> Result<ProjectionPlan, AdapterError> {
    let mut plan = ProjectionPlan {
        projected_bytes: 0,
        runs: 0,
    };
    let mut previous_width = None;
    for (index, scalar) in utf16_scalars(raw, encoding).enumerate() {
        check_periodically(index, cancellation)?;
        let width = strict_scalar(scalar)?.len_utf8();
        plan.projected_bytes = plan
            .projected_bytes
            .checked_add(width)
            .ok_or(SinkError::AccountingOverflow)?;
        if previous_width != Some(width) {
            plan.runs = plan
                .runs
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?;
            previous_width = Some(width);
        }
        require_limit(
            ResourceKind::SourceBytes,
            plan.projected_bytes,
            max_projected_bytes,
        )?;
        require_limit(
            ResourceKind::ReportedMemoryBytes,
            projection_owned_bytes(plan.projected_bytes, plan.runs)?,
            max_owned_bytes,
        )?;
    }
    cancellation.check()?;
    Ok(plan)
}

fn utf16_scalars(
    raw: &[u8],
    encoding: SourceTextEncoding,
) -> impl Iterator<Item = Result<char, std::char::DecodeUtf16Error>> + '_ {
    char::decode_utf16(
        raw.as_chunks::<2>()
            .0
            .iter()
            .map(move |unit| match encoding {
                SourceTextEncoding::Utf16Le => u16::from_le_bytes(*unit),
                SourceTextEncoding::Utf16Be => u16::from_be_bytes(*unit),
            }),
    )
}

fn strict_scalar(scalar: Result<char, std::char::DecodeUtf16Error>) -> Result<char, AdapterError> {
    let scalar = scalar.map_err(|_| source_failure("native-source-invalid-utf16"))?;
    if scalar == '\0' {
        return Err(source_failure("native-source-invalid-utf16"));
    }
    Ok(scalar)
}

fn projection_owned_bytes(
    text_capacity: usize,
    run_capacity: usize,
) -> Result<usize, AdapterError> {
    run_capacity
        .checked_mul(size_of::<ScalarWidthRun>())
        .and_then(|bytes| bytes.checked_add(text_capacity))
        .ok_or_else(|| SinkError::AccountingOverflow.into())
}

fn map_run_offset(
    offset: usize,
    source_start: usize,
    source_width: usize,
    target_start: usize,
    target_width: usize,
    scalar_count: usize,
) -> Result<usize, AdapterError> {
    let relative = offset
        .checked_sub(source_start)
        .ok_or_else(|| source_failure("native-source-boundary"))?;
    if !relative.is_multiple_of(source_width) {
        return Err(source_failure("native-source-boundary"));
    }
    let scalars = relative / source_width;
    if scalars >= scalar_count {
        return Err(source_failure("native-source-boundary"));
    }
    scalars
        .checked_mul(target_width)
        .and_then(|bytes| target_start.checked_add(bytes))
        .ok_or_else(|| SinkError::AccountingOverflow.into())
}

fn mapped_span(original: SourceSpan, start: usize, end: usize) -> Result<SourceSpan, AdapterError> {
    let start = u64::try_from(start).map_err(|_| SinkError::SourceMismatch)?;
    let end = u64::try_from(end).map_err(|_| SinkError::SourceMismatch)?;
    SourceSpan::new(original.file(), start, end).map_err(|_| SinkError::SourceMismatch.into())
}

fn require_limit(
    resource: ResourceKind,
    observed: usize,
    limit: usize,
) -> Result<(), AdapterError> {
    if observed > limit {
        return Err(SinkError::StreamLimit {
            resource,
            observed,
            limit,
        }
        .into());
    }
    Ok(())
}

fn check_periodically(index: usize, cancellation: &Cancellation) -> Result<(), AdapterError> {
    if index.is_multiple_of(CANCELLATION_SCALAR_INTERVAL) {
        cancellation.check()?;
    }
    Ok(())
}

fn source_failure(code: &'static str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(code).expect("hard-coded diagnostic code is valid"),
    }
}

#[cfg(test)]
mod tests {
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::FileId;

    use super::*;

    fn encoded(text: &str, encoding: SourceTextEncoding) -> Vec<u8> {
        text.encode_utf16()
            .flat_map(|unit| match encoding {
                SourceTextEncoding::Utf16Le => unit.to_le_bytes(),
                SourceTextEncoding::Utf16Be => unit.to_be_bytes(),
            })
            .collect()
    }

    fn encoded_view(raw: &[u8], encoding: SourceTextEncoding) -> NativeSourceView<'_> {
        NativeSourceView::new(
            raw,
            encoding.as_str(),
            1 << 20,
            1 << 20,
            &Cancellation::new(),
        )
        .expect("neutral encoded source is admitted")
    }

    #[test]
    fn bounded_prefix_preserves_eof_and_omits_only_budget_split_scalars() {
        let cancellation = Cancellation::new();
        let text = "a🧭z";
        let short = NativeSourceView::bounded_prefix(text.as_bytes(), "utf-8", 4, 0, &cancellation)
            .unwrap();
        assert_eq!(short.as_str(), "a");
        assert_eq!(short.physical_bytes(), 1);
        let whole = NativeSourceView::bounded_prefix(
            text.as_bytes(),
            "utf-8",
            text.len(),
            0,
            &cancellation,
        )
        .unwrap();
        assert_eq!(whole.as_str(), text);
        assert_eq!(whole.physical_bytes(), text.len());
        for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
            let raw = encoded("\u{feff}a🧭z", encoding);
            let short =
                NativeSourceView::bounded_prefix(&raw, encoding.as_str(), 6, 1024, &cancellation)
                    .unwrap();
            assert_eq!(short.as_str(), "\u{feff}a");
            assert_eq!(short.physical_bytes(), 4);
            let whole = NativeSourceView::bounded_prefix(
                &raw,
                encoding.as_str(),
                raw.len(),
                1024,
                &cancellation,
            )
            .unwrap();
            assert_eq!(whole.as_str(), "\u{feff}a🧭z");
            assert_eq!(whole.physical_bytes(), raw.len());
        }
    }

    #[test]
    fn bounded_prefix_caps_utf16_expansion_and_keeps_original_mapping() {
        for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
            let raw = encoded("\u{feff}中界x", encoding);
            let view = NativeSourceView::bounded_prefix(
                &raw,
                encoding.as_str(),
                8,
                1024,
                &Cancellation::new(),
            )
            .unwrap();
            assert_eq!(view.as_str(), "\u{feff}中");
            assert_eq!(view.physical_bytes(), 4);
            assert_eq!(view.as_bytes().len(), 6);
            assert_eq!(view.to_physical_offset(3).unwrap(), 2);
            assert_eq!(view.to_physical_offset(6).unwrap(), 4);
        }
    }

    #[test]
    fn bounded_prefix_keeps_malformed_eof_resource_and_cancellation_outcomes() {
        let cancellation = Cancellation::new();
        for bytes in [b"a\xffz".as_slice(), b"\xe2".as_slice()] {
            assert!(
                NativeSourceView::bounded_prefix(bytes, "utf-8", bytes.len(), 0, &cancellation)
                    .is_err()
            );
        }
        for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
            let mut raw = encoded("\u{feff}a", encoding);
            raw.extend(match encoding {
                SourceTextEncoding::Utf16Le => 0xd800_u16.to_le_bytes(),
                SourceTextEncoding::Utf16Be => 0xd800_u16.to_be_bytes(),
            });
            assert!(
                NativeSourceView::bounded_prefix(
                    &raw,
                    encoding.as_str(),
                    raw.len(),
                    1024,
                    &cancellation
                )
                .is_err()
            );
            let raw = encoded("\u{feff}a", encoding);
            assert!(matches!(
                NativeSourceView::bounded_prefix(
                    &raw,
                    encoding.as_str(),
                    raw.len(),
                    1,
                    &cancellation
                ),
                Err(AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::ReportedMemoryBytes,
                    ..
                }))
            ));
        }
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            NativeSourceView::bounded_prefix(b"a", "utf-8", 1, 0, &cancellation),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }

    #[test]
    fn utf8_borrows_and_preserves_exact_scalar_boundaries() {
        let text = "\u{feff}ASCII\r\né中🧭";
        let view = NativeSourceView::new(
            text.as_bytes(),
            "utf-8",
            text.len(),
            0,
            &Cancellation::new(),
        )
        .unwrap();
        assert!(matches!(view.text, Cow::Borrowed(_)));
        assert_eq!(view.as_bytes().as_ptr(), text.as_bytes().as_ptr());
        assert_eq!(view.as_str(), text);
        assert_eq!(view.encoding(), None);
        assert_eq!(view.owned_bytes(), 0);
        for offset in 0..=text.len() + 1 {
            if text.is_char_boundary(offset) {
                assert_eq!(view.to_physical_offset(offset).unwrap(), offset);
                assert_eq!(view.to_projected_offset(offset).unwrap(), offset);
            } else {
                assert!(view.to_physical_offset(offset).is_err());
                assert!(view.to_projected_offset(offset).is_err());
            }
        }
    }

    #[test]
    fn utf16_both_endiannesses_map_every_scalar_and_reject_every_interior_byte() {
        let text = "\u{feff}ASCII\r\néΩ中界🧭🜁xé中🧭";
        for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
            let raw = encoded(text, encoding);
            let view = encoded_view(&raw, encoding);
            assert_eq!(view.as_str(), text);
            assert_eq!(view.encoding(), Some(encoding));
            let mut physical = 0;
            let mut pairs = vec![(0, 0)];
            for (projected, scalar) in text.char_indices() {
                assert_eq!(view.to_physical_offset(projected).unwrap(), physical);
                assert_eq!(view.to_projected_offset(physical).unwrap(), projected);
                physical += scalar.len_utf16() * 2;
                pairs.push((physical, projected + scalar.len_utf8()));
            }
            for offset in 0..=raw.len() + 1 {
                match pairs.iter().find(|(physical, _)| *physical == offset) {
                    Some((_, projected)) => {
                        assert_eq!(view.to_projected_offset(offset).unwrap(), *projected)
                    }
                    None => assert!(view.to_projected_offset(offset).is_err()),
                }
            }
            for offset in 0..=text.len() + 1 {
                match pairs.iter().find(|(_, projected)| *projected == offset) {
                    Some((physical, _)) => {
                        assert_eq!(view.to_physical_offset(offset).unwrap(), *physical)
                    }
                    None => assert!(view.to_physical_offset(offset).is_err()),
                }
            }
            assert_eq!(view.to_physical_offset(3).unwrap(), 2);
            assert_eq!(view.to_projected_offset(2).unwrap(), 3);
        }
    }

    #[test]
    fn utf16_bom_only_and_spans_keep_exact_original_file_identity() {
        let file = FileId::from_bytes([7; 20]);
        for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
            let raw = encoded("\u{feff}", encoding);
            let view = encoded_view(&raw, encoding);
            assert_eq!(view.as_bytes(), b"\xef\xbb\xbf");
            assert_eq!(view.runs.len(), 1);
            let physical = SourceSpan::new(file, 0, 2).unwrap();
            let projected = SourceSpan::new(file, 0, 3).unwrap();
            assert_eq!(view.projected_span(physical).unwrap(), projected);
            assert_eq!(view.physical_span(projected).unwrap(), physical);
            assert!(
                view.projected_span(SourceSpan::new(file, 1, 2).unwrap())
                    .is_err()
            );
            assert!(
                view.physical_span(SourceSpan::new(file, 0, 1).unwrap())
                    .is_err()
            );
            assert!(
                view.projected_span(SourceSpan::new(file, 0, u64::MAX).unwrap())
                    .is_err()
            );
        }
    }

    #[test]
    fn utf16_sparse_runs_coalesce_ascii_and_equal_scalar_widths() {
        let text = format!("\u{feff}{}éΩ中界🧭🜁", "a".repeat(32_768));
        let raw = encoded(&text, SourceTextEncoding::Utf16Le);
        let view = encoded_view(&raw, SourceTextEncoding::Utf16Le);
        assert_eq!(view.runs.len(), 5);
        assert_eq!(view.runs.get(1).unwrap().scalar_count, 32_768);
        assert!(view.owned_bytes() < raw.len());
        let same_width = encoded("\u{feff}中界", SourceTextEncoding::Utf16Be);
        assert_eq!(
            encoded_view(&same_width, SourceTextEncoding::Utf16Be)
                .runs
                .len(),
            1
        );
    }

    #[test]
    fn utf16_malformed_boms_nul_and_unpaired_surrogate_tails_fail_closed() {
        for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
            let bom = encoded("\u{feff}", encoding);
            let mut odd_tail = bom.clone();
            odd_tail.push(0x61);
            let mut nul = bom.clone();
            nul.extend(encoded("\0", encoding));
            let mut bad = vec![Vec::new(), encoded("a", encoding), odd_tail, nul];
            for units in [
                vec![0xd800],
                vec![0xdc00],
                vec![0xd800, 0x0061],
                vec![0xd800, 0xd800],
            ] {
                let mut raw = bom.clone();
                raw.extend(units.into_iter().flat_map(|unit: u16| match encoding {
                    SourceTextEncoding::Utf16Le => unit.to_le_bytes(),
                    SourceTextEncoding::Utf16Be => unit.to_be_bytes(),
                }));
                bad.push(raw);
            }
            let opposite = match encoding {
                SourceTextEncoding::Utf16Le => SourceTextEncoding::Utf16Be,
                SourceTextEncoding::Utf16Be => SourceTextEncoding::Utf16Le,
            };
            bad.push(encoded("\u{feff}a", opposite));
            for raw in bad {
                assert!(
                    matches!(NativeSourceView::new(&raw, encoding.as_str(), usize::MAX,
                    usize::MAX, &Cancellation::new()), Err(AdapterError::ProviderFailed { code })
                    if code.as_str() == "native-source-invalid-utf16")
                );
            }
        }
    }

    #[test]
    fn source_projection_limits_include_expansion_and_fragmented_mapping_storage() {
        let raw = encoded("\u{feff}中界", SourceTextEncoding::Utf16Le);
        let cancellation = Cancellation::new();
        let full = encoded_view(&raw, SourceTextEncoding::Utf16Le);
        let charge = full.owned_bytes();
        assert_eq!(full.as_bytes().len(), 9);
        assert!(matches!(
            NativeSourceView::new(&raw, "utf-16le", 8, charge, &cancellation),
            Err(AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::SourceBytes,
                ..
            }))
        ));
        assert!(matches!(
            NativeSourceView::new(&raw, "utf-16le", 9, charge - 1, &cancellation),
            Err(AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::ReportedMemoryBytes,
                ..
            }))
        ));
        assert_eq!(
            NativeSourceView::new(&raw, "utf-16le", 9, charge, &cancellation)
                .unwrap()
                .owned_bytes(),
            charge
        );
        let fragmented = encoded("\u{feff}aé中🧭aé中🧭", SourceTextEncoding::Utf16Le);
        let planned = preflight_utf16(
            &fragmented,
            SourceTextEncoding::Utf16Le,
            usize::MAX,
            usize::MAX,
            &cancellation,
        )
        .unwrap();
        assert_eq!(planned.runs, 9);
        let required = projection_owned_bytes(planned.projected_bytes, planned.runs).unwrap();
        assert!(
            NativeSourceView::new(
                &fragmented,
                "utf-16le",
                usize::MAX,
                required - 1,
                &cancellation
            )
            .is_err()
        );
        assert!(matches!(
            NativeSourceView::new(b"abc", "utf-8", 2, 0, &cancellation),
            Err(AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::SourceBytes,
                ..
            }))
        ));
    }

    #[test]
    fn source_projection_cancellation_and_unsupported_inputs_are_source_free() {
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        let raw = encoded("\u{feff}private_marker", SourceTextEncoding::Utf16Le);
        assert!(matches!(
            NativeSourceView::new(&raw, "utf-16le", usize::MAX, usize::MAX, &cancellation),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest,
            })
        ));
        let active = Cancellation::new();
        assert!(matches!(
            NativeSourceView::new(&raw, "utf-32le", usize::MAX, usize::MAX, &active),
            Err(AdapterError::RejectedRequest(
                RequestError::UnsupportedEncoding
            ))
        ));
        assert!(matches!(NativeSourceView::new(&[0xff], "utf-8", usize::MAX,
            0, &active), Err(AdapterError::ProviderFailed { code }) if code.as_str() == "invalid-utf8"));
        let debug = format!("{:?}", encoded_view(&raw, SourceTextEncoding::Utf16Le));
        assert!(!debug.contains("private_marker"));
        assert!(!debug.contains("\\u{feff}"));
    }
}
