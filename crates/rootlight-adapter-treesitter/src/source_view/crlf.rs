//! Explicit file-text projection with exact physical newline ownership.
//! Only paired CRLF is folded; source snapshots, lone CR and BOMs remain exact.

use super::*;

#[derive(Clone, Copy)]
struct ProjectedScalar {
    value: char,
    physical_width: u8,
    projected_width: u8,
    paired: bool,
}

struct FileTextPlan {
    projected_bytes: usize,
    physical_bytes: usize,
    runs: usize,
    has_pairs: bool,
}

pub(super) fn new<'a>(
    raw: &'a [u8],
    encoding: &str,
    maximum: usize,
    max_owned_bytes: usize,
    cancellation: &Cancellation,
) -> Result<NativeSourceView<'a>, AdapterError> {
    cancellation.check()?;
    if encoding == "utf-8" {
        // Safe borrowing retains the existing whole-buffer standard-library
        // validation boundary; custom projection passes poll independently.
        let text = std::str::from_utf8(raw).map_err(|_| source_failure("invalid-utf8"))?;
        cancellation.check()?;
        return build(
            raw,
            Some(text),
            None,
            || text.chars().map(Ok),
            maximum,
            max_owned_bytes,
            cancellation,
        );
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
    if !raw.len().is_multiple_of(2) || !raw.starts_with(&bom) {
        return Err(source_failure("native-source-invalid-utf16"));
    }
    build(
        raw,
        None,
        Some(encoding),
        || utf16_scalars(raw, encoding).map(strict_scalar),
        maximum,
        max_owned_bytes,
        cancellation,
    )
}

fn walk<I>(
    scalars: I,
    encoded: bool,
    cancellation: &Cancellation,
    mut visit: impl FnMut(ProjectedScalar) -> Result<(), AdapterError>,
) -> Result<(), AdapterError>
where
    I: Iterator<Item = Result<char, AdapterError>>,
{
    cancellation.check()?;
    let mut scalars = scalars.peekable();
    let mut index = 0usize;
    while let Some(scalar) = scalars.next() {
        check_periodically(index, cancellation)?;
        let mut value = scalar?;
        let width = if encoded {
            value.len_utf16() * 2
        } else {
            value.len_utf8()
        };
        let mut physical_width = u8::try_from(width).map_err(|_| SinkError::AccountingOverflow)?;
        let paired = value == '\r' && matches!(scalars.peek(), Some(Ok('\n')));
        if paired {
            let _ = scalars.next();
            physical_width = physical_width
                .checked_add(if encoded { 2 } else { 1 })
                .ok_or(SinkError::AccountingOverflow)?;
            value = '\n';
        }
        visit(ProjectedScalar {
            value,
            physical_width,
            projected_width: u8::try_from(value.len_utf8())
                .map_err(|_| SinkError::AccountingOverflow)?,
            paired,
        })?;
        index = index.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
    }
    cancellation.check()?;
    Ok(())
}

