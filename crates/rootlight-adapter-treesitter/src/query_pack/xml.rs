//! XML/DTD captures retain physical source, never namespace or entity validity.
//! Parent value and PI ranges include bytes omitted by positive native leaves.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, query_failure};
use crate::GrammarFamily;

#[derive(Clone)]
struct OwnerProof {
    name: Range<usize>,
    header: Range<usize>,
    syntax: &'static str,
}

#[derive(Default)]
struct Entry {
    proof: Option<Option<OwnerProof>>,
    chain: Option<bool>,
    depth: Option<usize>,
}

/// Memoizes physical owner proofs and depths within the capture/body work ceiling.
pub(super) struct CaptureCache<'tree> {
    root: Node<'tree>,
    entries: HashMap<usize, Entry>,
    remaining_work: usize,
    maximum_entries: usize,
    exhausted: bool,
}

impl<'tree> CaptureCache<'tree> {
    /// Creates a bounded request-local cache without source-sized preallocation.
    pub(super) fn new(root: Node<'tree>, maximum_entries: usize) -> Self {
        Self {
            root,
            entries: HashMap::new(),
            remaining_work: maximum_entries,
            maximum_entries,
            exhausted: false,
        }
    }

    /// Reports proof work exhaustion that the caller must expose as a capture limit.
    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn visit(&mut self, cancellation: &Cancellation) -> Result<bool, AdapterError> {
        cancellation.check()?;
        let Some(remaining) = self.remaining_work.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining_work = remaining;
        Ok(true)
    }

    fn entry(&mut self, node: Node<'_>) -> Result<bool, AdapterError> {
        if self.entries.contains_key(&node.id()) {
            return Ok(true);
        }
        if self.entries.len() == self.maximum_entries {
            self.exhausted = true;
            return Ok(false);
        }
        self.entries
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.entries.insert(node.id(), Entry::default());
        Ok(true)
    }

    fn depth(
        &mut self,
        node: Node<'tree>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<usize>, AdapterError> {
        cancellation.check()?;
        if source.get(node.byte_range()).is_none() || source.get(self.root.byte_range()).is_none() {
            return Err(query_failure("query-xml-source-range"));
        }
        if self.exhausted {
            return Ok(None);
        }
        if let Some(depth) = self.entries.get(&node.id()).and_then(|entry| entry.depth) {
            return Ok(Some(depth));
        }
        let mut current = self.root;
        let mut depth = 0usize;
        // Native parent() restarts at the root. Descend once and retain depths
        // for the repeated declaration, definition and header captures.
        loop {
            if !self.visit(cancellation)? || !self.entry(current)? {
                return Ok(None);
            }
            if source.get(current.byte_range()).is_none() {
                return Err(query_failure("query-xml-source-range"));
            }
            let entry = self
                .entries
                .get_mut(&current.id())
                .ok_or_else(|| query_failure("query-xml-depth-entry"))?;
            entry.depth = Some(depth);
            if current == node {
                return Ok(Some(depth));
            }
            current = current
                .child_with_descendant(node)
                .filter(|child| *child != current)
                .ok_or_else(|| query_failure("query-xml-depth-node"))?;
            depth = depth
                .checked_add(1)
                .ok_or_else(|| query_failure("query-xml-depth"))?;
        }
    }

    fn proof(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<OwnerProof>, AdapterError> {
        cancellation.check()?;
        if !self.entry(node)? {
            return Ok(None);
        }
        if let Some(proof) = self
            .entries
            .get(&node.id())
            .and_then(|entry| entry.proof.as_ref())
        {
            return Ok(proof.clone());
        }
        let proof = owner_proof(node, source, cancellation, self)?;
        if let Some(entry) = self.entries.get_mut(&node.id()) {
            entry.proof = Some(proof.clone());
        }
        Ok(proof)
    }

    fn chain(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut current = Some(node);
        let mut pending = Vec::new();
        let mut valid = true;
        while let Some(owner) = current {
            cancellation.check()?;
            if !self.entry(owner)? {
                return Ok(false);
            }
            if let Some(cached) = self.entries.get(&owner.id()).and_then(|entry| entry.chain) {
                valid = cached;
                break;
            }
            if !self.visit(cancellation)? {
                return Ok(false);
            }
            pending
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            pending.push(owner.id());
            if owner.is_missing()
                || owner.kind() == "ERROR"
                || (matches!(owner.kind(), "element" | "Attribute")
                    && self.proof(owner, source, cancellation)?.is_none())
            {
                valid = false;
                break;
            }
            current = owner.parent();
        }
        for id in pending {
            cancellation.check()?;
            if let Some(entry) = self.entries.get_mut(&id) {
                entry.chain = Some(valid);
            }
        }
        Ok(valid)
    }
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}

fn name(node: Node<'_>, source: &[u8]) -> bool {
    node.kind() == "Name"
        && clean(node, source)
        && node.utf8_text(source).ok().is_some_and(|raw| {
            structural_captured_name_for_language("xml", raw, raw.len()).as_deref() == Some(raw)
        })
}

fn direct<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut result = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        if child.kind() == kind && result.replace(child).is_some() {
            return Ok(None);
        }
    }
    Ok(result)
}

