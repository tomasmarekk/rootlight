//! Admit native Coq declaration names without inferring a checked environment.
//! Flat vernacular sentences cannot establish module or section containment.

use rootlight_adapter_sdk::{AdapterError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, query_failure};

pub(super) struct CaptureCache<'tree> {
    root: Node<'tree>,
    remaining: usize,
    exhausted: bool,
}

impl<'tree> CaptureCache<'tree> {
    pub(super) const fn new(root: Node<'tree>, remaining: usize) -> Self {
        Self {
            root,
            remaining,
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn visit(&mut self, cancellation: &Cancellation) -> Result<bool, AdapterError> {
        cancellation.check()?;
        let Some(remaining) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = remaining;
        Ok(true)
    }

    fn depth(&self, node: Node<'tree>, cancellation: &Cancellation) -> Result<usize, AdapterError> {
        self.path(node, cancellation, |_, _, _| {})
    }

    fn path(
        &self,
        node: Node<'tree>,
        cancellation: &Cancellation,
        mut visit: impl FnMut(Node<'tree>, Option<Node<'tree>>, usize),
    ) -> Result<usize, AdapterError> {
        cancellation.check()?;
        let mut current = self.root;
        let mut parent = None;
        let mut depth = 0_usize;
        // Tree-sitter parent() itself searches from the root. Descend once
        // instead of repeating that search for every ancestor of a capture.
        loop {
            cancellation.check()?;
            visit(current, parent, depth);
            if current == node {
                break;
            }
            let child = current
                .child_with_descendant(node)
                .filter(|child| *child != current)
                .ok_or_else(|| query_failure("query-native-depth-node"))?;
            depth = depth
                .checked_add(1)
                .ok_or_else(|| query_failure("query-native-depth"))?;
            parent = Some(current);
            current = child;
        }
        cancellation.check()?;
        Ok(depth)
    }
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}

fn conditional(
    node: Node<'_>,
    cancellation: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<bool, AdapterError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !cache.visit(cancellation)? || matches!(child.kind(), "fail" | "attributes" | "selector")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn admitted<'tree>(
    node: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache<'tree>,
) -> Result<bool, AdapterError> {
    if !clean(node, source) {
        return Ok(false);
    }
    if !cache.visit(cancellation)? {
        return Ok(false);
    }
    let (mut boundary, mut boundary_depth, mut sentence_is_rooted) = (cache.root, 0, false);
    let node_depth = cache.path(node, cancellation, |current, parent, depth| {
        if matches!(
            current.kind(),
            "sentence"
                | "ERROR"
                | "comment"
                | "string"
                | "generic_tactic_body"
                | "custom_notation_block"
                | "proof_block"
        ) {
            boundary = current;
            boundary_depth = depth;
            sentence_is_rooted = parent.is_some_and(|parent| parent.kind() == "source_file");
        }
    })?;
    // The last stop on the descending path is the first stop on the old
    // upward walk. Charge the same logical visits despite finding it once.
    for _ in boundary_depth..node_depth {
        if !cache.visit(cancellation)? {
            return Ok(false);
        }
    }
    Ok(boundary.kind() == "sentence"
        && clean(boundary, source)
        && sentence_is_rooted
        && !conditional(boundary, cancellation, cache)?)
}

fn direct_name<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut name = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        if child.kind() == "ident" && name.replace(child).is_some() {
            return Ok(None);
        }
    }
    Ok(name)
}

fn named(node: Node<'_>, source: &[u8]) -> bool {
    clean(node, source)
        && std::str::from_utf8(&source[node.byte_range()])
            .ok()
            .is_some_and(|name| {
                structural_captured_name_for_language("coq", name, name.len()).as_deref()
                    == Some(name)
            })
}

fn reviewed_command(kind: &str) -> bool {
    matches!(
        kind,
        "definition_command"
            | "theorem_command"
            | "fixpoint_command"
            | "instance_command"
            | "declare_instance_command"
            | "existing_instance_command"
            | "existing_class_command"
            | "record_command"
            | "class_command"
            | "inductive_command"
            | "module_command"
            | "module_type_command"
    )
}

