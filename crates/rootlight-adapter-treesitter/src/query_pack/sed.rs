//! Static Sed label operands are admitted only from complete clean native input.
//! These captures describe written names, never command execution or label bodies.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use std::ops::Range;
use tree_sitter::Node;

use super::{HARD_MAX_QUERY_CAPTURES, QueryCandidate, StructuralRole};

pub(super) struct CaptureCache<'tree, 'source> {
    root: Node<'tree>,
    source: &'source [u8],
    full_input: bool,
    complete: bool,
    initialized: bool,
    remaining: usize,
    exhausted: bool,
    opaque_label: bool,
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_error()
        && !node.is_missing()
        && !node.has_error()
        && node.start_byte() <= node.end_byte()
        && node.end_byte() <= source.len()
}

impl<'tree, 'source> CaptureCache<'tree, 'source> {
    pub(super) fn new(
        source: &'source [u8],
        root: Node<'tree>,
        maximum: usize,
        complete: bool,
    ) -> Self {
        let full_input = complete
            && root.kind() == "script"
            && (root.start_byte() == 0
                || (root.start_byte() == 3 && source.starts_with(b"\xef\xbb\xbf")))
            && root.end_byte() == source.len();
        Self {
            root,
            source,
            full_input,
            complete: full_input && clean(root, source),
            initialized: false,
            remaining: maximum.min(HARD_MAX_QUERY_CAPTURES).saturating_mul(64),
            exhausted: false,
            opaque_label: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(remaining) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = remaining;
        Ok(true)
    }

    fn name(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        if !self.step(cancel)? || node.kind() != "label_text" || !clean(node, self.source) {
            return Ok(false);
        }
        let Some(raw) = self.source.get(node.byte_range()) else {
            return Ok(false);
        };
        if !raw
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        {
            return Ok(false);
        }
        for chunk in raw.chunks(64) {
            if !self.step(cancel)? {
                return Ok(false);
            }
            if !chunk
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn operand(
        &mut self,
        command: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        if !self.step(cancel)? || !clean(command, self.source) {
            return Ok(None);
        }
        let Some(outer) = command.parent().filter(|node| node.kind() == "command") else {
            return Ok(None);
        };
        if !clean(outer, self.source) || outer.child_by_field_name("issue").is_some() {
            return Ok(None);
        }
        let Some(verb) = command.child_by_field_name("name") else {
            return Ok(None);
        };
        let supported = match command.kind() {
            "label_command" => {
                self.source.get(verb.byte_range()) == Some(b":".as_slice())
                    && outer.child_by_field_name("address").is_none()
                    && outer.child_by_field_name("negation").is_none()
            }
            "branch_command" => self
                .source
                .get(verb.byte_range())
                .is_some_and(|raw| matches!(raw, b"b" | b"t" | b"T")),
            _ => false,
        };
        let Some(label) = command.child_by_field_name("label") else {
            return Ok(None);
        };
        if !supported || label.named_child_count() != 1 || !clean(label, self.source) {
            return Ok(None);
        }
        let Some(name) = label.named_child(0) else {
            return Ok(None);
        };
        Ok((name.byte_range() == label.byte_range() && self.name(name, cancel)?).then_some(name))
    }

    fn initialize(&mut self, cancel: &Cancellation) -> Result<(), AdapterError> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;
        if !self.complete {
            return Ok(());
        }
        let mut cursor = self.root.walk();
        loop {
            if !self.step(cancel)? {
                return Ok(());
            }
            let node = cursor.node();
            // The grammar represents some malformed syntax as ordinary named
            // nodes; Tree-sitter's has_error bit alone does not reject it.
            if node.kind() == "syntax_issue" || !clean(node, self.source) {
                self.complete = false;
            }
            if node.kind() == "label_command" && self.operand(node, cancel)?.is_none() {
                // An opaque declaration can collide with a simple written name;
                // do not bind references from an incomplete label census.
                self.opaque_label = true;
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if !self.step(cancel)? {
                    return Ok(());
                }
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Ok(());
                }
            }
        }
    }

    fn section(
        &mut self,
        branch: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Range<usize>>, AdapterError> {
        let Some(operand) = self.operand(branch, cancel)? else {
            return Ok(None);
        };
        let Some(command) = branch.parent() else {
            return Ok(None);
        };
        let Some(list) = command
            .parent()
            .filter(|node| node.kind() == "command_list")
        else {
            return Ok(None);
        };
        // This is an internal written-section witness across direct siblings.
        // It creates neither a label body nor a semantic enclosing edge.
        if list.parent() != Some(self.root)
            || self.opaque_label
            || command.child_by_field_name("address").is_some()
            || command.child_by_field_name("negation").is_some()
        {
            return Ok(None);
        }
        let mut previous = command.prev_named_sibling();
        while let Some(node) = previous {
            if !self.step(cancel)? || !clean(node, self.source) {
                return Ok(None);
            }
            if !matches!(node.kind(), "comment" | "command_separator") {
                if node.kind() != "command"
                    || node.child_by_field_name("address").is_some()
                    || node.child_by_field_name("negation").is_some()
                    || node.named_child_count() != 1
                {
                    return Ok(None);
                }
                let Some(body) = node.named_child(0) else {
                    return Ok(None);
                };
                match body.kind() {
                    "label_command" => {
                        return Ok(self
                            .operand(body, cancel)?
                            .map(|_| body.start_byte()..operand.end_byte()));
                    }
                    "simple_command" => {
                        let simple = body
                            .child_by_field_name("name")
                            .and_then(|name| self.source.get(name.byte_range()));
                        if !simple.is_some_and(|name| {
                            matches!(
                                name,
                                b"g" | b"G" | b"h" | b"H" | b"x" | b"p" | b"P" | b"l" | b"="
                            )
                        }) {
                            return Ok(None);
                        }
                    }
                    "substitute_command" => {
                        let mut children = body.walk();
                        for child in body.named_children(&mut children) {
                            if !self.step(cancel)? || child.kind() == "substitution_flags" {
                                return Ok(None);
                            }
                        }
                    }
                    _ => return Ok(None),
                }
            }
            previous = node.prev_named_sibling();
        }
        Ok(None)
    }
}

pub(super) fn candidate<'tree>(
    node: Node<'tree>,
    role: StructuralRole,
    cancel: &Cancellation,
    cache: &mut CaptureCache<'tree, '_>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cache.initialize(cancel)?;
    if !cache.step(cancel)? {
        return Ok(None);
    }
    let root = role == StructuralRole::Root && node.kind() == "script";
    let mut section = None;
    let (role, syntax) = if !cache.complete {
        // An independently clean operand is precise source evidence, even when
        // another command prevents a complete label census. It proves no target.
        let unproved_operand = role == StructuralRole::Reference
            && cache.full_input
            && node.kind() == "label_text"
            && if let Some(command) = node.parent().and_then(|label| label.parent()) {
                command.kind() == "branch_command" && cache.operand(command, cancel)? == Some(node)
            } else {
                false
            };
        if unproved_operand {
            (StructuralRole::Reference, "sed.unproved_branch")
        } else {
            (
                if root {
                    role
                } else {
                    StructuralRole::Signature
                },
                "sed.native_context_unavailable",
            )
        }
    } else {
        match (role, node.kind()) {
            (StructuralRole::Root, "script") => (role, "sed.script"),
            (StructuralRole::Module, "script") => (role, "sed.file"),
            (StructuralRole::Declaration, "label_command") => {
                if cache.operand(node, cancel)?.is_some() {
                    (role, "sed.statement_label")
                } else {
                    (StructuralRole::Signature, "sed.label_unavailable")
                }
            }
            (StructuralRole::Definition | StructuralRole::Reference, "label_text") => {
                let command = node.parent().and_then(|label| label.parent());
                let admitted = match command {
                    Some(command) => cache.operand(command, cancel)? == Some(node),
                    None => false,
                };
                match (role, command.map(|command| command.kind()), admitted) {
                    (StructuralRole::Definition, Some("label_command"), true) => {
                        (role, "sed.label_name")
                    }
                    (StructuralRole::Reference, Some("branch_command"), true)
                        if !cache.opaque_label =>
                    {
                        (role, "sed.written_branch")
                    }
                    (StructuralRole::Reference, Some("branch_command"), _) => {
                        (role, "sed.unproved_branch")
                    }
                    _ => (StructuralRole::Signature, "sed.label_unavailable"),
                }
            }
            (StructuralRole::Signature, "branch_command") => {
                section = cache.section(node, cancel)?;
                if section.is_some() {
                    (role, "sed.written_section_target")
                } else if cache.operand(node, cancel)?.is_none() {
                    (role, "sed.branch_unavailable")
                } else {
                    (role, "sed.section_unavailable")
                }
            }
            (StructuralRole::Comment, "comment") => (role, "sed.comment"),
            (StructuralRole::StringLiteral, "translation_string") => (role, "sed.string"),
            _ => return Ok(None),
        }
    };
    let mut result = QueryCandidate {
        start: section.as_ref().map_or_else(
            || {
                if !cache.complete && !root && role != StructuralRole::Reference {
                    0
                } else {
                    node.start_byte()
                }
            },
            |range| range.start,
        ),
        end: section.as_ref().map_or_else(
            || {
                if !cache.complete && !root && role != StructuralRole::Reference {
                    cache.source.len()
                } else {
                    node.end_byte()
                }
            },
            |range| range.end,
        ),
        role,
        syntax,
        required: role.belongs_to_identity_closure() || role == StructuralRole::Reference,
        native_depth: 0,
    };
    super::canonicalize_file_root_preamble(&mut result, node, cache.source, cache.complete);
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    struct Probe(Cancellation);
    impl tree_sitter_gsed::CancellationProbe for Probe {
        fn is_cancelled(&self) -> bool {
            self.0.check().is_err()
        }
    }

    fn tree(source: &[u8]) -> tree_sitter::Tree {
        let scope =
            tree_sitter_gsed::ScannerScope::begin(Probe(Cancellation::new()), 10_000_000).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_gsed::LANGUAGE_BRE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        drop(parser);
        scope.finish().unwrap();
        tree
    }

    #[test]
    fn original_six_native_names_and_two_written_section_certificates_are_exact() {
        let raw = include_bytes!("../../tests/fixtures/sed/mario.sed");
        let cancel = Cancellation::new();
        let view = crate::source_view::NativeSourceView::new_with_policy(
            raw,
            "utf-8",
            crate::source_view::LineEndingPolicy::CrlfFileText,
            1_000_000,
            1_000_000,
            &cancel,
        )
        .unwrap();
        let source = view.as_bytes();
        assert_eq!(source.len(), 8509);
        let parsed = tree(source);
        let root = parsed.root_node();
        let mut cache = CaptureCache::new(source, root, 16_384, true);
        for (start, end, role, expected) in [
            (938, 942, StructuralRole::Definition, "sed.label_name"),
            (951, 955, StructuralRole::Definition, "sed.label_name"),
            (977, 982, StructuralRole::Definition, "sed.label_name"),
            (5202, 5205, StructuralRole::Definition, "sed.label_name"),
            (946, 949, StructuralRole::Reference, "sed.written_branch"),
            (972, 975, StructuralRole::Reference, "sed.written_branch"),
        ] {
            let name = root.descendant_for_byte_range(start, end).unwrap();
            let captured = candidate(name, role, &cancel, &mut cache).unwrap().unwrap();
            assert_eq!(
                (captured.start, captured.end, captured.role, captured.syntax),
                (start, end, role, expected)
            );
            if role == StructuralRole::Reference {
                let command = name.parent().unwrap().parent().unwrap();
                let section = candidate(command, StructuralRole::Signature, &cancel, &mut cache)
                    .unwrap()
                    .unwrap();
                assert_eq!(section.syntax, "sed.written_section_target");
                assert_eq!(section.start, if start == 946 { 937 } else { 950 });
                assert_eq!(section.end, end);
            }
        }
        assert!(!cache.exhausted());
    }

    #[test]
    fn nested_commands_and_control_boundaries_do_not_create_written_sections() {
        for source in [
            b":owner\n{\n b end\n}\n:end\n".as_slice(),
            b":owner\nq\nb end\n:end\n",
            b":owner\nb missing\nb end\n:end\n",
            b":owner\ns/x/y/e\nb end\n:end\n",
            b":owner\n1b end\n:end\n",
        ] {
            let parsed = tree(source);
            let start = source
                .windows(5)
                .position(|bytes| bytes == b"b end")
                .unwrap();
            let command = parsed
                .root_node()
                .descendant_for_byte_range(start + 2, start + 5)
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap();
            let mut cache = CaptureCache::new(source, parsed.root_node(), 16_384, true);
            let captured = candidate(
                command,
                StructuralRole::Signature,
                &Cancellation::new(),
                &mut cache,
            )
            .unwrap()
            .unwrap();
            assert_eq!(captured.syntax, "sed.section_unavailable");
        }
    }

    #[test]
    fn opaque_label_and_incomplete_native_input_never_certify_branch_binding() {
        let source = b":valid\nb end\n:end\n:opaque name\n";
        let parsed = tree(source);
        let name = parsed.root_node().descendant_for_byte_range(9, 12).unwrap();
        let mut cache = CaptureCache::new(source, parsed.root_node(), 16_384, true);
        let captured = candidate(
            name,
            StructuralRole::Reference,
            &Cancellation::new(),
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!(captured.syntax, "sed.unproved_branch");
        assert_eq!(captured.role, StructuralRole::Reference);
        assert!(captured.required);
        assert_eq!((captured.start, captured.end), (9, 12));
        assert_eq!(&source[captured.start..captured.end], b"end");
        assert!(cache.full_input);
        assert!(cache.opaque_label || !cache.complete);
        eprintln!(
            "sed-opaque-native-context: {}",
            parsed.root_node().to_sexp()
        );
        let mut partial = CaptureCache::new(source, parsed.root_node(), 16_384, false);
        let captured = candidate(
            name,
            StructuralRole::Reference,
            &Cancellation::new(),
            &mut partial,
        )
        .unwrap()
        .unwrap();
        assert_eq!(captured.syntax, "sed.native_context_unavailable");
        assert_eq!(captured.role, StructuralRole::Signature);
        assert!(!partial.full_input);
        assert_eq!((captured.start, captured.end), (0, source.len()));
    }

    #[test]
    fn query_credit_refusal_and_cancellation_do_not_admit_partial_identity() {
        let source = b":name\nb name\n";
        let parsed = tree(source);
        let root = parsed.root_node();
        let mut empty = CaptureCache::new(source, root, 0, true);
        assert!(
            candidate(root, StructuralRole::Root, &Cancellation::new(), &mut empty)
                .unwrap()
                .is_none()
        );
        assert!(empty.exhausted());
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        let mut cache = CaptureCache::new(source, root, 16_384, true);
        assert!(candidate(root, StructuralRole::Root, &cancel, &mut cache).is_err());
        assert!(!cache.exhausted());
    }
}