fn owner_proof(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<Option<OwnerProof>, AdapterError> {
    if node.is_missing() || source.get(node.byte_range()).is_none() {
        return Ok(None);
    }
    if node.kind() == "element" {
        let empty = direct(node, "EmptyElemTag", cancellation, cache)?;
        let opening = direct(node, "STag", cancellation, cache)?;
        let closing = direct(node, "ETag", cancellation, cache)?;
        let header = match (empty, opening, closing) {
            (Some(empty), None, None)
                if clean(empty, source) && empty.byte_range() == node.byte_range() =>
            {
                empty
            }
            (None, Some(opening), Some(closing))
                if clean(opening, source)
                    && clean(closing, source)
                    && opening.start_byte() == node.start_byte()
                    && closing.end_byte() == node.end_byte() =>
            {
                let Some(start_name) = direct(opening, "Name", cancellation, cache)? else {
                    return Ok(None);
                };
                let Some(end_name) = direct(closing, "Name", cancellation, cache)? else {
                    return Ok(None);
                };
                if !name(start_name, source)
                    || !name(end_name, source)
                    || source.get(start_name.byte_range()) != source.get(end_name.byte_range())
                {
                    return Ok(None);
                }
                opening
            }
            _ => return Ok(None),
        };
        let Some(written) = direct(header, "Name", cancellation, cache)? else {
            return Ok(None);
        };
        if !name(written, source) {
            return Ok(None);
        }
        return Ok(Some(OwnerProof {
            name: written.byte_range(),
            header: header.byte_range(),
            syntax: "xml.element",
        }));
    }
    if node.kind() == "Attribute" && clean(node, source) {
        let Some(tag) = node
            .parent()
            .filter(|parent| matches!(parent.kind(), "STag" | "EmptyElemTag"))
        else {
            return Ok(None);
        };
        let Some(element) = tag.parent().filter(|parent| parent.kind() == "element") else {
            return Ok(None);
        };
        let Some(parent) = cache.proof(element, source, cancellation)? else {
            return Ok(None);
        };
        let Some(written) = direct(node, "Name", cancellation, cache)? else {
            return Ok(None);
        };
        let Some(value) = direct(node, "AttValue", cancellation, cache)? else {
            return Ok(None);
        };
        if !name(written, source)
            || !clean(value, source)
            || node.start_byte() < parent.header.start
            || node.end_byte() > parent.header.end
        {
            return Ok(None);
        }
        return Ok(Some(OwnerProof {
            name: written.byte_range(),
            header: node.byte_range(),
            syntax: "xml.attribute",
        }));
    }
    Ok(None)
}

fn declaration_context(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "elementdecl"
            | "AttlistDecl"
            | "AttDef"
            | "GEDecl"
            | "PEDecl"
            | "NotationDecl"
            | "conditionalSect"
            | "doctypedecl"
    )
}

fn reference_syntax(
    family: GrammarFamily,
    node: Node<'_>,
    cancellation: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<Option<&'static str>, AdapterError> {
    let xml = family == GrammarFamily::Xml;
    let Some(parent) = node.parent() else {
        return Ok(None);
    };
    Ok(Some(match parent.kind() {
        "STag" | "EmptyElemTag" | "Attribute" => return Ok(None),
        "ETag" if xml => "xml.end_name",
        "EntityRef" => {
            if xml {
                "xml.entity_name"
            } else {
                "dtd.entity_name"
            }
        }
        "PEReference" => {
            if xml {
                "xml.parameter_entity_name"
            } else {
                "dtd.parameter_entity_name"
            }
        }
        "PI" if node.kind() == "PITarget" => {
            if xml {
                "xml.pi_target"
            } else {
                "dtd.pi_target"
            }
        }
        "elementdecl" | "AttDef" | "GEDecl" | "PEDecl" | "NotationDecl" => {
            let first = direct(parent, "Name", cancellation, cache)?;
            if first != Some(node) {
                return Ok(None);
            }
            if xml {
                "xml.dtd_declared_name"
            } else {
                "dtd.dtd_declared_name"
            }
        }
        _ => {
            if xml {
                "xml.dtd_name"
            } else {
                "dtd.dtd_name"
            }
        }
    }))
}

