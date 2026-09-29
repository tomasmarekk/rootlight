//! Adapts first-party call-chain and construction evidence before optional retention.
//! Native trees locate expression starts and assignment values. Complete source
//! spans replace reference fragments inside admitted ranges before query retention.

use std::{collections::BTreeMap, ops::Range};

use rootlight_adapter_sdk::AdapterError;
use rootlight_vba::{
    CallChainRead, CallChainRole, call_chain_statement, value_application_expression,
};

use super::{QueryCandidate, QueryInput, QueryLimit, QueryScanLimits, StructuralRole};

#[derive(Default)]
pub(super) struct Calls {
    intervals: BTreeMap<usize, usize>,
}

enum ScanError {
    Adapter(AdapterError),
    Limit,
}

#[derive(Clone, Copy)]
enum ReadKind {
    Statement,
    SplitValue,
}

impl Calls {
    pub(super) fn contains(&self, range: Range<usize>) -> bool {
        self.intervals
            .range(..=range.start)
            .next_back()
            .is_some_and(|(_, end)| range.end <= *end)
    }

    pub(super) fn discover(
        &mut self,
        input: QueryInput<'_>,
        limits: QueryScanLimits,
        work: &mut usize,
        captures: &mut usize,
        retain: &mut impl FnMut(QueryCandidate) -> Result<(), AdapterError>,
    ) -> Result<Option<QueryLimit>, AdapterError> {
        let mut cursor = input.tree.walk();
        let mut check = || {
            input
                .cancellation
                .check()
                .map_err(|error| ScanError::Adapter(error.into()))?;
            if *work >= limits.matches {
                return Err(ScanError::Limit);
            }
            *work += 1;
            Ok(())
        };
        let scan = (|| {
            loop {
                check()?;
                let node = cursor.node();
                if node.kind() == "new_expression"
                    && !node.has_error()
                    && !self.contains(node.byte_range())
                    && let Some(range) = rootlight_vba::constructed_type_range(
                        input.source.get(node.start_byte()..).unwrap_or_default(),
                        &mut check,
                    )?
                {
                    let start = node.start_byte();
                    let end = start + range.end;
                    if end >= node.end_byte() {
                        if limits.captures.saturating_sub(*captures) < 2 {
                            return Ok(Some(QueryLimit::Capture));
                        }
                        // The class path can extend past the native New node
                        // into a recovered member statement. Own both pieces,
                        // but keep lifecycle semantics separate from type lookup.
                        self.intervals.insert(start, end);
                        for (start, syntax) in [
                            (start, "vba.construction"),
                            (start + range.start, "vba.constructed_type_identifier"),
                        ] {
                            check()?;
                            *captures += 1;
                            retain(QueryCandidate {
                                start,
                                end,
                                role: StructuralRole::Reference,
                                syntax,
                                required: false,
                                native_depth: 0,
                            })
                            .map_err(ScanError::Adapter)?;
                        }
                    }
                }
                let mut value = matches!(
                    node.kind(),
                    "assignment_statement" | "let_statement" | "set_statement"
                )
                .then(|| node.child_by_field_name("right"))
                .flatten();
                if let Some(right) = value {
                    let suffix = input.source.get(right.end_byte()..).unwrap_or_default();
                    let trivia = rootlight_vba::logical_whitespace_len(suffix, &mut check)?;
                    if suffix.get(trivia) != Some(&b'(') {
                        value = None;
                    }
                }
                if (node.kind() == "call_statement" || value.is_some())
                    && !node.has_error()
                    && !self.contains(node.byte_range())
                    && statement_boundary(input.source, node.start_byte(), &mut check)?
                {
                    let remaining = limits.captures.saturating_sub(*captures);
                    let native = value.unwrap_or(node).byte_range();
                    let start = native.start;
                    match self.consider(
                        input.source,
                        native,
                        if value.is_some() {
                            ReadKind::SplitValue
                        } else {
                            ReadKind::Statement
                        },
                        remaining / 2,
                        &mut check,
                    )? {
                        CallChainRead::TokenLimit => return Ok(Some(QueryLimit::Capture)),
                        CallChainRead::Unsupported => {}
                        CallChainRead::Parsed(chain) => {
                            for part in chain.parts() {
                                check()?;
                                *captures += 1;
                                let span = part.span();
                                let syntax = match part.role() {
                                    CallChainRole::ValueIdentifier => "vba.value_identifier",
                                    CallChainRole::InvokedValueIdentifier => {
                                        "vba.invoked_get_identifier"
                                    }
                                    CallChainRole::Member => "vba.get_member",
                                    CallChainRole::InvokedMember => "vba.invoked_member",
                                    CallChainRole::InvokedValueMember => "vba.invoked_get_member",
                                    CallChainRole::Application => "vba.application",
                                    CallChainRole::ArgumentName => "vba.nonvalue_identifier",
                                    CallChainRole::Instance => "vba.instance",
                                    CallChainRole::SelfMember => "vba.self_get_member",
                                    CallChainRole::InvokedSelfMember => {
                                        "vba.self_invoked_get_member"
                                    }
                                };
                                retain(QueryCandidate {
                                    start: start + span.start,
                                    end: start + span.end,
                                    role: StructuralRole::Reference,
                                    syntax,
                                    required: false,
                                    native_depth: 0,
                                })
                                .map_err(ScanError::Adapter)?;
                            }
                        }
                    }
                }
                if cursor.goto_first_child() {
                    continue;
                }
                while !cursor.goto_next_sibling() {
                    check()?;
                    if !cursor.goto_parent() {
                        return Ok(None);
                    }
                }
            }
        })();
        match scan {
            Ok(limit) => Ok(limit),
            Err(ScanError::Limit) => Ok(Some(QueryLimit::Match)),
            Err(ScanError::Adapter(error)) => Err(error),
        }
    }