fn plan<I>(
    scalars: I,
    encoded: bool,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<FileTextPlan, AdapterError>
where
    I: Iterator<Item = Result<char, AdapterError>>,
{
    let mut plan = FileTextPlan {
        projected_bytes: 0,
        physical_bytes: 0,
        runs: 0,
        has_pairs: false,
    };
    let mut previous = None;
    walk(scalars, encoded, cancellation, |scalar| {
        plan.projected_bytes = plan
            .projected_bytes
            .checked_add(usize::from(scalar.projected_width))
            .ok_or(SinkError::AccountingOverflow)?;
        plan.physical_bytes = plan
            .physical_bytes
            .checked_add(usize::from(scalar.physical_width))
            .ok_or(SinkError::AccountingOverflow)?;
        let widths = (scalar.physical_width, scalar.projected_width);
        if previous != Some(widths) {
            plan.runs = plan
                .runs
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?;
            previous = Some(widths);
        }
        plan.has_pairs |= scalar.paired;
        require_limit(ResourceKind::SourceBytes, plan.projected_bytes, maximum)
    })?;
    Ok(plan)
}

fn build<'a, I, F>(
    raw: &'a [u8],
    utf8: Option<&'a str>,
    encoding: Option<SourceTextEncoding>,
    mut scalars: F,
    maximum: usize,
    max_owned_bytes: usize,
    cancellation: &Cancellation,
) -> Result<NativeSourceView<'a>, AdapterError>
where
    I: Iterator<Item = Result<char, AdapterError>>,
    F: FnMut() -> I,
{
    let plan = plan(scalars(), encoding.is_some(), maximum, cancellation)?;
    if plan.physical_bytes != raw.len() {
        return Err(source_failure("native-source-projection-invariant"));
    }
    if let Some(text) = utf8.filter(|_| !plan.has_pairs) {
        return Ok(NativeSourceView {
            text: Cow::Borrowed(text),
            encoding,
            physical_bytes: raw.len(),
            runs: Vec::new(),
            owned_bytes: 0,
            line_endings: LineEndingPolicy::CrlfFileText,
        });
    }
    require_limit(
        ResourceKind::ReportedMemoryBytes,
        projection_owned_bytes(plan.projected_bytes, plan.runs)?,
        max_owned_bytes,
    )?;
    cancellation.check()?;
    let mut runs = Vec::<ScalarWidthRun>::new();
    runs.try_reserve_exact(plan.runs)
        .map_err(|_| SinkError::AllocationFailed)?;
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
    let mut physical = 0usize;
    walk(scalars(), encoding.is_some(), cancellation, |scalar| {
        let next = text
            .len()
            .checked_add(usize::from(scalar.projected_width))
            .ok_or(SinkError::AccountingOverflow)?;
        if next > plan.projected_bytes {
            return Err(source_failure("native-source-projection-invariant"));
        }
        if let Some(run) = runs.last_mut().filter(|run| {
            run.physical_width == scalar.physical_width
                && run.projected_width == scalar.projected_width
        }) {
            run.scalar_count = run
                .scalar_count
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?;
        } else {
            if runs.len() >= plan.runs {
                return Err(source_failure("native-source-projection-invariant"));
            }
            runs.push(ScalarWidthRun {
                physical_start: physical,
                projected_start: text.len(),
                scalar_count: 1,
                physical_width: scalar.physical_width,
                projected_width: scalar.projected_width,
            });
        }
        text.push(scalar.value);
        physical = physical
            .checked_add(usize::from(scalar.physical_width))
            .ok_or(SinkError::AccountingOverflow)?;
        Ok(())
    })?;
    if physical != raw.len() || text.len() != plan.projected_bytes || runs.len() != plan.runs {
        return Err(source_failure("native-source-projection-invariant"));
    }
    Ok(NativeSourceView {
        text: Cow::Owned(text),
        encoding,
        physical_bytes: raw.len(),
        runs,
        owned_bytes,
        line_endings: LineEndingPolicy::CrlfFileText,
    })
}

pub(super) fn complete_pair_prefix(
    raw: &[u8],
    encoding: &str,
    end: usize,
) -> Result<usize, AdapterError> {
    if end == raw.len() {
        return Ok(end);
    }
    if encoding == "utf-8" {
        return Ok(
            if end.checked_sub(1).and_then(|start| raw.get(start)) == Some(&b'\r')
                && raw.get(end) == Some(&b'\n')
            {
                end - 1
            } else {
                end
            },
        );
    }
    let unit = |offset: usize| -> Option<u16> {
        let bytes: [u8; 2] = raw.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
        match encoding {
            "utf-16le" => Some(u16::from_le_bytes(bytes)),
            "utf-16be" => Some(u16::from_be_bytes(bytes)),
            _ => None,
        }
    };
    if !matches!(encoding, "utf-16le" | "utf-16be") {
        return Err(RequestError::UnsupportedEncoding.into());
    }
    Ok(
        if end.checked_sub(2).and_then(unit) == Some(13) && unit(end) == Some(10) {
            end - 2
        } else {
            end
        },
    )
}

#[cfg(test)]
mod tests;
