//! Independent typed canaries require every native endpoint and current frame.
//! Public native-provider regressions separately exercise real extraction;
//! these cases isolate malformed retained facts and materialized competitors.

use super::*;
use rootlight_adapter_sdk::SyntaxKindLabel;
use rootlight_ids::{FileId, GenerationId, RepositoryId, content_hash};
use rootlight_ir::{
    AnalysisTier, EntityRecord, EntityVisibility, FactEvidence, SymbolIdentityClaim,
};

const TEXT: &str = "struct Written:\n    var data: List[Int]\n    fn size(self) -> Int:\n        return 1\n    fn read(self, index: Int) -> Int:\n        return self.data[index]\n    fn total(self) -> Int:\n        return self.size()\n";

fn full() -> SourceRef {
    SourceRef::new(
        RepositoryId::from_bytes([1; 16]),
        GenerationId::from_bytes([2; 20]),
        SourceSpan::new(
            FileId::from_bytes([3; 20]),
            0,
            u64::try_from(TEXT.len()).unwrap(),
        )
        .unwrap(),
        content_hash(TEXT.as_bytes()),
        None,
    )
}

fn at(start: usize, end: usize) -> SourceRef {
    let source = full();
    SourceRef::new(
        source.repository(),
        source.generation(),
        SourceSpan::new(
            source.span().file(),
            u64::try_from(start).unwrap(),
            u64::try_from(end).unwrap(),
        )
        .unwrap(),
        source.content_hash(),
        None,
    )
}

