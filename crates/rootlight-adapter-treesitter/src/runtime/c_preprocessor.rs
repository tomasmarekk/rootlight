//! Repair native C-family recovery using admitted written macro replacement masks.
//! Parser bytes remain private; identity, lexical evidence and physical source
//! coordinates always come from the unchanged NativeSourceView.

use std::mem::size_of;

use rootlight_adapter_sdk::{AdapterError, ResourceKind, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Tree;

use crate::{
    query_pack::c::{MacroHeader, PreprocessorDialect, cpp_macro_headers, macro_header},
    source_view::NativeSourceView,
};

use super::{CANCELLATION_BYTE_INTERVAL, provider_failure, require_provider_limit};

pub(crate) const C_PREPROCESSOR_PROJECTION_POLICY: &str =
    "rootlight.treesitter.c-preprocessor-replacement-input/3";

struct Replacement {
    header: MacroHeader,
    placeholder: Option<usize>,
    preserve: bool,
}

pub(super) struct ProjectionLimits {
    pub(super) nodes: usize,
    pub(super) depth: usize,
    pub(super) owned_bytes: usize,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ProofLimit {
    Nodes,
    Depth,
}

pub(super) enum ProjectionOutcome {
    Unchanged {
        work_nodes: usize,
        work_depth: usize,
    },
    Projected(MacroProjection),
    Unavailable {
        work_nodes: usize,
        work_depth: usize,
    },
    Limited {
        work_nodes: usize,
        work_depth: usize,
        limit: ProofLimit,
    },
}

pub(super) struct MacroProjection {
    bytes: Vec<u8>,
    pub(super) work_nodes: usize,
    pub(super) work_depth: usize,
}

impl MacroProjection {
    /// Classifies source-proved native directive anchors under one walk budget.
    ///
    /// Upstream staging may hide generated CMake names. Any changed header is
    /// withheld here rather than being reinterpreted as an authored C macro.
    /// Every outcome retains consumed walk work, including a partial proof that
    /// cannot establish a repair roster or a successful unavailable partition.
    ///
    /// # Errors
    /// Propagates cancellation, checked accounting, allocation and memory limits.
    pub(super) fn recognize(
        tree: &Tree,
        source: &NativeSourceView<'_>,
        input: &[u8],
        upstream_owned_bytes: usize,
        range_count: usize,
        limits: ProjectionLimits,
        cancellation: &Cancellation,
    ) -> Result<ProjectionOutcome, AdapterError> {
        Self::recognize_for(
            tree,
            source,
            input,
            upstream_owned_bytes,
            range_count,
            limits,
            PreprocessorDialect::C,
            cancellation,
        )
    }

    /// Applies the separately proved CPP literal and complete-header rules.
    ///
    /// # Errors
    /// Propagates the same admission, source-coordinate and cancellation errors.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn recognize_cpp(
        tree: &Tree,
        source: &NativeSourceView<'_>,
        input: &[u8],
        upstream_owned_bytes: usize,
        range_count: usize,
        limits: ProjectionLimits,
        cancellation: &Cancellation,
    ) -> Result<ProjectionOutcome, AdapterError> {
        Self::recognize_for(
            tree,
            source,
            input,
            upstream_owned_bytes,
            range_count,
            limits,
            PreprocessorDialect::Cpp,
            cancellation,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn recognize_for(
        tree: &Tree,
        source: &NativeSourceView<'_>,
        input: &[u8],
        upstream_owned_bytes: usize,
        range_count: usize,
        limits: ProjectionLimits,
        dialect: PreprocessorDialect,
        cancellation: &Cancellation,
    ) -> Result<ProjectionOutcome, AdapterError> {
        cancellation.check()?;
        let original = source.as_bytes();
        if original.len() != input.len() {
            return Err(provider_failure("c-projection-input-length"));
        }
        let existing = source
            .owned_bytes()
            .checked_add(upstream_owned_bytes)
            .ok_or(SinkError::AccountingOverflow)?;
        let mut existing = existing;
        let mut ranges = None;
        let mut headers = Vec::<Replacement>::new();
        let mut native_anchors = Vec::<usize>::new();
        let mut repair = false;
        let mut work_nodes = 0usize;
        let mut work_depth = 0usize;
        let mut cursor = tree.walk();
        let mut depth = 0usize;
        'walk: loop {
            cancellation.check()?;
            let node = cursor.node();
            if work_nodes == limits.nodes {
                return Ok(ProjectionOutcome::Limited {
                    work_nodes,
                    work_depth,
                    limit: ProofLimit::Nodes,
                });
            }
            work_nodes = work_nodes
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?;
            work_depth = work_depth.max(depth);
            if depth >= limits.depth && node.child_count() > 0 {
                return Ok(ProjectionOutcome::Limited {
                    work_nodes,
                    work_depth,
                    limit: ProofLimit::Depth,
                });
            }
            if dialect == PreprocessorDialect::Cpp
                && matches!(node.kind(), "preproc_def" | "preproc_function_def")
            {
                if native_anchors.len() == native_anchors.capacity() {
                    let next = native_anchors
                        .len()
                        .saturating_mul(2)
                        .max(1)
                        .min(limits.nodes);
                    admit(
                        existing,
                        next.checked_mul(size_of::<usize>())
                            .ok_or(SinkError::AccountingOverflow)?,
                        limits.owned_bytes,
                    )?;
                    native_anchors
                        .try_reserve_exact(next.saturating_sub(native_anchors.len()))
                        .map_err(|_| SinkError::AllocationFailed)?;
                }
                admit(
                    existing,
                    native_anchors
                        .capacity()
                        .checked_mul(size_of::<usize>())
                        .ok_or(SinkError::AccountingOverflow)?,
                    limits.owned_bytes,
                )?;
                native_anchors.push(node.start_byte());
            }
            if dialect == PreprocessorDialect::C
                && matches!(node.kind(), "preproc_def" | "preproc_function_def")
                && ranges.is_none()
            {
                let range_bytes = range_count
                    .max(1)
                    .checked_mul(size_of::<tree_sitter::Range>())
                    .ok_or(SinkError::AccountingOverflow)?;
                admit(existing, range_bytes, limits.owned_bytes)?;
                let selected = tree.included_ranges();
                let bytes = selected
                    .capacity()
                    .checked_mul(size_of::<tree_sitter::Range>())
                    .ok_or(SinkError::AccountingOverflow)?;
                existing = existing
                    .checked_add(bytes)
                    .ok_or(SinkError::AccountingOverflow)?;
                admit(existing, 0, limits.owned_bytes)?;
                ranges = Some(selected);
            }
            if dialect == PreprocessorDialect::C
                && matches!(node.kind(), "preproc_def" | "preproc_function_def")
                && let Some(header) = macro_header(
                    node,
                    original,
                    ranges.as_deref().unwrap_or(&[]),
                    cancellation,
                )?
                && headers
                    .last()
                    .is_none_or(|previous| header.start >= previous.header.end)
                && input.get(header.start..header.replacement_start)
                    == original.get(header.start..header.replacement_start)
            {
                repair |= append_replacement(
                    &mut headers,
                    header,
                    Some(node),
                    original,
                    existing,
                    &limits,
                    cancellation,
                )?;
            }
            if cursor.goto_first_child() {
                depth += 1;
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    break 'walk;
                }
                depth = depth.checked_sub(1).ok_or(SinkError::AccountingOverflow)?;
            }
        }
        if dialect == PreprocessorDialect::Cpp {
            existing = existing
                .checked_add(
                    native_anchors
                        .capacity()
                        .checked_mul(size_of::<usize>())
                        .ok_or(SinkError::AccountingOverflow)?,
                )
                .ok_or(SinkError::AccountingOverflow)?;
            let range_bytes = range_count
                .max(1)
                .checked_mul(size_of::<tree_sitter::Range>())
                .ok_or(SinkError::AccountingOverflow)?;
            admit(existing, range_bytes, limits.owned_bytes)?;
            let selected = tree.included_ranges();
            existing = existing
                .checked_add(
                    selected
                        .capacity()
                        .checked_mul(size_of::<tree_sitter::Range>())
                        .ok_or(SinkError::AccountingOverflow)?,
                )
                .ok_or(SinkError::AccountingOverflow)?;
            admit(existing, 0, limits.owned_bytes)?;
            let complete = cpp_macro_headers(
                tree.root_node(),
                original,
                &selected,
                cancellation,
                &mut |header, owner| {
                    if input.get(header.start..header.replacement_start)
                        != original.get(header.start..header.replacement_start)
                    {
                        return Err(provider_failure("cpp-projection-header-staging"));
                    }
                    repair |= append_replacement(
                        &mut headers,
                        header,
                        owner,
                        original,
                        existing,
                        &limits,
                        cancellation,
                    )?;
                    Ok(())
                },
            )?;
            let mut all_anchors_proved = true;
            for anchor in native_anchors {
                cancellation.check()?;
                let index = headers.partition_point(|entry| entry.header.start <= anchor);
                all_anchors_proved &= index
                    .checked_sub(1)
                    .and_then(|index| headers.get(index))
                    .is_some_and(|entry| anchor < entry.header.end);
            }
            if !complete || !all_anchors_proved {
                return Ok(ProjectionOutcome::Unavailable {
                    work_nodes,
                    work_depth,
                });
            }
        }
        if !repair {
            return Ok(ProjectionOutcome::Unchanged {
                work_nodes,
                work_depth,
            });
        }
        for replacement in &headers {
            cancellation.check()?;
            if !replacement.preserve && replacement.placeholder.is_none() {
                return Ok(ProjectionOutcome::Unavailable {
                    work_nodes,
                    work_depth,
                });
            }
        }
        let descriptor_bytes = headers
            .capacity()
            .checked_mul(size_of::<Replacement>())
            .ok_or(SinkError::AccountingOverflow)?;
        let existing = existing
            .checked_add(descriptor_bytes)
            .ok_or(SinkError::AccountingOverflow)?;
        admit(existing, input.len(), limits.owned_bytes)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(input.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        admit(existing, bytes.capacity(), limits.owned_bytes)?;
        for chunk in input.chunks(CANCELLATION_BYTE_INTERVAL) {
            cancellation.check()?;
            bytes.extend_from_slice(chunk);
        }
        for Replacement {
            header,
            placeholder,
            ..
        } in headers
        {
            let Some(placeholder) = placeholder else {
                continue;
            };
            let replacement = bytes
                .get_mut(header.replacement_start..header.end)
                .ok_or_else(|| provider_failure("c-projection-replacement-range"))?;
            for chunk in replacement.chunks_mut(CANCELLATION_BYTE_INTERVAL) {
                cancellation.check()?;
                for byte in chunk {
                    if !matches!(*byte, b'\r' | b'\n') {
                        *byte = b' ';
                    }
                }
            }
            // An all-whitespace value lets the pinned grammar's optional
            // preproc_arg consume the following physical line. A private token
            // terminates that value; public evidence still reads original bytes.
            *bytes
                .get_mut(placeholder)
                .ok_or_else(|| provider_failure("c-projection-placeholder"))? = b'0';
        }
        cancellation.check()?;
        Ok(ProjectionOutcome::Projected(Self {
            bytes,
            work_nodes,
            work_depth,
        }))
    }

    pub(super) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

fn append_replacement(
    headers: &mut Vec<Replacement>,
    header: MacroHeader,
    owner: Option<tree_sitter::Node<'_>>,
    original: &[u8],
    existing: usize,
    limits: &ProjectionLimits,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let needs_repair = owner.is_none_or(|node| {
        node.has_error()
            || node.end_byte() != header.end
            || (node.kind() == "preproc_function_def") != header.function_like
            || node
                .child_by_field_name("name")
                .is_none_or(|name| name.byte_range() != header.name)
    });
    let preserve = !needs_repair && replacement_is_empty(original, &header, cancellation)?;
    let placeholder = if preserve {
        None
    } else {
        placeholder_position(&header, original, cancellation)?
    };
    if headers.len() == headers.capacity() {
        let next = headers.len().saturating_mul(2).max(1).min(limits.nodes);
        let bytes = next
            .checked_mul(size_of::<Replacement>())
            .ok_or(SinkError::AccountingOverflow)?;
        admit(existing, bytes, limits.owned_bytes)?;
        headers
            .try_reserve_exact(next.saturating_sub(headers.len()))
            .map_err(|_| SinkError::AllocationFailed)?;
    }
    let bytes = headers
        .capacity()
        .checked_mul(size_of::<Replacement>())
        .ok_or(SinkError::AccountingOverflow)?;
    admit(existing, bytes, limits.owned_bytes)?;
    headers.push(Replacement {
        header,
        placeholder,
        preserve,
    });
    Ok(needs_repair)
}

fn replacement_is_empty(
    source: &[u8],
    header: &MacroHeader,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let bytes = source
        .get(header.replacement_start..header.end)
        .ok_or_else(|| provider_failure("c-projection-replacement-range"))?;
    for chunk in bytes.chunks(CANCELLATION_BYTE_INTERVAL) {
        cancellation.check()?;
        if !chunk.iter().all(u8::is_ascii_whitespace) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn placeholder_position(
    header: &MacroHeader,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    let bytes = source
        .get(header.replacement_start..header.end)
        .ok_or_else(|| provider_failure("c-projection-replacement-range"))?;
    let mut separated = header.function_like;
    for (index, byte) in bytes.iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_BYTE_INTERVAL) {
            cancellation.check()?;
        }
        if *byte == b'\n' {
            break;
        }
        if *byte == b'\r' {
            continue;
        }
        if separated {
            return header
                .replacement_start
                .checked_add(index)
                .map(Some)
                .ok_or(SinkError::AccountingOverflow.into());
        }
        separated = true;
    }
    Ok(None)
}

fn admit(existing: usize, added: usize, maximum: usize) -> Result<(), AdapterError> {
    require_provider_limit(
        ResourceKind::ReportedMemoryBytes,
        existing
            .checked_add(added)
            .ok_or(SinkError::AccountingOverflow)?,
        maximum,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse(input: &[u8]) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .unwrap();
        parser.parse(input, None).unwrap()
    }

    fn limits() -> ProjectionLimits {
        ProjectionLimits {
            nodes: 10_000,
            depth: 128,
            owned_bytes: 1_000_000,
        }
    }

    #[test]
    fn replacement_projection_recovers_authored_names_and_later_functions() {
        for source in [
            "#define WRITTEN 1000 /* comment */ \"tail\"\nint after(void) { return 0; }\n",
            "#define WRITTEN 1'000 /* comment\r\n#define FAKE \"hidden\"\r\n*/ \"tail\"\r\nint after(void) { return 0; }\r\n",
            "#define CALL 1 /* comment */ phantom()\nint after(void) { return 0; }\n",
            "#define DECL 1 /* comment */ int phantom(void);\nint after(void) { return 0; }\n",
        ] {
            let cancel = Cancellation::new();
            let view =
                NativeSourceView::new(source.as_bytes(), "utf-8", usize::MAX, usize::MAX, &cancel)
                    .unwrap();
            let initial = parse(source.as_bytes());
            let ProjectionOutcome::Projected(projected) = MacroProjection::recognize(
                &initial,
                &view,
                view.as_bytes(),
                0,
                1,
                limits(),
                &cancel,
            )
            .unwrap() else {
                panic!("expected replacement projection")
            };
            assert_eq!(projected.as_bytes().len(), source.len());
            for (original, masked) in source.bytes().zip(projected.as_bytes()) {
                if matches!(original, b'\r' | b'\n') {
                    assert_eq!(original, *masked);
                }
            }
            let repaired = parse(projected.as_bytes());
            assert!(
                !repaired.root_node().has_error(),
                "{}",
                repaired.root_node().to_sexp()
            );
            let first = repaired.root_node().named_child(0).unwrap();
            let name = first.child_by_field_name("name").unwrap();
            assert!(matches!(
                &source[name.byte_range()],
                "WRITTEN" | "CALL" | "DECL"
            ));
            assert_eq!(first.kind(), "preproc_def");
            let last = repaired
                .root_node()
                .named_child(u32::try_from(repaired.root_node().named_child_count() - 1).unwrap())
                .unwrap();
            assert_eq!(last.kind(), "function_definition");
            assert_eq!(&source[last.byte_range()], "int after(void) { return 0; }");
            assert_eq!(view.as_bytes(), source.as_bytes());
        }
    }

    #[test]
    fn projection_admits_live_upstream_memory_and_withholds_changed_headers() {
        let source = b"#define VALUE 1 /* comment */ \"tail\"\n";
        let cancel = Cancellation::new();
        let view = NativeSourceView::new(source, "utf-8", usize::MAX, usize::MAX, &cancel).unwrap();
        let tree = parse(source);
        let mut changed = source.to_vec();
        changed[8] = b'x';
        assert!(matches!(
            MacroProjection::recognize(
                &tree,
                &view,
                &changed,
                changed.capacity(),
                1,
                limits(),
                &cancel
            )
            .unwrap(),
            ProjectionOutcome::Unchanged { .. }
        ));
        let maximum = limits().owned_bytes;
        assert!(
            MacroProjection::recognize(&tree, &view, source, maximum, 1, limits(), &cancel)
                .is_err()
        );
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(MacroProjection::recognize(&tree, &view, source, 0, 1, limits(), &cancel).is_err());
        assert_eq!(view.as_bytes(), source);
    }

    #[test]
    fn projection_preserves_excluded_gap_bytes_and_selected_macro_names() {
        let first = "#define FIRST 1 /* comment */ \"tail\"\n";
        let gap = "#define DECOY \"hidden\"\n";
        let second = "\n#define SECOND \"second\"\n";
        let source = format!("{first}{gap}{second}");
        let point = |index: usize| {
            let prefix = &source.as_bytes()[..index];
            tree_sitter::Point::new(
                prefix.iter().filter(|byte| **byte == b'\n').count(),
                prefix
                    .iter()
                    .rposition(|byte| *byte == b'\n')
                    .map_or(index, |last| index - last - 1),
            )
        };
        let ranges = [0..first.len(), (first.len() + gap.len())..source.len()].map(|span| {
            tree_sitter::Range {
                start_byte: span.start,
                end_byte: span.end,
                start_point: point(span.start),
                end_point: point(span.end),
            }
        });
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .unwrap();
        parser.set_included_ranges(&ranges).unwrap();
        let tree = parser.parse(source.as_bytes(), None).unwrap();
        let cancel = Cancellation::new();
        let view =
            NativeSourceView::new(source.as_bytes(), "utf-8", usize::MAX, usize::MAX, &cancel)
                .unwrap();
        let ProjectionOutcome::Projected(plan) =
            MacroProjection::recognize(&tree, &view, source.as_bytes(), 0, 2, limits(), &cancel)
                .unwrap()
        else {
            panic!("expected replacement projection")
        };
        assert_eq!(
            &plan.as_bytes()[first.len()..first.len() + gap.len()],
            gap.as_bytes()
        );
        let repaired = parser.parse(plan.as_bytes(), None).unwrap();
        assert!(
            !repaired.root_node().has_error(),
            "{}",
            repaired.root_node().to_sexp()
        );
        let mut cursor = repaired.root_node().walk();
        let names = repaired
            .root_node()
            .named_children(&mut cursor)
            .filter_map(|node| {
                node.child_by_field_name("name")
                    .map(|name| &source[name.byte_range()])
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["FIRST", "SECOND"]);
        assert_eq!(view.as_bytes(), source.as_bytes());
    }

    #[test]
    fn empty_macro_headers_keep_original_bytes_and_no_room_refuses_private_repair() {
        let source = b"#define EMPTY\n#define WRITTEN 1 /* comment */ \"tail\"\nint after(void) { return 0; }\n";
        let cancel = Cancellation::new();
        let view = NativeSourceView::new(source, "utf-8", usize::MAX, usize::MAX, &cancel).unwrap();
        let tree = parse(source);
        let ProjectionOutcome::Projected(plan) =
            MacroProjection::recognize(&tree, &view, source, 0, 1, limits(), &cancel).unwrap()
        else {
            panic!("expected replacement projection")
        };
        assert_eq!(
            &plan.as_bytes()[..b"#define EMPTY\n".len()],
            b"#define EMPTY\n"
        );
        let repaired = parse(plan.as_bytes());
        let first = repaired.root_node().named_child(0).unwrap();
        assert_eq!(first.kind(), "preproc_def");
        assert!(first.child_by_field_name("value").is_none());
        let no_room = b"#define EMPTY \nint ordinary_after_empty(void) { return 0; }\n";
        let view =
            NativeSourceView::new(no_room, "utf-8", usize::MAX, usize::MAX, &cancel).unwrap();
        let tree = parse(no_room);
        assert!(
            matches!(MacroProjection::recognize(&tree, &view, no_room, 0, 1, limits(), &cancel).unwrap(), ProjectionOutcome::Unavailable { work_nodes, work_depth } if work_nodes == tree.root_node().descendant_count() && work_depth > 0)
        );
        assert_eq!(view.as_bytes(), no_room);
    }

    #[test]
    fn complete_and_limited_proofs_retain_native_node_credit() {
        let cancel = Cancellation::new();
        for (source, expected) in [
            (
                b"#define WRITTEN 1'000 \"tail\"\nint after(void) { return 0; }\n".as_slice(),
                0,
            ),
            (
                b"#define WRITTEN 1000 /* comment */ \"tail\"\nint after(void) { return 0; }\n"
                    .as_slice(),
                1,
            ),
            (
                b"#define EMPTY \nint ordinary_after_empty(void) { return 0; }\n".as_slice(),
                2,
            ),
        ] {
            let tree = parse(source);
            let nodes = tree.root_node().descendant_count();
            let view =
                NativeSourceView::new(source, "utf-8", usize::MAX, usize::MAX, &cancel).unwrap();
            let mut budget = limits();
            budget.nodes = nodes;
            let outcome =
                MacroProjection::recognize(&tree, &view, source, 0, 1, budget, &cancel).unwrap();
            let (kind, processed, depth) = match outcome {
                ProjectionOutcome::Unchanged {
                    work_nodes,
                    work_depth,
                } => (0, work_nodes, work_depth),
                ProjectionOutcome::Projected(plan) => (1, plan.work_nodes, plan.work_depth),
                ProjectionOutcome::Unavailable {
                    work_nodes,
                    work_depth,
                } => (2, work_nodes, work_depth),
                ProjectionOutcome::Limited { .. } => panic!("a complete native tree fits exactly"),
            };
            assert_eq!(kind, expected);
            assert_eq!(processed, nodes);
            assert!(depth > 0);
            let mut budget = limits();
            budget.nodes = nodes - 1;
            assert!(
                matches!(MacroProjection::recognize(&tree, &view, source, 0, 1, budget, &cancel).unwrap(), ProjectionOutcome::Limited { work_nodes, limit: ProofLimit::Nodes, .. } if work_nodes == nodes - 1)
            );
            let mut budget = limits();
            budget.depth = 1;
            assert!(matches!(
                MacroProjection::recognize(&tree, &view, source, 0, 1, budget, &cancel).unwrap(),
                ProjectionOutcome::Limited {
                    work_nodes: 2,
                    work_depth: 1,
                    limit: ProofLimit::Depth
                }
            ));
            assert_eq!(view.as_bytes(), source);
        }
    }

    #[test]
    fn cpp_source_proof_repairs_raw_recovery_and_excludes_nested_directive_decoys() {
        let source = b"#define RAW u8R\"tag(\r\n#define FAKE \"hidden\" /* literal */\r\n)tag\"\r\nnamespace written { int after(void) { return 0; } }\r\n";
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let cancellation = Cancellation::new();
        let view =
            NativeSourceView::new(source, "utf-8", usize::MAX, usize::MAX, &cancellation).unwrap();
        let ProjectionOutcome::Projected(plan) =
            MacroProjection::recognize_cpp(&tree, &view, source, 0, 1, limits(), &cancellation)
                .unwrap()
        else {
            panic!("CPP raw recovery requires a private projection");
        };
        assert_eq!(plan.as_bytes().len(), source.len());
        assert_eq!(
            plan.as_bytes()
                .iter()
                .enumerate()
                .filter(|(_, byte)| matches!(**byte, b'\r' | b'\n'))
                .collect::<Vec<_>>(),
            source
                .iter()
                .enumerate()
                .filter(|(_, byte)| matches!(**byte, b'\r' | b'\n'))
                .collect::<Vec<_>>()
        );
        let repaired = parser.parse(plan.as_bytes(), None).unwrap();
        assert!(
            !repaired.root_node().has_error(),
            "{}",
            repaired.root_node().to_sexp()
        );
        let first = repaired.root_node().named_child(0).unwrap();
        assert_eq!(first.kind(), "preproc_def");
        assert_eq!(
            &source[first.child_by_field_name("name").unwrap().byte_range()],
            b"RAW"
        );
        assert_eq!(
            repaired.root_node().named_child(1).unwrap().kind(),
            "namespace_definition"
        );
        assert_eq!(view.as_bytes(), source);
        let mut short = limits();
        short.nodes = tree.root_node().descendant_count() - 1;
        assert!(matches!(
            MacroProjection::recognize_cpp(&tree, &view, source, 0, 1, short, &cancellation)
                .unwrap(),
            ProjectionOutcome::Limited {
                limit: ProofLimit::Nodes,
                ..
            }
        ));
        let mut memory = limits();
        memory.owned_bytes = 1;
        assert!(
            MacroProjection::recognize_cpp(&tree, &view, source, 0, 1, memory, &cancellation)
                .is_err()
        );
    }
}
