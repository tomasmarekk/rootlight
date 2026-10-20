//! Consumer controls mutate source witnesses rather than trusting a name match.

use super::*;
use crate::lowering::unresolved_occurrence;
use rootlight_adapter_sdk::SyntaxKindLabel;
use rootlight_cancel::CancellationReason;
use rootlight_ids::{FileId, GenerationId, RepositoryId};
use rootlight_ir::{
    AnalysisTier, Confidence, EntityRecord, EntityVisibility, FactEvidence, SymbolIdentityClaim,
};

const TEXT: &str = ".size(@width;@height){width:@width;} .square(@size){.size(@size;@size);}";

fn span(start: usize, end: usize) -> SourceSpan {
    SourceSpan::new(FileId::from_bytes([2; 20]), start as u64, end as u64).unwrap()
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
        u16::from(parent.is_some()).into(),
        SyntaxKindLabel::new(label).unwrap(),
    )
}
fn witnesses() -> Vec<SyntaxFact> {
    let square = TEXT.find(".square").unwrap();
    let size_end = TEXT.find('}').unwrap() + 1;
    let square_body = square + TEXT[square..].find('{').unwrap();
    let call = TEXT.rfind(".size").unwrap();
    vec![
        fact(
            1,
            None,
            SyntaxFactKind::Root,
            source().span(),
            "less.source_file",
        ),
        fact(
            2,
            Some(1),
            SyntaxFactKind::Signature,
            source().span(),
            "less.clean_parse.signature",
        ),
        fact(
            3,
            Some(1),
            SyntaxFactKind::Signature,
            source().span(),
            "less.closed_mixin_namespace.signature",
        ),
        fact(
            10,
            Some(1),
            SyntaxFactKind::Declaration,
            span(0, size_end),
            "less.mixin.declaration",
        ),
        fact(
            11,
            Some(10),
            SyntaxFactKind::Signature,
            span(0, 5),
            "less.name.signature",
        ),
        fact(
            12,
            Some(10),
            SyntaxFactKind::Occurrence,
            span(0, 5),
            "less.name.definition",
        ),
        fact(
            13,
            Some(10),
            SyntaxFactKind::Signature,
            span(0, TEXT.find('{').unwrap()),
            "less.mixin.header.signature",
        ),
        fact(
            20,
            Some(1),
            SyntaxFactKind::Declaration,
            span(square, TEXT.len()),
            "less.mixin.declaration",
        ),
        fact(
            21,
            Some(20),
            SyntaxFactKind::Signature,
            span(square, square + 7),
            "less.name.signature",
        ),
        fact(
            22,
            Some(20),
            SyntaxFactKind::Occurrence,
            span(square, square + 7),
            "less.name.definition",
        ),
        fact(
            23,
            Some(20),
            SyntaxFactKind::Signature,
            span(square, square_body),
            "less.mixin.header.signature",
        ),
        fact(
            24,
            Some(20),
            SyntaxFactKind::Scope,
            span(square_body, TEXT.len()),
            "less.block.scope",
        ),
        fact(
            25,
            Some(24),
            SyntaxFactKind::Occurrence,
            span(call, call + 5),
            "less.direct_declared_mixin.call",
        ),
    ]
}
fn entity(name: &str, range: SourceSpan, definition: u64) -> MaterializedEntity {
    let source = source();
    let parent = ContainerRef::File(source.span().file());
    let mut claim = SymbolIdentityClaim {
        symbol: SymbolId::from_bytes([0; 20]),
        repository: source.repository(),
        language: "less".to_owned(),
        kind: EntityKind::Function,
        container: Some(parent),
        container_identity: b"native-less-file".to_vec(),
        declared_identity: name.to_owned(),
        signature_discriminator: Vec::new(),
        build_context_discriminator: content_hash(b"less-source-control").as_bytes().to_vec(),
    };
    claim.symbol = claim.derived_symbol();
    MaterializedEntity {
        record: EntityRecord {
            id: claim.symbol,
            repository: source.repository(),
            generation: source.generation(),
            kind: EntityKind::Function,
            language: "less".to_owned(),
            tier: AnalysisTier::TierD,
            canonical_name: name.to_owned(),
            display_name: name.to_owned(),
            qualified_name: name.to_owned(),
            container: Some(parent),
            visibility: EntityVisibility::Unknown,
            flags: Vec::new(),
            provenance: FactId::from_bytes([4; 20]),
            evidence: FactEvidence {
                source: Some(SourceRef::new(
                    source.repository(),
                    source.generation(),
                    range,
                    source.content_hash(),
                    None,
                )),
                derivation: Vec::new(),
            },
        },
        identity_claim: claim,
        direct_parent: parent,
        identity_guard: [0; 32],
        definition_local_id: Some(definition),
        signature_evidence: None,
        signature_span: None,
    }
}
fn entities() -> HashMap<u64, MaterializedEntity> {
    HashMap::from([
        (
            10,
            entity(".size", span(0, TEXT.find('}').unwrap() + 1), 12),
        ),
        (
            20,
            entity(
                ".square",
                span(TEXT.find(".square").unwrap(), TEXT.len()),
                22,
            ),
        ),
    ])
}
fn plan<'a>(
    facts: &[SyntaxFact],
    entities: &'a HashMap<u64, MaterializedEntity>,
    frame: &SourceRef,
) -> CallTargets<'a> {
    CallTargets::new(
        entities,
        facts,
        frame,
        Ok,
        |range| {
            Ok(&TEXT[usize::try_from(range.start_byte()).unwrap()
                ..usize::try_from(range.end_byte()).unwrap()])
        },
        &Cancellation::new(),
    )
    .unwrap()
}
fn site(facts: &[SyntaxFact], entities: &HashMap<u64, MaterializedEntity>) -> OccurrenceRecord {
    let call = facts.iter().find(|f| f.local_id() == 25).unwrap();
    let frame = source();
    unresolved_occurrence(
        call,
        OccurrenceRole::CallSite,
        Some(entities[&20].record.id),
        entities[&20].record.provenance,
        Confidence::new(850).unwrap(),
        SourceRef::new(
            frame.repository(),
            frame.generation(),
            call.span(),
            frame.content_hash(),
            None,
        ),
        ".size",
    )
    .unwrap()
}
fn resolved(facts: &[SyntaxFact], entities: &HashMap<u64, MaterializedEntity>) -> Option<SymbolId> {
    let call = facts.iter().find(|f| f.local_id() == 25)?;
    plan(facts, entities, &source())
        .resolve(
            call,
            ".size",
            &site(facts, entities),
            entities.get(&20),
            &Cancellation::new(),
        )
        .unwrap()
}