    fn consider<E>(
        &mut self,
        source: &[u8],
        native: Range<usize>,
        kind: ReadKind,
        maximum_tokens: usize,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<CallChainRead, E> {
        let Some(tail) = source.get(native.start..) else {
            return Ok(CallChainRead::Unsupported);
        };
        let result = match kind {
            ReadKind::Statement => call_chain_statement(tail, maximum_tokens, check)?,
            ReadKind::SplitValue => value_application_expression(tail, maximum_tokens, check)?,
        };
        if let CallChainRead::Parsed(chain) = &result {
            let end = native.start + chain.end();
            // Never suppress a native statement extending beyond the source
            // reader's complete boundary. Value recovery must add missing bytes;
            // already complete native expressions keep their original evidence.
            if end < native.end || (matches!(kind, ReadKind::SplitValue) && end == native.end) {
                return Ok(CallChainRead::Unsupported);
            }
            self.intervals.insert(native.start, end);
        }
        Ok(result)
    }
}

fn statement_boundary<E>(
    source: &[u8],
    mut start: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    while start > 0 {
        if start.is_multiple_of(512) {
            check()?
        }
        match source.get(start - 1) {
            Some(b' ' | b'\t') => start -= 1,
            Some(b'\r' | b'\n' | b':') => return Ok(true),
            _ => return Ok(false),
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;

    #[test]
    fn construction_capture_owns_the_full_type_path_without_reference_fragments() {
        use super::super::QueryPackRegistry;
        use crate::{GrammarFamily, registry::language_for};
        use rootlight_cancel::Cancellation;

        let registry = QueryPackRegistry::audited().unwrap();
        let pack = registry.get(GrammarFamily::Vba).unwrap();
        for expression in [
            "New Target",
            "New[Target]",
            "New Primary.Target",
            "New [Primary].[Target]",
            "New _\r\n Primary _\r\n .Target",
            "New Primary.Target.Extra",
        ] {
            for body in [
                format!("Set item = {expression}"),
                format!("With {expression}\n.Touch\nEnd With"),
                format!("If {expression} Is Nothing Then\nEnd If"),
            ] {
                let source = format!("Sub Run()\n{body}\nEnd Sub\n");
                let mut parser = tree_sitter::Parser::new();
                parser
                    .set_language(&language_for(GrammarFamily::Vba))
                    .unwrap();
                let tree = parser.parse(&source, None).unwrap();
                let extraction = pack
                    .extract(
                        GrammarFamily::Vba,
                        &tree,
                        source.as_bytes(),
                        10_000,
                        1024,
                        &Cancellation::new(),
                    )
                    .unwrap();
                assert!(extraction.limit.is_none());
                let start = source.find(expression).unwrap();
                let range =
                    rootlight_vba::constructed_type_range(expression.as_bytes(), &mut || {
                        Ok::<_, Infallible>(())
                    })
                    .unwrap()
                    .unwrap();
                let references: Vec<_> = extraction
                    .candidates
                    .iter()
                    .filter(|part| {
                        part.role == StructuralRole::Reference
                            && part.start >= start
                            && part.start < start + expression.len()
                    })
                    .collect();
                assert_eq!(references.len(), 2, "{source}: {references:?}");
                for (syntax, begin) in [
                    ("vba.construction", start),
                    ("vba.constructed_type_identifier", start + range.start),
                ] {
                    assert!(
                        references.iter().any(|part| part.syntax == syntax
                            && part.start == begin
                            && part.end == start + expression.len()),
                        "{source}: {references:?}"
                    );
                }
                let limited = pack
                    .extract(
                        GrammarFamily::Vba,
                        &tree,
                        source.as_bytes(),
                        10_000,
                        1,
                        &Cancellation::new(),
                    )
                    .unwrap();
                assert!(limited.limit.is_some());
                assert!(
                    !limited
                        .candidates
                        .iter()
                        .any(|part| part.syntax == "vba.constructed_type_identifier")
                );
            }
        }
    }

    #[test]
    fn malformed_constructions_never_publish_a_valid_type_prefix() {
        use super::super::QueryPackRegistry;
        use crate::{GrammarFamily, registry::language_for};
        use rootlight_cancel::Cancellation;

        let registry = QueryPackRegistry::audited().unwrap();
        let pack = registry.get(GrammarFamily::Vba).unwrap();
        for expression in [
            "New Target()",
            "New Target(1)",
            "New Target!Member",
            "New Primary.",
            "New Primary .Target",
            "New Target$",
            "New Target&",
            "New Primary _\n .",
        ] {
            let source = format!("Sub Run()\nSet item = {expression}\nEnd Sub\n");
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&language_for(GrammarFamily::Vba))
                .unwrap();
            let tree = parser.parse(&source, None).unwrap();
            let extraction = pack
                .extract(
                    GrammarFamily::Vba,
                    &tree,
                    source.as_bytes(),
                    10_000,
                    1024,
                    &Cancellation::new(),
                )
                .unwrap();
            assert!(extraction.limit.is_none());
            assert!(
                !extraction
                    .candidates
                    .iter()
                    .any(|part| part.syntax == "vba.constructed_type_identifier"),
                "{source}"
            );
        }
    }

    #[test]
    fn extraction_retains_one_whole_call_and_obeys_the_optional_fact_budget() {
        use super::super::{QueryPackRegistry, StructuralRole};
        use crate::{GrammarFamily, registry::language_for};
        use rootlight_cancel::Cancellation;

        let source = b"Sub Run()\nfactory.Make().Save First(1) + Second(2), \"note\"\nEnd Sub\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Vba))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let registry = QueryPackRegistry::audited().unwrap();
        let pack = registry.get(GrammarFamily::Vba).unwrap();
        let extraction = pack
            .extract(
                GrammarFamily::Vba,
                &tree,
                source,
                10_000,
                1024,
                &Cancellation::new(),
            )
            .unwrap();
        assert!(extraction.limit.is_none());
        for (text, syntax) in [
            (b"factory".as_slice(), "vba.value_identifier"),
            (
                b"factory.Make().Save First(1) + Second(2), \"note\"".as_slice(),
                "vba.application",
            ),
            (b"factory.Make().Save".as_slice(), "vba.invoked_member"),
            (b"First(1)".as_slice(), "vba.application"),
            (b"Second(2)".as_slice(), "vba.application"),
        ] {
            assert_eq!(
                extraction
                    .candidates
                    .iter()
                    .filter(|part| part.syntax == syntax
                        && source.get(part.start..part.end) == Some(text))
                    .count(),
                1,
                "{text:?}: {syntax}"
            );
        }
        assert!(
            extraction
                .candidates
                .iter()
                .any(|part| part.role == StructuralRole::StringLiteral
                    && source.get(part.start..part.end) == Some(b"\"note\"".as_slice()))
        );
        let limited = pack
            .extract(
                GrammarFamily::Vba,
                &tree,
                source,
                10_000,
                2,
                &Cancellation::new(),
            )
            .unwrap();
        assert!(limited.limit.is_some());
        assert!(
            limited
                .candidates
                .iter()
                .filter(|part| part.role == StructuralRole::Reference)
                .count()
                <= 2
        );
        assert!(
            limited
                .candidates
                .iter()
                .any(|part| part.role == StructuralRole::Definition)
        );
    }

    #[test]
    fn native_statement_partition_does_not_change_chain_evidence() {
        let source = b"factory.Make().Save First(1) + Second(2)";
        let mut complete = Calls::default();
        let mut split = Calls::default();
        let whole = complete
            .consider(
                source,
                0..source.len(),
                ReadKind::Statement,
                256,
                &mut || Ok::<(), Infallible>(()),
            )
            .unwrap();
        let fragmented = split
            .consider(source, 0..14, ReadKind::Statement, 256, &mut || {
                Ok::<(), Infallible>(())
            })
            .unwrap();
        assert!(matches!(whole, CallChainRead::Parsed(_)));
        assert_eq!(whole, fragmented);
        assert_eq!(complete.intervals, split.intervals);
        for range in [0..14, 14..19, 20..source.len(), 20..25, 31..37] {
            assert!(split.contains(range));
        }
        assert!(!split.contains(source.len()..source.len() + 1));
    }

    #[test]
    fn split_values_extend_only_within_the_same_logical_statement() {
        for (source, end) in [
            (b"Pick(1)\nOther(2)".as_slice(), Some(7)),
            (b"Pick( _\n 1): Other(2)".as_slice(), Some(11)),
            (b"Pick\n(1)".as_slice(), None),
            (b"Pick: (1)".as_slice(), None),
            (b"Pick(1) Other(2)".as_slice(), None),
        ] {
            let mut calls = Calls::default();
            let result = calls
                .consider(source, 0..4, ReadKind::SplitValue, 128, &mut || {
                    Ok::<(), Infallible>(())
                })
                .unwrap();
            if let Some(end) = end {
                let CallChainRead::Parsed(expression) = result else {
                    panic!("{source:?}")
                };
                assert_eq!(expression.end(), end);
                assert!(calls.contains(0..end));
                assert!(!calls.contains(end..source.len()));
            } else {
                assert_eq!(result, CallChainRead::Unsupported);
                assert!(calls.intervals.is_empty());
            }
        }
    }

    #[test]
    fn unsupported_or_limited_statements_never_suppress_native_evidence() {
        for (source, limit) in [
            (b"factory.Make().Save AddressOf Handler".as_slice(), 256),
            (b"factory.Make().Save 1".as_slice(), 2),
            (b"factory.Make().Save 1\nNextLine".as_slice(), 256),
        ] {
            let mut calls = Calls::default();
            let result = calls
                .consider(
                    source,
                    0..source.len(),
                    ReadKind::Statement,
                    limit,
                    &mut || Ok::<(), Infallible>(()),
                )
                .unwrap();
            assert!(!matches!(result, CallChainRead::Parsed(_)));
            assert!(calls.intervals.is_empty());
        }
    }
}
