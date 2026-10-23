//! Certify direct Eiffel formal receivers without resolving their called features.
//! A whole clean native census rejects aliases and ambiguous case-insensitive binders;
//! the original Reference remains alongside the mandatory endpoint certificate.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth};

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    clean: bool,
    initial: usize,
    remaining: usize,
    exhausted: bool,
    receivers: HashMap<(usize, usize), usize>,
}

impl<'source> CaptureCache<'source> {
    /// Census one complete clean source under the native query work allowance.
    pub(super) fn new(
        source: &'source [u8],
        root: Node<'_>,
        remaining: usize,
        complete: bool,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let clean = complete
            && root.kind() == "source_file"
            && !root.has_error()
            && !root.is_missing()
            && root.end_byte() == source.len()
            && matches!(source.get(..root.start_byte()), Some(b"" | b"\xef\xbb\xbf"));
        let mut result = Self {
            source,
            clean,
            initial: remaining,
            remaining,
            exhausted: false,
            receivers: HashMap::new(),
        };
        if clean {
            let mut cursor = root.walk();
            loop {
                if !result.step(cancel)? {
                    break;
                }
                let node = cursor.node();
                let feature = node.kind() == "feature_declaration";
                if feature {
                    result.feature(node, cancel)?;
                }
                if !feature && cursor.goto_first_child() {
                    continue;
                }
                while !cursor.goto_next_sibling() {
                    if !cursor.goto_parent() {
                        return Ok(result);
                    }
                }
            }
        }
        Ok(result)
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    pub(super) const fn initial_work(&self) -> usize {
        self.initial
    }

    pub(super) const fn remaining_work(&self) -> usize {
        self.remaining
    }

    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(next) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            self.receivers.clear();
            return Ok(false);
        };
        self.remaining = next;
        Ok(true)
    }

    fn direct<'tree>(
        &mut self,
        node: Node<'tree>,
        kind: &str,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        let mut found = None;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if child.kind() == kind && found.replace(child).is_some() {
                return Ok(None);
            }
        }
        Ok(found)
    }

    fn name(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<Vec<u8>>, AdapterError> {
        let Some(raw) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        if node.kind() != "identifier" || raw.is_empty() {
            return Ok(None);
        }
        let mut name = Vec::new();
        name.try_reserve_exact(raw.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for chunk in raw.chunks(64) {
            if !self.step(cancel)? {
                return Ok(None);
            }
            for byte in chunk {
                if !(byte.is_ascii_alphanumeric() || *byte == b'_') {
                    return Ok(None);
                }
                name.push(byte.to_ascii_lowercase());
            }
        }
        if name.first().is_none_or(u8::is_ascii_digit) {
            return Ok(None);
        }
        Ok(Some(name))
    }

    fn receiver(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        if !self.step(cancel)? || node.kind() != "identifier" {
            return Ok(false);
        }
        let Some(unqualified) = node.parent().filter(|parent| {
            parent.kind() == "unqualified_call" && parent.named_child_count() == 1
        }) else {
            return Ok(false);
        };
        let Some(inner) = unqualified
            .parent()
            .filter(|parent| parent.kind() == "call" && parent.named_child_count() == 1)
        else {
            return Ok(false);
        };
        let Some(outer) = inner
            .parent()
            .filter(|parent| parent.kind() == "call" && parent.named_child(0) == Some(inner))
        else {
            return Ok(false);
        };
        let Some(feature) = outer
            .named_child(1)
            .filter(|child| child.kind() == "unqualified_call")
        else {
            return Ok(false);
        };
        let Some(separator) = self.source.get(inner.end_byte()..feature.start_byte()) else {
            return Ok(false);
        };
        let mut dot = false;
        for chunk in separator.chunks(64) {
            if !self.step(cancel)? {
                return Ok(false);
            }
            for byte in chunk {
                if *byte == b'.' && !dot {
                    dot = true;
                } else if !byte.is_ascii_whitespace() {
                    return Ok(false);
                }
            }
        }
        Ok(dot)
    }

    fn feature(&mut self, feature: Node<'_>, cancel: &Cancellation) -> Result<(), AdapterError> {
        let Some(head) = self.direct(feature, "new_feature", cancel)? else {
            return Ok(());
        };
        let Some(extended) = self.direct(head, "extended_feature_name", cancel)? else {
            return Ok(());
        };
        if extended.named_child_count() != 1
            || extended
                .named_child(0)
                .is_none_or(|node| node.kind() != "identifier")
        {
            return Ok(());
        }
        let Some(formals) = self.direct(feature, "formal_arguments", cancel)? else {
            return Ok(());
        };
        let Some(routine) = self.direct(feature, "attribute_or_routine", cancel)? else {
            return Ok(());
        };
        let Some(body) = self.direct(routine, "feature_body", cancel)? else {
            return Ok(());
        };
        let Some(effective) = self.direct(body, "effective_routine", cancel)? else {
            return Ok(());
        };
        let Some(internal) = self.direct(effective, "internal", cancel)? else {
            return Ok(());
        };
        let mut binders: HashMap<Vec<u8>, Option<(usize, bool)>> = HashMap::new();
        let mut uses = Vec::new();
        let mut cursor = feature.walk();
        loop {
            if !self.step(cancel)? {
                return Ok(());
            }
            let node = cursor.node();
            if matches!(
                node.kind(),
                "inline_agent" | "iteration" | "object_test" | "separate"
            ) || node.is_missing()
                || node.is_error()
            {
                return Ok(());
            }
            if node.kind() == "identifier" {
                let group = node
                    .parent()
                    .filter(|parent| parent.kind() == "entity_declaration_group");
                if let Some(group) = group {
                    let owner = group.parent();
                    let formal = owner == Some(formals);
                    if !formal && owner.is_none_or(|owner| owner.kind() != "local_declarations") {
                        return Ok(());
                    }
                    let Some(name) = self.name(node, cancel)? else {
                        return Ok(());
                    };
                    binders
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    binders
                        .entry(name)
                        .and_modify(|old| *old = None)
                        .or_insert(Some((node.start_byte(), formal)));
                } else if internal.start_byte() <= node.start_byte()
                    && node.end_byte() <= internal.end_byte()
                    && self.receiver(node, cancel)?
                {
                    let Some(name) = self.name(node, cancel)? else {
                        return Ok(());
                    };
                    uses.try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    uses.push((node.byte_range(), name));
                }
            }
            if cursor.goto_first_child() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    for binding in binders.values() {
                        if !self.step(cancel)? || binding.is_none() {
                            return Ok(());
                        }
                    }
                    for (range, name) in uses {
                        if !self.step(cancel)? {
                            return Ok(());
                        }
                        if let Some(Some((target, true))) = binders.get(&name) {
                            self.receivers
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            self.receivers.insert((range.start, range.end), *target);
                        }
                    }
                    return Ok(());
                }
            }
        }
    }

    /// Supply only mandatory clean-file, endpoint, and original Reference captures.
    pub(super) fn candidate(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        cancel: &Cancellation,
    ) -> Result<Option<QueryCandidate>, AdapterError> {
        if !self.clean || !self.step(cancel)? || self.exhausted {
            return Ok(None);
        }
        let (start, end, syntax) =
            if role == StructuralRole::Signature && node.kind() == "source_file" {
                if self.receivers.is_empty() {
                    return Ok(None);
                }
                (0, self.source.len(), "eiffel.formal_receiver_clean_file")
            } else if matches!(role, StructuralRole::Reference | StructuralRole::Signature)
                && let Some(target) = self
                    .receivers
                    .get(&(node.start_byte(), node.end_byte()))
                    .copied()
            {
                if role == StructuralRole::Signature && node.kind() == "identifier" {
                    (target, node.end_byte(), "eiffel.formal_receiver_target")
                } else if role == StructuralRole::Reference && node.kind() == "unqualified_call" {
                    // Keep the existing wrapper depth: the same-span identifier
                    // would otherwise become a distinct fact before deduplication.
                    (node.start_byte(), node.end_byte(), "eiffel.identifier")
                } else {
                    return Ok(None);
                }
            } else {
                return Ok(None);
            };
        Ok(Some(QueryCandidate {
            start,
            end,
            role,
            syntax,
            required: true,
            native_depth: native_depth(node, cancel)?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn parse(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_eiffel::LANGUAGE_EIFFEL.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn targets(source: &str, complete: bool, maximum: usize) -> (Vec<(usize, usize, usize)>, bool) {
        let tree = parse(source);
        let cache = CaptureCache::new(
            source.as_bytes(),
            tree.root_node(),
            maximum,
            complete,
            &Cancellation::new(),
        )
        .unwrap();
        let mut targets = cache
            .receivers
            .iter()
            .map(|(&(start, end), &target)| (start, end, target))
            .collect::<Vec<_>>();
        targets.sort_unstable();
        (targets, cache.exhausted())
    }

    #[test]
    fn native_eiffel_formal_receiver_is_case_insensitive_and_feature_local() {
        for source in [
            "class C feature run (value: ANY) do value.use end end\n",
            "cLaSs C fEaTuRe run (VaLuE: ANY) do vAlUe.use end end\n",
            "class C feature run (value: ANY) local other: ANY do value.use end end\n",
            "class C feature run (value: ANY) do from until True loop value.use end end end\n",
            "class C feature run (value: ANY) do value.use end other (value: ANY) do end end\n",
        ] {
            let (proofs, exhausted) = targets(source, true, 16_384);
            assert!(!exhausted, "{source}");
            assert_eq!(proofs.len(), 1, "{source}");
            let (start, end, target) = proofs[0];
            assert_eq!(
                source.get(start..end).unwrap().to_ascii_lowercase(),
                "value"
            );
            assert_eq!(
                source.get(target..target + 5).unwrap().to_ascii_lowercase(),
                "value"
            );
            assert!(target < start);
        }
    }

    #[test]
    fn unsupported_eiffel_binders_and_non_receiver_spelling_withhold_certificates() {
        for source in [
            "class C feature run (value, VALUE: ANY) do value.use end end\n",
            "class C feature run (value: ANY) local VALUE: ANY do value.use end end\n",
            "class C feature run (value: ANY) do consume (agent do value.use end) end end\n",
            "class C feature run (value: ANY) do across items as item loop value.use end end end\n",
            "class C feature run (value: ANY) do if attached other as held then value.use end end end\n",
            "class C feature run (value: ANY) do separate other as held do value.use end end end\n",
            "class C feature run (value: ANY) do value.use ( end end\n",
            "class C feature run do value.use end end\n",
            "class C feature run (value: ANY) do consume (value) end end\n",
            "class C feature run (value: ANY) do Current.value.use end end\n",
            "class C feature run (value: ANY) do value (1).use end end\n",
            "class C feature run (value: ANY) do (value).use end end\n",
            "class C feature run (value: ANY) do consume (\"value.use\") -- value.use\nend end\n",
        ] {
            assert!(targets(source, true, 16_384).0.is_empty(), "{source}");
        }
        assert!(
            targets(
                "class C feature run (value: ANY) do value.use end end\n",
                false,
                16_384
            )
            .0
            .is_empty()
        );
    }

    #[test]
    fn eiffel_native_work_and_cancellation_precede_any_retained_proof() {
        let source = "class C feature run (value: ANY) do value.use end end\n";
        for maximum in 0..8 {
            let (proofs, exhausted) = targets(source, true, maximum);
            assert!(proofs.is_empty());
            assert!(exhausted);
        }
        let tree = parse(source);
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            CaptureCache::new(source.as_bytes(), tree.root_node(), 16_384, true, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
        let mut cache = CaptureCache::new(
            source.as_bytes(),
            tree.root_node(),
            16_384,
            true,
            &Cancellation::new(),
        )
        .unwrap();
        assert!(matches!(
            cache.candidate(tree.root_node(), StructuralRole::Signature, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn eiffel_native_work_refusal_drops_certificates_from_earlier_features() {
        let source = "class C feature first (value: ANY) do value.use end second (value: ANY) do value.use end end\n";
        assert_eq!(targets(source, true, 16_384).0.len(), 2);
        for maximum in 0..512 {
            let (proofs, exhausted) = targets(source, true, maximum);
            if exhausted {
                assert!(proofs.is_empty(), "maximum={maximum}");
            } else {
                assert_eq!(proofs.len(), 2, "maximum={maximum}");
            }
        }
    }

    #[test]
    fn eiffel_certified_reference_keeps_one_original_wrapper_in_both_partitions() {
        use crate::{
            GrammarFamily,
            query_pack::{QueryPack, eiffel},
        };

        let source = "class C feature run (value: ANY) do value.use consume (unknown) end end\n";
        let tree = parse(source);
        let start = source.rfind("value").unwrap();
        let identifier = tree
            .root_node()
            .descendant_for_byte_range(start, start + 5)
            .unwrap();
        assert_eq!(identifier.kind(), "identifier");
        let wrapper = identifier.parent().unwrap();
        assert_eq!(wrapper.kind(), "unqualified_call");
        let cancel = Cancellation::new();
        let mut cache =
            CaptureCache::new(source.as_bytes(), tree.root_node(), 16_384, true, &cancel).unwrap();
        assert!(
            cache
                .candidate(identifier, StructuralRole::Reference, &cancel)
                .unwrap()
                .is_none()
        );
        let certified = cache
            .candidate(wrapper, StructuralRole::Reference, &cancel)
            .unwrap()
            .unwrap();
        let mut ordinary_cache = eiffel::CaptureCache::new(16_384);
        let ordinary = eiffel::candidate(
            wrapper,
            StructuralRole::Reference,
            source.as_bytes(),
            &cancel,
            &mut ordinary_cache,
        )
        .unwrap()
        .unwrap();
        assert!(certified.required);
        assert!(!ordinary.required);
        assert!(certified.same_fact(ordinary));
        assert_eq!(
            cache
                .candidate(identifier, StructuralRole::Signature, &cancel)
                .unwrap()
                .unwrap()
                .syntax,
            "eiffel.formal_receiver_target"
        );
        let pack = QueryPack::compile(
            GrammarFamily::Eiffel,
            include_str!("../../queries/eiffel.scm"),
        )
        .unwrap();
        let identity = pack
            .extract_identity(
                GrammarFamily::Eiffel,
                &tree,
                source.as_bytes(),
                16_384,
                &cancel,
            )
            .unwrap();
        let references = identity
            .iter()
            .filter(|candidate| candidate.role == StructuralRole::Reference)
            .collect::<Vec<_>>();
        assert_eq!(
            references.len(),
            1,
            "only the certified receiver is mandatory"
        );
        assert!(references[0].same_fact(ordinary));
        assert!(references[0].required);
        let extraction = pack
            .extract(
                GrammarFamily::Eiffel,
                &tree,
                source.as_bytes(),
                16_384,
                16_384,
                &cancel,
            )
            .unwrap();
        let references = extraction
            .candidates
            .iter()
            .filter(|candidate| {
                candidate.role == StructuralRole::Reference
                    && candidate.start == start
                    && candidate.end == start + 5
            })
            .collect::<Vec<_>>();
        assert!(!references.is_empty());
        assert!(
            references
                .iter()
                .all(|candidate| candidate.same_fact(ordinary)),
            "both partitions must deduplicate to the original wrapper fact"
        );
    }
}
