//! Retain clean written Meson bindings without declaring a recovered file clean.
//! Only an immediately preceding top-level assignment can certify a bare foreach
//! iterable. Includes, dynamic names and every intervening statement break proof.

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{HARD_MAX_QUERY_CAPTURES, QueryCandidate, StructuralRole};

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete: bool,
    remaining: usize,
    exhausted: bool,
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_error()
        && !node.is_missing()
        && !node.has_error()
        && source.get(node.byte_range()).is_some()
}

impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], maximum: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            remaining: maximum.min(HARD_MAX_QUERY_CAPTURES).saturating_mul(32),
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn visit(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(next) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = next;
        Ok(true)
    }

    fn children<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Vec<Node<'tree>>, AdapterError> {
        if node.child_count() > self.remaining {
            self.exhausted = true;
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        result
            .try_reserve_exact(node.child_count())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(Vec::new());
            }
            if child.kind() != "comment" {
                result.push(child);
            }
        }
        Ok(result)
    }

    fn word(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'source [u8]>, AdapterError> {
        if !self.visit(cancel)? || node.kind() != "identifier" || !clean(node, self.source) {
            return Ok(None);
        }
        let Some(bytes) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        for _ in bytes.chunks(64) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
        }
        let Some((first, tail)) = bytes.split_first() else {
            return Ok(None);
        };
        Ok(((*first == b'_' || first.is_ascii_alphabetic())
            && tail.iter().all(|b| *b == b'_' || b.is_ascii_alphanumeric()))
        .then_some(bytes))
    }

    fn top_statement<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        if !self.complete || !self.visit(cancel)? || node.is_missing() || node.is_error() {
            return Ok(None);
        }
        let Some(statement) = node
            .parent()
            .filter(|p| p.kind() == "statement" && clean(*p, self.source))
        else {
            return Ok(None);
        };
        let Some(root) = statement.parent().filter(|p| {
            p.kind() == "build_definition"
                && p.parent().is_none()
                && !p.is_error()
                && !p.is_missing()
        }) else {
            return Ok(None);
        };
        let Some(prefix) = self.source.get(..root.start_byte()) else {
            return Ok(None);
        };
        let Some(suffix) = self.source.get(root.end_byte()..) else {
            return Ok(None);
        };
        for part in [
            prefix.strip_prefix(b"\xef\xbb\xbf").unwrap_or(prefix),
            suffix,
        ] {
            for chunk in part.chunks(64) {
                if !self.visit(cancel)? || !chunk.iter().all(u8::is_ascii_whitespace) {
                    return Ok(None);
                }
            }
        }
        let children = self.children(statement, cancel)?;
        if children.iter().filter(|c| c.is_named()).count() != 1
            || children
                .iter()
                .find(|c| c.is_named())
                .is_none_or(|c| c.id() != node.id())
        {
            return Ok(None);
        }
        Ok(Some(statement))
    }

    fn assignment<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        if node.kind() != "assignment_statement"
            || !clean(node, self.source)
            || self.top_statement(node, cancel)?.is_none()
        {
            return Ok(None);
        }
        let children = self.children(node, cancel)?;
        let [left, operator, right] = children.as_slice() else {
            return Ok(None);
        };
        if left.kind() != "expression"
            || operator.kind() != "assignment_operator"
            || self.source.get(operator.byte_range()) != Some(b"=")
            || right.kind() != "expression"
        {
            return Ok(None);
        }
        let left_children = self.children(*left, cancel)?;
        let [name] = left_children.as_slice() else {
            return Ok(None);
        };
        Ok(self.word(*name, cancel)?.map(|_| *name))
    }

    fn iteration<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<(Vec<Node<'tree>>, Node<'tree>)>, AdapterError> {
        if node.kind() != "iteration_statement"
            || !clean(node, self.source)
            || self.top_statement(node, cancel)?.is_none()
        {
            return Ok(None);
        }
        let children = self.children(node, cancel)?;
        let Some(first) = children.first() else {
            return Ok(None);
        };
        if first.kind() != "foreach" || self.source.get(first.byte_range()) != Some(b"foreach") {
            return Ok(None);
        }
        let Some(list) = children.get(1).filter(|n| n.kind() == "identifier_list") else {
            return Ok(None);
        };
        let Some(colon) = children.get(2).filter(|n| n.kind() == ":") else {
            return Ok(None);
        };
        if self.source.get(colon.byte_range()) != Some(b":") {
            return Ok(None);
        }
        let Some(iterable) = children.get(3).filter(|n| n.kind() == "identifier") else {
            return Ok(None);
        };
        if self.word(*iterable, cancel)?.is_none() {
            return Ok(None);
        }
        let list_children = self.children(*list, cancel)?;
        let mut binders = Vec::new();
        binders
            .try_reserve_exact(list_children.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut expect_name = true;
        for child in list_children {
            if expect_name {
                let Some(word) = self.word(child, cancel)? else {
                    return Ok(None);
                };
                for prior in &binders {
                    if !self.visit(cancel)? || self.word(*prior, cancel)? == Some(word) {
                        return Ok(None);
                    }
                }
                binders.push(child);
            } else if child.kind() != "," {
                return Ok(None);
            }
            expect_name = !expect_name;
        }
        if binders.is_empty() || expect_name {
            return Ok(None);
        }
        Ok(Some((binders, *iterable)))
    }

    fn adjacent<'tree>(
        &mut self,
        iteration: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<(Node<'tree>, Node<'tree>)>, AdapterError> {
        let Some((binders, iterable)) = self.iteration(iteration, cancel)? else {
            return Ok(None);
        };
        let Some(statement) = self.top_statement(iteration, cancel)? else {
            return Ok(None);
        };
        let Some(previous) = statement
            .prev_named_sibling()
            .filter(|n| n.kind() == "statement" && clean(*n, self.source))
        else {
            return Ok(None);
        };
        let previous_children = self.children(previous, cancel)?;
        let Some(assignment) = previous_children.iter().find(|n| n.is_named()).copied() else {
            return Ok(None);
        };
        let Some(name) = self.assignment(assignment, cancel)? else {
            return Ok(None);
        };
        if self.word(name, cancel)? != self.word(iterable, cancel)? {
            return Ok(None);
        }
        let Some(between) = self.source.get(previous.end_byte()..statement.start_byte()) else {
            return Ok(None);
        };
        for chunk in between.chunks(64) {
            if !self.visit(cancel)? || !chunk.iter().all(u8::is_ascii_whitespace) {
                return Ok(None);
            }
        }
        for binder in binders {
            if !self.visit(cancel)? || self.word(binder, cancel)? == self.word(iterable, cancel)? {
                return Ok(None);
            }
        }
        Ok(Some((assignment, iterable)))
    }
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    cancel: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    if !cache.visit(cancel)? {
        return Ok(None);
    }
    let mut range = node.byte_range();
    let mut selected_role = role;
    let syntax = match role {
        StructuralRole::Signature if node.is_error() && !node.is_missing() => {
            "meson.recovered_region"
        }
        StructuralRole::Root if node.kind() == "build_definition" => "meson.build_definition",
        StructuralRole::Declaration if node.kind() == "assignment_statement" => {
            if cache.assignment(node, cancel)?.is_none() {
                return Ok(None);
            }
            "meson.assignment"
        }
        StructuralRole::Declaration | StructuralRole::Definition if node.kind() == "identifier" => {
            let assignment = node
                .parent()
                .and_then(|p| p.parent())
                .filter(|p| p.kind() == "assignment_statement");
            if let Some(assignment) = assignment {
                if cache
                    .assignment(assignment, cancel)?
                    .is_none_or(|name| name.id() != node.id())
                {
                    return Ok(None);
                }
                "meson.name"
            } else {
                let Some(iteration) = node
                    .parent()
                    .filter(|p| p.kind() == "identifier_list")
                    .and_then(|p| p.parent())
                else {
                    return Ok(None);
                };
                let Some((binders, _)) = cache.iteration(iteration, cancel)? else {
                    return Ok(None);
                };
                if !binders.iter().any(|binder| binder.id() == node.id()) {
                    return Ok(None);
                }
                if role == StructuralRole::Declaration {
                    "meson.foreach_variable"
                } else {
                    "meson.name"
                }
            }
        }
        StructuralRole::Signature if node.kind() == "iteration_statement" => {
            let Some((assignment, iterable)) = cache.adjacent(node, cancel)? else {
                return Ok(None);
            };
            range = assignment.start_byte()..iterable.end_byte();
            "meson.adjacent_iterable_context"
        }
        StructuralRole::Signature if node.kind() == "identifier" => {
            let Some(iteration) = node.parent().filter(|p| p.kind() == "iteration_statement")
            else {
                return Ok(None);
            };
            let Some((_, iterable)) = cache.adjacent(iteration, cancel)? else {
                return Ok(None);
            };
            if iterable.id() != node.id() {
                return Ok(None);
            }
            selected_role = StructuralRole::Reference;
            "meson.adjacent_iterable"
        }
        StructuralRole::Reference if node.kind() == "identifier" && clean(node, cache.source) => {
            if let Some(iteration) = node.parent().filter(|p| p.kind() == "iteration_statement")
                && cache
                    .adjacent(iteration, cancel)?
                    .is_some_and(|(_, iterable)| iterable.id() == node.id())
            {
                return Ok(None);
            }
            if node.parent().is_some_and(|p| p.kind() == "identifier_list") {
                return Ok(None);
            }
            if let Some(assignment) = node
                .parent()
                .and_then(|p| p.parent())
                .filter(|p| p.kind() == "assignment_statement")
                && cache
                    .assignment(assignment, cancel)?
                    .is_some_and(|name| name.id() == node.id())
            {
                return Ok(None);
            }
            "meson.unproved_name"
        }
        StructuralRole::Comment => "meson.comment",
        StructuralRole::StringLiteral => "meson.string",
        _ => return Ok(None),
    };
    let mut depth = 0_usize;
    let mut current = node.parent();
    while let Some(parent) = current {
        if !cache.visit(cancel)? {
            return Ok(None);
        }
        depth = depth.saturating_add(1);
        current = parent.parent();
    }
    if syntax == "meson.adjacent_iterable_context" {
        depth = 1;
    }
    if cache.exhausted {
        return Ok(None);
    }
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role: selected_role,
        syntax,
        required: selected_role.belongs_to_identity_closure()
            || syntax == "meson.adjacent_iterable",
        native_depth: depth,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn tree(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_meson::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    fn original_recovered_file_keeps_only_clean_native_assignment_and_iterable_authority() {
        let source = include_bytes!("../../tests/fixtures/meson/meson.build");
        let parsed = tree(source);
        assert_eq!(source.len(), 1095);
        assert!(parsed.root_node().has_error());
        let site = parsed
            .root_node()
            .descendant_for_byte_range(557, 562)
            .unwrap();
        assert_eq!(site.kind(), "identifier");
        let mut cache = CaptureCache::new(source, 16_384, true);
        let proof = candidate(
            site,
            StructuralRole::Signature,
            &Cancellation::new(),
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (
                proof.start,
                proof.end,
                proof.role,
                proof.syntax,
                proof.required
            ),
            (
                557,
                562,
                StructuralRole::Reference,
                "meson.adjacent_iterable",
                true
            )
        );
        let iteration = site.parent().unwrap();
        let context = candidate(
            iteration,
            StructuralRole::Signature,
            &Cancellation::new(),
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (context.start, context.end, context.syntax),
            (516, 562, "meson.adjacent_iterable_context")
        );
        assert!(
            candidate(
                site,
                StructuralRole::Reference,
                &Cancellation::new(),
                &mut cache
            )
            .unwrap()
            .is_none()
        );
        let binder = parsed
            .root_node()
            .descendant_for_byte_range(553, 554)
            .unwrap();
        assert!(
            candidate(
                binder,
                StructuralRole::Declaration,
                &Cancellation::new(),
                &mut cache
            )
            .unwrap()
            .is_some()
        );
        assert!(!cache.exhausted());
    }

    #[test]
    fn clean_intervening_statements_and_binder_collisions_cannot_donate_an_iterable_target() {
        for source in [
            b"items = []\nset_variable('items', [])\nforeach item : items\nendforeach\n".as_ref(),
            b"items = []\nforeach items : items\nendforeach\n".as_ref(),
            b"items = []\nforeach item, item : items\nendforeach\n".as_ref(),
            b"if true\nitems = []\nforeach item : items\nendforeach\nendif\n".as_ref(),
        ] {
            let parsed = tree(source);
            assert!(
                !parsed.root_node().has_error(),
                "{}",
                String::from_utf8_lossy(source)
            );
            let at = std::str::from_utf8(source).unwrap().rfind("items").unwrap();
            let site = parsed
                .root_node()
                .descendant_for_byte_range(at, at + 5)
                .unwrap();
            let mut cache = CaptureCache::new(source, 16_384, true);
            assert!(
                candidate(
                    site,
                    StructuralRole::Signature,
                    &Cancellation::new(),
                    &mut cache
                )
                .unwrap()
                .is_none()
            );
        }
    }

    #[test]
    fn partial_context_finite_work_and_typed_cancellation_never_issue_a_partial_certificate() {
        let source = b"items = []\nforeach item : items\nendforeach\n";
        let parsed = tree(source);
        assert!(!parsed.root_node().has_error());
        let at = std::str::from_utf8(source).unwrap().rfind("items").unwrap();
        let site = parsed
            .root_node()
            .descendant_for_byte_range(at, at + 5)
            .unwrap();
        let mut full = CaptureCache::new(source, 16_384, true);
        assert!(
            candidate(
                site,
                StructuralRole::Signature,
                &Cancellation::new(),
                &mut full
            )
            .unwrap()
            .is_some()
        );
        let mut partial = CaptureCache::new(source, 16_384, false);
        assert!(
            candidate(
                site,
                StructuralRole::Signature,
                &Cancellation::new(),
                &mut partial
            )
            .unwrap()
            .is_none()
        );
        let mut small = CaptureCache::new(source, 1, true);
        small.remaining = 1;
        assert!(
            candidate(
                site,
                StructuralRole::Signature,
                &Cancellation::new(),
                &mut small
            )
            .unwrap()
            .is_none()
        );
        assert!(small.exhausted());
        let stop = Cancellation::new();
        assert!(stop.cancel(CancellationReason::ClientRequest));
        let mut cache = CaptureCache::new(source, 16_384, true);
        assert!(matches!(
            candidate(site, StructuralRole::Signature, &stop, &mut cache),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }

    #[test]
    fn helper_refusal_names_meson_and_keeps_unexposed_work_counts_unknown() {
        use crate::query_pack::{
            GrammarFamily, QueryInput, QueryLimit, QueryPackRegistry, QueryScanLimits,
        };
        let source = b"items = []\nforeach item : items\nendforeach\n";
        let parsed = tree(source);
        assert!(!parsed.root_node().has_error());
        let registry = QueryPackRegistry::audited().unwrap();
        let pack = registry.get(GrammarFamily::Meson).unwrap();
        let mut witness = None;
        let limit = pack
            .scan_query_with_refusal(
                &pack.identity_query,
                QueryInput {
                    family: GrammarFamily::Meson,
                    tree: &parsed,
                    source,
                    cancellation: &Cancellation::new(),
                    normalize_calls: false,
                },
                QueryScanLimits {
                    matches: 128,
                    captures: 128,
                    body_nodes: 1,
                },
                |_| Ok(()),
                Some(&mut witness),
            )
            .unwrap();
        assert_eq!(limit, Some(QueryLimit::Capture));
        let event = witness.unwrap();
        let details = event.details();
        assert_eq!(details.origin(), "query-identity-meson-captures-limit");
        assert_eq!((details.observed(), details.limit()), (None, None));
        assert_eq!(details.unit(), "helper-allowance");
        assert!(details.progress().0.unwrap() < 128);
        assert!(details.progress().1.unwrap() < 128);
        assert!(crate::query_pack::identity_refusal::IdentityRefusal::recognizes(&event.error()));
    }
}
