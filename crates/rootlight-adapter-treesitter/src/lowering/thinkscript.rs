//! Re-census complete current ThinkScript source before consuming a Read plan.
//! A raw label, source-contained name or materialized parameter cannot replace
//! exact singular source-plan, owner, definition and generation joins.

use super::{MaterializedEntity, source_for_span};
use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId};
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};
use rootlight_thinkscript::Role;
use std::collections::HashMap;

pub(super) const GAP: &str = "thinkscript-evaluation-builtins-and-omitted-syntax-unqualified";

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence && fact.syntax_kind().as_str() == Role::Read.label()
}

fn kind(role: Role) -> SyntaxFactKind {
    match role {
        Role::Root => SyntaxFactKind::Root,
        Role::Environment | Role::Header | Role::ReadContext => SyntaxFactKind::Signature,
        Role::Script | Role::Input => SyntaxFactKind::Declaration,
        Role::ScriptName | Role::InputName | Role::Read => SyntaxFactKind::Occurrence,
    }
}

fn hash(text: &[u8], cancel: &Cancellation) -> Result<ContentHash, AdapterError> {
    let mut hash = blake3::Hasher::new();
    for chunk in text.chunks(64) {
        cancel.check()?;
        hash.update(chunk);
    }
    Ok(ContentHash::from_bytes(*hash.finalize().as_bytes()))
}

