//! Native written-header and declaration-time notation certificates.
//! This subset does not model elaboration, proof acceptance or imported names.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::SinkError;

use super::*;

pub(super) struct Header {
    pub(super) prefix: Range<usize>,
    pub(super) formals: Vec<Range<usize>>,
}

pub(super) enum HeaderAdmission {
    Proved(Header),
    Legacy,
    Blocked,
}

#[derive(Clone, Copy)]
struct Target<'tree> {
    command: Node<'tree>,
    name: Node<'tree>,
    eligible: bool,
}

#[derive(Default)]
pub(super) struct Proofs {
    pub(super) headers: HashMap<usize, HeaderAdmission>,
    pub(super) notations: HashMap<usize, Range<usize>>,
    pub(super) references: HashMap<usize, (Range<usize>, usize)>,
}

fn push<T>(values: &mut Vec<T>, value: T) -> Result<(), AdapterError> {
    values
        .try_reserve(1)
        .map_err(|_| SinkError::AllocationFailed)?;
    values.push(value);
    Ok(())
}

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}

fn children<'tree>(
    node: Node<'tree>,
    cache: &mut CaptureCache<'tree>,
    cancel: &Cancellation,
) -> Result<Vec<Node<'tree>>, AdapterError> {
    let mut result = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if !cache.visit(cancel)? {
            return Ok(Vec::new());
        }
        if child.kind() != "comment" {
            push(&mut result, child)?;
        }
    }
    Ok(result)
}

fn only_command<'tree>(
    sentence: Node<'tree>,
    cache: &mut CaptureCache<'tree>,
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut command: Option<Node<'tree>> = None;
    for child in children(sentence, cache, cancel)? {
        if child.is_named() {
            if let Some(previous) = command {
                if previous.kind() == "theorem_command"
                    && child.is_error()
                    && child.start_byte() >= previous.end_byte()
                {
                    continue;
                }
                return Ok(None);
            }
            command = Some(child);
        }
        if !child.is_named() && child.kind() != "." {
            return Ok(None);
        }
    }
    Ok(command)
}

fn ordinary_names<'tree>(
    group: Node<'tree>,
    source: &[u8],
    cache: &mut CaptureCache<'tree>,
    cancel: &Cancellation,
) -> Result<Option<(Vec<Node<'tree>>, bool)>, AdapterError> {
    if !clean(group, source) {
        return Ok(None);
    }
    let mut group_children = children(group, cache, cancel)?;
    let implicit = group_children.len() == 1
        && group_children
            .first()
            .is_some_and(|child| child.kind() == "implicit_binders");
    if implicit {
        let Some(inner) = group_children.first().copied() else {
            return Ok(None);
        };
        group_children = children(inner, cache, cancel)?;
    }
    let Some(first) = group_children.first() else {
        return Ok(None);
    };
    let Some(last) = group_children.last() else {
        return Ok(None);
    };
    if (implicit && !matches!((first.kind(), last.kind()), ("{", "}") | ("[", "]")))
        || (!implicit && (first.kind(), last.kind()) != ("(", ")"))
    {
        return Ok(None);
    }
    let mut names = Vec::new();
    let mut colon = false;
    let mut ty = false;
    for child in group_children
        .iter()
        .skip(1)
        .take(group_children.len().saturating_sub(2))
    {
        if !clean(*child, source) {
            return Ok(None);
        }
        match child.kind() {
            "ident" if !colon && named(*child, source) => push(&mut names, *child)?,
            ":" if !colon && !names.is_empty() => colon = true,
            "type" if colon && !ty => ty = true,
            _ => return Ok(None),
        }
    }
    Ok((!(names.is_empty() || !implicit && !colon || colon && !ty)).then_some((names, implicit)))
}

