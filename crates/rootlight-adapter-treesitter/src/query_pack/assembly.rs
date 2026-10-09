//! Assembly captures preserve complete written owners without assembled binding.
//! Direct names and matching closers fence identities; operands stay qualified.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

#[derive(Clone)]
struct OwnerProof {
    syntax: &'static str,
    name: Range<usize>,
    header: Range<usize>,
    container: bool,
}

/// Shares the caller's capture bound instead of constructing a source-sized map.
pub(super) struct CaptureCache {
    owners: HashMap<usize, Option<OwnerProof>>,
    maximum_entries: usize,
    exhausted: bool,
}

impl CaptureCache {
    pub(super) fn new(maximum_entries: usize) -> Self {
        Self {
            owners: HashMap::new(),
            maximum_entries,
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn owner(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<OwnerProof>, AdapterError> {
        cancellation.check()?;
        if let Some(proof) = self.owners.get(&node.id()) {
            return Ok(proof.clone());
        }
        if self.owners.len() >= self.maximum_entries {
            self.exhausted = true;
            return Ok(None);
        }
        let proof = owner_proof(node, source, cancellation)?;
        self.owners
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.owners.insert(node.id(), proof.clone());
        Ok(proof)
    }
}

fn is_owner(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "procedure_definition"
            | "macro_definition"
            | "segment_definition"
            | "data_definition"
            | "constant_definition"
            | "label"
            | "const"
    )
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}

fn field<'tree>(node: Node<'tree>, name: &str) -> Option<Node<'tree>> {
    node.child_by_field_name(name)
        .filter(|child| child.parent() == Some(node))
}

fn text<'source>(node: Node<'_>, source: &'source [u8]) -> Option<&'source str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}

fn masm_name(node: Node<'_>, source: &[u8]) -> bool {
    node.kind() == "ident"
        && clean(node, source)
        && text(node, source).is_some_and(|name| {
            structural_captured_name_for_language("assembly", name, 247).is_some()
        })
}

fn source_name(node: Node<'_>, source: &[u8], dotted: bool) -> bool {
    let first = |byte: u8| byte.is_ascii_alphabetic() || byte == b'_' || (dotted && byte == b'.');
    clean(node, source)
        && source.get(node.byte_range()).is_some_and(|bytes| {
            bytes != b"."
                && bytes.first().is_some_and(|byte| first(*byte))
                && bytes
                    .iter()
                    .all(|byte| first(*byte) || byte.is_ascii_digit())
        })
}

fn label_name<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    if let Some(name) = field(node, "name") {
        return Ok((name.kind() == "word").then_some(name));
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if matches!(child.kind(), "line_comment" | "block_comment") {
            continue;
        }
        return Ok(matches!(child.kind(), "ident" | "meta_ident").then_some(child));
    }
    Ok(None)
}

fn closer_matches(
    node: Node<'_>,
    name: Node<'_>,
    keyword: &[u8],
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let Some(keyword_start) = node.end_byte().checked_sub(keyword.len()) else {
        return Ok(false);
    };
    if !source
        .get(keyword_start..node.end_byte())
        .is_some_and(|bytes| bytes.eq_ignore_ascii_case(keyword))
    {
        return Ok(false);
    }
    let anchor = if keyword == b"ENDM" {
        field(node, "body").or_else(|| field(node, "header"))
    } else {
        field(node, "end_name").filter(|end_name| {
            masm_name(*end_name, source)
                && source
                    .get(name.byte_range())
                    .zip(source.get(end_name.byte_range()))
                    .is_some_and(|(opening, closing)| opening.eq_ignore_ascii_case(closing))
        })
    };
    let Some(anchor) = anchor.filter(|anchor| clean(*anchor, source)) else {
        return Ok(false);
    };
    let mut position = anchor.end_byte();
    if position > keyword_start {
        return Ok(false);
    }
    let whitespace = |start, end| {
        source.get(start..end).is_some_and(|bytes| {
            bytes
                .iter()
                .all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
        })
    };
    // The grammar hides closing markers, so prove their original source tail
    // after the named closer/body instead of inventing a missing token node.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if !matches!(child.kind(), "line_comment" | "block_comment") || child.end_byte() <= position
        {
            continue;
        }
        if !clean(child, source)
            || child.start_byte() < position
            || child.end_byte() > keyword_start
            || !whitespace(position, child.start_byte())
        {
            return Ok(false);
        }
        position = child.end_byte();
    }
    Ok(whitespace(position, keyword_start))
}

