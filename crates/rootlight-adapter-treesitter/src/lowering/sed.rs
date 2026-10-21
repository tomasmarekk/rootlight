//! Resolve native Sed branch operands to unique written labels in one source.
//! A separate direct-sibling certificate can justify a written-section dependency;
//! it supplies no semantic enclosure, control-flow or runtime reachability proof.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId};
use rootlight_ir::{
    ContainerRef, EntityKind, FactDomain, FactRef, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, RelationEndpoint, RelationRecord, SourceRef, SourceSpan,
};

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    kind == EntityKind::StatementLabel && label == "sed.statement_label.declaration"
}

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            "sed.written_branch.reference" | "sed.unproved_branch.reference"
        )
}

pub(super) fn metadata(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature && fact.syntax_kind().as_str().starts_with("sed.")
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (
            SyntaxFactKind::Occurrence,
            "sed.written_branch.reference" | "sed.unproved_branch.reference",
        ) => Some(OccurrenceRole::Reference),
        (SyntaxFactKind::Comment, "sed.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "sed.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}

pub(super) fn reference_gap(fact: &SyntaxFact) -> Option<&'static str> {
    required(fact).then_some("sed-written-branch-unique-local-label-target-unavailable")
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let detail = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Root, "sed.script.root") => Some(
            "sed-static-bre-file-text-labels-only-command-execution-control-flow-and-raw-carriage-return-runtime-semantics-unqualified",
        ),
        (SyntaxFactKind::Root, "sed.native_context_unavailable.root")
        | (SyntaxFactKind::Signature, "sed.native_context_unavailable.signature") => {
            Some("sed-complete-clean-native-source-context-unavailable")
        }
        (SyntaxFactKind::Signature, "sed.label_unavailable.signature") => {
            Some("sed-plain-ascii-written-label-surface-unavailable")
        }
        (SyntaxFactKind::Signature, "sed.branch_unavailable.signature") => {
            Some("sed-explicit-written-branch-label-operand-unavailable")
        }
        (SyntaxFactKind::Signature, "sed.section_unavailable.signature") => {
            Some("sed-direct-sibling-written-section-dependency-unavailable")
        }
        _ => None,
    };
    [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ]
    .into_iter()
    .filter_map(move |domain| detail.map(|detail| (domain, detail)))
}

fn unique<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|old| *old = None)
        .or_insert(Some(value));
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    left.repository() == right.repository()
        && left.generation() == right.generation()
        && left.content_hash() == right.content_hash()
        && left.span().file() == right.span().file()
}

fn plain_name_digest(
    text: &str,
    cancel: &Cancellation,
) -> Result<Option<ContentHash>, AdapterError> {
    cancel.check()?;
    if !text
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
    {
        return Ok(None);
    }
    let mut digest = blake3::Hasher::new();
    for chunk in text.as_bytes().chunks(64) {
        cancel.check()?;
        if !chunk
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Ok(None);
        }
        digest.update(chunk);
    }
    Ok(Some(ContentHash::from_bytes(*digest.finalize().as_bytes())))
}

fn same_name(left: &str, right: &str, cancel: &Cancellation) -> Result<bool, AdapterError> {
    cancel.check()?;
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left.as_bytes().chunks(64).zip(right.as_bytes().chunks(64)) {
        cancel.check()?;
        if left != right {
            return Ok(false);
        }
    }
    Ok(true)
}

fn label_prefix(
    text: &str,
    name_bytes: usize,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let Some(prefix_end) = text.len().checked_sub(name_bytes) else {
        return Ok(false);
    };
    let Some(prefix) = text
        .as_bytes()
        .get(..prefix_end)
        .and_then(|raw| raw.strip_prefix(b":"))
    else {
        return Ok(false);
    };
    for chunk in prefix.chunks(64) {
        cancel.check()?;
        if !chunk.iter().all(|byte| matches!(byte, b' ' | b'\t')) {
            return Ok(false);
        }
    }
    Ok(true)
}

struct Binding {
    target: SymbolId,
    owner: Option<SymbolId>,
    source: SourceSpan,
    fact: SourceSpan,
    provenance: FactId,
    spelling: ContentHash,
}

pub(super) struct BranchTargets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

