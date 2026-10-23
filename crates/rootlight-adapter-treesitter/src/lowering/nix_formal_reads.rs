//! Join native formal-use endpoints to current Nix Parameter definitions.
//! The existing lexical binding plan must independently select the certified
//! target before a Reference can receive its additional Read facet.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, FileId, SymbolId, content_hash};
use rootlight_ir::{
    EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef, SourceSpan,
};

use super::MaterializedEntity;

pub(super) const READ: &str = "nix.formal_read.read";
const TARGET: &str = "nix.formal_read_target.signature";

/// Index the exact mandatory native marker endpoints for required-fact accounting.
pub(super) fn read_ends(
    facts: &[SyntaxFact],
    cancel: &Cancellation,
) -> Result<HashMap<(FileId, u64), ()>, AdapterError> {
    cancel.check()?;
    let mut ends = HashMap::new();
    for fact in facts {
        cancel.check()?;
        if fact.kind() == SyntaxFactKind::Signature && fact.syntax_kind().as_str() == TARGET {
            ends.try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            ends.insert((fact.span().file(), fact.span().end_byte()), ());
        }
    }
    Ok(ends)
}

pub(super) fn reference(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "nix.identifier.reference"
}

struct Binding {
    target: SymbolId,
    span: SourceSpan,
    spelling: ContentHash,
    provenance: FactId,
}

pub(super) struct ReadTargets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

fn unique<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|old| *old = None)
        .or_insert(Some(value));
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    left.repository() == right.repository()
        && left.generation() == right.generation()
        && left.span().file() == right.span().file()
        && left.content_hash() == right.content_hash()
}

fn reaches(
    mut parent: Option<u64>,
    target: u64,
    by_id: &HashMap<u64, &SyntaxFact>,
    site: SourceSpan,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    for _ in 0..by_id.len() {
        cancel.check()?;
        let Some(local) = parent else {
            return Ok(false);
        };
        let Some(fact) = by_id.get(&local) else {
            return Ok(false);
        };
        if !super::span_contains(fact.span(), site) {
            return Ok(false);
        }
        if local == target {
            return Ok(true);
        }
        parent = fact.parent();
    }
    Ok(false)
}

fn function_scope<'facts>(
    mut parent: Option<u64>,
    by_id: &HashMap<u64, &'facts SyntaxFact>,
    cancel: &Cancellation,
) -> Result<Option<&'facts SyntaxFact>, AdapterError> {
    for _ in 0..by_id.len() {
        cancel.check()?;
        let Some(fact) = parent.and_then(|id| by_id.get(&id)).copied() else {
            return Ok(None);
        };
        if fact.kind() == SyntaxFactKind::Scope
            && fact.syntax_kind().as_str() == "nix.function.scope"
        {
            return Ok(Some(fact));
        }
        parent = fact.parent();
    }
    Ok(None)
}

fn clean_file_envelope(
    mut parent: Option<u64>,
    module: u64,
    by_id: &HashMap<u64, &SyntaxFact>,
    span: SourceSpan,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    // A whole-file expression can share the source_code extent. Native parent
    // assignment then places its clean signature beneath those exact containers.
    for _ in 0..by_id.len() {
        cancel.check()?;
        let Some(fact) = parent.and_then(|id| by_id.get(&id)).copied() else {
            return Ok(false);
        };
        if fact.span() != span {
            return Ok(false);
        }
        if fact.local_id() == module {
            return Ok(true);
        }
        if !matches!(
            (fact.kind(), fact.syntax_kind().as_str()),
            (
                SyntaxFactKind::Declaration,
                "nix.anonymous_function.declaration"
            ) | (
                SyntaxFactKind::Scope,
                "nix.function.scope"
                    | "nix.let.scope"
                    | "nix.attrset.scope"
                    | "nix.rec_attrset.scope"
                    | "nix.bound_attrset.scope"
                    | "nix.bound_rec_attrset.scope"
                    | "nix.with.scope"
                    | "nix.selection.scope"
                    | "nix.inherit_from.scope"
            )
        ) {
            return Ok(false);
        }
        parent = fact.parent();
    }
    Ok(false)
}