#[test]
fn closed_native_root_headers_and_unique_name_witnesses_are_jointly_required() {
    let facts = witnesses();
    let entities = entities();
    assert_eq!(resolved(&facts, &entities), Some(entities[&10].record.id));
    for missing in 0..facts.len() {
        let mut changed = facts.clone();
        changed.remove(missing);
        assert_eq!(resolved(&changed, &entities), None, "missing={missing}");
    }
    for id in [1, 2, 3, 11, 13, 21, 23] {
        let mut changed = facts.clone();
        let old = facts.iter().find(|f| f.local_id() == id).unwrap();
        changed.push(fact(
            99,
            old.parent(),
            old.kind(),
            old.span(),
            old.syntax_kind().as_str(),
        ));
        assert_eq!(resolved(&changed, &entities), None, "duplicate={id}");
    }
    let mut duplicate_id = facts.clone();
    duplicate_id.push(facts[0].clone());
    assert!(matches!(
        CallTargets::new(
            &entities,
            &duplicate_id,
            &source(),
            Ok,
            |r| Ok(&TEXT
                [usize::try_from(r.start_byte()).unwrap()..usize::try_from(r.end_byte()).unwrap()]),
            &Cancellation::new()
        ),
        Err(AdapterError::Sink(SinkError::SourceMismatch))
    ));
    for fact in &facts {
        assert!(required(fact));
    }
    assert!(!signature(&facts[2]));
    assert!(signature(&facts[6]));
}