/// Projects a physical XML/DTD capture without claiming validity or target binding.
///
/// Returns no candidate for unproved owners or unsupported roles. Cancellation,
/// invalid source ranges and allocation failures propagate as adapter errors.
pub(super) fn candidate<'tree>(
    family: GrammarFamily,
    node: Node<'tree>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache<'tree>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancellation.check()?;
    if !matches!(family, GrammarFamily::Xml | GrammarFamily::Dtd) {
        return Err(query_failure("query-xml-family"));
    }
    let xml = family == GrammarFamily::Xml;
    let mut range = node.byte_range();
    if source.get(range.clone()).is_none() {
        return Err(query_failure("query-xml-source-range"));
    }
    let syntax = if matches!(node.kind(), "document" | "extSubset") {
        if !matches!(role, StructuralRole::Root | StructuralRole::Module) {
            return Ok(None);
        }
        range = 0..source.len();
        if xml { "xml.file" } else { "dtd.file" }
    } else if matches!(node.kind(), "element" | "Attribute") && xml {
        let proof = cache.proof(node, source, cancellation)?;
        let admitted = proof.is_some() && cache.chain(node, source, cancellation)?;
        if !admitted {
            if role != StructuralRole::Signature {
                return Ok(None);
            }
            "xml.unsupported_owner"
        } else {
            let proof = proof.ok_or_else(|| query_failure("query-xml-owner-proof"))?;
            match role {
                StructuralRole::Declaration => proof.syntax,
                StructuralRole::Definition => {
                    range = proof.name;
                    "xml.name"
                }
                StructuralRole::Signature => {
                    range = proof.header;
                    if node.kind() == "element" {
                        "xml.opening_header"
                    } else {
                        "xml.attribute_header"
                    }
                }
                _ => return Ok(None),
            }
        }
    } else if role == StructuralRole::Signature && declaration_context(node) {
        if xml {
            "xml.dtd_declaration_source"
        } else {
            "dtd.declaration_source"
        }
    } else if role == StructuralRole::Reference {
        if !clean(node, source) || (node.kind() != "PITarget" && !name(node, source)) {
            return Ok(None);
        }
        if node.kind() == "PITarget"
            && !node.utf8_text(source).ok().is_some_and(|raw| {
                structural_captured_name_for_language(
                    if xml { "xml" } else { "dtd" },
                    raw,
                    raw.len(),
                )
                .as_deref()
                    == Some(raw)
            })
        {
            return Ok(None);
        }
        let Some(syntax) = reference_syntax(family, node, cancellation, cache)? else {
            return Ok(None);
        };
        syntax
    } else if matches!(
        role,
        StructuralRole::Comment | StructuralRole::Documentation
    ) {
        if xml { "xml.comment" } else { "dtd.comment" }
    } else if role == StructuralRole::StringLiteral {
        // Preserve the whole native parent, never just its named children.
        if xml {
            "xml.source_text"
        } else {
            "dtd.source_text"
        }
    } else {
        return Ok(None);
    };
    let Some(native_depth) = cache.depth(node, source, cancellation)? else {
        return Ok(None);
    };
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth,
    }))
}

#[cfg(test)]
mod tests {
    use rootlight_cancel::CancellationReason;
    use tree_sitter::Parser;

    use super::*;
    use crate::query_pack::QueryPackRegistry;