impl BranchTargets {
    pub(super) fn new<'source>(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        full_source: &SourceRef,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        mut text: impl FnMut(SourceSpan) -> Result<&'source str, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut result = Self {
            source: full_source.clone(),
            bindings: HashMap::new(),
        };
        let mut roots = Vec::new();
        let mut modules = Vec::new();
        let mut declarations = HashMap::new();
        let mut definitions = HashMap::new();
        let mut references = HashMap::new();
        let mut markers = HashMap::new();
        let mut opaque = false;
        let mut local_ids = HashSet::new();
        local_ids
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for map in [&mut declarations, &mut references, &mut markers] {
            map.try_reserve(facts.len())
                .map_err(|_| SinkError::AllocationFailed)?;
        }
        definitions
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        roots
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        modules
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancel.check()?;
            if fact.span().file() != full_source.span().file() || !local_ids.insert(fact.local_id())
            {
                return Ok(result);
            }
            match (fact.kind(), fact.syntax_kind().as_str()) {
                (SyntaxFactKind::Root, "sed.script.root") => {
                    if !roots.is_empty() {
                        return Ok(result);
                    }
                    roots.push(fact);
                }
                (SyntaxFactKind::Module, "sed.file.module") => {
                    if !modules.is_empty() {
                        return Ok(result);
                    }
                    modules.push(fact);
                }
                (SyntaxFactKind::Declaration, "sed.statement_label.declaration") => {
                    unique(&mut declarations, fact.span().start_byte(), fact)
                }
                (SyntaxFactKind::Occurrence, "sed.label_name.definition") => {
                    let Some(parent) = fact.parent() else {
                        return Ok(result);
                    };
                    unique(&mut definitions, parent, fact);
                }
                (SyntaxFactKind::Occurrence, "sed.written_branch.reference") => {
                    unique(&mut references, fact.span().end_byte(), fact)
                }
                (SyntaxFactKind::Signature, "sed.written_section_target.signature") => {
                    unique(&mut markers, fact.span().end_byte(), fact)
                }
                (
                    _,
                    "sed.label_unavailable.signature"
                    | "sed.native_context_unavailable.signature"
                    | "sed.native_context_unavailable.root",
                ) => opaque = true,
                (SyntaxFactKind::Occurrence, "sed.unproved_branch.reference")
                | (SyntaxFactKind::Signature, "sed.branch_unavailable.signature")
                | (SyntaxFactKind::Signature, "sed.section_unavailable.signature")
                | (SyntaxFactKind::Comment, "sed.comment.comment")
                | (SyntaxFactKind::StringLiteral, "sed.string.string") => {}
                _ => opaque = true,
            }
        }
        let ([root], [module]) = (roots.as_slice(), modules.as_slice()) else {
            return Ok(result);
        };
        if opaque
            || definitions.len() != declarations.len()
            || root.parent().is_some()
            || module.parent() != Some(root.local_id())
            || physical(root.span())? != full_source.span()
            || physical(module.span())? != full_source.span()
        {
            return Ok(result);
        }
        for fact in facts {
            cancel.check()?;
            if (matches!(
                fact.kind(),
                SyntaxFactKind::Declaration | SyntaxFactKind::Signature
            ) || (fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() != "sed.label_name.definition"))
                && fact.parent() != Some(module.local_id())
            {
                return Ok(result);
            }
        }
        let Some(module_entity) = entities.get(&module.local_id()) else {
            return Ok(result);
        };
        if module_entity.record.kind != EntityKind::Module
            || module_entity.record.language != "sed"
            || module_entity.record.repository != full_source.repository()
            || module_entity.record.generation != full_source.generation()
            || module_entity.direct_parent != ContainerRef::File(full_source.span().file())
            || module_entity.record.container != Some(module_entity.direct_parent)
            || module_entity.definition_local_id.is_some()
        {
            return Ok(result);
        }
        let Some(module_source) = module_entity.record.evidence.source.as_ref() else {
            return Ok(result);
        };
        if !same_source(module_source, full_source) || module_source.span() != full_source.span() {
            return Ok(result);
        }
        let mut names = HashMap::new();
        let mut labels = HashMap::new();
        let mut label_order = Vec::new();
        names
            .try_reserve(declarations.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        labels
            .try_reserve(declarations.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        label_order
            .try_reserve_exact(declarations.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for declaration in declarations.values().copied() {
            cancel.check()?;
            let Some(declaration) = declaration else {
                return Ok(result);
            };
            let Some(definition) = definitions.get(&declaration.local_id()).copied().flatten()
            else {
                return Ok(result);
            };
            let Some(entity) = entities.get(&declaration.local_id()) else {
                return Ok(result);
            };
            let Some(source) = entity.record.evidence.source.as_ref() else {
                return Ok(result);
            };
            let spelling = text(definition.span())?;
            let declaration_text = text(declaration.span())?;
            let Some(name_digest) = plain_name_digest(spelling, cancel)? else {
                return Ok(result);
            };
            if entity.record.language != "sed"
                || entity.record.kind != EntityKind::StatementLabel
                || entity.record.repository != full_source.repository()
                || entity.record.generation != full_source.generation()
                || entity.record.provenance != module_entity.record.provenance
                || entity.direct_parent != ContainerRef::Entity(module_entity.record.id)
                || entity.record.container != Some(entity.direct_parent)
                || entity.definition_local_id != Some(definition.local_id())
                || !same_source(source, full_source)
                || source.span() != physical(declaration.span())?
                || definition.span().start_byte() < declaration.span().start_byte()
                || definition.span().end_byte() != declaration.span().end_byte()
                || !same_name(spelling, &entity.record.canonical_name, cancel)?
                || !label_prefix(declaration_text, spelling.len(), cancel)?
            {
                return Ok(result);
            }
            unique(&mut names, name_digest, entity);
            labels.insert(declaration.span().start_byte(), (declaration, entity));
            label_order.push(declaration.span().start_byte());
        }
        crate::runtime::sort_cancellable_by(&mut label_order, cancel, Ord::cmp)?;
        result
            .bindings
            .try_reserve(references.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for reference in references.values().copied().flatten() {
            cancel.check()?;
            let spelling = text(reference.span())?;
            let Some(name_digest) = plain_name_digest(spelling, cancel)? else {
                continue;
            };
            let Some(target) = names.get(&name_digest).copied().flatten() else {
                continue;
            };
            if !same_name(spelling, &target.record.canonical_name, cancel)? {
                continue;
            }
            let owner = if let Some(marker) =
                markers.get(&reference.span().end_byte()).copied().flatten()
            {
                let candidate = labels.get(&marker.span().start_byte());
                if let Some((declaration, entity)) = candidate {
                    let before =
                        label_order.partition_point(|start| *start < reference.span().start_byte());
                    let nearest = before
                        .checked_sub(1)
                        .and_then(|index| label_order.get(index))
                        .copied()
                        == Some(declaration.span().start_byte());
                    (nearest
                        && declaration.span().end_byte() < reference.span().start_byte()
                        && marker.span().end_byte() == reference.span().end_byte())
                    .then_some(entity.record.id)
                } else {
                    None
                }
            } else {
                None
            };
            result.bindings.insert(
                reference.local_id(),
                Binding {
                    target: target.record.id,
                    owner,
                    source: physical(reference.span())?,
                    fact: reference.span(),
                    provenance: target.record.provenance,
                    spelling: name_digest,
                },
            );
        }
        cancel.check()?;
        Ok(result)
    }

    fn binding(&self, fact: &SyntaxFact, site: &OccurrenceRecord) -> Option<&Binding> {
        if fact.kind() != SyntaxFactKind::Occurrence
            || fact.syntax_kind().as_str() != "sed.written_branch.reference"
            || site.syntax_kind != "sed.written_branch.reference"
            || site.role != OccurrenceRole::Reference
            || !same_source(&site.source, &self.source)
            || site.repository != self.source.repository()
            || site.generation != self.source.generation()
            || site.file != self.source.span().file()
        {
            return None;
        }
        self.bindings.get(&fact.local_id()).filter(|binding| {
            binding.fact == fact.span()
                && binding.source == site.source.span()
                && binding.provenance == site.provenance
                && binding.spelling == site.syntactic_text_hash
        })
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        site: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        Ok(matches!(site.target, OccurrenceTarget::Unresolved { .. })
            .then(|| self.binding(fact, site).map(|binding| binding.target))
            .flatten())
    }

    pub(super) fn section_dependency(
        &self,
        fact: &SyntaxFact,
        site: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<RelationRecord>, AdapterError> {
        cancel.check()?;
        let Some(binding) = self.binding(fact, site) else {
            return Ok(None);
        };
        let Some(owner) = binding.owner else {
            return Ok(None);
        };
        if site.target
            != (OccurrenceTarget::Resolved {
                symbol: binding.target,
            })
        {
            return Ok(None);
        }
        let mut relation = lexical_reference_relation(site, binding.target)?;
        relation.subject = RelationEndpoint::Entity(owner);
        relation
            .evidence
            .derivation
            .try_reserve_exact(3)
            .map_err(|_| SinkError::AllocationFailed)?;
        relation.evidence.derivation.extend([
            FactRef::Entity(owner),
            FactRef::Fact(site.id),
            FactRef::Entity(binding.target),
        ]);
        relation.evidence.derivation.sort_unstable();
        relation.evidence.derivation.dedup();
        relation.id = derive_relation_record_id(&relation)
            .map_err(|_| provider_failure("sed-written-section-dependency-identity"))?;
        cancel.check()?;
        Ok(Some(relation))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_ids::{FileId, GenerationId, RepositoryId, content_hash};
    use rootlight_ir::{
        AnalysisTier, EntityRecord, EntityVisibility, FactEvidence, SymbolIdentityClaim,
    };

    const TEXT: &str = ":owner\nb target\n:target\n";

    fn span(start: u64, end: u64) -> SourceSpan {
        SourceSpan::new(FileId::from_bytes([2; 20]), start, end).unwrap()
    }

    fn source(range: SourceSpan) -> SourceRef {
        SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([3; 20]),
            range,
            content_hash(TEXT.as_bytes()),
            None,
        )
    }

    fn fact(
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        start: u64,
        end: u64,
        label: &str,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            span(start, end),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn facts() -> Vec<SyntaxFact> {
        vec![
            fact(1, None, SyntaxFactKind::Root, 0, 24, "sed.script.root"),
            fact(2, Some(1), SyntaxFactKind::Module, 0, 24, "sed.file.module"),
            fact(
                3,
                Some(2),
                SyntaxFactKind::Declaration,
                0,
                6,
                "sed.statement_label.declaration",
            ),
            fact(
                4,
                Some(3),
                SyntaxFactKind::Occurrence,
                1,
                6,
                "sed.label_name.definition",
            ),
            fact(
                5,
                Some(2),
                SyntaxFactKind::Declaration,
                16,
                23,
                "sed.statement_label.declaration",
            ),
            fact(
                6,
                Some(5),
                SyntaxFactKind::Occurrence,
                17,
                23,
                "sed.label_name.definition",
            ),
            fact(
                7,
                Some(2),
                SyntaxFactKind::Occurrence,
                9,
                15,
                "sed.written_branch.reference",
            ),
            fact(
                8,
                Some(2),
                SyntaxFactKind::Signature,
                0,
                15,
                "sed.written_section_target.signature",
            ),
        ]
    }

    fn entity(
        local: u64,
        kind: EntityKind,
        name: &str,
        range: SourceSpan,
        definition: Option<u64>,
        owner: ContainerRef,
    ) -> MaterializedEntity {
        let full = source(span(0, 24));
        let mut claim = SymbolIdentityClaim {
            symbol: SymbolId::from_bytes([0; 20]),
            repository: full.repository(),
            language: "sed".to_owned(),
            kind,
            container: Some(owner),
            container_identity: vec![u8::try_from(local).unwrap()],
            declared_identity: name.to_owned(),
            signature_discriminator: Vec::new(),
            build_context_discriminator: content_hash(b"sed-written-test").as_bytes().to_vec(),
        };
        claim.symbol = claim.derived_symbol();
        MaterializedEntity {
            record: EntityRecord {
                id: claim.symbol,
                repository: full.repository(),
                generation: full.generation(),
                kind,
                language: "sed".to_owned(),
                tier: AnalysisTier::TierD,
                canonical_name: name.to_owned(),
                display_name: name.to_owned(),
                qualified_name: name.to_owned(),
                container: Some(owner),
                visibility: EntityVisibility::Unknown,
                flags: Vec::new(),
                provenance: FactId::from_bytes([4; 20]),
                evidence: FactEvidence {
                    source: Some(source(range)),
                    derivation: Vec::new(),
                },
            },
            identity_claim: claim,
            direct_parent: owner,
            identity_guard: [0; 32],
            definition_local_id: definition,
            signature_evidence: None,
            signature_span: None,
        }
    }

    fn entities() -> HashMap<u64, MaterializedEntity> {
        let module = entity(
            2,
            EntityKind::Module,
            "written.sed",
            span(0, 24),
            None,
            ContainerRef::File(span(0, 24).file()),
        );
        let owner = ContainerRef::Entity(module.record.id);
        HashMap::from([
            (2, module),
            (
                3,
                entity(
                    3,
                    EntityKind::StatementLabel,
                    "owner",
                    span(0, 6),
                    Some(4),
                    owner,
                ),
            ),
            (
                5,
                entity(
                    5,
                    EntityKind::StatementLabel,
                    "target",
                    span(16, 23),
                    Some(6),
                    owner,
                ),
            ),
        ])
    }

    fn plan(facts: &[SyntaxFact], entities: &HashMap<u64, MaterializedEntity>) -> BranchTargets {
        BranchTargets::new(
            facts,
            entities,
            &source(span(0, 24)),
            Ok,
            |span| {
                Ok(&TEXT[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()])
            },
            &Cancellation::new(),
        )
        .unwrap()
    }

    #[test]
    fn section_certificate_is_optional_for_operand_binding_but_mandatory_for_entity_dependency() {
        assert_eq!(TEXT.len(), 24);
        let original = facts();
        let entities = entities();
        let qualified = plan(&original, &entities);
        assert_eq!(qualified.bindings.len(), 1);
        assert_eq!(qualified.bindings[&7].target, entities[&5].record.id);
        assert_eq!(qualified.bindings[&7].owner, Some(entities[&3].record.id));
        let missing = plan(&original[..7], &entities);
        assert_eq!(missing.bindings.len(), 1);
        assert!(missing.bindings[&7].owner.is_none());
        let mut duplicate = original.clone();
        duplicate.push(fact(
            9,
            Some(2),
            SyntaxFactKind::Signature,
            0,
            15,
            "sed.written_section_target.signature",
        ));
        assert!(plan(&duplicate, &entities).bindings[&7].owner.is_none());
    }

    #[test]
    fn missing_or_duplicate_native_binding_facts_do_not_recover_targets_by_name() {
        let original = facts();
        for missing in 0..7 {
            let mut mutated = original.clone();
            mutated.remove(missing);
            assert!(
                plan(&mutated, &entities()).bindings.is_empty(),
                "missing={missing}"
            );
        }
        for duplicate in 0..7 {
            let mut mutated = original.clone();
            let old = &original[duplicate];
            mutated.push(fact(
                9,
                old.parent(),
                old.kind(),
                old.span().start_byte(),
                old.span().end_byte(),
                old.syntax_kind().as_str(),
            ));
            assert!(
                plan(&mutated, &entities()).bindings.is_empty(),
                "duplicate={duplicate}"
            );
        }
    }

    #[test]
    fn source_frame_owner_and_cancellation_are_required_before_binding() {
        let original = facts();
        for mutant in 0..4 {
            let mut entities = entities();
            let target = entities.get_mut(&5).unwrap();
            match mutant {
                0 => target.record.generation = GenerationId::from_bytes([8; 20]),
                1 => target.record.provenance = FactId::from_bytes([8; 20]),
                2 => target.direct_parent = ContainerRef::File(span(0, 24).file()),
                _ => target.definition_local_id = Some(90),
            }
            assert!(
                plan(&original, &entities).bindings.is_empty(),
                "mutant={mutant}"
            );
        }
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(
            BranchTargets::new(
                &original,
                &entities(),
                &source(span(0, 24)),
                Ok,
                |_| Ok(""),
                &cancel
            )
            .is_err()
        );
    }

    #[test]
    fn foreign_raw_roles_and_reparented_proof_facts_cannot_certify_a_binding() {
        let original = facts();
        assert_eq!(plan(&original, &entities()).bindings.len(), 1);
        for (kind, start, end, label, parent) in [
            (
                SyntaxFactKind::Declaration,
                16,
                23,
                "foreign.label.declaration",
                Some(2),
            ),
            (
                SyntaxFactKind::Occurrence,
                17,
                23,
                "foreign.label.definition",
                Some(5),
            ),
            (
                SyntaxFactKind::Occurrence,
                9,
                15,
                "foreign.branch.reference",
                Some(2),
            ),
            (
                SyntaxFactKind::Signature,
                0,
                15,
                "foreign.section.signature",
                Some(2),
            ),
            (SyntaxFactKind::Root, 0, 24, "foreign.script.root", None),
            (
                SyntaxFactKind::Module,
                0,
                24,
                "foreign.file.module",
                Some(1),
            ),
        ] {
            let mut mutated = original.clone();
            mutated.push(fact(9, parent, kind, start, end, label));
            assert!(plan(&mutated, &entities()).bindings.is_empty(), "{label}");
        }
        for (index, parent) in [
            (0, Some(2)),
            (1, None),
            (1, Some(3)),
            (2, Some(1)),
            (3, Some(2)),
            (4, Some(3)),
            (5, Some(3)),
            (6, Some(3)),
            (7, Some(3)),
        ] {
            let mut mutated = original.clone();
            let old = &original[index];
            mutated[index] = fact(
                old.local_id(),
                parent,
                old.kind(),
                old.span().start_byte(),
                old.span().end_byte(),
                old.syntax_kind().as_str(),
            );
            assert!(
                plan(&mutated, &entities()).bindings.is_empty(),
                "index={index}, parent={parent:?}"
            );
        }
        let mut duplicate_id = original.clone();
        duplicate_id.push(fact(
            7,
            Some(2),
            SyntaxFactKind::Comment,
            0,
            1,
            "sed.comment.comment",
        ));
        assert!(plan(&duplicate_id, &entities()).bindings.is_empty());
        for mutant in 0..3 {
            let mut entities = entities();
            let module = entities.get_mut(&2).unwrap();
            match mutant {
                0 => module.direct_parent = ContainerRef::Entity(module.record.id),
                1 => module.record.container = None,
                _ => module.definition_local_id = Some(4),
            }
            assert!(
                plan(&original, &entities).bindings.is_empty(),
                "module={mutant}"
            );
        }
    }

    #[test]
    fn sed_file_module_admission_is_exact_and_does_not_require_a_written_definition() {
        let original = facts();
        assert!(super::super::is_explicit_file_module(&original[1], "sed"));
        assert!(!super::super::is_explicit_file_module(
            &original[1],
            "python"
        ));
        for (kind, label) in [
            (SyntaxFactKind::Declaration, "sed.file.module"),
            (SyntaxFactKind::Root, "sed.file.module"),
            (SyntaxFactKind::Module, "sed.script.module"),
            (SyntaxFactKind::Module, "python.file.module"),
        ] {
            assert!(!super::super::is_explicit_file_module(
                &fact(2, Some(1), kind, 0, 24, label),
                "sed"
            ));
        }
        assert!(entities()[&2].definition_local_id.is_none());
        assert_eq!(plan(&original, &entities()).bindings.len(), 1);
    }

    #[test]
    fn cancellable_name_digest_preserves_exact_spelling_and_content_identity() {
        let cancel = Cancellation::new();
        let mut name = "a".repeat(257);
        name.push_str("_-9");
        assert_eq!(
            plain_name_digest(&name, &cancel).unwrap(),
            Some(content_hash(name.as_bytes()))
        );
        assert!(same_name(&name, &name, &cancel).unwrap());
        let mut other = name.clone();
        other.pop();
        other.push('8');
        assert!(!same_name(&name, &other, &cancel).unwrap());
        assert!(!same_name(&name, &name[..name.len() - 1], &cancel).unwrap());
        for invalid in ["", "9name", "has space", "λ", "with\0nul", "with\rcr"] {
            assert!(plain_name_digest(invalid, &cancel).unwrap().is_none());
        }
        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(plain_name_digest(&name, &cancelled).is_err());
        assert!(same_name(&name, &name, &cancelled).is_err());
    }
}