impl ReadTargets {
    /// Accept exact native formal/Definition/Reference joins in one clean source.
    /// Duplicate and foreign witnesses withhold the facet without altering binding.
    pub(super) fn new<'text>(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        mut text: impl FnMut(SourceSpan) -> Result<&'text str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut result = Self {
            source: source.clone(),
            bindings: HashMap::new(),
        };
        let mut by_id = HashMap::new();
        let mut definitions = HashMap::new();
        let mut declarations = HashMap::new();
        let mut reads = HashMap::new();
        let mut markers = HashMap::new();
        let mut headers = HashMap::new();
        let mut scopes = HashMap::new();
        for map in [
            &mut definitions,
            &mut declarations,
            &mut reads,
            &mut markers,
            &mut headers,
            &mut scopes,
        ] {
            map.try_reserve(facts.len())
                .map_err(|_| SinkError::AllocationFailed)?;
        }
        by_id
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut root = None;
        let mut module = None;
        let mut clean = None;
        let mut invalid = false;
        for fact in facts {
            cancel.check()?;
            invalid |= by_id.insert(fact.local_id(), fact).is_some();
            invalid |= fact.kind() == SyntaxFactKind::ErrorRecovery;
            let span = physical(fact.span())?;
            match (fact.kind(), fact.syntax_kind().as_str()) {
                (SyntaxFactKind::Root, "nix.file.root") => {
                    invalid |= root.is_some() || span != source.span() || fact.parent().is_some();
                    root = Some(fact.local_id());
                }
                (SyntaxFactKind::Module, "nix.file.module") => {
                    invalid |= module.is_some() || span != source.span();
                    module = Some(fact);
                }
                (SyntaxFactKind::Signature, "nix.formal_clean_file.signature") => {
                    invalid |= clean.is_some() || span != source.span();
                    clean = Some(fact);
                }
                (SyntaxFactKind::Occurrence, "nix.binding_name.definition") => {
                    unique(&mut definitions, span.start_byte(), fact);
                }
                (SyntaxFactKind::Declaration, "nix.parameter.declaration") => {
                    unique(&mut declarations, span.start_byte(), fact);
                }
                (SyntaxFactKind::Signature, "nix.function_header.signature") => {
                    unique(&mut headers, span.start_byte(), fact);
                }
                (SyntaxFactKind::Scope, "nix.function.scope") => {
                    unique(&mut scopes, span.start_byte(), fact);
                }
                (SyntaxFactKind::Occurrence, "nix.identifier.reference") => {
                    unique(&mut reads, span.end_byte(), fact);
                }
                (SyntaxFactKind::Signature, TARGET) => {
                    unique(&mut markers, span.end_byte(), fact);
                }
                _ => {}
            }
        }
        let (Some(root), Some(module), Some(clean)) = (root, module, clean) else {
            return Ok(result);
        };
        if invalid
            || module.parent() != Some(root)
            || !clean_file_envelope(
                clean.parent(),
                module.local_id(),
                &by_id,
                clean.span(),
                cancel,
            )?
        {
            return Ok(result);
        }
        result
            .bindings
            .try_reserve(markers.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for marker in markers.values().copied().flatten() {
            cancel.check()?;
            let target_span = physical(marker.span())?;
            let Some(definition) = definitions
                .get(&target_span.start_byte())
                .copied()
                .flatten()
            else {
                continue;
            };
            let Some(read) = reads.get(&target_span.end_byte()).copied().flatten() else {
                continue;
            };
            let Some(declaration) = definition.parent().and_then(|id| by_id.get(&id)).copied()
            else {
                continue;
            };
            let Some(scope) = function_scope(declaration.parent(), &by_id, cancel)? else {
                continue;
            };
            let Some(parameter) = entities.get(&declaration.local_id()) else {
                continue;
            };
            let Some(evidence) = parameter.record.evidence.source.as_ref() else {
                continue;
            };
            let definition_span = physical(definition.span())?;
            let declaration_span = physical(declaration.span())?;
            let read_span = physical(read.span())?;
            let scope_span = physical(scope.span())?;
            let Some(header) = headers.get(&scope_span.start_byte()).copied().flatten() else {
                continue;
            };
            if declaration.kind() != SyntaxFactKind::Declaration
                || declaration.syntax_kind().as_str() != "nix.parameter.declaration"
                || scope.kind() != SyntaxFactKind::Scope
                || scope.syntax_kind().as_str() != "nix.function.scope"
                || parameter.record.language != "nix"
                || parameter.record.kind != EntityKind::Parameter
                || parameter.record.repository != source.repository()
                || parameter.record.generation != source.generation()
                || parameter.definition_local_id != Some(definition.local_id())
                || parameter.record.container != Some(parameter.direct_parent)
                || evidence.span() != declaration_span
                || !same_source(evidence, source)
                || declarations
                    .get(&declaration_span.start_byte())
                    .copied()
                    .flatten()
                    .map(SyntaxFact::local_id)
                    != Some(declaration.local_id())
                || scopes
                    .get(&scope_span.start_byte())
                    .copied()
                    .flatten()
                    .map(SyntaxFact::local_id)
                    != Some(scope.local_id())
                || !super::span_contains(physical(header.span())?, definition_span)
                || !super::span_contains(scope_span, physical(header.span())?)
                || physical(header.span())?.end_byte() > read_span.start_byte()
                || !reaches(
                    header.parent(),
                    scope.local_id(),
                    &by_id,
                    header.span(),
                    cancel,
                )?
                || !super::span_contains(declaration_span, definition_span)
                || !super::span_contains(target_span, definition_span)
                || !super::span_contains(target_span, read_span)
                || definition_span.end_byte() > read_span.start_byte()
                || text(definition.span())? != parameter.record.canonical_name
                || text(read.span())? != parameter.record.canonical_name
                || !reaches(
                    declaration.parent(),
                    module.local_id(),
                    &by_id,
                    declaration.span(),
                    cancel,
                )?
                || !reaches(
                    marker.parent(),
                    scope.local_id(),
                    &by_id,
                    marker.span(),
                    cancel,
                )?
                || !reaches(read.parent(), scope.local_id(), &by_id, read.span(), cancel)?
            {
                continue;
            }
            result.bindings.insert(
                read.local_id(),
                Binding {
                    target: parameter.record.id,
                    span: read_span,
                    spelling: content_hash(text(read.span())?.as_bytes()),
                    provenance: parameter.record.provenance,
                },
            );
        }
        Ok(result)
    }

