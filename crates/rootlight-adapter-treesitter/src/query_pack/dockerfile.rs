//! Certify written Docker stages from a complete, clean native instruction list.
//! COPY flags have invisible value tokens in this grammar. Their exact literal
//! spans therefore require clean native param, operator and instruction owners.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{HARD_MAX_QUERY_CAPTURES, QueryCandidate, StructuralRole};

struct Stage {
    extent: Range<usize>,
    header: Range<usize>,
    alias: Option<Range<usize>>,
}

struct EnvPair {
    extent: Range<usize>,
    name: Range<usize>,
}

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete: bool,
    initialized: bool,
    root: Option<usize>,
    remaining: usize,
    exhausted: bool,
    stages: HashMap<usize, Stage>,
    env: HashMap<usize, EnvPair>,
    env_instructions: HashSet<usize>,
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_error()
        && !node.is_missing()
        && !node.has_error()
        && source.get(node.byte_range()).is_some()
}

fn ordinary_alias(bytes: &[u8]) -> bool {
    bytes.split_first().is_some_and(|(first, rest)| {
        first.is_ascii_lowercase()
            && rest.iter().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.-".contains(byte)
            })
    })
}

impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], maximum: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            initialized: false,
            root: None,
            remaining: maximum.min(HARD_MAX_QUERY_CAPTURES).saturating_mul(32),
            exhausted: false,
            stages: HashMap::new(),
            env: HashMap::new(),
            env_instructions: HashSet::new(),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn visit(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(remaining) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = remaining;
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
        let mut children = Vec::new();
        children
            .try_reserve_exact(node.child_count())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(Vec::new());
            }
            children.push(child);
        }
        Ok(children)
    }

    fn literal(
        &mut self,
        range: Range<usize>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let Some(bytes) = self.source.get(range) else {
            return Ok(false);
        };
        for _ in bytes.chunks(64) {
            if !self.visit(cancel)? {
                return Ok(false);
            }
        }
        Ok(ordinary_alias(bytes))
    }

    fn initialize(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<(), AdapterError> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;
        let mut root = node;
        while let Some(parent) = root.parent() {
            if !self.visit(cancel)? {
                return Ok(());
            }
            root = parent;
        }
        if !self.complete
            || root.kind() != "source_file"
            || !(root.start_byte() == 0
                || (root.start_byte() == 3 && self.source.starts_with(b"\xef\xbb\xbf")))
            || root.end_byte() != self.source.len()
            || !clean(root, self.source)
        {
            return Ok(());
        }
        let children = self.children(root, cancel)?;
        let mut boundaries = Vec::new();
        boundaries
            .try_reserve_exact(children.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for child in children.iter().copied() {
            if !self.visit(cancel)? {
                return Ok(());
            }
            if child.kind() == "from_instruction" {
                boundaries.push(child);
            }
        }
        self.stages
            .try_reserve(boundaries.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (index, header) in boundaries.iter().copied().enumerate() {
            if !self.visit(cancel)? {
                return Ok(());
            }
            let alias = header.child_by_field_name("as");
            let alias = if let Some(alias) = alias {
                (alias.kind() == "image_alias"
                    && clean(alias, self.source)
                    && self.literal(alias.byte_range(), cancel)?)
                .then(|| alias.byte_range())
            } else {
                None
            };
            let end = boundaries
                .get(index + 1)
                .map_or(self.source.len(), Node::start_byte);
            self.stages.insert(
                header.id(),
                Stage {
                    extent: header.start_byte()..end,
                    header: header.byte_range(),
                    alias,
                },
            );
        }
        for instruction in children {
            if !self.visit(cancel)? {
                return Ok(());
            }
            if instruction.kind() != "env_instruction" || !clean(instruction, self.source) {
                continue;
            }
            for pair in self.children(instruction, cancel)? {
                if pair.kind() != "env_pair" || !clean(pair, self.source) {
                    continue;
                }
                let Some(name) = pair.child_by_field_name("name") else {
                    continue;
                };
                if name.kind() != "unquoted_string"
                    || !clean(name, self.source)
                    || name.start_byte() != pair.start_byte()
                    || name.child_count() != 0
                    || !self.env_name(name.byte_range(), cancel)?
                {
                    continue;
                }
                let children = self.children(pair, cancel)?;
                if children
                    .iter()
                    .filter(|child| {
                        child.kind() == "="
                            && child.start_byte() == name.end_byte()
                            && child.end_byte() == name.end_byte() + 1
                    })
                    .count()
                    != 1
                {
                    continue;
                }
                self.env
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                self.env_instructions
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                self.env.insert(
                    pair.id(),
                    EnvPair {
                        extent: pair.byte_range(),
                        name: name.byte_range(),
                    },
                );
                self.env_instructions.insert(instruction.id());
            }
        }
        if !self.exhausted {
            self.root = Some(root.id());
        }
        Ok(())
    }

    fn env_name(
        &mut self,
        range: Range<usize>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let Some(bytes) = self.source.get(range) else {
            return Ok(false);
        };
        let Some(first) = bytes.first() else {
            return Ok(false);
        };
        if !first.is_ascii_alphabetic() && *first != b'_' {
            return Ok(false);
        }
        for chunk in bytes.chunks(64) {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            if !chunk
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn copy_literal(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<Range<usize>>, AdapterError> {
        if node.kind() != "copy_instruction"
            || !clean(node, self.source)
            || node.parent().map(|parent| parent.id()) != self.root
        {
            return Ok(None);
        }
        let mut result = None;
        for param in self.children(node, cancel)? {
            if param.kind() != "param" {
                continue;
            }
            let Some(bytes) = self.source.get(param.byte_range()) else {
                return Ok(None);
            };
            if !bytes.starts_with(b"--from=") {
                continue;
            }
            if result.is_some() || !clean(param, self.source) {
                return Ok(None);
            }
            let children = self.children(param, cancel)?;
            let [dash, equal] = children.as_slice() else {
                return Ok(None);
            };
            if dash.kind() != "--"
                || equal.kind() != "="
                || dash.start_byte() != param.start_byte()
                || dash.end_byte() != param.start_byte() + 2
                || equal.start_byte() != param.start_byte() + 6
                || equal.end_byte() != param.start_byte() + 7
            {
                return Ok(None);
            }
            let value = equal.end_byte()..param.end_byte();
            if !self.literal(value.clone(), cancel)? {
                return Ok(None);
            }
            result = Some(value);
        }
        Ok(result)
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
    cache.initialize(node, cancel)?;
    let mut range = node.byte_range();
    let mut selected_role = role;
    let (syntax, depth) = match role {
        StructuralRole::Root if node.kind() == "source_file" => ("dockerfile.source_file", 0),
        StructuralRole::Declaration if node.kind() == "from_instruction" => {
            let Some(stage) = cache
                .stages
                .get(&node.id())
                .filter(|stage| stage.alias.is_some())
            else {
                return Ok(None);
            };
            range = stage.extent.clone();
            ("dockerfile.stage", 1)
        }
        StructuralRole::Definition if node.kind() == "image_alias" => {
            let Some(stage) = node
                .parent()
                .and_then(|owner| cache.stages.get(&owner.id()))
            else {
                return Ok(None);
            };
            if stage.alias.as_ref() == Some(&range) {
                ("dockerfile.stage_alias", 2)
            } else {
                selected_role = StructuralRole::Signature;
                ("dockerfile.unproved_stage_alias", 2)
            }
        }
        StructuralRole::Signature if node.kind() == "from_instruction" => {
            let Some(stage) = cache.stages.get(&node.id()) else {
                return Ok(None);
            };
            range = stage.header.clone();
            ("dockerfile.stage_header", 2)
        }
        StructuralRole::Reference | StructuralRole::Signature
            if node.kind() == "copy_instruction" =>
        {
            match cache.copy_literal(node, cancel)? {
                Some(value) if role == StructuralRole::Reference => {
                    range = value;
                    ("dockerfile.copy_from", 2)
                }
                Some(_) => ("dockerfile.copy_from_context", 2),
                None if role == StructuralRole::Signature => ("dockerfile.unproved_copy", 2),
                None => return Ok(None),
            }
        }
        StructuralRole::Declaration if node.kind() == "env_pair" => {
            let Some(pair) = cache.env.get(&node.id()).filter(|_| cache.root.is_some()) else {
                return Ok(None);
            };
            range = pair.extent.clone();
            ("dockerfile.env", 2)
        }
        StructuralRole::Definition if node.kind() == "unquoted_string" => {
            let Some(pair) = node
                .parent()
                .and_then(|node| cache.env.get(&node.id()))
                .filter(|_| cache.root.is_some())
            else {
                return Ok(None);
            };
            if range != pair.name {
                return Ok(None);
            }
            ("dockerfile.env_name", 3)
        }
        StructuralRole::Signature if node.kind() == "env_pair" => {
            if cache.root.is_none() || !cache.env.contains_key(&node.id()) {
                return Ok(None);
            }
            ("dockerfile.env_pair_context", 3)
        }
        StructuralRole::Signature if node.kind() == "env_instruction" => {
            if cache.root.is_none() {
                return Ok(None);
            }
            if cache.env_instructions.contains(&node.id()) {
                ("dockerfile.env_instruction", 2)
            } else {
                ("dockerfile.unproved_env", 2)
            }
        }
        StructuralRole::Signature if node.is_error() && !node.is_missing() => {
            ("dockerfile.recovered_region", 1)
        }
        StructuralRole::Comment => ("dockerfile.comment", 1),
        _ => return Ok(None),
    };
    if cache.exhausted {
        return Ok(None);
    }
    let mut candidate = QueryCandidate {
        start: range.start,
        end: range.end,
        role: selected_role,
        syntax,
        required: selected_role.belongs_to_identity_closure() || syntax == "dockerfile.copy_from",
        native_depth: depth,
    };
    super::canonicalize_file_root_preamble(&mut candidate, node, cache.source, cache.complete);
    Ok(Some(candidate))
}

#[cfg(test)]
mod bom_tests {
    use super::*;
    use crate::{GrammarFamily, query_pack::QueryPack};

    const SOURCE: &[u8] =
        b"\xef\xbb\xbfFROM scratch AS first\nFROM scratch AS second\nCOPY --from=first /in /out\n";

    fn parsed(partial: bool) -> tree_sitter::Tree {
        let scope = tree_sitter_dockerfile::ScannerScope::begin((), |_| false).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_dockerfile::LANGUAGE.into())
            .unwrap();
        if partial {
            parser
                .set_included_ranges(&[tree_sitter::Range {
                    start_byte: 3,
                    end_byte: SOURCE.len(),
                    start_point: tree_sitter::Point::new(0, 3),
                    end_point: tree_sitter::Point::new(3, 0),
                }])
                .unwrap();
        }
        let tree = parser.parse(SOURCE, None).unwrap();
        drop(parser);
        scope.finish().unwrap();
        assert!(!tree.root_node().has_error());
        assert!(!tree.root_node().is_missing());
        assert_eq!(tree.root_node().start_byte(), 3);
        assert_eq!(tree.root_node().end_byte(), SOURCE.len());
        tree
    }

    #[test]
    fn complete_bom_stage_proofs_keep_native_children_and_full_file_root() {
        let tree = parsed(false);
        let pack = QueryPack::compile(
            GrammarFamily::Dockerfile,
            include_str!("../../queries/dockerfile.scm"),
        )
        .unwrap();
        let candidates = pack
            .extract_identity(
                GrammarFamily::Dockerfile,
                &tree,
                SOURCE,
                16_384,
                &Cancellation::new(),
            )
            .unwrap();
        let roots: Vec<_> = candidates
            .iter()
            .filter(|fact| fact.role == StructuralRole::Root)
            .collect();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].start..roots[0].end, 0..SOURCE.len());
        assert!(roots[0].required);
        let definitions: Vec<_> = candidates
            .iter()
            .filter(|fact| fact.role == StructuralRole::Definition)
            .map(|fact| {
                assert!(fact.start >= 3);
                &SOURCE[fact.start..fact.end]
            })
            .collect();
        assert_eq!(definitions, vec![b"first".as_slice(), b"second".as_slice()]);
        let copies: Vec<_> = candidates
            .iter()
            .filter(|fact| fact.syntax == "dockerfile.copy_from")
            .collect();
        assert_eq!(copies.len(), 1);
        assert_eq!(&SOURCE[copies[0].start..copies[0].end], b"first");
        assert!(copies[0].required);
        for fact in candidates
            .iter()
            .filter(|fact| fact.role != StructuralRole::Root)
        {
            assert!(fact.start >= 3, "a child cannot absorb the file preamble");
        }
    }

    #[test]
    fn partial_bom_ranges_keep_native_root_and_decline_stage_proofs() {
        let tree = parsed(true);
        let pack = QueryPack::compile(
            GrammarFamily::Dockerfile,
            include_str!("../../queries/dockerfile.scm"),
        )
        .unwrap();
        let cancel = Cancellation::new();
        let candidates = pack
            .extract_identity(GrammarFamily::Dockerfile, &tree, SOURCE, 16_384, &cancel)
            .unwrap();
        let roots: Vec<_> = candidates
            .iter()
            .filter(|fact| fact.role == StructuralRole::Root)
            .collect();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].start..roots[0].end, 3..SOURCE.len());
        assert!(!candidates.iter().any(|fact| matches!(
            fact.role,
            StructuralRole::Declaration | StructuralRole::Definition
        ) || fact.syntax == "dockerfile.copy_from"));
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            pack.extract_identity(GrammarFamily::Dockerfile, &tree, SOURCE, 16_384, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn a_native_three_byte_prefix_requires_the_exact_bom_bytes() {
        let tree = parsed(false);
        let mut foreign = SOURCE.to_vec();
        foreign[..3].copy_from_slice(b"   ");
        let mut cache = CaptureCache::new(&foreign, 16_384, true);
        let cancel = Cancellation::new();
        let root = candidate(tree.root_node(), StructuralRole::Root, &cancel, &mut cache)
            .unwrap()
            .unwrap();
        assert_eq!(root.start..root.end, 3..SOURCE.len());
        assert!(cache.stages.is_empty());
        assert!(cache.root.is_none());
        let mut cursor = tree.root_node().walk();
        let header = tree
            .root_node()
            .named_children(&mut cursor)
            .find(|node| node.kind() == "from_instruction")
            .unwrap();
        let alias = header.child_by_field_name("as").unwrap();
        assert!(
            candidate(alias, StructuralRole::Definition, &cancel, &mut cache)
                .unwrap()
                .is_none()
        );
    }
}

#[cfg(test)]
mod env_tests {
    use super::*;
    #[test]
    fn literal_env_fields_keep_required_identity_and_complete_native_owner() {
        let source = b"FROM scratch\nENV Alpha=one Beta2=\"two words\"\n";
        let scope = tree_sitter_dockerfile::ScannerScope::begin((), |_| false).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_dockerfile::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let pack = super::super::QueryPack::compile(
            crate::GrammarFamily::Dockerfile,
            include_str!("../../queries/dockerfile.scm"),
        )
        .unwrap();
        let facts = pack
            .extract_identity(
                crate::GrammarFamily::Dockerfile,
                &tree,
                source,
                16384,
                &Cancellation::new(),
            )
            .unwrap();
        let names: Vec<_> = facts
            .iter()
            .filter(|fact| fact.syntax == "dockerfile.env_name")
            .map(|fact| {
                assert!(fact.required);
                &source[fact.start..fact.end]
            })
            .collect();
        assert_eq!(names, vec![b"Alpha".as_slice(), b"Beta2".as_slice()]);
        assert_eq!(
            facts
                .iter()
                .filter(|fact| fact.syntax == "dockerfile.env_pair_context" && fact.required)
                .count(),
            2
        );
        assert_eq!(
            facts
                .iter()
                .filter(|fact| fact.syntax == "dockerfile.env_instruction" && fact.required)
                .count(),
            1
        );
        drop(parser);
        scope.finish().unwrap();
    }
    #[test]
    fn partial_recovered_cancelled_and_exhausted_env_proofs_fail_closed() {
        let source = b"FROM scratch\nENV Alpha=one\n";
        let scope = tree_sitter_dockerfile::ScannerScope::begin((), |_| false).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_dockerfile::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let cancel = Cancellation::new();
        let mut cursor = tree.root_node().walk();
        let instruction = tree
            .root_node()
            .named_children(&mut cursor)
            .find(|node| node.kind() == "env_instruction")
            .unwrap();
        let mut cursor = instruction.walk();
        let pair = instruction
            .named_children(&mut cursor)
            .find(|node| node.kind() == "env_pair")
            .unwrap();
        let mut cache = CaptureCache::new(source, 16384, false);
        assert!(
            candidate(pair, StructuralRole::Declaration, &cancel, &mut cache)
                .unwrap()
                .is_none()
        );
        let mut cache = CaptureCache::new(source, 0, true);
        assert!(
            candidate(pair, StructuralRole::Declaration, &cancel, &mut cache)
                .unwrap()
                .is_none()
        );
        assert!(cache.exhausted());
        let mut cache = CaptureCache::new(source, 16384, true);
        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            candidate(pair, StructuralRole::Declaration, &cancelled, &mut cache),
            Err(AdapterError::Cancelled { .. })
        ));
        let recovered = parser
            .parse(b"FROM scratch\nENV Alpha=one\n!\n", None)
            .unwrap();
        assert!(recovered.root_node().has_error());
        let mut cache = CaptureCache::new(b"FROM scratch\nENV Alpha=one\n!\n", 16384, true);
        let mut cursor = recovered.root_node().walk();
        for node in recovered
            .root_node()
            .named_children(&mut cursor)
            .filter(|node| node.kind() == "env_instruction")
        {
            assert!(
                candidate(node, StructuralRole::Signature, &cancel, &mut cache)
                    .unwrap()
                    .is_none()
            );
        }
        drop(parser);
        scope.finish().unwrap();
    }
}