fn header<'tree>(
    command: Node<'tree>,
    context_known: bool,
    source: &[u8],
    cache: &mut CaptureCache<'tree>,
    cancel: &Cancellation,
) -> Result<HeaderAdmission, AdapterError> {
    let members = children(command, cache, cancel)?;
    if !members.iter().any(|child| child.kind() == "binder") {
        return Ok(if clean(command, source) {
            HeaderAdmission::Legacy
        } else {
            HeaderAdmission::Blocked
        });
    }
    if !context_known
        || members.first().is_none_or(|child| {
            !clean(*child, source)
                || text(*child, source) != Some(child.kind())
                || !matches!(
                    child.kind(),
                    "Theorem"
                        | "Lemma"
                        | "Fact"
                        | "Remark"
                        | "Corollary"
                        | "Proposition"
                        | "Property"
                )
        })
    {
        return Ok(HeaderAdmission::Blocked);
    }
    let mut name_seen = false;
    let mut colon_end = None;
    let mut inventory = Vec::<Node<'tree>>::new();
    let mut formals = Vec::new();
    for child in members.iter().skip(1) {
        if !cache.visit(cancel)? {
            return Ok(HeaderAdmission::Blocked);
        }
        if child.kind() == ":" {
            if !name_seen || child.is_missing() {
                return Ok(HeaderAdmission::Blocked);
            }
            colon_end = Some(child.end_byte());
            break;
        }
        if !clean(*child, source) {
            return Ok(HeaderAdmission::Blocked);
        }
        match child.kind() {
            "ident_decl" if !name_seen => {
                let Some(name) =
                    direct_name(*child, cancel, cache)?.filter(|name| named(*name, source))
                else {
                    return Ok(HeaderAdmission::Blocked);
                };
                push(&mut inventory, name)?;
                name_seen = true;
            }
            "binder" if name_seen => {
                let Some((names, implicit)) = ordinary_names(*child, source, cache, cancel)? else {
                    return Ok(HeaderAdmission::Blocked);
                };
                for name in names {
                    for previous in &inventory {
                        if !cache.visit(cancel)? {
                            return Ok(HeaderAdmission::Blocked);
                        }
                        if text(*previous, source) == text(name, source) {
                            return Ok(HeaderAdmission::Blocked);
                        }
                    }
                    push(&mut inventory, name)?;
                    if !implicit {
                        push(&mut formals, name.byte_range())?
                    }
                }
            }
            _ => return Ok(HeaderAdmission::Blocked),
        }
    }
    let Some(end) = colon_end else {
        return Ok(HeaderAdmission::Blocked);
    };
    // Recovery after the colon cannot invalidate an independently clean prefix;
    // a mutual assertion still changes that prefix's declaration ownership.
    if members.iter().any(|child| child.kind() == "with") {
        return Ok(HeaderAdmission::Blocked);
    }
    if formals.is_empty() {
        Ok(if clean(command, source) {
            HeaderAdmission::Legacy
        } else {
            HeaderAdmission::Blocked
        })
    } else {
        Ok(HeaderAdmission::Proved(Header {
            prefix: command.start_byte()..end,
            formals,
        }))
    }
}

fn same_name<'tree>(
    left: &str,
    right: &str,
    cache: &mut CaptureCache<'tree>,
    cancel: &Cancellation,
) -> Result<Option<bool>, AdapterError> {
    for _ in 0..left.len().min(right.len()).div_ceil(64).max(1) {
        if !cache.visit(cancel)? {
            return Ok(None);
        }
    }
    Ok(Some(left == right))
}

fn contains_parameter<'tree>(
    names: &[&str],
    name: &str,
    cache: &mut CaptureCache<'tree>,
    cancel: &Cancellation,
) -> Result<Option<bool>, AdapterError> {
    for previous in names {
        match same_name(previous, name, cache, cancel)? {
            Some(true) => return Ok(Some(true)),
            Some(false) => {}
            None => return Ok(None),
        }
    }
    Ok(Some(false))
}

fn ordinary_binders<'tree>(
    owner: Node<'tree>,
    source: &[u8],
    cache: &mut CaptureCache<'tree>,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut inventory = Vec::new();
    for member in children(owner, cache, cancel)? {
        if member.kind() != "binder" {
            continue;
        }
        let Some((names, _)) = ordinary_names(member, source, cache, cancel)? else {
            return Ok(false);
        };
        for name in names {
            let Some(name) = text(name, source) else {
                return Ok(false);
            };
            if contains_parameter(&inventory, name, cache, cancel)? != Some(false) {
                return Ok(false);
            }
            push(&mut inventory, name)?;
        }
    }
    Ok(!cache.exhausted())
}