    /// Permit a second Read facet only after lexical binding selected this target.
    pub(super) fn permits(
        &self,
        fact: &SyntaxFact,
        physical: SourceSpan,
        site: &OccurrenceRecord,
        symbol: SymbolId,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancel.check()?;
        if !reference(fact)
            || site.role != OccurrenceRole::Reference
            || site.syntax_kind != "nix.identifier.reference"
            || site.target != (OccurrenceTarget::Resolved { symbol })
            || site.repository != self.source.repository()
            || site.generation != self.source.generation()
            || site.file != self.source.span().file()
            || !same_source(&site.source, &self.source)
        {
            return Ok(false);
        }
        Ok(self.bindings.get(&fact.local_id()).is_some_and(|binding| {
            binding.target == symbol
                && binding.span == physical
                && binding.span == site.source.span()
                && binding.spelling == site.syntactic_text_hash
                && binding.provenance == site.provenance
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::{GenerationId, RepositoryId};
    use rootlight_ir::{
        AnalysisTier, ContainerRef, EntityRecord, EntityVisibility, FactEvidence,
        SymbolIdentityClaim,
    };

    const TEXT: &str = "{ value }: (inner: value)";

    fn span(start: usize, end: usize) -> SourceSpan {
        SourceSpan::new(
            FileId::from_bytes([2; 20]),
            u64::try_from(start).unwrap(),
            u64::try_from(end).unwrap(),
        )
        .unwrap()
    }

    fn source() -> SourceRef {
        SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([3; 20]),
            span(0, TEXT.len()),
            content_hash(TEXT.as_bytes()),
            None,
        )
    }

    fn fact(
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        range: SourceSpan,
        label: &str,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            range,
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn witnesses() -> Vec<SyntaxFact> {
        use SyntaxFactKind::{Declaration, Module, Occurrence, Root, Scope, Signature};
        let parameter = TEXT.find("value").unwrap();
        let read = TEXT.rfind("value").unwrap();
        vec![
            fact(1, None, Root, span(0, TEXT.len()), "nix.file.root"),
            fact(2, Some(1), Module, span(0, TEXT.len()), "nix.file.module"),
            fact(3, Some(2), Scope, span(0, TEXT.len()), "nix.function.scope"),
            fact(
                4,
                Some(3),
                Declaration,
                span(parameter, parameter + 5),
                "nix.parameter.declaration",
            ),
            fact(
                5,
                Some(4),
                Occurrence,
                span(parameter, parameter + 5),
                "nix.binding_name.definition",
            ),
            fact(
                6,
                Some(3),
                Scope,
                span(TEXT.find("inner").unwrap(), TEXT.len() - 1),
                "nix.function.scope",
            ),
            fact(
                7,
                Some(6),
                Occurrence,
                span(read, read + 5),
                "nix.identifier.reference",
            ),
            fact(8, Some(3), Signature, span(parameter, read + 5), TARGET),
            fact(
                9,
                Some(2),
                Signature,
                span(0, TEXT.len()),
                "nix.formal_clean_file.signature",
            ),
            fact(
                10,
                Some(3),
                Signature,
                span(0, TEXT.find('(').unwrap()),
                "nix.function_header.signature",
            ),
        ]
    }

    fn entities() -> HashMap<u64, MaterializedEntity> {
        let frame = source();
        let parent = ContainerRef::File(frame.span().file());
        let mut claim = SymbolIdentityClaim {
            symbol: SymbolId::from_bytes([0; 20]),
            repository: frame.repository(),
            language: "nix".to_owned(),
            kind: EntityKind::Parameter,
            container: Some(parent),
            container_identity: b"native-formal-owner".to_vec(),
            declared_identity: "value".to_owned(),
            signature_discriminator: Vec::new(),
            build_context_discriminator: Vec::new(),
        };
        claim.symbol = claim.derived_symbol();
        let range = witnesses()[3].span();
        let record = EntityRecord {
            id: claim.symbol,
            repository: frame.repository(),
            generation: frame.generation(),
            kind: EntityKind::Parameter,
            language: "nix".to_owned(),
            tier: AnalysisTier::TierD,
            canonical_name: "value".to_owned(),
            display_name: "value".to_owned(),
            qualified_name: "value".to_owned(),
            container: Some(parent),
            visibility: EntityVisibility::Unknown,
            flags: Vec::new(),
            provenance: FactId::from_bytes([4; 20]),
            evidence: FactEvidence {
                source: Some(SourceRef::new(
                    frame.repository(),
                    frame.generation(),
                    range,
                    frame.content_hash(),
                    None,
                )),
                derivation: Vec::new(),
            },
        };
        HashMap::from([(
            4,
            MaterializedEntity {
                record,
                identity_claim: claim,
                direct_parent: parent,
                identity_guard: [0; 32],
                definition_local_id: Some(5),
                signature_evidence: None,
                signature_span: None,
            },
        )])
    }

    fn plan(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        frame: &SourceRef,
    ) -> ReadTargets {
        ReadTargets::new(
            facts,
            entities,
            frame,
            |span| {
                Ok(&TEXT[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()])
            },
            Ok,
            &Cancellation::new(),
        )
        .unwrap()
    }

    #[test]
    fn exact_current_formal_definition_ancestry_and_native_endpoints_are_jointly_required() {
        let original = witnesses();
        assert_eq!(plan(&original, &entities(), &source()).bindings.len(), 1);
        for missing in 0..original.len() {
            let mut facts = original.clone();
            facts.remove(missing);
            assert!(
                plan(&facts, &entities(), &source()).bindings.is_empty(),
                "missing={missing}"
            );
        }
        for duplicate in [0, 1, 2, 3, 4, 6, 7, 8, 9] {
            let mut facts = original.clone();
            let prior = &original[duplicate];
            facts.push(fact(
                99,
                prior.parent(),
                prior.kind(),
                prior.span(),
                prior.syntax_kind().as_str(),
            ));
            assert!(
                plan(&facts, &entities(), &source()).bindings.is_empty(),
                "duplicate={duplicate}"
            );
        }
        for (kind, label, range) in [
            (SyntaxFactKind::Occurrence, TARGET, original[7].span()),
            (
                SyntaxFactKind::Signature,
                "nix.formal_read_target.extra",
                original[7].span(),
            ),
            (
                SyntaxFactKind::Signature,
                TARGET,
                span(0, usize::try_from(original[7].span().end_byte()).unwrap()),
            ),
            (SyntaxFactKind::Signature, TARGET, span(2, TEXT.len())),
        ] {
            let mut facts = original.clone();
            facts[7] = fact(8, Some(3), kind, range, label);
            assert!(
                plan(&facts, &entities(), &source()).bindings.is_empty(),
                "{kind:?} {label} {range:?}"
            );
        }
        for parent in [None, Some(2), Some(10), Some(99)] {
            let mut facts = original.clone();
            facts[9] = fact(
                10,
                parent,
                original[9].kind(),
                original[9].span(),
                original[9].syntax_kind().as_str(),
            );
            assert!(
                plan(&facts, &entities(), &source()).bindings.is_empty(),
                "header parent={parent:?}"
            );
        }
        for parent in [None, Some(1), Some(9), Some(99)] {
            let mut facts = original.clone();
            facts[8] = fact(
                9,
                parent,
                original[8].kind(),
                original[8].span(),
                original[8].syntax_kind().as_str(),
            );
            assert!(
                plan(&facts, &entities(), &source()).bindings.is_empty(),
                "clean parent={parent:?}"
            );
        }
        let mut enclosed = original.clone();
        enclosed[8] = fact(
            9,
            Some(3),
            original[8].kind(),
            original[8].span(),
            original[8].syntax_kind().as_str(),
        );
        assert_eq!(plan(&enclosed, &entities(), &source()).bindings.len(), 1);
        enclosed.push(fact(
            99,
            Some(2),
            SyntaxFactKind::Scope,
            original[8].span(),
            "other.file.scope",
        ));
        enclosed[8] = fact(
            9,
            Some(99),
            original[8].kind(),
            original[8].span(),
            original[8].syntax_kind().as_str(),
        );
        assert!(plan(&enclosed, &entities(), &source()).bindings.is_empty());
    }

    #[test]
    fn parameter_kind_spelling_definition_and_current_source_cannot_be_substituted() {
        let facts = witnesses();
        for mutation in 0..7 {
            let mut entities = entities();
            let parameter = entities.get_mut(&4).unwrap();
            match mutation {
                0 => parameter.record.kind = EntityKind::Variable,
                1 => parameter.record.language = "other".to_owned(),
                2 => parameter.definition_local_id = Some(7),
                3 => parameter.record.generation = GenerationId::from_bytes([9; 20]),
                4 => parameter.record.canonical_name = "other".to_owned(),
                5 => parameter.record.container = None,
                _ => {
                    parameter.record.evidence.source = Some(SourceRef::new(
                        source().repository(),
                        source().generation(),
                        facts[3].span(),
                        content_hash(b"changed"),
                        None,
                    ))
                }
            }
            assert!(
                plan(&facts, &entities, &source()).bindings.is_empty(),
                "mutation={mutation}"
            );
        }
        let stale = SourceRef::new(
            source().repository(),
            GenerationId::from_bytes([9; 20]),
            source().span(),
            source().content_hash(),
            None,
        );
        assert!(plan(&facts, &entities(), &stale).bindings.is_empty());
    }

    #[test]
    fn ordinary_reference_requires_its_independent_lexical_target_and_current_evidence() {
        let facts = witnesses();
        let entities = entities();
        let targets = plan(&facts, &entities, &source());
        let frame = source();
        let read = &facts[6];
        let symbol = entities[&4].record.id;
        let mut site = super::super::unresolved_occurrence(
            read,
            OccurrenceRole::Reference,
            None,
            FactId::from_bytes([4; 20]),
            rootlight_ir::Confidence::new(500).unwrap(),
            SourceRef::new(
                frame.repository(),
                frame.generation(),
                read.span(),
                frame.content_hash(),
                None,
            ),
            "value",
        )
        .unwrap();
        assert!(
            !targets
                .permits(read, read.span(), &site, symbol, &Cancellation::new())
                .unwrap()
        );
        site.target = OccurrenceTarget::Resolved { symbol };
        assert!(
            targets
                .permits(read, read.span(), &site, symbol, &Cancellation::new())
                .unwrap()
        );
        for mutation in 0..5 {
            let mut changed = site.clone();
            match mutation {
                0 => changed.role = OccurrenceRole::Read,
                1 => changed.syntactic_text_hash = content_hash(b"other"),
                2 => changed.provenance = FactId::from_bytes([7; 20]),
                3 => changed.generation = GenerationId::from_bytes([9; 20]),
                _ => {
                    changed.target = OccurrenceTarget::Resolved {
                        symbol: SymbolId::from_bytes([8; 20]),
                    }
                }
            }
            assert!(
                !targets
                    .permits(read, read.span(), &changed, symbol, &Cancellation::new())
                    .unwrap()
            );
        }
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            targets.permits(read, read.span(), &site, symbol, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            ReadTargets::new(&facts, &entities, &frame, |_| Ok("value"), Ok, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            read_ends(&[], &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            ReadTargets::new(&[], &HashMap::new(), &frame, |_| Ok(""), Ok, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn decoded_native_endpoints_keep_physical_current_source_evidence() {
        let facts = witnesses();
        let mut entities = entities();
        let original = source();
        let raw = TEXT
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let physical = |span: SourceSpan| {
            SourceSpan::new(span.file(), span.start_byte() * 2, span.end_byte() * 2)
                .map_err(|_| AdapterError::from(SinkError::SourceMismatch))
        };
        let frame = SourceRef::new(
            original.repository(),
            original.generation(),
            physical(original.span()).unwrap(),
            content_hash(&raw),
            None,
        );
        for entity in entities.values_mut() {
            let prior = entity.record.evidence.source.as_ref().unwrap();
            entity.record.evidence.source = Some(SourceRef::new(
                frame.repository(),
                frame.generation(),
                physical(prior.span()).unwrap(),
                frame.content_hash(),
                None,
            ));
        }
        let targets = ReadTargets::new(
            &facts,
            &entities,
            &frame,
            |span| {
                Ok(&TEXT[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()])
            },
            physical,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(targets.bindings.len(), 1);
        let read = &facts[6];
        let symbol = entities[&4].record.id;
        let range = physical(read.span()).unwrap();
        let mut site = super::super::unresolved_occurrence(
            read,
            OccurrenceRole::Reference,
            None,
            entities[&4].record.provenance,
            rootlight_ir::Confidence::new(500).unwrap(),
            SourceRef::new(
                frame.repository(),
                frame.generation(),
                range,
                frame.content_hash(),
                None,
            ),
            "value",
        )
        .unwrap();
        site.target = OccurrenceTarget::Resolved { symbol };
        assert!(
            targets
                .permits(read, range, &site, symbol, &Cancellation::new())
                .unwrap()
        );
        assert!(
            !targets
                .permits(read, read.span(), &site, symbol, &Cancellation::new())
                .unwrap()
        );
    }
}