fn owner_proof(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<OwnerProof>, AdapterError> {
    cancellation.check()?;
    if !clean(node, source)
        || !node
            .parent()
            .is_some_and(|parent| matches!(parent.kind(), "program" | "masm_body"))
    {
        return Ok(None);
    }
    let container = match node.kind() {
        "procedure_definition" => {
            Some(("assembly.procedure", "procedure_header", b"ENDP".as_slice()))
        }
        "macro_definition" => Some(("assembly.macro", "macro_header", b"ENDM".as_slice())),
        "segment_definition" => Some(("assembly.segment", "segment_header", b"ENDS".as_slice())),
        _ => None,
    };
    if let Some((syntax, header_kind, closer)) = container {
        let Some(name) = field(node, "name").filter(|name| masm_name(*name, source)) else {
            return Ok(None);
        };
        let Some(header) = field(node, "header")
            .filter(|header| clean(*header, source) && header.kind() == header_kind)
        else {
            return Ok(None);
        };
        if name.end_byte() > header.start_byte()
            || !closer_matches(node, name, closer, source, cancellation)?
        {
            return Ok(None);
        }
        return Ok(Some(OwnerProof {
            syntax,
            name: name.byte_range(),
            header: node.start_byte()..header.end_byte(),
            container: true,
        }));
    }
    let (syntax, name) = match node.kind() {
        "data_definition" | "constant_definition" => {
            let Some(name) = field(node, "name").filter(|name| masm_name(*name, source)) else {
                return Ok(None);
            };
            if field(node, "value").is_none() {
                return Ok(None);
            }
            let syntax = if node.kind() == "data_definition" {
                "assembly.data"
            } else {
                "assembly.constant"
            };
            (syntax, name)
        }
        "const" => {
            let Some(name) = field(node, "name")
                .filter(|name| name.kind() == "word" && source_name(*name, source, false))
            else {
                return Ok(None);
            };
            ("assembly.source_constant", name)
        }
        "label" => {
            let Some(name) =
                label_name(node, cancellation)?.filter(|name| source_name(*name, source, true))
            else {
                return Ok(None);
            };
            ("assembly.source_label", name)
        }
        _ => return Ok(None),
    };
    Ok(Some(OwnerProof {
        syntax,
        name: name.byte_range(),
        header: node.byte_range(),
        container: false,
    }))
}

fn valid_owner_chain(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let mut current = Some(node);
    while let Some(owner) = current {
        cancellation.check()?;
        if owner.kind() == "ERROR"
            || owner.is_missing()
            || (is_owner(owner) && cache.owner(owner, source, cancellation)?.is_none())
        {
            return Ok(false);
        }
        current = owner.parent();
    }
    Ok(true)
}

/// Admits only complete source identities; unsupported contexts retain gap captures.
pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if node.kind() == "program" || role == StructuralRole::Expression {
        return Ok(true);
    }
    if matches!(
        role,
        StructuralRole::Declaration
            | StructuralRole::Definition
            | StructuralRole::Signature
            | StructuralRole::Scope
    ) {
        if !is_owner(node) || !valid_owner_chain(node, source, cancellation, cache)? {
            return Ok(false);
        }
        return Ok(cache
            .owner(node, source, cancellation)?
            .is_some_and(|proof| role != StructuralRole::Scope || proof.container));
    }
    Ok(matches!(
        role,
        StructuralRole::Comment | StructuralRole::StringLiteral
    ) && clean(node, source))
}