fn written_namespace<'tree>(
    command: Node<'tree>,
    head: Node<'tree>,
    source: &[u8],
    cache: &mut CaptureCache<'tree>,
    cancel: &Cancellation,
) -> Result<Option<Vec<Node<'tree>>>, AdapterError> {
    if !ordinary_binders(command, source, cache, cancel)? {
        return Ok(None);
    }
    let mut names = Vec::new();
    push(&mut names, head)?;
    if command.kind() == "definition_command" {
        return Ok(Some(names));
    }
    let mut constructor_seen = false;
    for member in children(command, cache, cancel)? {
        if member.kind() != "record_constructor" {
            continue;
        }
        if constructor_seen || !clean(member, source) {
            return Ok(None);
        }
        let Some(name) = member
            .child_by_field_name("name")
            .filter(|name| name.kind() == "ident" && named(*name, source))
        else {
            return Ok(None);
        };
        push(&mut names, name)?;
        constructor_seen = true;
    }
    // An omitted constructor introduces a generated global name. This native
    // certificate deliberately inventories only explicitly written globals.
    if !constructor_seen {
        return Ok(None);
    }
    let Some(body) = command.child_by_field_name("fields").filter(|body| {
        matches!(body.kind(), "record_body" | "class_record_body") && clean(*body, source)
    }) else {
        return Ok(None);
    };
    for field in children(body, cache, cancel)?
        .into_iter()
        .filter(|field| field.is_named())
    {
        if field.kind() != "record_field"
            || !clean(field, source)
            || !ordinary_binders(field, source, cache, cancel)?
        {
            return Ok(None);
        }
        for member in children(field, cache, cancel)? {
            if matches!(member.kind(), "decl_notations" | "instance_priority")
                || field.child_by_field_name("attributes").is_some()
            {
                return Ok(None);
            }
        }
        let Some(name) = field.child_by_field_name("name") else {
            return Ok(None);
        };
        if name.kind() == "wildcard" && text(name, source) == Some("_") {
            continue;
        }
        if name.kind() != "ident" || !named(name, source) {
            return Ok(None);
        }
        push(&mut names, name)?;
    }
    Ok((!cache.exhausted()).then_some(names))
}

fn parameters<'a, 'tree>(
    pattern: Node<'tree>,
    source: &'a [u8],
    cache: &mut CaptureCache<'tree>,
    cancel: &Cancellation,
) -> Result<Option<Vec<&'a str>>, AdapterError> {
    let Some(raw) = text(pattern, source)
        .and_then(|s| s.strip_prefix('"'))
        .and_then(|s| s.strip_suffix('"'))
    else {
        return Ok(None);
    };
    if raw.contains(['"', '\'', '\\']) {
        return Ok(None);
    }
    for _ in raw.as_bytes().chunks(64) {
        if !cache.visit(cancel)? {
            return Ok(None);
        }
    }
    let mut result = Vec::new();
    let mut tokens = 0_usize;
    for token in raw.split_whitespace() {
        if !cache.visit(cancel)? {
            return Ok(None);
        }
        tokens = tokens
            .checked_add(1)
            .ok_or_else(|| query_failure("coq-pattern-count"))?;
        if structural_captured_name_for_language("coq", token, token.len()).as_deref()
            == Some(token)
        {
            if contains_parameter(&result, token, cache, cancel)? != Some(false) {
                return Ok(None);
            }
            push(&mut result, token)?;
        } else if token
            .chars()
            .any(|c| c.is_alphanumeric() || matches!(c, '_' | '.'))
        {
            return Ok(None);
        }
    }
    Ok((tokens > 0).then_some(result))
}

fn notation_head<'tree>(
    declaration: Node<'tree>,
    source: &[u8],
    cache: &mut CaptureCache<'tree>,
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    if !clean(declaration, source) {
        return Ok(None);
    }
    let Some(pattern) = declaration.child_by_field_name("notation") else {
        return Ok(None);
    };
    let Some(params) = parameters(pattern, source, cache, cancel)? else {
        return Ok(None);
    };
    for member in children(declaration, cache, cancel)? {
        if member.kind() == "notation_scope" {
            return Ok(None);
        }
        if member.kind() == "syntax_modifier_list" {
            for wrapper in children(member, cache, cancel)?
                .into_iter()
                .filter(|n| n.is_named())
            {
                let parts = children(wrapper, cache, cancel)?;
                if wrapper.kind() != "syntax_modifier"
                    || parts.len() != 1
                    || parts.first().is_none_or(|n| n.kind() != "level_modifier")
                {
                    return Ok(None);
                }
            }
        }
    }
    let Some(mut term) = declaration.child_by_field_name("definition") else {
        return Ok(None);
    };
    while term.kind() == "parenthesized_term" {
        let parts = children(term, cache, cancel)?;
        let mut named = parts.into_iter().filter(|n| n.is_named());
        let Some(inner) = named.next() else {
            return Ok(None);
        };
        if named.next().is_some() {
            return Ok(None);
        }
        term = inner;
    }
    // Application is left associative in the native grammar. Only its clean
    // application spine and plain parameter tails are a notation certificate.
    while term.kind() == "application" {
        if !cache.visit(cancel)? || !clean(term, source) {
            return Ok(None);
        }
        let parts = children(term, cache, cancel)?;
        let Some(head) = parts.first().copied() else {
            return Ok(None);
        };
        if parts.len() < 2 || head.end_byte() >= term.end_byte() {
            return Ok(None);
        }
        for argument in parts.iter().skip(1) {
            if argument.kind() != "ident" || !named(*argument, source) {
                return Ok(None);
            }
            let Some(name) = text(*argument, source) else {
                return Ok(None);
            };
            if contains_parameter(&params, name, cache, cancel)? != Some(true) {
                return Ok(None);
            }
        }
        term = head;
    }
    let head = term;
    if head.kind() != "ident" || !named(head, source) {
        return Ok(None);
    }
    let Some(head_name) = text(head, source) else {
        return Ok(None);
    };
    if contains_parameter(&params, head_name, cache, cancel)? != Some(false) {
        return Ok(None);
    }
    Ok(Some(head))
}