fn member(
    kind: EntityKind,
    name: &str,
    parent: ContainerRef,
    span: (usize, usize),
    definition: u64,
) -> MaterializedEntity {
    let source = full();
    let mut claim = SymbolIdentityClaim {
        symbol: SymbolId::from_bytes([0; 20]),
        repository: source.repository(),
        language: "mojo".into(),
        kind,
        container: Some(parent),
        container_identity: format!("{parent:?}").into_bytes(),
        declared_identity: name.into(),
        signature_discriminator: Vec::new(),
        build_context_discriminator: Vec::new(),
    };
    claim.symbol = claim.derived_symbol();
    MaterializedEntity {
        record: EntityRecord {
            id: claim.symbol,
            repository: source.repository(),
            generation: source.generation(),
            kind,
            language: "mojo".into(),
            tier: AnalysisTier::TierD,
            canonical_name: name.into(),
            display_name: name.into(),
            qualified_name: name.into(),
            container: Some(parent),
            visibility: EntityVisibility::Unknown,
            flags: Vec::new(),
            provenance: FactId::from_bytes([4; 20]),
            evidence: FactEvidence {
                source: Some(at(span.0, span.1)),
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

fn records() -> HashMap<u64, MaterializedEntity> {
    let structure = member(
        EntityKind::Struct,
        "Written",
        ContainerRef::File(full().span().file()),
        (0, TEXT.trim_end().len()),
        11,
    );
    let parent = ContainerRef::Entity(structure.record.id);
    let field_start = TEXT.find("var data").unwrap();
    let size_start = TEXT.find("fn size").unwrap();
    let read_start = TEXT.find("fn read").unwrap();
    let total_start = TEXT.find("fn total").unwrap();
    let field = member(
        EntityKind::Field,
        "data",
        parent,
        (field_start, TEXT[..size_start].trim_end().len()),
        21,
    );
    let size = member(
        EntityKind::Method,
        "size",
        parent,
        (size_start, TEXT[..read_start].trim_end().len()),
        31,
    );
    let read = member(
        EntityKind::Method,
        "read",
        parent,
        (read_start, TEXT[..total_start].trim_end().len()),
        51,
    );
    let total = member(
        EntityKind::Method,
        "total",
        parent,
        (total_start, TEXT.trim_end().len()),
        71,
    );
    let formal = |method: &MaterializedEntity, anchor: &str, definition| {
        let start = TEXT.find(anchor).unwrap() + anchor.find("self").unwrap();
        member(
            EntityKind::Parameter,
            "self",
            ContainerRef::Entity(method.record.id),
            (start, start + 4),
            definition,
        )
    };
    let size_self = formal(&size, "size(self", 41);
    let read_self = formal(&read, "read(self", 61);
    let total_self = formal(&total, "total(self", 81);
    HashMap::from([
        (10, structure),
        (20, field),
        (30, size),
        (40, size_self),
        (50, read),
        (60, read_self),
        (70, total),
        (80, total_self),
    ])
}

fn facts(records: &HashMap<u64, MaterializedEntity>) -> Vec<SyntaxFact> {
    let fact = |id, parent, kind, source: SourceRef, depth, label| {
        SyntaxFact::new(
            id,
            parent,
            kind,
            source.span(),
            depth,
            SyntaxKindLabel::new(label).unwrap(),
        )
    };
    let mut facts = vec![fact(
        0,
        None,
        SyntaxFactKind::Root,
        full(),
        0,
        "mojo.source_file.root",
    )];
    for (local, name, anchor, kind, label, parent, depth) in [
        (
            10,
            "Written",
            "struct Written",
            EntityKind::Struct,
            "mojo.struct.declaration",
            0,
            1,
        ),
        (
            20,
            "data",
            "var data",
            EntityKind::Field,
            "mojo.field.declaration",
            10,
            2,
        ),
        (
            30,
            "size",
            "fn size",
            EntityKind::Method,
            "mojo.method.declaration",
            10,
            2,
        ),
        (
            40,
            "self",
            "size(self",
            EntityKind::Parameter,
            "mojo.parameter.declaration",
            30,
            3,
        ),
        (
            50,
            "read",
            "fn read",
            EntityKind::Method,
            "mojo.method.declaration",
            10,
            2,
        ),
        (
            60,
            "self",
            "read(self",
            EntityKind::Parameter,
            "mojo.parameter.declaration",
            50,
            3,
        ),
        (
            70,
            "total",
            "fn total",
            EntityKind::Method,
            "mojo.method.declaration",
            10,
            2,
        ),
        (
            80,
            "self",
            "total(self",
            EntityKind::Parameter,
            "mojo.parameter.declaration",
            70,
            3,
        ),
    ] {
        assert_eq!(records[&local].record.kind, kind);
        let source = records[&local].record.evidence.source.clone().unwrap();
        let start = TEXT.find(anchor).unwrap() + anchor.find(name).unwrap();
        facts.push(fact(
            local,
            Some(parent),
            SyntaxFactKind::Declaration,
            source,
            depth,
            label,
        ));
        facts.push(fact(
            local + 1,
            Some(local),
            SyntaxFactKind::Occurrence,
            at(start, start + name.len()),
            depth + 1,
            "mojo.name.definition",
        ));
    }
    for (id, owner, anchor, label, target, self_anchor) in [
        (
            90,
            50,
            "self.data",
            "mojo.self_field_read.reference",
            "var data",
            "read(self",
        ),
        (
            91,
            70,
            "self.size",
            "mojo.self_method_call.call",
            "fn size",
            "total(self",
        ),
    ] {
        let start = TEXT.rfind(anchor).unwrap() + 5;
        let name = &anchor[5..];
        facts.push(fact(
            id,
            Some(owner),
            SyntaxFactKind::Occurrence,
            at(start, start + name.len()),
            3,
            label,
        ));
        let target_start = TEXT.find(target).unwrap() + target.find(name).unwrap();
        let self_start = TEXT.find(self_anchor).unwrap() + self_anchor.find("self").unwrap();
        let marker = if id == 90 { 100 } else { 102 };
        facts.push(fact(
            marker,
            Some(10),
            SyntaxFactKind::Signature,
            at(target_start, start + name.len()),
            2,
            if id == 90 {
                "mojo.self_field_target.signature"
            } else {
                "mojo.self_method_target.signature"
            },
        ));
        facts.push(fact(
            marker + 1,
            Some(owner),
            SyntaxFactKind::Signature,
            at(self_start, start + name.len()),
            3,
            "mojo.self_formal_target.signature",
        ));
    }
    facts
}

fn occurrence(fact: &SyntaxFact, owner: SymbolId) -> OccurrenceRecord {
    let source = super::super::source_for_span(&full(), fact.span());
    let text = &TEXT[usize::try_from(fact.span().start_byte()).unwrap()
        ..usize::try_from(fact.span().end_byte()).unwrap()];
    OccurrenceRecord {
        id: FactId::from_bytes([5; 20]),
        repository: source.repository(),
        generation: source.generation(),
        file: source.span().file(),
        source: source.clone(),
        role: if call(fact) {
            OccurrenceRole::CallSite
        } else {
            OccurrenceRole::Read
        },
        enclosing: Some(owner),
        target: OccurrenceTarget::Unresolved {
            text_hash: content_hash(text.as_bytes()),
        },
        syntactic_text_hash: content_hash(text.as_bytes()),
        syntax_kind: fact.syntax_kind().as_str().into(),
        provenance: FactId::from_bytes([4; 20]),
        confidence: rootlight_ir::Confidence::new(850).unwrap(),
        evidence: FactEvidence {
            source: Some(source),
            derivation: Vec::new(),
        },
    }
}

fn resolve(
    input: &[SyntaxFact],
    records: &HashMap<u64, MaterializedEntity>,
    fact: &SyntaxFact,
    occurrence: &OccurrenceRecord,
    owner: u64,
) -> Option<SymbolId> {
    let by_id = input.iter().map(|item| (item.local_id(), item)).collect();
    let source = full();
    let cancel = Cancellation::new();
    let index = MemberTargets::new(
        &by_id,
        records,
        &source,
        FactId::from_bytes([4; 20]),
        Ok,
        &cancel,
    )
    .unwrap();
    index
        .resolve(
            fact,
            occurrence,
            Some(&records[&owner]),
            |span| {
                Ok(TEXT
                    .get(
                        usize::try_from(span.start_byte()).unwrap()
                            ..usize::try_from(span.end_byte()).unwrap(),
                    )
                    .unwrap())
            },
            Ok,
            &cancel,
        )
        .unwrap()
}

#[test]
fn current_native_target_and_self_endpoints_require_every_actual_declaration_and_definition() {
    let records = records();
    let input = facts(&records);
    for (id, owner, target, removed) in [
        (
            90,
            50,
            20,
            vec![0, 10, 11, 20, 21, 50, 51, 60, 61, 90, 100, 101],
        ),
        (
            91,
            70,
            30,
            vec![0, 10, 11, 30, 31, 40, 41, 70, 71, 80, 81, 91, 102, 103],
        ),
    ] {
        let fact = input.iter().find(|item| item.local_id() == id).unwrap();
        let site = occurrence(fact, records[&owner].record.id);
        assert_eq!(
            resolve(&input, &records, fact, &site, owner),
            Some(records[&target].record.id)
        );
        for removed in removed {
            let retained: Vec<_> = input
                .iter()
                .filter(|item| item.local_id() != removed)
                .cloned()
                .collect();
            assert_eq!(
                resolve(&retained, &records, fact, &site, owner),
                None,
                "missing {removed}"
            );
        }
    }
}

#[test]
fn duplicate_foreign_parent_hash_generation_and_kind_witnesses_cannot_repair_member_proof() {
    let records = records();
    let input = facts(&records);
    let fact = input.iter().find(|item| item.local_id() == 90).unwrap();
    let site = occurrence(fact, records[&50].record.id);
    assert_eq!(
        resolve(&input, &records, fact, &site, 50),
        Some(records[&20].record.id)
    );
    for bad in 0..7 {
        let mut site = site.clone();
        match bad {
            0 => site.generation = GenerationId::from_bytes([9; 20]),
            1 => site.provenance = FactId::from_bytes([9; 20]),
            2 => {
                site.source = SourceRef::new(
                    full().repository(),
                    full().generation(),
                    fact.span(),
                    content_hash(b"other"),
                    None,
                )
            }
            3 => site.role = OccurrenceRole::CallSite,
            4 => site.enclosing = Some(records[&30].record.id),
            5 => site.syntactic_text_hash = content_hash(b"other"),
            _ => site.evidence.source = None,
        }
        assert_eq!(resolve(&input, &records, fact, &site, 50), None);
    }
    for bad in 0..4 {
        let mut retained = input.clone();
        let marker = retained
            .iter()
            .find(|item| item.local_id() == 100)
            .unwrap()
            .clone();
        match bad {
            0 => retained.push(SyntaxFact::new(
                200,
                marker.parent(),
                marker.kind(),
                marker.span(),
                marker.depth(),
                marker.syntax_kind().clone(),
            )),
            1 => {
                retained.retain(|item| item.local_id() != 100);
                retained.push(SyntaxFact::new(
                    100,
                    Some(0),
                    marker.kind(),
                    marker.span(),
                    marker.depth(),
                    marker.syntax_kind().clone(),
                ));
            }
            2 => {
                retained.retain(|item| item.local_id() != 100);
                let wrong = SourceSpan::new(
                    FileId::from_bytes([9; 20]),
                    marker.span().start_byte(),
                    marker.span().end_byte(),
                )
                .unwrap();
                retained.push(SyntaxFact::new(
                    100,
                    marker.parent(),
                    marker.kind(),
                    wrong,
                    marker.depth(),
                    marker.syntax_kind().clone(),
                ));
            }
            _ => {
                retained.retain(|item| item.local_id() != 50);
                let owner = input.iter().find(|item| item.local_id() == 50).unwrap();
                retained.push(SyntaxFact::new(
                    50,
                    Some(0),
                    owner.kind(),
                    owner.span(),
                    owner.depth(),
                    owner.syntax_kind().clone(),
                ));
            }
        }
        assert_eq!(resolve(&retained, &records, fact, &site, 50), None);
    }
}

#[test]
fn cancellation_and_duplicate_same_owner_members_refuse_before_publication() {
    let mut records = records();
    let input = facts(&records);
    let fact = input.iter().find(|item| item.local_id() == 90).unwrap();
    let site = occurrence(fact, records[&50].record.id);
    let duplicate = member(
        EntityKind::Field,
        "data",
        ContainerRef::Entity(records[&10].record.id),
        (
            TEXT.find("self.data").unwrap() + 5,
            TEXT.find("self.data").unwrap() + 9,
        ),
        21,
    );
    records.insert(200, duplicate);
    assert_eq!(resolve(&input, &records, fact, &site, 50), None);
    let by_id = input.iter().map(|item| (item.local_id(), item)).collect();
    let source = full();
    let cancelled = Cancellation::new();
    cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    assert!(matches!(
        MemberTargets::new(
            &by_id,
            &records,
            &source,
            FactId::from_bytes([4; 20]),
            Ok,
            &cancelled
        ),
        Err(AdapterError::Cancelled { .. })
    ));
}

#[test]
fn selected_target_and_formal_records_cannot_borrow_foreign_identity_or_source_claims() {
    for (id, owner, changed_local) in [(90, 50, 20), (90, 50, 60), (91, 70, 30), (91, 70, 40)] {
        for bad in 0..8 {
            let mut records = records();
            let input = facts(&records);
            let fact = input.iter().find(|item| item.local_id() == id).unwrap();
            let site = occurrence(fact, records[&owner].record.id);
            assert!(resolve(&input, &records, fact, &site, owner).is_some());
            let entity = records.get_mut(&changed_local).unwrap();
            match bad {
                0 => entity.record.generation = GenerationId::from_bytes([8; 20]),
                1 => entity.record.repository = RepositoryId::from_bytes([8; 16]),
                2 => entity.record.provenance = FactId::from_bytes([8; 20]),
                3 => {
                    let source = entity.record.evidence.source.as_ref().unwrap();
                    entity.record.evidence.source = Some(SourceRef::new(
                        source.repository(),
                        source.generation(),
                        source.span(),
                        content_hash(b"foreign"),
                        None,
                    ));
                }
                4 => entity.record.evidence.source = None,
                5 => entity.record.kind = EntityKind::Variable,
                6 => entity.identity_claim.declared_identity = "foreign".into(),
                _ => entity.record.container = Some(ContainerRef::File(full().span().file())),
            }
            assert_eq!(
                resolve(&input, &records, fact, &site, owner),
                None,
                "target/formal {changed_local}, mutation {bad}"
            );
        }
    }
}

#[test]
fn exact_member_relations_preserve_source_and_three_call_derivations() {
    let records = records();
    let input = facts(&records);
    for (id, owner, target) in [(90, 50, 20), (91, 70, 30)] {
        let fact = input.iter().find(|item| item.local_id() == id).unwrap();
        let mut site = occurrence(fact, records[&owner].record.id);
        let symbol = resolve(&input, &records, fact, &site, owner).unwrap();
        assert_eq!(symbol, records[&target].record.id);
        site.target = OccurrenceTarget::Resolved { symbol };
        let relation = relation(&site, &records[&owner], symbol, &Cancellation::new()).unwrap();
        assert_eq!(relation.evidence.source, Some(site.source.clone()));
        assert_eq!(relation.repository, site.repository);
        assert_eq!(relation.generation, site.generation);
        assert_eq!(relation.provenance, site.provenance);
        assert_eq!(relation.object, RelationEndpoint::Entity(symbol));
        if id == 91 {
            assert_eq!(relation.predicate, RelationPredicate::Calls);
            assert_eq!(
                relation.subject,
                RelationEndpoint::Entity(records[&owner].record.id)
            );
            assert_eq!(relation.evidence.derivation.len(), 3);
            for required in [
                FactRef::Entity(records[&owner].record.id),
                FactRef::Fact(site.id),
                FactRef::Entity(symbol),
            ] {
                assert!(relation.evidence.derivation.contains(&required));
            }
        } else {
            assert_eq!(relation.predicate, RelationPredicate::RefersTo);
            assert_eq!(relation.subject, RelationEndpoint::Occurrence(site.id));
        }
        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            super::relation(&site, &records[&owner], symbol, &cancelled),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
