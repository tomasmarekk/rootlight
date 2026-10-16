//! Configure markers in written command names use a private, length-preserving mask.
//! Each complete callee is proved outside comments and arguments. Later malformed
//! source keeps native diagnostics and cannot erase an already proved prefix.

use std::ops::Range;

use rootlight_adapter_sdk::{AdapterError, AnalysisLimits, ResourceKind, SinkError};
use rootlight_cancel::Cancellation;

use super::{
    NativeSourceView, QueryCandidate, StructuralRole,
    cmake_header::{BracketEnd, bracket_end, command_end, line_end},
    require_provider_limit, try_reserve_cancellable,
};

pub(crate) const CMAKE_COMMAND_TEMPLATE_PROJECTION_POLICY: &str =
    "rootlight.treesitter.cmake-command-template-input/1";

pub(super) struct CommandTemplatePlan {
    markers: Vec<Range<usize>>,
}

impl CommandTemplatePlan {
    pub(super) fn recognize(
        source: &NativeSourceView<'_>,
        limits: &AnalysisLimits,
        cancellation: &Cancellation,
    ) -> Result<Option<Self>, AdapterError> {
        Self::scan(
            source.as_bytes(),
            source.owned_bytes(),
            limits,
            cancellation,
        )
    }

    fn scan(
        bytes: &[u8],
        source_owned_bytes: usize,
        limits: &AnalysisLimits,
        cancellation: &Cancellation,
    ) -> Result<Option<Self>, AdapterError> {
        cancellation.check()?;
        let mut plan = Self {
            markers: Vec::new(),
        };
        let mut index = usize::from(bytes.starts_with(b"\xef\xbb\xbf")) * 3;
        while let Some(byte) = bytes.get(index).copied() {
            checkpoint(index, cancellation)?;
            if byte.is_ascii_whitespace() {
                index += 1;
                continue;
            }
            if byte == b'#' {
                index = match bracket_end(bytes, index + 1, cancellation)? {
                    BracketEnd::Closed(end) => end,
                    BracketEnd::NotBracket => line_end(bytes, index, cancellation)?,
                    BracketEnd::Unclosed => return plan.finish(cancellation),
                };
                continue;
            }
            if !(byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'@')) {
                return plan.finish(cancellation);
            }
            let start = index;
            let mut marker_count = 0usize;
            let mut last_marker = None;
            while let Some(byte) = bytes.get(index).copied() {
                checkpoint(index, cancellation)?;
                if byte.is_ascii_alphanumeric() || byte == b'_' {
                    index += 1;
                } else if byte == b'@' {
                    let Some(end) = marker_end(bytes, index, cancellation)? else {
                        return plan.finish(cancellation);
                    };
                    marker_count = marker_count
                        .checked_add(1)
                        .ok_or(SinkError::AccountingOverflow)?;
                    last_marker = Some(index..end);
                    index = end;
                } else {
                    break;
                }
            }
            let name_end = index;
            let templated = marker_count != 0;
            let standalone = marker_count == 1 && last_marker == Some(start..name_end);
            while bytes
                .get(index)
                .is_some_and(|byte| matches!(*byte, b' ' | b'\t' | b'\r'))
            {
                checkpoint(index, cancellation)?;
                index += 1;
            }
            if standalone && bytes.get(index) != Some(&b'(') {
                continue;
            }
            if !templated {
                while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
                    checkpoint(index, cancellation)?;
                    index += 1;
                }
            }
            if bytes.get(index) != Some(&b'(') {
                return plan.finish(cancellation);
            }
            let Some(end) = command_end(bytes, index, cancellation)? else {
                return plan.finish(cancellation);
            };
            // Only a complete invocation can spend marker-vector credits. A
            // standalone placeholder must leave the ordinary native route free.
            if templated {
                let mut marker_start = start;
                while marker_start < name_end {
                    checkpoint(marker_start, cancellation)?;
                    if bytes[marker_start] == b'@' {
                        let marker_end = marker_end(bytes, marker_start, cancellation)?
                            .ok_or(SinkError::AccountingOverflow)?;
                        plan.push(
                            marker_start..marker_end,
                            source_owned_bytes,
                            limits,
                            cancellation,
                        )?;
                        marker_start = marker_end;
                    } else {
                        marker_start += 1;
                    }
                }
            }
            index = end;
        }
        plan.finish(cancellation)
    }

    fn finish(self, cancellation: &Cancellation) -> Result<Option<Self>, AdapterError> {
        cancellation.check()?;
        Ok((!self.markers.is_empty()).then_some(self))
    }

    fn push(
        &mut self,
        marker: Range<usize>,
        source_owned_bytes: usize,
        limits: &AnalysisLimits,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        require_provider_limit(
            ResourceKind::SyntaxNodes,
            self.markers
                .len()
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?,
            limits.max_syntax_nodes(),
        )?;
        let required = source_owned_bytes
            .checked_add(
                self.markers
                    .len()
                    .saturating_add(1)
                    .checked_mul(size_of::<Range<usize>>())
                    .ok_or(SinkError::AccountingOverflow)?,
            )
            .ok_or(SinkError::AccountingOverflow)?;
        require_provider_limit(
            ResourceKind::ReportedMemoryBytes,
            required,
            limits.max_reported_memory_bytes(),
        )?;
        if self.markers.len() == self.markers.capacity() {
            self.markers
                .try_reserve_exact(1)
                .map_err(|_| SinkError::AllocationFailed)?;
        }
        self.check_memory(source_owned_bytes, 0, limits)?;
        cancellation.check()?;
        self.markers.push(marker);
        Ok(())
    }

    fn check_memory(
        &self,
        source_owned_bytes: usize,
        parse_capacity: usize,
        limits: &AnalysisLimits,
    ) -> Result<(), AdapterError> {
        let required = self
            .markers
            .capacity()
            .checked_mul(size_of::<Range<usize>>())
            .and_then(|bytes| bytes.checked_add(source_owned_bytes))
            .and_then(|bytes| bytes.checked_add(parse_capacity))
            .ok_or(SinkError::AccountingOverflow)?;
        require_provider_limit(
            ResourceKind::ReportedMemoryBytes,
            required,
            limits.max_reported_memory_bytes(),
        )
    }

    pub(super) fn parse_bytes(
        &self,
        source: &NativeSourceView<'_>,
        limits: &AnalysisLimits,
        cancellation: &Cancellation,
    ) -> Result<Vec<u8>, AdapterError> {
        self.check_memory(source.owned_bytes(), source.as_bytes().len(), limits)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(source.as_bytes().len())
            .map_err(|_| SinkError::AllocationFailed)?;
        self.check_memory(source.owned_bytes(), bytes.capacity(), limits)?;
        for chunk in source.as_bytes().chunks(256) {
            cancellation.check()?;
            bytes.extend_from_slice(chunk);
        }
        for marker in &self.markers {
            cancellation.check()?;
            let span = bytes
                .get_mut(marker.clone())
                .ok_or(SinkError::AccountingOverflow)?;
            for chunk in span.chunks_mut(256) {
                cancellation.check()?;
                chunk.fill(b'_');
            }
        }
        Ok(bytes)
    }

    pub(super) fn append_gaps(
        &self,
        candidates: &mut Vec<QueryCandidate>,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        try_reserve_cancellable(
            candidates,
            self.markers.len(),
            cancellation,
            "cmake-command-template-gap-allocation",
        )?;
        for marker in &self.markers {
            cancellation.check()?;
            candidates.push(QueryCandidate {
                start: marker.start,
                end: marker.end,
                role: StructuralRole::Signature,
                syntax: "cmake.configure_placeholder",
                required: true,
                native_depth: 1,
            });
        }
        Ok(())
    }

    pub(super) fn required_candidate_count(&self) -> usize {
        self.markers.len()
    }
}