/// Projects original source ranges without operand, register or runtime target guesses.
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<QueryCandidate, AdapterError> {
    cancellation.check()?;
    let mut range = node.byte_range();
    let syntax = if node.kind() == "program" {
        range = 0..source.len();
        if role == StructuralRole::Expression {
            "assembly.file_semantics"
        } else {
            "assembly.file"
        }
    } else if role == StructuralRole::Expression {
        if is_owner(node) && !valid_owner_chain(node, source, cancellation, cache)? {
            "assembly.unsupported_owner"
        } else {
            match node.kind() {
                "macro_definition" => "assembly.macro_parameter_context",
                "instruction" | "data_definition" | "constant_definition" | "const" => {
                    "assembly.operand_context"
                }
                "meta" => "assembly.directive_context",
                "ERROR" => "assembly.unsupported_owner",
                _ => "assembly.owner_semantics",
            }
        }
    } else if matches!(
        role,
        StructuralRole::Declaration
            | StructuralRole::Definition
            | StructuralRole::Signature
            | StructuralRole::Scope
    ) {
        let proof = cache
            .owner(node, source, cancellation)?
            .ok_or_else(|| query_failure("query-assembly-unproved-owner"))?;
        match role {
            StructuralRole::Definition => {
                range = proof.name;
                match node.kind() {
                    "label" => "assembly.source_label",
                    "const" => "assembly.source_constant",
                    _ => "assembly.masm_name",
                }
            }
            StructuralRole::Signature => {
                range = proof.header;
                proof.syntax
            }
            _ => proof.syntax,
        }
    } else {
        match role {
            StructuralRole::Comment => "assembly.comment",
            StructuralRole::StringLiteral => "assembly.string",
            _ => return Err(query_failure("query-assembly-unproved-role")),
        }
    };
    let depth = native_depth(node, cancellation)?;
    Ok(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: if role == StructuralRole::Scope && node.kind() != "program" {
            depth
                .checked_add(1)
                .ok_or_else(|| query_failure("query-assembly-depth-overflow"))?
        } else {
            depth
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn extract(source: &str) -> (tree_sitter::Tree, Vec<QueryCandidate>) {
        let family = crate::GrammarFamily::Assembly;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::registry::language_for(family))
            .unwrap();
        let tree = parser.parse(source.as_bytes(), None).unwrap();
        let pack =
            super::super::QueryPack::compile(family, include_str!("../../queries/assembly.scm"))
                .unwrap();
        let extraction = pack
            .extract(
                family,
                &tree,
                source.as_bytes(),
                10_000,
                1024,
                &Cancellation::new(),
            )
            .unwrap();
        assert!(extraction.limit.is_none());
        (tree, extraction.candidates)
    }

    fn definitions<'source>(
        source: &'source str,
        captures: &[QueryCandidate],
    ) -> Vec<&'source str> {
        let mut definitions = captures
            .iter()
            .filter(|capture| capture.role == StructuralRole::Definition)
            .collect::<Vec<_>>();
        definitions.sort_by_key(|capture| (capture.start, capture.end));
        definitions
            .into_iter()
            .map(|capture| &source[capture.start..capture.end])
            .collect()
    }

    #[test]
    fn complete_written_owners_keep_exact_names_and_headers() {
        let source = "Code SEGMENT\nRun PROC NEAR\nnested:\nmov rax, rbx\nRun ENDP\nExpand MACRO value\nmov rax, value\nENDM\nValue DB 1, 2\nSize EQU 4\nconst LIMIT 5\nlabel entry\nCode ENDS\n";
        let (tree, captures) = extract(source);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        assert_eq!(
            definitions(source, &captures),
            [
                "Code", "Run", "nested", "Expand", "Value", "Size", "LIMIT", "entry"
            ]
        );
        let headers = captures
            .iter()
            .filter(|capture| capture.role == StructuralRole::Signature)
            .map(|capture| &source[capture.start..capture.end])
            .collect::<Vec<_>>();
        assert!(headers.contains(&"Run PROC NEAR"));
        assert!(headers.contains(&"Expand MACRO value"));
        assert!(headers.contains(&"Code SEGMENT"));
        for capture in &captures {
            assert!(source.get(capture.start..capture.end).is_some());
            if capture.role.belongs_to_identity_closure() && capture.role != StructuralRole::Scope {
                assert!(capture.required);
            }
        }
        assert!(!captures.iter().any(|capture| matches!(
            capture.role,
            StructuralRole::Call
                | StructuralRole::CallName
                | StructuralRole::ScopedCall
                | StructuralRole::Reference
                | StructuralRole::Import
        )));
    }

    #[test]
    fn instructions_directives_and_parameters_remain_scoped_contexts() {
        let source = ".globl external\nExpand MACRO formal\nmov rax, formal\nENDM\n";
        let (tree, captures) = extract(source);
        assert!(!tree.root_node().has_error());
        assert_eq!(definitions(source, &captures), ["Expand"]);
        for context in [
            "assembly.directive_context",
            "assembly.operand_context",
            "assembly.macro_parameter_context",
            "assembly.file_semantics",
        ] {
            assert!(
                captures
                    .iter()
                    .any(|capture| capture.role == StructuralRole::Expression
                        && capture.syntax == context),
                "{context}"
            );
        }
    }

    #[test]
    fn hidden_closers_keep_comments_empty_bodies_and_written_case() {
        for (source, names) in [
            (
                "Mixed PROC\nMOV AX, BX\nmixed /* physical closer */ ENDP\n",
                vec!["Mixed"],
            ),
            ("Empty MACRO\nENDM\n", vec!["Empty"]),
            ("Data SEGMENT\n; body trivia\nDATA ENDS\n", vec!["Data"]),
            (
                "Expand MACRO\nnop\n/* before closer */ ENDM\n",
                vec!["Expand"],
            ),
        ] {
            let (tree, captures) = extract(source);
            assert!(
                !tree.root_node().has_error(),
                "{}",
                tree.root_node().to_sexp()
            );
            assert_eq!(definitions(source, &captures), names, "{source}");
        }
    }

    #[test]
    fn inline_label_recovery_is_not_clean_instruction_coverage() {
        let source = "entry: mov rax, rbx\n";
        let (tree, captures) = extract(source);
        assert!(tree.root_node().has_error());
        let mut cursor = tree.root_node().walk();
        assert!(
            tree.root_node()
                .children(&mut cursor)
                .any(|child| child.is_missing())
        );
        assert_eq!(definitions(source, &captures), ["entry"]);
        assert!(captures.iter().any(|capture| {
            capture.role == StructuralRole::Expression
                && capture.syntax == "assembly.file_semantics"
        }));
        assert!(!captures.iter().any(|capture| {
            matches!(
                capture.role,
                StructuralRole::Call | StructuralRole::Reference
            )
        }));
    }

    #[test]
    fn mismatched_closers_and_recovery_cannot_borrow_written_children() {
        for source in [
            "Start PROC\ninside:\nmov rax, rbx\nOther ENDP\n",
            "Run PROC\ninside:\nnop\nRunner ENDP\n",
            "Broken PROC\ninside:\nmov rax, rbx\n",
            "Broken SEGMENT\nValue DB 1\nOther ENDS\n",
            "Empty MACRO\n; ENDM\n",
        ] {
            let (_, captures) = extract(source);
            assert!(definitions(source, &captures).is_empty(), "{source}");
            assert!(
                captures
                    .iter()
                    .any(|capture| capture.syntax == "assembly.unsupported_owner")
            );
        }
    }

    #[test]
    fn closing_keyword_prefixes_cannot_complete_an_owner() {
        for source in [
            "Run PROC\ninside:\nnop\nRun ENDPextra\n",
            "Data SEGMENT\nValue DB 1\nData ENDSextra\n",
            "Expand MACRO\ninside:\nnop\nENDMextra\n",
        ] {
            let (_, captures) = extract(source);
            assert!(definitions(source, &captures).is_empty(), "{source}");
        }
    }

    #[test]
    fn numeric_and_location_counter_labels_do_not_become_symbols() {
        let source = "1:\nnop\n.:\nnop\n.local:\nnop\nMixed:\nnop\n";
        let (tree, captures) = extract(source);
        assert!(!tree.root_node().has_error());
        assert_eq!(definitions(source, &captures), [".local", "Mixed"]);
        assert_eq!(
            captures
                .iter()
                .filter(|capture| capture.syntax == "assembly.unsupported_owner")
                .count(),
            2
        );
    }

    #[test]
    fn owner_cache_bound_and_cancellation_do_not_invent_a_pass() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::registry::language_for(
                crate::GrammarFamily::Assembly,
            ))
            .unwrap();
        let source = b"entry:\nnop\n";
        let tree = parser.parse(source, None).unwrap();
        let owner = tree.root_node().named_child(0).unwrap();
        let mut cache = CaptureCache::new(0);
        let cancellation = Cancellation::new();
        assert!(
            !retain_capture(
                owner,
                StructuralRole::Definition,
                source,
                &cancellation,
                &mut cache
            )
            .unwrap()
        );
        assert!(cache.exhausted());
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(
            retain_capture(
                owner,
                StructuralRole::Expression,
                source,
                &cancellation,
                &mut cache
            )
            .is_err()
        );
    }
}