impl Proofs {
    pub(super) fn build<'tree>(
        source: &[u8],
        complete: bool,
        cache: &mut CaptureCache<'tree>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut proofs = Self::default();
        let mut targets = Vec::<Target<'tree>>::new();
        let (mut ownership, mut environment, mut proof) = (complete, complete, false);
        for sentence in children(cache.root, cache, cancel)? {
            if sentence.kind() == "comment" {
                continue;
            }
            if sentence.kind() != "sentence" {
                ownership = false;
                environment = false;
                continue;
            }
            let Some(command) = only_command(sentence, cache, cancel)? else {
                ownership = false;
                environment = false;
                continue;
            };
            let sentence_clean = clean(sentence, source);
            if command.kind() == "theorem_command" {
                proofs
                    .headers
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                let admission = header(command, ownership && !proof, source, cache, cancel)?;
                proofs.headers.insert(command.start_byte(), admission);
                if proof {
                    ownership = false
                }
                proof = true;
                environment = false;
                if !sentence_clean {
                    ownership = false
                }
                continue;
            }
            if !sentence_clean {
                ownership = false;
                environment = false;
                continue;
            }
            match command.kind() {
                "record_command" | "class_command" | "definition_command"
                    if ownership && !proof =>
                {
                    let Some(declaration) = command.child_by_field_name("name").or_else(|| {
                        let mut cursor = command.walk();
                        command
                            .named_children(&mut cursor)
                            .find(|n| n.kind() == "ident_decl")
                    }) else {
                        environment = false;
                        continue;
                    };
                    if direct_name(declaration, cancel, cache)?
                        .filter(|n| named(*n, source))
                        .is_none()
                    {
                        environment = false;
                        continue;
                    }
                    let Some(name) = direct_name(declaration, cancel, cache)? else {
                        environment = false;
                        continue;
                    };
                    let mut count = 0_usize;
                    for child in children(command, cache, cancel)? {
                        count += usize::from(child.kind() == "ident_decl");
                    }
                    if command.kind() == "definition_command"
                        && command.child_by_field_name("body").is_none()
                    {
                        ownership = false;
                        environment = false;
                        continue;
                    }
                    if count != 1
                        || (matches!(command.kind(), "record_command" | "class_command")
                            && record_guard(command, source, cancel, cache)?.is_some())
                    {
                        environment = false;
                        continue;
                    }
                    let Some(names) = written_namespace(command, name, source, cache, cancel)?
                    else {
                        environment = false;
                        continue;
                    };
                    for (index, name) in names.into_iter().enumerate() {
                        push(
                            &mut targets,
                            Target {
                                command,
                                name,
                                eligible: index == 0,
                            },
                        )?;
                    }
                }
                "notation_command" if ownership && environment && !proof => {
                    let mut cursor = command.walk();
                    let declaration = command
                        .named_children(&mut cursor)
                        .find(|n| n.kind() == "notation_declaration");
                    if let Some(declaration) = declaration
                        && let Some(head) = notation_head(declaration, source, cache, cancel)?
                    {
                        let mut found = None;
                        let mut matched = false;
                        let mut ambiguous = false;
                        for target in &targets {
                            let (Some(name), Some(head_name)) =
                                (text(target.name, source), text(head, source))
                            else {
                                ambiguous = true;
                                break;
                            };
                            match same_name(name, head_name, cache, cancel)? {
                                Some(true) => {
                                    ambiguous |= matched;
                                    matched = true;
                                    if target.eligible {
                                        found = Some(*target);
                                    }
                                }
                                Some(false) => {}
                                None => break,
                            }
                        }
                        if !ambiguous
                            && !cache.exhausted()
                            && let Some(target) = found
                        {
                            proofs
                                .notations
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            proofs
                                .references
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            proofs.notations.insert(
                                declaration.start_byte(),
                                target.command.start_byte()..head.end_byte(),
                            );
                            proofs.references.insert(
                                head.end_byte(),
                                (head.byte_range(), cache.depth(head, cancel)?),
                            );
                        }
                    }
                    // Unknown syntax-extension effects can influence later parsing;
                    // this certificate is limited to the already closed prefix.
                    environment = false;
                }
                "arguments_command" if ownership && !proof => {}
                "proof_command" if proof => {}
                "qed_command" | "defined_command" | "admitted_command" | "abort_command"
                    if proof =>
                {
                    proof = false
                }
                "tactic_sequence"
                | "tactic_invocation"
                | "parenthesized_tactic"
                | "tactic_branch"
                | "tactic_or"
                | "match_goal"
                | "match_tactic"
                    if proof => {}
                "require_command" | "import_command" | "export_command" if ownership && !proof => {
                    environment = false
                }
                _ => {
                    ownership = false;
                    environment = false;
                }
            }
        }
        cancel.check()?;
        Ok(proofs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GrammarFamily, registry::language_for};
    use rootlight_cancel::CancellationReason;