fn equal(left: &[u8], right: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
    if left.len() != right.len() {
        return Ok(false);
    }
    for (a, b) in left.chunks(64).zip(right.chunks(64)) {
        cancel.check()?;
        if a != b {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Exact current-source raw census shared by declaration and Read admission.
fn source_plan(
    facts: &[SyntaxFact],
    source: &SourceRef,
    text: &[u8],
    maximum: usize,
    cancel: &Cancellation,
) -> Result<Option<rootlight_thinkscript::Plan>, AdapterError> {
    cancel.check()?;
    if source.span().start_byte() != 0 || source.span().end_byte() != text.len() as u64 {
        return Ok(None);
    }
    if hash(text, cancel)? != source.content_hash() {
        return Ok(None);
    }
    let plan = match rootlight_thinkscript::scan(text, maximum, maximum, cancel) {
        Ok(plan) => plan,
        Err(rootlight_thinkscript::Error::Cancelled(error)) => return Err(error.into()),
        Err(rootlight_thinkscript::Error::Allocation) => {
            return Err(SinkError::AllocationFailed.into());
        }
        Err(rootlight_thinkscript::Error::Limit) => return Ok(None),
    };
    if !plan.lexical_complete || facts.len() != plan.records.len() {
        return Ok(None);
    }
    let mut raw = HashMap::new();
    raw.try_reserve(facts.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for fact in facts {
        cancel.check()?;
        if raw.insert(fact.local_id(), fact).is_some() {
            return Ok(None);
        }
    }
    for expected in &plan.records {
        cancel.check()?;
        let Some(fact) = raw.get(&expected.id).copied() else {
            return Ok(None);
        };
        if fact.kind() != kind(expected.role)
            || fact.syntax_kind().as_str() != expected.role.label()
            || fact.parent() != expected.parent
            || fact.depth() != expected.depth
            || fact.span()
                != SourceSpan::new(
                    source.span().file(),
                    expected.start as u64,
                    expected.end as u64,
                )
                .map_err(|_| SinkError::InvalidIr)?
        {
            return Ok(None);
        }
    }
    Ok(Some(plan))
}

pub(super) fn admits_raw(
    facts: &[SyntaxFact],
    source: &SourceRef,
    text: &[u8],
    maximum: usize,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    Ok(source_plan(facts, source, text, maximum, cancel)?.is_some())
}

struct Binding {
    span: SourceSpan,
    owner: SymbolId,
    target: SymbolId,
    provenance: FactId,
    text: ContentHash,
}

pub(super) struct Targets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

impl Targets {
    pub(super) fn new(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        text: &[u8],
        maximum: usize,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut out = Self {
            source: source.clone(),
            bindings: HashMap::new(),
        };
        let Some(plan) = source_plan(facts, source, text, maximum, cancel)? else {
            return Ok(out);
        };
        let mut raw = HashMap::new();
        raw.try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancel.check()?;
            raw.insert(fact.local_id(), fact);
        }
        let mut count = 0;
        for record in &plan.records {
            cancel.check()?;
            count += usize::from(matches!(record.role, Role::Script | Role::Input));
        }
        if entities.len() != count {
            return Ok(out);
        }
        for record in &plan.records {
            cancel.check()?;
            if !matches!(record.role, Role::Script | Role::Input) {
                continue;
            }
            let Some(entity) = entities.get(&record.id) else {
                return Ok(out);
            };
            let Ok(record_index) = usize::try_from(record.id) else {
                return Ok(out);
            };
            let Some(definition) = plan.records.get(record_index) else {
                return Ok(out);
            };
            let expected_role = if record.role == Role::Script {
                Role::ScriptName
            } else {
                Role::InputName
            };
            if definition.role != expected_role || definition.parent != Some(record.id) {
                return Ok(out);
            }
            let parent = if record.role == Role::Script {
                ContainerRef::File(source.span().file())
            } else {
                let Some(owner) = record.parent.and_then(|id| entities.get(&id)) else {
                    return Ok(out);
                };
                if owner.record.kind != EntityKind::Function {
                    return Ok(out);
                }
                ContainerRef::Entity(owner.record.id)
            };
            let span = raw[&record.id].span();
            if entity.record.kind
                != if record.role == Role::Script {
                    EntityKind::Function
                } else {
                    EntityKind::Parameter
                }
                || entity.record.language != "thinkscript"
                || entity.record.repository != source.repository()
                || entity.record.generation != source.generation()
                || entity.record.id != entity.identity_claim.symbol
                || entity.record.id != entity.identity_claim.derived_symbol()
                || entity.identity_claim.repository != source.repository()
                || entity.identity_claim.language != "thinkscript"
                || entity.identity_claim.kind != entity.record.kind
                || entity.identity_claim.container != Some(parent)
                || entity.direct_parent != parent
                || entity.record.container != Some(parent)
                || entity.record.evidence.source.as_ref() != Some(&source_for_span(source, span))
                || entity.definition_local_id != Some(definition.id)
                || !equal(
                    entity.record.canonical_name.as_bytes(),
                    &text[definition.start..definition.end],
                    cancel,
                )?
            {
                return Ok(out);
            }
        }
        out.bindings
            .try_reserve(plan.bindings.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for binding in &plan.bindings {
            cancel.check()?;
            let owner = &entities[&binding.owner];
            let target = &entities[&binding.target];
            if owner.record.provenance != target.record.provenance {
                return Ok(Self {
                    source: source.clone(),
                    bindings: HashMap::new(),
                });
            }
            let fact = raw[&binding.read];
            let start =
                usize::try_from(fact.span().start_byte()).map_err(|_| SinkError::InvalidIr)?;
            let end = usize::try_from(fact.span().end_byte()).map_err(|_| SinkError::InvalidIr)?;
            out.bindings.insert(
                binding.read,
                Binding {
                    span: fact.span(),
                    owner: owner.record.id,
                    target: target.record.id,
                    provenance: target.record.provenance,
                    text: hash(&text[start..end], cancel)?,
                },
            );
        }
        Ok(out)
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        site: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(binding) = self.bindings.get(&fact.local_id()) else {
            return Ok(None);
        };
        if !required(fact)
            || site.role != OccurrenceRole::Read
            || site.syntax_kind != Role::Read.label()
            || fact.span() != binding.span
            || site.enclosing != Some(binding.owner)
            || site.provenance != binding.provenance
            || site.repository != self.source.repository()
            || site.generation != self.source.generation()
            || site.file != self.source.span().file()
            || site.source != source_for_span(&self.source, binding.span)
            || site.evidence.source.as_ref() != Some(&site.source)
            || site.syntactic_text_hash != binding.text
            || !matches!(site.target, OccurrenceTarget::Unresolved { text_hash } if text_hash == binding.text)
        {
            return Ok(None);
        }
        Ok(Some(binding.target))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_ids::{FileId, GenerationId, RepositoryId, content_hash};
    use rootlight_ir::{
        AnalysisTier, Confidence, EntityRecord, EntityVisibility, FactEvidence, SymbolIdentityClaim,
    };

    const RAW: &[u8] = b"script Named { input value=2; plot p=value; }";

    fn fixture() -> (SourceRef, Vec<SyntaxFact>, HashMap<u64, MaterializedEntity>) {
        let source = SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([3; 20]),
            SourceSpan::new(FileId::from_bytes([2; 20]), 0, RAW.len() as u64).unwrap(),
            content_hash(RAW),
            None,
        );
        let plan = rootlight_thinkscript::scan(RAW, RAW.len(), 4096, &Cancellation::new()).unwrap();
        let mut facts = Vec::new();
        let mut entities: HashMap<u64, MaterializedEntity> = HashMap::new();
        for record in &plan.records {
            let span =
                SourceSpan::new(source.span().file(), record.start as u64, record.end as u64)
                    .unwrap();
            facts.push(SyntaxFact::new(
                record.id,
                record.parent,
                kind(record.role),
                span,
                record.depth,
                SyntaxKindLabel::new(record.role.label()).unwrap(),
            ));
            if !matches!(record.role, Role::Script | Role::Input) {
                continue;
            }
            let definition = &plan.records[usize::try_from(record.id).unwrap()];
            let name = std::str::from_utf8(&RAW[definition.start..definition.end]).unwrap();
            let parent = record
                .parent
                .and_then(|id| entities.get(&id))
                .map_or(ContainerRef::File(source.span().file()), |entity| {
                    ContainerRef::Entity(entity.record.id)
                });
            let kind = if record.role == Role::Script {
                EntityKind::Function
            } else {
                EntityKind::Parameter
            };
            let mut claim = SymbolIdentityClaim {
                symbol: SymbolId::from_bytes([0; 20]),
                repository: source.repository(),
                language: "thinkscript".to_owned(),
                kind,
                container: Some(parent),
                container_identity: vec![u8::try_from(record.id).unwrap()],
                declared_identity: name.to_owned(),
                signature_discriminator: Vec::new(),
                build_context_discriminator: content_hash(b"thinkscript-consumer-test")
                    .as_bytes()
                    .to_vec(),
            };
            claim.symbol = claim.derived_symbol();
            entities.insert(
                record.id,
                MaterializedEntity {
                    record: EntityRecord {
                        id: claim.symbol,
                        repository: source.repository(),
                        generation: source.generation(),
                        kind,
                        language: "thinkscript".to_owned(),
                        tier: AnalysisTier::TierD,
                        canonical_name: name.to_owned(),
                        display_name: name.to_owned(),
                        qualified_name: name.to_owned(),
                        container: Some(parent),
                        visibility: EntityVisibility::Unknown,
                        flags: Vec::new(),
                        provenance: FactId::from_bytes([8; 20]),
                        evidence: FactEvidence {
                            source: Some(source_for_span(&source, span)),
                            derivation: Vec::new(),
                        },
                    },
                    identity_claim: claim,
                    direct_parent: parent,
                    identity_guard: [0; 32],
                    definition_local_id: Some(definition.id),
                    signature_evidence: None,
                    signature_span: None,
                },
            );
        }
        (source, facts, entities)
    }

    #[test]
    fn baseline_current_source_binding_rejects_entity_identity_generation_and_provenance_mutations()
    {
        let (source, facts, entities) = fixture();
        let build = |entities: &HashMap<u64, MaterializedEntity>, text: &[u8]| {
            Targets::new(&facts, entities, &source, text, 4096, &Cancellation::new()).unwrap()
        };
        let baseline = build(&entities, RAW);
        assert_eq!(baseline.bindings.len(), 1);
        let target = entities
            .iter()
            .find(|(_, entity)| entity.record.kind == EntityKind::Parameter)
            .unwrap()
            .0;
        for mode in 0..7 {
            let mut changed = entities.clone();
            let entity = changed.get_mut(target).unwrap();
            match mode {
                0 => entity.record.generation = GenerationId::from_bytes([9; 20]),
                1 => entity.record.provenance = FactId::from_bytes([9; 20]),
                2 => {
                    entity.record.evidence.source = Some(SourceRef::new(
                        source.repository(),
                        source.generation(),
                        entity.record.evidence.source.as_ref().unwrap().span(),
                        content_hash(b"foreign"),
                        None,
                    ))
                }
                3 => entity.definition_local_id = Some(999),
                4 => entity.record.id = SymbolId::from_bytes([9; 20]),
                5 => entity.direct_parent = ContainerRef::File(source.span().file()),
                _ => entity.record.canonical_name = "foreign".to_owned(),
            }
            assert!(
                build(&changed, RAW).bindings.is_empty(),
                "entity mutant {mode}"
            );
        }
        let mut changed = RAW.to_vec();
        changed[0] = b'S';
        assert!(build(&entities, &changed).bindings.is_empty());
    }

    #[test]
    fn resolved_site_requires_the_exact_current_source_owner_hash_and_evidence() {
        let (source, facts, entities) = fixture();
        let targets =
            Targets::new(&facts, &entities, &source, RAW, 4096, &Cancellation::new()).unwrap();
        let fact = facts.iter().find(|fact| required(fact)).unwrap();
        let binding = &targets.bindings[&fact.local_id()];
        let mut site = OccurrenceRecord {
            id: FactId::from_bytes([9; 20]),
            repository: source.repository(),
            generation: source.generation(),
            file: source.span().file(),
            source: source_for_span(&source, fact.span()),
            role: OccurrenceRole::Read,
            enclosing: Some(binding.owner),
            target: OccurrenceTarget::Unresolved {
                text_hash: binding.text,
            },
            syntactic_text_hash: binding.text,
            syntax_kind: Role::Read.label().to_owned(),
            provenance: binding.provenance,
            confidence: Confidence::new(0).unwrap(),
            evidence: FactEvidence {
                source: Some(source_for_span(&source, fact.span())),
                derivation: Vec::new(),
            },
        };
        assert_eq!(
            targets.resolve(fact, &site, &Cancellation::new()).unwrap(),
            Some(binding.target)
        );
        for mode in 0..7 {
            let mut bad = site.clone();
            match mode {
                0 => bad.generation = GenerationId::from_bytes([9; 20]),
                1 => bad.provenance = FactId::from_bytes([9; 20]),
                2 => bad.syntactic_text_hash = content_hash(b"foreign"),
                3 => bad.enclosing = None,
                4 => bad.evidence.source = None,
                5 => bad.source = source.clone(),
                _ => {
                    bad.target = OccurrenceTarget::Unresolved {
                        text_hash: content_hash(b"foreign"),
                    }
                }
            }
            assert_eq!(
                targets.resolve(fact, &bad, &Cancellation::new()).unwrap(),
                None,
                "site mutant {mode}"
            );
        }
        site.target = OccurrenceTarget::Resolved {
            symbol: binding.target,
        };
        assert_eq!(
            targets.resolve(fact, &site, &Cancellation::new()).unwrap(),
            None
        );
    }
}