fn term_reference(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    matches!(
        parent.kind(),
        "type"
            | "application"
            | "arrow_term"
            | "parenthesized_term"
            | "term_cast"
            | "record_field_value"
            | "projection_object"
            | "projection_name"
    ) || (matches!(parent.kind(), "definition_command" | "fixpoint_command")
        && parent
            .child_by_field_name("body")
            .is_some_and(|body| body.id() == node.id()))
        || (matches!(parent.kind(), "instance_command" | "field_assignment")
            && parent
                .child_by_field_name(if parent.kind() == "field_assignment" {
                    "value"
                } else {
                    "body"
                })
                // A term field may contain several native children (for example
                // `@value`). Its earlier name and binder fields are not uses.
                .is_some_and(|value| node.start_byte() >= value.start_byte()))
        || (parent.kind() == "projection_argument"
            && parent
                .child_by_field_name("value")
                // The terminal value may start with anonymous `@`; the earlier
                // named-argument label does not refer to a source value.
                .is_some_and(|value| node.start_byte() >= value.start_byte()))
}

fn record_owner<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
    cache: &CaptureCache<'tree>,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut owner = None;
    cache.path(node, cancellation, |current, _, _| {
        if matches!(current.kind(), "record_command" | "class_command") {
            owner = Some(current);
        }
    })?;
    Ok(owner)
}

fn record_guard<'tree>(
    owner: Node<'tree>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache<'tree>,
) -> Result<Option<&'static str>, AdapterError> {
    if !owner
        .child_by_field_name("name")
        .is_some_and(|name| named(name, source))
    {
        return Ok(Some("coq.invalid_name"));
    }
    if let Some(body) = owner.child_by_field_name("fields") {
        let mut cursor = body.walk();
        for field in body.named_children(&mut cursor) {
            if !cache.visit(cancellation)? || field.child_by_field_name("attributes").is_some() {
                return Ok(Some("coq.conditional_sentence"));
            }
        }
    }
    Ok(None)
}

fn field_has_custom_syntax(
    node: Node<'_>,
    cancellation: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<bool, AdapterError> {
    let mut cursor = node.walk();
    loop {
        if !cache.visit(cancellation)?
            || matches!(
                cursor.node().kind(),
                "custom_operation" | "custom_notation_block"
            )
        {
            return Ok(true);
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(false);
            }
        }
    }
}