    fn captures(family: GrammarFamily, source: &str) -> Vec<QueryCandidate> {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::registry::language_for(family))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        QueryPackRegistry::audited()
            .unwrap()
            .get(family)
            .unwrap()
            .extract(
                family,
                &tree,
                source.as_bytes(),
                4096,
                4096,
                &Cancellation::new(),
            )
            .unwrap()
            .candidates
    }

    #[test]
    fn xml_direct_written_owners_have_exact_names_headers_and_no_call_roles() {
        let source = "<root><item value=\"plain\"/><item value='other'>text</item></root>";
        let facts = captures(GrammarFamily::Xml, source);
        let mut names = facts
            .iter()
            .filter(|fact| fact.role == StructuralRole::Definition)
            .map(|fact| &source[fact.start..fact.end])
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(names, ["item", "item", "root", "value", "value"]);
        for header in [
            "<root>",
            "<item value=\"plain\"/>",
            "<item value='other'>",
            "value=\"plain\"",
            "value='other'",
        ] {
            assert!(
                facts
                    .iter()
                    .any(|fact| fact.role == StructuralRole::Signature
                        && &source[fact.start..fact.end] == header),
                "{header}"
            );
        }
        assert!(!facts.iter().any(|fact| matches!(
            fact.role,
            StructuralRole::Call | StructuralRole::Import | StructuralRole::Scope
        )));
    }

    #[test]
    fn xml_native_clean_unequal_names_never_form_physical_entities() {
        for source in ["<Upper></upper>", "<À></ǀ>", "<À></À>"] {
            let facts = captures(GrammarFamily::Xml, source);
            assert!(!facts.iter().any(|fact| matches!(
                fact.role,
                StructuralRole::Declaration | StructuralRole::Definition
            )));
            assert!(
                facts
                    .iter()
                    .any(|fact| fact.role == StructuralRole::Signature
                        && fact.syntax == "xml.unsupported_owner"
                        && &source[fact.start..fact.end] == source)
            );
        }
    }

    #[test]
    fn xml_source_parents_retain_attribute_pi_and_empty_cdata_bytes() {
        for (source, expected) in [
            (
                "<root><item value=\"plain\"/><item value='other'>text</item></root>",
                "\"plain\"",
            ),
            (
                "<root><?go first?second\nthird???></root>",
                "<?go first?second\nthird???>",
            ),
            ("<?go   ?><root/>", "<?go   ?>"),
            ("<root><![CDATA[]]></root>", "<![CDATA[]]>"),
            (
                "<?xml-stylesheet href=\"local.xsl\"?><root/>",
                "<?xml-stylesheet href=\"local.xsl\"?>",
            ),
        ] {
            let facts = captures(GrammarFamily::Xml, source);
            assert!(
                facts
                    .iter()
                    .any(|fact| fact.role == StructuralRole::StringLiteral
                        && &source[fact.start..fact.end] == expected),
                "{expected}"
            );
        }
    }

    #[test]
    fn dtd_context_and_optional_written_name_are_separate_patterns() {
        let source = "<!ELEMENT root EMPTY>";
        let facts = captures(GrammarFamily::Dtd, source);
        assert!(
            facts
                .iter()
                .any(|fact| fact.role == StructuralRole::Signature
                    && fact.syntax == "dtd.declaration_source"
                    && &source[fact.start..fact.end] == source)
        );
        assert!(
            facts
                .iter()
                .any(|fact| fact.role == StructuralRole::Reference
                    && fact.syntax == "dtd.dtd_declared_name"
                    && &source[fact.start..fact.end] == "root")
        );
        assert!(
            facts
                .iter()
                .any(|fact| fact.role == StructuralRole::StringLiteral
                    && &source[fact.start..fact.end] == source)
        );
        assert!(!facts.iter().any(|fact| matches!(
            fact.role,
            StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Scope
        )));
    }

    #[test]
    fn cached_depth_matches_native_parents_and_cursor_for_xml_and_dtd() {
        let nested = format!("{}text{}", "<item>".repeat(16), "</item>".repeat(16));
        let mut parser = Parser::new();
        let cancellation = Cancellation::new();
        let (mut saw_recovery, mut saw_anonymous, mut maximum_depth) = (false, false, 0);
        for (family, source) in [
            (GrammarFamily::Xml, ""),
            (
                GrammarFamily::Xml,
                "<?xml version='1.0'?><root><item value='plain'/>text</root>",
            ),
            (GrammarFamily::Xml, "<root><item value="),
            (GrammarFamily::Xml, nested.as_str()),
            (
                GrammarFamily::Dtd,
                "<!ELEMENT root (item*)>\n<!ATTLIST root id ID #IMPLIED>\n<!ENTITY name 'text'>",
            ),
            (GrammarFamily::Dtd, "<!ELEMENT root (item|)>"),
        ] {
            parser
                .set_language(&crate::registry::language_for(family))
                .unwrap();
            let tree = parser.parse(source, None).unwrap();
            let mut cache = CaptureCache::new(tree.root_node(), 65_536);
            let mut cursor = tree.walk();
            let mut expected_depth = 0usize;
            loop {
                let node = cursor.node();
                saw_recovery |= node.is_error() || node.is_missing();
                saw_anonymous |= !node.is_named();
                let depth = cache
                    .depth(node, source.as_bytes(), &cancellation)
                    .unwrap()
                    .unwrap();
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
                maximum_depth = maximum_depth.max(depth);
                let remaining = cache.remaining_work;
                let entries = cache.entries.len();
                assert_eq!(
                    cache.depth(node, source.as_bytes(), &cancellation).unwrap(),
                    Some(depth)
                );
                assert_eq!(cache.remaining_work, remaining);
                assert_eq!(cache.entries.len(), entries);
                if cursor.goto_first_child() {
                    expected_depth += 1;
                    continue;
                }
                while !cursor.goto_next_sibling() {
                    if !cursor.goto_parent() {
                        break;
                    }
                    expected_depth -= 1;
                }
                if cursor.node() == tree.root_node() {
                    break;
                }
            }
            assert!(!cache.exhausted());
        }
        assert!(saw_recovery);
        assert!(saw_anonymous);
        assert!(maximum_depth >= 16);
    }

    #[test]
    fn depth_cache_rejects_foreign_ranges_and_observes_cached_cancellation() {
        let source = b"<root><item/></root>";
        let mut parser = Parser::new();
        parser
            .set_language(&crate::registry::language_for(GrammarFamily::Xml))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let other = parser.parse(source, None).unwrap();
        let node = tree.root_node().child_by_field_name("root").unwrap();
        let mut cache = CaptureCache::new(tree.root_node(), 4096);
        let cancellation = Cancellation::new();
        assert_eq!(cache.depth(node, source, &cancellation).unwrap(), Some(1));
        assert!(matches!(
            cache.depth(node, &source[..source.len() - 1], &cancellation),
            Err(AdapterError::ProviderFailed { code })
                if code.as_str() == "query-xml-source-range"
        ));
        assert!(matches!(
            cache.depth(other.root_node(), source, &cancellation),
            Err(AdapterError::ProviderFailed { code })
                if code.as_str() == "query-xml-depth-node"
        ));
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            cache.depth(node, source, &cancellation),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }

    #[test]
    fn depth_work_exhaustion_cannot_publish_a_cached_partial_identity() {
        let source = b"<root><item/></root>";
        let mut parser = Parser::new();
        parser
            .set_language(&crate::registry::language_for(GrammarFamily::Xml))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let node = root.child_by_field_name("root").unwrap();
        let cancellation = Cancellation::new();
        for maximum in [0, 1] {
            let mut cache = CaptureCache::new(root, maximum);
            assert_eq!(cache.depth(node, source, &cancellation).unwrap(), None);
            assert!(cache.exhausted());
            assert!(cache.entries.len() <= maximum);
            assert_eq!(cache.depth(root, source, &cancellation).unwrap(), None);
            assert!(
                candidate(
                    GrammarFamily::Xml,
                    root,
                    StructuralRole::Module,
                    source,
                    &cancellation,
                    &mut cache,
                )
                .unwrap()
                .is_none()
            );
        }
        let registry = QueryPackRegistry::audited().unwrap();
        let error = registry
            .get(GrammarFamily::Xml)
            .unwrap()
            .extract_identity(GrammarFamily::Xml, &tree, source, 1, &cancellation)
            .unwrap_err();
        assert!(matches!(error, AdapterError::ProviderFailed { code }
            if code.as_str() == "query-identity-xml-captures-limit"));
    }

    #[test]
    fn xml_owner_proof_propagates_cancellation_and_existing_work_bound() {
        let source = "<root><item value=\"plain\"/><item value='other'>text</item></root>";
        let mut parser = Parser::new();
        parser
            .set_language(&crate::registry::language_for(GrammarFamily::Xml))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let node = tree.root_node().child_by_field_name("root").unwrap();
        let mut bounded = CaptureCache::new(tree.root_node(), 1);
        assert!(
            bounded
                .proof(node, source.as_bytes(), &Cancellation::new())
                .unwrap()
                .is_none()
        );
        assert!(bounded.exhausted());
        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            candidate(
                GrammarFamily::Xml,
                node,
                StructuralRole::Declaration,
                source.as_bytes(),
                &cancelled,
                &mut CaptureCache::new(tree.root_node(), 4096)
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
