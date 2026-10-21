//! Admit explicit Pug names only under a complete clean native source proof.
//! Capture work is bounded independently from the grammar's scanner credit.

use super::{HARD_MAX_QUERY_CAPTURES, QueryCandidate, StructuralRole};
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete_clean: bool,
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
    pub(super) fn new(
        source: &'source [u8],
        root: Node<'_>,
        maximum: usize,
        complete: bool,
    ) -> Self {
        Self {
            source,
            complete_clean: complete
                && root.kind() == "source_file"
                && (root.start_byte() == 0
                    || (root.start_byte() == 3 && source.starts_with(b"\xef\xbb\xbf")))
                && root.end_byte() == source.len()
                && clean(root, source),
            remaining: maximum.min(HARD_MAX_QUERY_CAPTURES).saturating_mul(8),
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn visit(&mut self, cancellation: &Cancellation) -> Result<bool, AdapterError> {
        cancellation.check()?;
        let Some(next) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = next;
        Ok(true)
    }

    fn name(&mut self, node: Node<'_>, cancellation: &Cancellation) -> Result<bool, AdapterError> {
        if !self.visit(cancellation)? || !clean(node, self.source) {
            return Ok(false);
        }
        let Some(bytes) = self.source.get(node.byte_range()) else {
            return Ok(false);
        };
        let Some(first) = bytes.first() else {
            return Ok(false);
        };
        if !(first.is_ascii_alphabetic() || *first == b'_') {
            return Ok(false);
        }
        for chunk in bytes.chunks(64) {
            if !self.visit(cancellation)? {
                return Ok(false);
            }
            if !chunk
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':'))
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn tag<'tree>(
        &mut self,
        node: Node<'tree>,
        cancellation: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        if !self.visit(cancellation)? || node.kind() != "tag" || !clean(node, self.source) {
            return Ok(None);
        }
        let Some(name) = node.named_child(0).filter(|name| name.kind() == "tag_name") else {
            return Ok(None);
        };
        Ok(self.name(name, cancellation)?.then_some(name))
    }

    fn attribute(
        &mut self,
        node: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.visit(cancellation)? || node.kind() != "attribute" || !clean(node, self.source) {
            return Ok(false);
        }
        let Some(name) = node
            .named_child(0)
            .filter(|name| name.kind() == "attribute_name")
        else {
            return Ok(false);
        };
        let Some(attributes) = node.parent().filter(|parent| parent.kind() == "attributes") else {
            return Ok(false);
        };
        let Some(tag) = attributes.parent().filter(|parent| parent.kind() == "tag") else {
            return Ok(false);
        };
        Ok(self.name(name, cancellation)? && self.tag(tag, cancellation)?.is_some())
    }
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    cancellation: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    if !cache.visit(cancellation)? {
        return Ok(None);
    }
    // This cache belongs to the one QueryInput tree that produced every capture.
    // Included ranges or recovery cannot promote a locally clean-looking header.
    if !cache.complete_clean {
        let root = role == StructuralRole::Root && node.kind() == "source_file";
        return Ok(Some(QueryCandidate {
            start: if root { node.start_byte() } else { 0 },
            end: if root {
                node.end_byte()
            } else {
                cache.source.len()
            },
            role: if root {
                StructuralRole::Root
            } else {
                StructuralRole::Signature
            },
            syntax: "pug.native_context_unavailable",
            required: true,
            native_depth: 0,
        }));
    }
    let (role, syntax) = match (role, node.kind()) {
        (StructuralRole::Root, "source_file") => (role, "pug.source_file"),
        (StructuralRole::Module, "source_file") => (role, "pug.file"),
        (StructuralRole::Declaration, "tag") => {
            if cache.tag(node, cancellation)?.is_some() {
                (role, "pug.element")
            } else {
                (
                    StructuralRole::Signature,
                    "pug.implicit_or_unavailable_owner",
                )
            }
        }
        (StructuralRole::Declaration, "attribute") => {
            if cache.attribute(node, cancellation)? {
                (role, "pug.attribute")
            } else {
                (
                    StructuralRole::Signature,
                    "pug.implicit_or_unavailable_owner",
                )
            }
        }
        (StructuralRole::Definition, "tag_name") => {
            let admitted = match node.parent() {
                Some(parent) => cache.tag(parent, cancellation)? == Some(node),
                None => false,
            };
            if admitted {
                (role, "pug.name")
            } else {
                (
                    StructuralRole::Signature,
                    "pug.implicit_or_unavailable_owner",
                )
            }
        }
        (StructuralRole::Definition, "attribute_name") => {
            let admitted = match node.parent() {
                Some(parent) => {
                    parent.named_child(0) == Some(node) && cache.attribute(parent, cancellation)?
                }
                None => false,
            };
            if admitted {
                (role, "pug.name")
            } else {
                (
                    StructuralRole::Signature,
                    "pug.implicit_or_unavailable_owner",
                )
            }
        }
        (StructuralRole::Comment, "comment") => (role, "pug.comment"),
        (StructuralRole::StringLiteral, "content" | "attribute_value") => (role, "pug.text"),
        (StructuralRole::Signature, _) => (role, "pug.construct_unavailable"),
        _ => return Ok(None),
    };
    let mut candidate = QueryCandidate {
        start: node.start_byte(),
        end: node.end_byte(),
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: 0,
    };
    super::canonicalize_file_root_preamble(
        &mut candidate,
        node,
        cache.source,
        cache.complete_clean,
    );
    Ok(Some(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    struct Probe(Cancellation);

    impl tree_sitter_pug::CancellationProbe for Probe {
        fn is_cancelled(&self) -> bool {
            self.0.check().is_err()
        }
    }

    fn tree(source: &[u8]) -> tree_sitter::Tree {
        let scope =
            tree_sitter_pug::ScannerScope::begin(Probe(Cancellation::new()), 1_000_000).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_pug::LANGUAGE.into())
            .unwrap();
        let parsed = parser.parse(source, None).unwrap();
        drop(parser);
        scope.finish().unwrap();
        parsed
    }

    #[test]
    fn original_native_tag_and_attribute_names_keep_their_exact_direct_owner() {
        let source = include_bytes!("../../tests/fixtures/pug/hash.pug");
        let parsed = tree(source);
        let root = parsed.root_node();
        assert_eq!(root.end_byte(), 718);
        assert!(!root.has_error());
        let mut cache = CaptureCache::new(source, root, 16_384, true);
        let cancel = Cancellation::new();
        for (range, kind, declaration) in [
            (410..415, "tag_name", "pug.element"),
            (434..438, "attribute_name", "pug.attribute"),
            (447..455, "attribute_name", "pug.attribute"),
        ] {
            let name = root
                .descendant_for_byte_range(range.start, range.end)
                .unwrap();
            assert_eq!(name.kind(), kind);
            let definition = candidate(name, StructuralRole::Definition, &cancel, &mut cache)
                .unwrap()
                .unwrap();
            assert_eq!(definition.role, StructuralRole::Definition);
            assert_eq!(definition.syntax, "pug.name");
            assert_eq!(definition.start..definition.end, range);
            let owner = name.parent().unwrap();
            let declared = candidate(owner, StructuralRole::Declaration, &cancel, &mut cache)
                .unwrap()
                .unwrap();
            assert_eq!(declared.syntax, declaration);
            assert_eq!(declared.role, StructuralRole::Declaration);
            if kind == "attribute_name" {
                let attributes = owner.parent().unwrap();
                assert_eq!(attributes.kind(), "attributes");
                let tag = attributes.parent().unwrap();
                assert_eq!(tag.kind(), "tag");
                assert_eq!(tag.named_child(0).unwrap().byte_range(), 410..415);
            }
        }
        let value = root.descendant_for_byte_range(440, 444).unwrap();
        assert!(
            candidate(value, StructuralRole::Definition, &cancel, &mut cache)
                .unwrap()
                .is_none()
        );
        assert!(!cache.exhausted());
    }

    #[test]
    fn implicit_tags_cannot_donate_named_owners_to_their_written_attributes() {
        let source = b".written(disabled)\n";
        let parsed = tree(source);
        let root = parsed.root_node();
        assert!(!root.has_error());
        let name = root.descendant_for_byte_range(9, 17).unwrap();
        assert_eq!(&source[name.byte_range()], b"disabled");
        let mut cache = CaptureCache::new(source, root, 16_384, true);
        let result = candidate(
            name,
            StructuralRole::Definition,
            &Cancellation::new(),
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.role, StructuralRole::Signature);
        assert_eq!(result.syntax, "pug.implicit_or_unavailable_owner");
    }

    #[test]
    fn incomplete_or_recovered_native_context_never_admits_a_definition() {
        let source = include_bytes!("../../tests/fixtures/pug/hash.pug");
        let parsed = tree(source);
        let root = parsed.root_node();
        let name = root.descendant_for_byte_range(410, 415).unwrap();
        let mut partial = CaptureCache::new(source, root, 16_384, false);
        let result = candidate(
            name,
            StructuralRole::Definition,
            &Cancellation::new(),
            &mut partial,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.role, StructuralRole::Signature);
        assert_eq!(result.syntax, "pug.native_context_unavailable");
        assert_eq!(result.start..result.end, 0..718);
        let broken_source = b"input(type=\n";
        let broken = tree(broken_source);
        assert!(broken.root_node().has_error());
        let mut recovered = CaptureCache::new(broken_source, broken.root_node(), 16_384, true);
        let result = candidate(
            broken.root_node(),
            StructuralRole::Root,
            &Cancellation::new(),
            &mut recovered,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.role, StructuralRole::Root);
        assert_eq!(result.syntax, "pug.native_context_unavailable");
    }

    #[test]
    fn complete_bom_root_keeps_exact_children_and_cannot_authorize_an_arbitrary_prefix() {
        let source = b"\xef\xbb\xbfinput(type='text', disabled)\r\n";
        let parsed = tree(source);
        let root = parsed.root_node();
        assert!(!root.has_error());
        assert_eq!(root.start_byte(), 3);
        assert_eq!(root.end_byte(), source.len());
        let cancel = Cancellation::new();
        let mut cache = CaptureCache::new(source, root, 16_384, true);
        for role in [StructuralRole::Root, StructuralRole::Module] {
            let file = candidate(root, role, &cancel, &mut cache).unwrap().unwrap();
            assert_eq!(file.role, role);
            assert_eq!(file.start..file.end, 0..source.len());
        }
        for (range, name) in [
            (3..8, b"input".as_slice()),
            (9..13, b"type"),
            (22..30, b"disabled"),
        ] {
            let node = root
                .descendant_for_byte_range(range.start, range.end)
                .unwrap();
            let result = candidate(node, StructuralRole::Definition, &cancel, &mut cache)
                .unwrap()
                .unwrap();
            assert_eq!(result.role, StructuralRole::Definition);
            assert_eq!(&source[result.start..result.end], name);
            assert_eq!(result.start..result.end, range);
        }
        let name = root.descendant_for_byte_range(3, 8).unwrap();
        let mut partial = CaptureCache::new(source, root, 16_384, false);
        assert_eq!(
            candidate(name, StructuralRole::Definition, &cancel, &mut partial)
                .unwrap()
                .unwrap()
                .role,
            StructuralRole::Signature
        );
        let mut foreign = source.to_vec();
        foreign[..3].copy_from_slice(b"   ");
        let mut wrong_prefix = CaptureCache::new(&foreign, root, 16_384, true);
        assert_eq!(
            candidate(name, StructuralRole::Definition, &cancel, &mut wrong_prefix)
                .unwrap()
                .unwrap()
                .role,
            StructuralRole::Signature
        );
        let scope =
            tree_sitter_pug::ScannerScope::begin(Probe(Cancellation::new()), 1_000_000).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_pug::LANGUAGE.into())
            .unwrap();
        parser
            .set_included_ranges(&[tree_sitter::Range {
                start_byte: 3,
                end_byte: source.len(),
                start_point: tree_sitter::Point::new(0, 3),
                end_point: tree_sitter::Point::new(1, 0),
            }])
            .unwrap();
        let partial_tree = parser.parse(source, None).unwrap();
        drop(parser);
        scope.finish().unwrap();
        assert!(!partial_tree.root_node().has_error());
        let pack = super::super::QueryPack::compile(
            crate::GrammarFamily::Pug,
            include_str!("../../queries/pug.scm"),
        )
        .unwrap();
        let candidates = pack
            .extract_identity(
                crate::GrammarFamily::Pug,
                &partial_tree,
                source,
                16_384,
                &cancel,
            )
            .unwrap();
        let roots: Vec<_> = candidates
            .iter()
            .filter(|fact| fact.role == StructuralRole::Root)
            .collect();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].start..roots[0].end, 3..source.len());
        assert!(!candidates.iter().any(|fact| matches!(
            fact.role,
            StructuralRole::Module | StructuralRole::Declaration | StructuralRole::Definition
        )));
    }

    #[test]
    fn helper_work_and_cancellation_cannot_promote_a_declined_capture() {
        let source = include_bytes!("../../tests/fixtures/pug/hash.pug");
        let parsed = tree(source);
        let root = parsed.root_node();
        let name = root.descendant_for_byte_range(410, 415).unwrap();
        let mut tiny = CaptureCache::new(source, root, 0, true);
        assert!(
            candidate(
                name,
                StructuralRole::Definition,
                &Cancellation::new(),
                &mut tiny
            )
            .unwrap()
            .is_none()
        );
        assert!(tiny.exhausted());
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        let mut cache = CaptureCache::new(source, root, 16_384, true);
        assert!(matches!(
            candidate(name, StructuralRole::Definition, &cancel, &mut cache),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert!(!cache.exhausted());
    }

    #[test]
    fn helper_refusal_names_its_operation_without_scanner_or_capture_counts() {
        let event = super::super::IdentityRefusal::helper(super::super::RefusalOrigin::PugCaptures);
        assert!(super::super::IdentityRefusal::recognizes(&event.error()));
        assert!(event.matches_error(&event.error()));
        assert_eq!(
            event.details().origin(),
            "query-identity-pug-captures-limit"
        );
        assert_eq!(event.details().unit(), "helper-allowance");
        assert_eq!(
            (event.details().observed(), event.details().limit()),
            (None, None)
        );
        let scanner = AdapterError::ProviderFailed {
            code: rootlight_adapter_sdk::DiagnosticCode::new("pug-scanner-capacity").unwrap(),
        };
        assert!(!super::super::IdentityRefusal::recognizes(&scanner));
        assert!(!event.matches_error(&scanner));
    }
}