#[test]
fn physical_mapping_and_current_materialized_owner_are_required() {
    let facts = witnesses();
    for mutation in 0..8 {
        let mut entities = entities();
        let target = entities.get_mut(&10).unwrap();
        match mutation {
            0 => target.record.language = "other".to_owned(),
            1 => target.record.kind = EntityKind::Variable,
            2 => target.record.generation = GenerationId::from_bytes([9; 20]),
            3 => target.record.canonical_name = ".other".to_owned(),
            4 => target.definition_local_id = Some(22),
            5 => target.record.container = None,
            6 => {
                target.record.evidence.source = Some(SourceRef::new(
                    source().repository(),
                    source().generation(),
                    target.record.evidence.source.as_ref().unwrap().span(),
                    content_hash(b"foreign"),
                    None,
                ))
            }
            _ => target.record.repository = RepositoryId::from_bytes([9; 16]),
        }
        assert_eq!(resolved(&facts, &entities), None, "mutation={mutation}");
    }
    let entities = entities();
    let frame = source();
    let stale = SourceRef::new(
        frame.repository(),
        GenerationId::from_bytes([9; 20]),
        frame.span(),
        frame.content_hash(),
        None,
    );
    assert!(plan(&facts, &entities, &stale).calls.is_empty());
    let shifted = CallTargets::new(
        &entities,
        &facts,
        &frame,
        |r| {
            Ok(span(
                usize::try_from(r.start_byte()).unwrap() + 1,
                usize::try_from(r.end_byte()).unwrap() + 1,
            ))
        },
        |r| {
            Ok(&TEXT
                [usize::try_from(r.start_byte()).unwrap()..usize::try_from(r.end_byte()).unwrap()])
        },
        &Cancellation::new(),
    )
    .unwrap();
    assert!(shifted.calls.is_empty());
}

#[test]
fn call_adoption_rechecks_actual_site_hash_provenance_and_cancellation() {
    let facts = witnesses();
    let entities = entities();
    let plan = plan(&facts, &entities, &source());
    let original = site(&facts, &entities);
    let call = facts.iter().find(|f| f.local_id() == 25).unwrap();
    assert_eq!(
        plan.resolve(
            call,
            ".size",
            &original,
            entities.get(&20),
            &Cancellation::new()
        )
        .unwrap(),
        Some(entities[&10].record.id)
    );
    for mutation in 0..7 {
        let mut changed = original.clone();
        match mutation {
            0 => changed.role = OccurrenceRole::Read,
            1 => changed.provenance = FactId::from_bytes([9; 20]),
            2 => changed.syntactic_text_hash = content_hash(b"other"),
            3 => changed.generation = GenerationId::from_bytes([9; 20]),
            4 => changed.enclosing = Some(entities[&10].record.id),
            5 => {
                changed.target = OccurrenceTarget::Resolved {
                    symbol: entities[&10].record.id,
                }
            }
            _ => {
                changed.source = SourceRef::new(
                    source().repository(),
                    source().generation(),
                    span(0, 5),
                    source().content_hash(),
                    None,
                )
            }
        }
        assert_eq!(
            plan.resolve(
                call,
                ".size",
                &changed,
                entities.get(&20),
                &Cancellation::new()
            )
            .unwrap(),
            None,
            "mutation={mutation}"
        );
    }
    let wrong = fact(
        25,
        call.parent(),
        call.kind(),
        span(0, 5),
        call.syntax_kind().as_str(),
    );
    assert_eq!(
        plan.resolve(
            &wrong,
            ".size",
            &original,
            entities.get(&20),
            &Cancellation::new()
        )
        .unwrap(),
        None
    );
    let cancel = Cancellation::new();
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        plan.resolve(call, ".size", &original, entities.get(&20), &cancel),
        Err(AdapterError::Cancelled { .. })
    ));
}
