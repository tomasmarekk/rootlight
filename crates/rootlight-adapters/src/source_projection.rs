//! Borrow ordinary UTF-8 and bound a private BOM-preserving UTF-16 parser view.
//! Syntax facts return to physical bytes before the SDK admits their source spans.

use std::{borrow::Cow, mem::size_of};

use rootlight_adapter_sdk::{AdapterError, ResourceKind, SinkError, SourceText, TextEncoding};
use rootlight_cancel::Cancellation;

use rootlight_adapter_sdk::DiagnosticCode;

struct Run {
    projected: usize,
    physical: usize,
    scalars: usize,
    width: usize,
}

pub(crate) struct View<'a> {
    text: Cow<'a, str>,
    runs: Vec<Run>,
    physical_bytes: usize,
}

impl<'a> View<'a> {
    pub(crate) fn new(
        raw: &'a [u8],
        encoding: &str,
        maximum: usize,
        maximum_owned: usize,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        require(ResourceKind::SourceBytes, raw.len(), maximum)?;
        let source = SourceText::parse(raw, cancel)?
            .ok_or_else(|| failure("source-projection-invalid-source-encoding"))?;
        if source.encoding().as_str() != encoding {
            return Err(failure("source-projection-source-encoding-mismatch"));
        }
        if let Some(text) = source.utf8() {
            return Ok(Self {
                text: Cow::Borrowed(text),
                runs: Vec::new(),
                physical_bytes: raw.len(),
            });
        }
        if !matches!(
            source.encoding(),
            TextEncoding::Utf16Le | TextEncoding::Utf16Be
        ) {
            return Err(failure("source-projection-source-context-unsupported"));
        }
        // SourceText validates the exact BOM and scalars but omits its BOM from
        // iteration. NativeSourceView retains that scalar in decoded coordinates.
        let characters = || std::iter::once('\u{feff}').chain(source.chars());
        let mut projected = 0_usize;
        let mut count = 0_usize;
        let mut previous = 0_usize;
        for (index, character) in characters().enumerate() {
            checkpoint(index, cancel)?;
            let width = character.len_utf8();
            projected = add(projected, width)?;
            require(ResourceKind::SourceBytes, projected, maximum)?;
            if width != previous {
                count = add(count, 1)?;
                previous = width;
            }
        }
        require(
            ResourceKind::ReportedMemoryBytes,
            owned(projected, count)?,
            maximum_owned,
        )?;
        let mut runs: Vec<Run> = Vec::new();
        runs.try_reserve_exact(count)
            .map_err(|_| SinkError::AllocationFailed)?;
        require(
            ResourceKind::ReportedMemoryBytes,
            owned(projected, runs.capacity())?,
            maximum_owned,
        )?;
        let mut text = String::new();
        text.try_reserve_exact(projected)
            .map_err(|_| SinkError::AllocationFailed)?;
        require(
            ResourceKind::ReportedMemoryBytes,
            owned(text.capacity(), runs.capacity())?,
            maximum_owned,
        )?;
        let mut physical = 0_usize;
        for (index, character) in characters().enumerate() {
            checkpoint(index, cancel)?;
            let width = character.len_utf8();
            if add(text.len(), width)? > projected {
                return Err(failure("source-projection-projection-invariant"));
            }
            if let Some(run) = runs.last_mut().filter(|run| run.width == width) {
                run.scalars = add(run.scalars, 1)?;
            } else {
                if runs.len() >= count {
                    return Err(failure("source-projection-projection-invariant"));
                }
                runs.push(Run {
                    projected: text.len(),
                    physical,
                    scalars: 1,
                    width,
                });
            }
            text.push(character);
            physical = add(physical, source.encoding().character_bytes(character))?;
        }
        cancel.check()?;
        if text.len() != projected || physical != raw.len() || runs.len() != count {
            return Err(failure("source-projection-projection-invariant"));
        }
        Ok(Self {
            text: Cow::Owned(text),
            runs,
            physical_bytes: raw.len(),
        })
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn physical(&self, offset: usize) -> Result<usize, AdapterError> {
        if !self.text.is_char_boundary(offset) {
            return Err(failure("source-projection-projection-boundary"));
        }
        if self.runs.is_empty() {
            return Ok(offset);
        }
        if offset == self.text.len() {
            return Ok(self.physical_bytes);
        }
        let index = self
            .runs
            .partition_point(|run| run.projected <= offset)
            .checked_sub(1)
            .ok_or_else(|| failure("source-projection-projection-boundary"))?;
        let run = &self.runs[index];
        let delta = offset
            .checked_sub(run.projected)
            .ok_or(SinkError::AccountingOverflow)?;
        if !delta.is_multiple_of(run.width) || delta / run.width >= run.scalars {
            return Err(failure("source-projection-projection-boundary"));
        }
        let width = if run.width == 4 { 4 } else { 2 };
        add(run.physical, multiply(delta / run.width, width)?)
    }
}

fn checkpoint(index: usize, cancel: &Cancellation) -> Result<(), AdapterError> {
    if index.is_multiple_of(256) {
        cancel.check()?;
    }
    Ok(())
}
fn add(left: usize, right: usize) -> Result<usize, AdapterError> {
    left.checked_add(right)
        .ok_or_else(|| SinkError::AccountingOverflow.into())
}
fn multiply(left: usize, right: usize) -> Result<usize, AdapterError> {
    left.checked_mul(right)
        .ok_or_else(|| SinkError::AccountingOverflow.into())
}
fn owned(text: usize, runs: usize) -> Result<usize, AdapterError> {
    add(text, multiply(runs, size_of::<Run>())?)
}
fn require(resource: ResourceKind, observed: usize, limit: usize) -> Result<(), AdapterError> {
    if observed > limit {
        Err(SinkError::StreamLimit {
            resource,
            observed,
            limit,
        }
        .into())
    } else {
        Ok(())
    }
}

fn failure(code: &'static str) -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new(code).expect("fixed source projection code"),
    }
}