fn checkpoint(index: usize, cancellation: &Cancellation) -> Result<(), AdapterError> {
    if index.is_multiple_of(256) {
        cancellation.check()?;
    }
    Ok(())
}

fn marker_end(
    bytes: &[u8],
    start: usize,
    cancellation: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    let mut index = start + 1;
    while let Some(byte) = bytes.get(index).copied() {
        checkpoint(index, cancellation)?;
        if byte == b'@' && index > start + 1 {
            return Ok(Some(index + 1));
        }
        if !(byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'/' | b'+' | b'-')) {
            break;
        }
        index += 1;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::{BatchThresholds, StreamLimits};
    use rootlight_ir::IrLimits;

    fn limits() -> AnalysisLimits {
        limits_with_memory(4 * 1024 * 1024)
    }

    fn limits_with_memory(memory: usize) -> AnalysisLimits {
        let batch = BatchThresholds::new(128, 256 * 1024, 8, 4096).unwrap();
        let stream =
            StreamLimits::new(64, 4096, 4 * 1024 * 1024, 64, 64 * 1024, 128 * 1024, batch).unwrap();
        AnalysisLimits::new(
            1024 * 1024,
            4096,
            128,
            16,
            memory,
            stream.clone(),
            stream,
            IrLimits::default(),
        )
        .unwrap()
    }

    fn markers(source: &[u8]) -> Option<Vec<Range<usize>>> {
        CommandTemplatePlan::scan(source, 0, &limits(), &Cancellation::new())
            .unwrap()
            .map(|plan| plan.markers)
    }

    #[test]
    fn complete_callees_keep_only_their_authored_markers() {
        for source in [
            "prefix_@NAME@_suffix(value)",
            "@NAME@(value)",
            "prefix@A.B/C+-1@middle@SECOND@(value)",
        ] {
            let found = markers(source.as_bytes()).unwrap();
            assert_eq!(found.len(), if source.contains("SECOND") { 2 } else { 1 });
            for range in found {
                assert!(source[range.clone()].starts_with('@'));
                assert!(source[range].ends_with('@'));
            }
        }
    }

    #[test]
    fn comments_and_argument_forms_cannot_authenticate_template_calls() {
        for source in [
            "# fake_@NAME@()\nmessage(value)",
            "#[=[ fake_@NAME@() ]=]\nmessage(value)",
            "message(\"fake_@NAME@()\")",
            "message([=[fake_@NAME@()]=])",
            "message(unquoted\"fake_@NAME@()\")",
            "message(nested(fake_@NAME@()))",
        ] {
            assert!(markers(source.as_bytes()).is_none(), "{source:?}");
        }
    }

    #[test]
    fn malformed_source_cannot_publish_a_partial_projection() {
        for source in [
            "prefix_@NAME@(",
            "prefix_@NAME()",
            "prefix_@@()",
            "prefix_@NAME@\n(value)",
            "message([=[unclosed)\nprefix_@NAME@()",
            "2prefix_@NAME@()",
        ] {
            assert!(markers(source.as_bytes()).is_none(), "{source:?}");
        }
    }

    #[test]
    fn ordinary_and_standalone_markers_keep_the_native_route() {
        for source in [
            "message(value)",
            "@STANDALONE@\nmessage(value)",
            "@STANDALONE@",
        ] {
            assert!(markers(source.as_bytes()).is_none(), "{source:?}");
        }
    }

    #[test]
    fn a_later_malformed_command_cannot_erase_a_proved_template_callee() {
        for source in [
            "prefix_@NAME@()\nmalformed",
            "prefix_@NAME@()\nother_@BAD()",
            "prefix_@NAME@()\nother_@BAD@(",
            "prefix_@NAME@()\n#[=[unclosed",
        ] {
            let found = markers(source.as_bytes()).unwrap();
            assert_eq!(found.len(), 1);
            assert_eq!(&source[found[0].clone()], "@NAME@");
        }
    }

    #[test]
    fn only_proved_invocations_spend_marker_storage_credits() {
        let budget = limits_with_memory(1);
        for source in [
            "@STANDALONE@\nmessage(value)",
            "prefix_@NAME@(",
            "prefix_@BAD()",
        ] {
            assert!(
                CommandTemplatePlan::scan(source.as_bytes(), 0, &budget, &Cancellation::new())
                    .unwrap()
                    .is_none(),
                "{source:?}"
            );
        }
        assert!(
            CommandTemplatePlan::scan(b"prefix_@NAME@()", 0, &budget, &Cancellation::new())
                .is_err()
        );
    }

    #[test]
    fn private_mask_changes_only_marker_bytes_and_keeps_original_source() {
        let raw = format!(
            "# physical λ😀\r\nprefix_@{}@_suffix(value)\r\n",
            "M".repeat(1024)
        );
        let cancellation = Cancellation::new();
        let source = NativeSourceView::new(
            raw.as_bytes(),
            "utf-8",
            raw.len(),
            usize::MAX,
            &cancellation,
        )
        .unwrap();
        let budget = limits();
        let plan = CommandTemplatePlan::recognize(&source, &budget, &cancellation)
            .unwrap()
            .unwrap();
        let projected = plan.parse_bytes(&source, &budget, &cancellation).unwrap();
        assert_eq!(projected.len(), raw.len());
        for (index, original) in raw.bytes().enumerate() {
            assert_eq!(
                projected[index],
                if plan.markers.iter().any(|marker| marker.contains(&index)) {
                    b'_'
                } else {
                    original
                }
            );
        }
        assert_eq!(source.as_bytes(), raw.as_bytes());
    }
}