    fn proofs(source: &str, complete: bool, maximum: usize) -> (Proofs, bool) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Coq))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let mut cache = CaptureCache::new(tree.root_node(), maximum);
        let result = Proofs::build(
            source.as_bytes(),
            complete,
            &mut cache,
            &Cancellation::new(),
        )
        .unwrap();
        (result, cache.exhausted())
    }

    #[test]
    fn original_native_prefix_and_notation_pair_keep_exact_physical_witnesses() {
        let original =
            include_str!("../../../../rootlight-service/tests/fixtures/coq-squash/MRSquash.v");
        let (proofs, exhausted) = proofs(original, true, 16_384);
        assert!(!exhausted);
        let Some(HeaderAdmission::Proved(header)) = proofs.headers.get(&128) else {
            panic!("missing clean prefix")
        };
        assert_eq!(header.prefix, 128..165);
        assert_eq!(header.formals.len(), 1);
        assert_eq!(header.formals[0], 152..153);
        assert_eq!(proofs.notations.get(&63), Some(&(2..85)));
        assert_eq!(
            proofs.references.get(&85).map(|(range, _)| range),
            Some(&(79..85))
        );
    }

    #[test]
    fn assertion_keyword_requires_exact_written_bytes_and_native_prefix() {
        let source = "Lemma written (x : nat) : True.";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Coq))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let mut cursor = tree.root_node().walk();
        let sentence = tree.root_node().named_children(&mut cursor).next().unwrap();
        let mut cursor = sentence.walk();
        let command = sentence.named_children(&mut cursor).next().unwrap();
        let mut cache = CaptureCache::new(tree.root_node(), 16_384);
        assert!(matches!(
            header(
                command,
                true,
                source.as_bytes(),
                &mut cache,
                &Cancellation::new()
            )
            .unwrap(),
            HeaderAdmission::Proved(_)
        ));
        let changed = source.replacen("Lemma", "Xemma", 1);
        let mut cache = CaptureCache::new(tree.root_node(), 16_384);
        assert!(matches!(
            header(
                command,
                true,
                changed.as_bytes(),
                &mut cache,
                &Cancellation::new()
            )
            .unwrap(),
            HeaderAdmission::Blocked
        ));
        let (missing, exhausted) = proofs("written (x : nat) : True.", true, 16_384);
        assert!(!exhausted);
        assert!(
            !missing
                .headers
                .values()
                .any(|admission| matches!(admission, HeaderAdmission::Proved(_)))
        );
    }

    #[test]
    fn assertion_aliases_and_multiple_typed_formals_share_one_native_rule() {
        for alias in [
            "Theorem",
            "Lemma",
            "Fact",
            "Remark",
            "Corollary",
            "Proposition",
            "Property",
        ] {
            let source = format!("{alias} written {{A B}} (x y : A) (z : B) : True.\n");
            let (proofs, exhausted) = proofs(&source, true, 16_384);
            assert!(!exhausted);
            let Some(HeaderAdmission::Proved(header)) = proofs.headers.get(&0) else {
                panic!("{source}")
            };
            let names: Vec<_> = header
                .formals
                .iter()
                .map(|span| &source[span.clone()])
                .collect();
            assert_eq!(names, ["x", "y", "z"]);
            assert_eq!(
                &source[header.prefix.clone()],
                &source[..source.find(": True").unwrap() + 1]
            );
        }
        for source in ["Lemma zero : True.", "Theorem zero {A} : True."] {
            let (proofs, exhausted) = proofs(source, true, 16_384);
            assert!(!exhausted);
            assert!(matches!(
                proofs.headers.get(&0),
                Some(HeaderAdmission::Legacy)
            ));
        }
    }

    #[test]
    fn defining_prefix_ambiguity_and_unmodeled_scope_never_fall_back_to_constants() {
        for source in [
            "Lemma bad (x x : nat) : True.",
            "Lemma bad {x} (x : nat) : True.",
            "Lemma bad (x := 0) : True.",
            "Lemma bad `(x : C) : True.",
            "Lemma bad (x : ) : True.",
            "Lemma bad x : True.",
            "Module M. Lemma bad (x : nat) : True.",
            "Section S. Lemma bad (x : nat) : True.",
            "Module M. End M. Lemma bad (x : nat) : True.",
            "Set Nested Proofs Allowed. Lemma bad (x : nat) : True.",
            "Lemma pending : True. Lemma bad (x : nat) : True.",
            "Definition pending : nat. Lemma bad (x : nat) : True.",
            "Fail Lemma bad (x : nat) : True.",
            "#[local] Lemma bad (x : nat) : True.",
        ] {
            let (proofs, exhausted) = proofs(source, true, 16_384);
            assert!(!exhausted);
            let at = source.find("Lemma bad").unwrap();
            assert!(
                !matches!(
                    proofs.headers.get(&at),
                    Some(HeaderAdmission::Proved(_) | HeaderAdmission::Legacy)
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn later_type_recovery_is_separate_from_clean_header_and_completed_proof_context() {
        for source in [
            "Lemma written (x : nat) : ∥ x ∥.\n",
            "Lemma earlier (x : nat) : True. Proof. exact I. Qed.\nLemma written (y : nat) : True.\n",
        ] {
            let (proofs, exhausted) = proofs(source, true, 16_384);
            assert!(!exhausted);
            let at = source.find("Lemma written").unwrap();
            assert!(
                matches!(proofs.headers.get(&at), Some(HeaderAdmission::Proved(_))),
                "{source}"
            );
        }
    }

    #[test]
    fn clean_record_class_and_definition_heads_are_declaration_time_targets() {
        for declaration in [
            "Record target (A : Type) := make { field : A }.",
            "Class target (A : Type) := make { field : A }.",
            "Definition target (A : Type) := A.",
        ] {
            let source = format!("{declaration}\nNotation \"! T\" := (target T) (at level 10).\n");
            let (proofs, exhausted) = proofs(&source, true, 16_384);
            assert!(!exhausted);
            let end = source.rfind("target").unwrap() + "target".len();
            assert_eq!(proofs.notations.len(), 1, "{source}");
            assert_eq!(
                proofs.references.get(&end).map(|(range, _)| range),
                Some(&(end - 6..end))
            );
            assert_eq!(proofs.notations.values().next(), Some(&(0..end)));
        }
    }

    #[test]
    fn notation_patterns_and_environment_barriers_never_promote_spelling() {
        for source in [
            "Definition target := 0. Definition target := 1. Notation \"! T\" := (target T).",
            "Definition target := 0. Notation \"target T\" := (target T).",
            "Definition target := 0. Notation \"'target' T\" := (target T).",
            "Definition target := 0. Notation \"! T\" := (M.target T).",
            "Definition target := 0. Notation \"! T\" := (target unknown).",
            "Definition target := 0. Notation \"! T\" := (target T) : scoped.",
            "Definition target := 0. Notation \"! T\" := (target T) (T binder).",
            "Definition target := 0. Require Import Other. Notation \"! T\" := (target T).",
            "Definition target := 0. Import Other. Notation \"! T\" := (target T).",
            "Definition target := 0. Export Other. Notation \"! T\" := (target T).",
            "Definition target := 0. Include Other. Notation \"! T\" := (target T).",
            "Definition target := 0. Section S. Notation \"! T\" := (target T).",
            "Definition target := 0. Module M. Notation \"! T\" := (target T).",
            "Lemma target (x : nat) : True. Abort. Notation \"! T\" := (target T).",
            "Definition broken := . Definition target := 0. Notation \"! T\" := (target T).",
            "(* Definition target := 0. *) Notation \"! T\" := (target T).",
            "Definition text := \"Definition target := 0.\". Notation \"! T\" := (target T).",
        ] {
            let (proofs, exhausted) = proofs(source, true, 16_384);
            assert!(!exhausted);
            assert!(proofs.notations.is_empty(), "{source}");
        }
    }

    #[test]
    fn finite_work_partition_and_cancellation_do_not_publish_complete_certificates() {
        let source = "Definition target := 0. Notation \"! T\" := (target T). Lemma written (x : nat) : True.";
        let (full, exhausted) = proofs(source, true, 16_384);
        assert!(!exhausted);
        assert_eq!(full.notations.len(), 1);
        let (partial, exhausted) = proofs(source, false, 16_384);
        assert!(!exhausted);
        assert!(partial.notations.is_empty());
        assert!(
            !partial
                .headers
                .values()
                .any(|value| matches!(value, HeaderAdmission::Proved(_)))
        );
        for maximum in [0, 1, 2, 4] {
            assert!(proofs(source, true, maximum).1);
        }
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Coq))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let mut cache = CaptureCache::new(tree.root_node(), 16_384);
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            Proofs::build(source.as_bytes(), true, &mut cache, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn written_constructor_and_projection_competitors_block_notation_targets() {
        for middle in [
            "Record holder := target { field : nat }.",
            "Record holder := make { target : nat }.",
            "Class holder := target { field : nat }.",
            "Class holder := make { target : nat }.",
            "Record holder := { field : nat }.",
            "Class holder := { field : nat }.",
            "Class holder := target : nat.",
            "Record holder `(x : C) := make { field : nat }.",
            "Record holder := make { field `(x : C) : nat }.",
            "Definition holder `(x : C) := x.",
            "Record holder := make { field : }.",
        ] {
            let source =
                format!("Definition target := 0. {middle} Notation \"! T\" := (target T).");
            let (proofs, exhausted) = proofs(&source, true, 16_384);
            assert!(!exhausted, "{source}");
            assert!(proofs.notations.is_empty(), "{source}");
            assert!(proofs.references.is_empty(), "{source}");
        }
        for prefix in [
            "Record holder := target { field : nat }.",
            "Class holder := make { target : nat }.",
            "Record holder := target { field : nat }. Definition target := 0.",
            "Class holder := make { target : nat }. Definition target := 0.",
            "Record target := target { field : nat }.",
            "Record target := make { target : nat }.",
        ] {
            let source = format!("{prefix} Notation \"! T\" := (target T).");
            let (proofs, exhausted) = proofs(&source, true, 16_384);
            assert!(!exhausted, "{source}");
            assert!(proofs.notations.is_empty(), "{source}");
        }
        for middle in [
            "Record holder := make { field : nat }.",
            "Class holder := make { field : nat }.",
            "Record holder := make { _ : nat }.",
        ] {
            let source =
                format!("Definition target := 0. {middle} Notation \"! T\" := (target T).");
            let (proofs, exhausted) = proofs(&source, true, 16_384);
            assert!(!exhausted, "{source}");
            assert_eq!(proofs.notations.len(), 1, "{source}");
            let end = source.rfind("target").unwrap() + 6;
            assert_eq!(proofs.notations.values().next(), Some(&(0..end)));
        }
    }

    #[test]
    fn notation_parameter_comparisons_obey_work_and_cancellation_authority() {
        let source = "Definition target := 0. Notation \"! P0 P1 P2 P3 P4 P5 P6 P7\" := (target P0 P1 P2 P3 P4 P5 P6 P7).";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Coq))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let start = source.find('"').unwrap();
        let end = source.rfind('"').unwrap() + 1;
        let pattern = tree
            .root_node()
            .named_descendant_for_byte_range(start, end)
            .unwrap();
        assert_eq!(pattern.kind(), "string");
        let mut cache = CaptureCache::new(tree.root_node(), 16);
        assert!(
            parameters(pattern, source.as_bytes(), &mut cache, &Cancellation::new())
                .unwrap()
                .is_none()
        );
        assert!(cache.exhausted());
        let mut cache = CaptureCache::new(tree.root_node(), 128);
        let names = parameters(pattern, source.as_bytes(), &mut cache, &Cancellation::new())
            .unwrap()
            .unwrap();
        assert_eq!(names, ["P0", "P1", "P2", "P3", "P4", "P5", "P6", "P7"]);
        assert!(!cache.exhausted());
        let mut cache = CaptureCache::new(tree.root_node(), 7);
        assert_eq!(
            contains_parameter(&names, "P7", &mut cache, &Cancellation::new()).unwrap(),
            None
        );
        assert!(cache.exhausted());
        let mut cache = CaptureCache::new(tree.root_node(), 8);
        assert_eq!(
            contains_parameter(&names, "P7", &mut cache, &Cancellation::new()).unwrap(),
            Some(true)
        );
        let mut cache = CaptureCache::new(tree.root_node(), 128);
        let long = "x".repeat(129);
        assert_eq!(
            same_name(&long, &long, &mut cache, &Cancellation::new()).unwrap(),
            Some(true)
        );
        assert_eq!(cache.remaining, 125);
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            contains_parameter(&names, "P7", &mut cache, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
        let (full, exhausted) = proofs(source, true, 16_384);
        assert!(!exhausted);
        assert_eq!(full.notations.len(), 1);
        let notation_start = source.find('"').unwrap();
        let mut cache = CaptureCache::new(tree.root_node(), 16);
        let mut cursor = tree.root_node().walk();
        let sentence = tree.root_node().named_children(&mut cursor).nth(1).unwrap();
        let mut cursor = sentence.walk();
        let command = sentence.named_children(&mut cursor).next().unwrap();
        let mut cursor = command.walk();
        let notation = command
            .named_children(&mut cursor)
            .find(|node| node.kind() == "notation_declaration")
            .unwrap();
        assert_eq!(notation.start_byte(), notation_start);
        let candidate = super::super::candidate(
            notation,
            StructuralRole::Signature,
            source.as_bytes(),
            &Cancellation::new(),
            &mut cache,
        )
        .unwrap();
        assert!(cache.exhausted());
        assert!(candidate.is_none_or(|candidate| candidate.syntax != "coq.notation_target"));
        assert!(
            super::super::required_reference_candidate(
                notation,
                StructuralRole::Signature,
                &Cancellation::new(),
                &cache
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn notation_pattern_uniqueness_accounts_comparison_work() {
        let source = "Notation \"! P0 P1 P2 P3 P4 P5 P6 P7\" := (target P0 P1 P2 P3 P4 P5 P6 P7).";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Coq))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let start = source.find('"').unwrap();
        let end = source.rfind('"').unwrap() + 1;
        let pattern = tree
            .root_node()
            .named_descendant_for_byte_range(start, end)
            .unwrap();
        assert_eq!(pattern.kind(), "string");
        let mut cache = CaptureCache::new(tree.root_node(), 16);
        assert!(
            parameters(pattern, source.as_bytes(), &mut cache, &Cancellation::new())
                .unwrap()
                .is_none()
        );
        assert!(cache.exhausted());
        let mut cache = CaptureCache::new(tree.root_node(), 128);
        let names = parameters(pattern, source.as_bytes(), &mut cache, &Cancellation::new())
            .unwrap()
            .unwrap();
        assert_eq!(names, ["P0", "P1", "P2", "P3", "P4", "P5", "P6", "P7"]);
        assert!(!cache.exhausted());
    }

    #[test]
    fn clean_left_application_spines_keep_all_plain_parameter_arguments() {
        for rhs in ["target T", "target T U", "target T U V", "((target T U V))"] {
            let source = format!("Definition target := 0. Notation \"! T U V\" := ({rhs}).");
            let (proofs, exhausted) = proofs(&source, true, 16_384);
            assert!(!exhausted, "{source}");
            assert_eq!(proofs.notations.len(), 1, "{source}");
            let start = source.rfind("target").unwrap();
            assert_eq!(proofs.notations.values().next(), Some(&(0..start + 6)));
            assert_eq!(
                proofs.references.get(&(start + 6)).map(|(span, _)| span),
                Some(&(start..start + 6))
            );
        }
        for rhs in [
            "target T (other U)",
            "target T 0",
            "target T unknown",
            "M.target T U",
            "(fun x => target x) T U",
            "target T (U : nat)",
            "target T (",
        ] {
            let source = format!("Definition target := 0. Notation \"! T U\" := ({rhs}).");
            let (proofs, exhausted) = proofs(&source, true, 16_384);
            assert!(!exhausted, "{source}");
            assert!(proofs.notations.is_empty(), "{source}");
            assert!(proofs.references.is_empty(), "{source}");
        }
        let (shadowed, exhausted) = proofs(
            "Definition target := 0. Notation \"! target T U\" := (target T U).",
            true,
            16_384,
        );
        assert!(!exhausted);
        assert!(shadowed.notations.is_empty());
    }

    #[test]
    fn left_application_traversal_preserves_work_and_cancellation_refusal() {
        let source = "Definition target := 0. Notation \"! P0 P1 P2 P3 P4 P5 P6 P7\" := (target P0 P1 P2 P3 P4 P5 P6 P7).";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Coq))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let mut cursor = tree.root_node().walk();
        let sentence = tree.root_node().named_children(&mut cursor).nth(1).unwrap();
        let mut cursor = sentence.walk();
        let command = sentence.named_children(&mut cursor).next().unwrap();
        let mut cursor = command.walk();
        let notation = command
            .named_children(&mut cursor)
            .find(|node| node.kind() == "notation_declaration")
            .unwrap();
        for maximum in [0, 1, 16, 32] {
            let mut cache = CaptureCache::new(tree.root_node(), maximum);
            assert!(
                notation_head(
                    notation,
                    source.as_bytes(),
                    &mut cache,
                    &Cancellation::new()
                )
                .unwrap()
                .is_none()
            );
            assert!(cache.exhausted());
        }
        let mut cache = CaptureCache::new(tree.root_node(), 16_384);
        let head = notation_head(
            notation,
            source.as_bytes(),
            &mut cache,
            &Cancellation::new(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(text(head, source.as_bytes()), Some("target"));
        assert!(!cache.exhausted());
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        let mut cache = CaptureCache::new(tree.root_node(), 16_384);
        assert!(matches!(
            notation_head(notation, source.as_bytes(), &mut cache, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