pub(super) fn candidate<'tree>(
    node: Node<'tree>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache<'tree>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    if !cache.visit(cancellation)? {
        return Ok(None);
    }
    let mut range = node.byte_range();
    // Parent walks are costly in deep recovery trees; rejected captures need none.
    let mut depth_node = node;
    let syntax = if node.kind() == "source_file"
        || (node.is_error()
            && node.parent().is_none()
            && matches!(role, StructuralRole::Root | StructuralRole::Module))
    {
        match role {
            StructuralRole::Root if node.is_error() => "coq.recovered_file",
            StructuralRole::Root => "coq.source_file",
            StructuralRole::Module => "coq.file",
            StructuralRole::Signature => "coq.proof_checking",
            _ => return Ok(None),
        }
    } else if role == StructuralRole::Signature && matches!(node.kind(), "binder" | "module_binder")
    {
        "coq.unmodeled_binder"
    } else if role == StructuralRole::Signature
        && matches!(node.kind(), "record_command" | "class_command")
    {
        record_guard(node, source, cancellation, cache)?.unwrap_or("coq.record_semantics")
    } else if role == StructuralRole::Signature
        && matches!(
            node.kind(),
            "instance_command"
                | "declare_instance_command"
                | "existing_instance_command"
                | "existing_class_command"
        )
    {
        "coq.instance_semantics"
    } else if role == StructuralRole::Signature
        && matches!(
            node.kind(),
            "ERROR" | "generic_tactic_body" | "custom_notation_block" | "proof_block"
        )
    {
        match node.kind() {
            "ERROR" => "coq.parser_recovery",
            "proof_block" => "coq.proof_checking",
            _ => "coq.custom_syntax",
        }
    } else if role == StructuralRole::Signature && node.kind() == "sentence" {
        if conditional(node, cancellation, cache)? {
            "coq.conditional_sentence"
        } else {
            let mut known = false;
            let mut context = false;
            let mut module = false;
            let mut proof = false;
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if !cache.visit(cancellation)? {
                    return Ok(None);
                }
                known |= reviewed_command(child.kind());
                context |= matches!(child.kind(), "context_command" | "assumption_command");
                module |= matches!(child.kind(), "section_command" | "end_command");
                proof |= matches!(
                    child.kind(),
                    "proof_command"
                        | "qed_command"
                        | "defined_command"
                        | "admitted_command"
                        | "abort_command"
                        | "save_command"
                );
            }
            if context {
                "coq.context_binding"
            } else if module {
                "coq.module_ownership"
            } else if proof {
                "coq.proof_checking"
            } else if known && clean(node, source) {
                return Ok(None);
            } else {
                "coq.unmodeled_sentence"
            }
        }
    } else if node.kind() == "ident_decl" {
        let Some(owner) = node.parent().filter(|owner| {
            matches!(
                owner.kind(),
                "definition_command"
                    | "theorem_command"
                    | "fixpoint_command"
                    | "instance_command"
                    | "declare_instance_command"
                    | "record_command"
                    | "class_command"
            )
        }) else {
            return Ok(None);
        };
        if matches!(owner.kind(), "record_command" | "class_command")
            && let Some(gap) = record_guard(owner, source, cancellation, cache)?
        {
            if role != StructuralRole::Signature {
                return Ok(None);
            }
            return Ok(Some(QueryCandidate {
                start: owner.start_byte(),
                end: owner.end_byte(),
                role,
                syntax: gap,
                required: true,
                native_depth: cache.depth(node, cancellation)?,
            }));
        }
        if !admitted(owner, source, cancellation, cache)? {
            return Ok(None);
        }
        let Some(name) =
            direct_name(node, cancellation, cache)?.filter(|name| named(*name, source))
        else {
            if role != StructuralRole::Signature {
                return Ok(None);
            }
            return Ok(Some(QueryCandidate {
                start: owner.start_byte(),
                end: owner.end_byte(),
                role,
                syntax: "coq.invalid_name",
                required: true,
                native_depth: cache.depth(node, cancellation)?,
            }));
        };
        let mut count = 0usize;
        let mut cursor = owner.walk();
        for child in owner.named_children(&mut cursor) {
            if !cache.visit(cancellation)? {
                return Ok(None);
            }
            count += usize::from(child.kind() == "ident_decl");
        }
        match role {
            StructuralRole::Declaration => {
                if count == 1 {
                    range = owner.byte_range();
                    depth_node = owner;
                }
                match owner.kind() {
                    "definition_command" => "coq.definition",
                    "theorem_command" => "coq.theorem",
                    "instance_command" => "coq.instance",
                    "declare_instance_command" => "coq.declared_instance",
                    "record_command" => "coq.record",
                    "class_command" => "coq.class",
                    _ => "coq.fixpoint",
                }
            }
            StructuralRole::Definition => {
                range = name.byte_range();
                depth_node = name;
                if owner.kind() == "declare_instance_command" {
                    "coq.declared_instance_name"
                } else {
                    "coq.name"
                }
            }
            StructuralRole::Signature => {
                range = owner.byte_range();
                if count > 1 {
                    "coq.shared_definition"
                } else {
                    if let Some(body) = owner.child_by_field_name("body") {
                        range.end = body.start_byte();
                    } else if let Some(body) = owner.child_by_field_name("fields") {
                        range.end = body.start_byte();
                    } else if owner.kind() == "class_command" {
                        let mut cursor = owner.walk();
                        for child in owner.named_children(&mut cursor) {
                            if !cache.visit(cancellation)? {
                                return Ok(None);
                            }
                            if child.kind() == "class_singleton_field" {
                                range.end = child.start_byte();
                                break;
                            }
                        }
                    }
                    "coq.written_header"
                }
            }
            _ => return Ok(None),
        }
    } else if matches!(
        node.kind(),
        "record_field" | "class_singleton_field" | "record_constructor"
    ) {
        let Some(owner) = record_owner(node, cancellation, cache)? else {
            return Ok(None);
        };
        let guard = record_guard(owner, source, cancellation, cache)?;
        if let Some(gap) = guard {
            if role != StructuralRole::Signature {
                return Ok(None);
            }
            range = owner.byte_range();
            gap
        } else {
            if !admitted(node, source, cancellation, cache)? {
                return Ok(None);
            }
            let Some(name) = node
                .child_by_field_name("name")
                .filter(|name| named(*name, source))
            else {
                if role != StructuralRole::Signature {
                    return Ok(None);
                }
                return Ok(Some(QueryCandidate {
                    start: range.start,
                    end: range.end,
                    role,
                    syntax: if node
                        .child_by_field_name("name")
                        .is_some_and(|name| name.kind() == "wildcard")
                    {
                        "coq.unmodeled_binder"
                    } else {
                        "coq.invalid_name"
                    },
                    required: true,
                    native_depth: cache.depth(node, cancellation)?,
                }));
            };
            match role {
                StructuralRole::Declaration => match node.kind() {
                    "record_field" => "coq.record_field",
                    "class_singleton_field" => "coq.class_field",
                    _ => "coq.record_constructor",
                },
                StructuralRole::Definition => {
                    range = name.byte_range();
                    depth_node = name;
                    match node.kind() {
                        "record_field" => "coq.record_field_name",
                        "class_singleton_field" => "coq.class_field_name",
                        _ => "coq.name",
                    }
                }
                StructuralRole::Signature => {
                    if field_has_custom_syntax(node, cancellation, cache)? {
                        "coq.custom_syntax"
                    } else {
                        if let Some(value) = node.child_by_field_name("value") {
                            range.end = value.start_byte();
                        }
                        "coq.written_header"
                    }
                }
                _ => return Ok(None),
            }
        }
    } else if matches!(
        node.kind(),
        "inductive_definition" | "constructor" | "module_command" | "module_type_command"
    ) {
        if !admitted(node, source, cancellation, cache)? {
            return Ok(None);
        }
        let Some(name) = node
            .child_by_field_name("name")
            .filter(|name| named(*name, source))
        else {
            if role != StructuralRole::Signature {
                return Ok(None);
            }
            return Ok(Some(QueryCandidate {
                start: range.start,
                end: range.end,
                role,
                syntax: "coq.invalid_name",
                required: true,
                native_depth: cache.depth(node, cancellation)?,
            }));
        };
        match role {
            StructuralRole::Declaration => match node.kind() {
                "inductive_definition" => "coq.inductive",
                "constructor" => "coq.constructor",
                "module_command" => "coq.module_header",
                _ => "coq.module_type_header",
            },
            StructuralRole::Definition => {
                range = name.byte_range();
                depth_node = name;
                "coq.name"
            }
            StructuralRole::Signature
                if matches!(node.kind(), "module_command" | "module_type_command") =>
            {
                "coq.module_ownership"
            }
            StructuralRole::Signature => {
                range.end = name.end_byte();
                "coq.written_header"
            }
            _ => return Ok(None),
        }
    } else if role == StructuralRole::Reference
        && (node.kind() == "reference"
            || (matches!(node.kind(), "ident" | "dotted_qualid") && term_reference(node)))
        && admitted(node, source, cancellation, cache)?
    {
        "coq.written_reference"
    } else if role == StructuralRole::Comment && node.kind() == "comment" && clean(node, source) {
        "coq.comment"
    } else if role == StructuralRole::StringLiteral
        && node.kind() == "string"
        && clean(node, source)
    {
        "coq.string"
    } else {
        return Ok(None);
    };
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: matches!(
            role,
            StructuralRole::Root
                | StructuralRole::Module
                | StructuralRole::Declaration
                | StructuralRole::Definition
                | StructuralRole::Signature
        ),
        native_depth: cache.depth(depth_node, cancellation)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GrammarFamily, registry::language_for};
    use rootlight_cancel::CancellationReason;

    fn admitted_with_native_parents(
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
        cache: &mut CaptureCache<'_>,
    ) -> Result<bool, AdapterError> {
        if !clean(node, source) {
            return Ok(false);
        }
        let mut ancestor = Some(node);
        while let Some(current) = ancestor {
            if !cache.visit(cancellation)? {
                return Ok(false);
            }
            if matches!(
                current.kind(),
                "ERROR"
                    | "comment"
                    | "string"
                    | "generic_tactic_body"
                    | "custom_notation_block"
                    | "proof_block"
            ) {
                return Ok(false);
            }
            if current.kind() == "sentence" {
                return Ok(clean(current, source)
                    && current
                        .parent()
                        .is_some_and(|parent| parent.kind() == "source_file")
                    && !conditional(current, cancellation, cache)?);
            }
            ancestor = current.parent();
        }
        Ok(false)
    }

    #[test]
    fn descending_admission_preserves_native_parent_results_and_visit_limits() {
        let cancellation = Cancellation::new();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Coq))
            .unwrap();
        let nested = format!("Definition deep := {}0{}.", "(".repeat(64), ")".repeat(64));
        for source in [
            "",
            "(* comment *) Definition value := 0. Lemma checked : True. Proof. exact I. Qed.",
            "Fail Definition conditional := 0. #[local] Definition local_value := 1.",
            "Definition tactic := ltac:(exact 0).",
            include_str!("../../tests/fixtures/coq-recovery/TemplateMonadCommon.v"),
            include_str!("../../tests/fixtures/coq-recovery/InHypUnderBindersDo.v"),
            include_str!("../../tests/fixtures/coq-recovery/MRListable.v"),
            nested.as_str(),
        ] {
            let tree = parser.parse(source, None).unwrap();
            let mut pending = vec![tree.root_node()];
            while let Some(node) = pending.pop() {
                for visits in [0, 1, 2, 8, usize::MAX] {
                    let mut previous = CaptureCache::new(tree.root_node(), visits);
                    let mut current = CaptureCache::new(tree.root_node(), visits);
                    let expected = admitted_with_native_parents(
                        node,
                        source.as_bytes(),
                        &cancellation,
                        &mut previous,
                    )
                    .unwrap();
                    let actual =
                        admitted(node, source.as_bytes(), &cancellation, &mut current).unwrap();
                    assert_eq!(
                        (actual, current.remaining, current.exhausted),
                        (expected, previous.remaining, previous.exhausted),
                        "{} {:?}, visits={visits}",
                        node.kind(),
                        node.byte_range()
                    );
                }
                let mut cursor = node.walk();
                pending.extend(node.children(&mut cursor));
            }
        }
    }

    #[test]
    fn descendant_depth_agrees_with_native_parents_and_cursor_depth() {
        let cancellation = Cancellation::new();
        let mut parser = tree_sitter::Parser::new();
        let original = include_str!("../../tests/fixtures/coq-recovery/TemplateMonadCommon.v");
        let recovered = include_str!("../../tests/fixtures/coq-recovery/InHypUnderBindersDo.v");
        let missing = include_str!("../../tests/fixtures/coq-recovery/MRListable.v");
        let nested = format!("Definition deep := {}0{}.", "(".repeat(64), ")".repeat(64));
        let mut maximum_depth = 0;
        let (mut saw_error, mut saw_missing, mut saw_anonymous) = (false, false, false);
        for (family, source) in [
            (GrammarFamily::Coq, ""),
            (
                GrammarFamily::Coq,
                "Definition nested (x : nat) := (((x))).\nModule M. End M.\n",
            ),
            (GrammarFamily::Coq, "Definition unfinished :=\n"),
            (GrammarFamily::Coq, original),
            (GrammarFamily::Coq, recovered),
            (GrammarFamily::Coq, missing),
            (GrammarFamily::Coq, nested.as_str()),
            (GrammarFamily::Rust, "fn missing_brace() { let value = 0;"),
        ] {
            parser.set_language(&language_for(family)).unwrap();
            let tree = parser.parse(source, None).unwrap();
            let cache = CaptureCache::new(tree.root_node(), usize::MAX);
            let mut cursor = tree.walk();
            let mut expected_depth = 0;
            loop {
                let node = cursor.node();
                saw_error |= node.is_error();
                saw_missing |= node.is_missing();
                saw_anonymous |= !node.is_named();
                let depth = cache.depth(node, &cancellation).unwrap();
                maximum_depth = maximum_depth.max(depth);
                assert_eq!(
                    depth,
                    expected_depth,
                    "{} {:?}",
                    node.kind(),
                    node.byte_range()
                );
                assert_eq!(
                    depth,
                    super::super::native_depth(node, &cancellation).unwrap()
                );
                if cursor.goto_first_child() {
                    expected_depth += 1;
                    continue;
                }
                loop {
                    if cursor.goto_next_sibling() {
                        break;
                    }
                    if !cursor.goto_parent() {
                        assert_eq!(expected_depth, 0);
                        break;
                    }
                    expected_depth -= 1;
                }
                if expected_depth == 0 && cursor.node() == tree.root_node() {
                    break;
                }
            }
        }
        assert!(maximum_depth >= 64);
        assert!(saw_error);
        assert!(saw_missing);
        assert!(saw_anonymous);
    }

    #[test]
    fn descendant_depth_rejects_a_foreign_tree_and_observes_cancellation() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Coq))
            .unwrap();
        let tree = parser.parse("Definition first := 0.", None).unwrap();
        let other = parser.parse("Definition second := 1.", None).unwrap();
        let cache = CaptureCache::new(tree.root_node(), usize::MAX);
        assert!(
            cache
                .depth(other.root_node(), &Cancellation::new())
                .is_err()
        );
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(cache.depth(tree.root_node(), &cancellation).is_err());
    }
}
