//! Explicit Groovy import proofs require complete native declaration inventories.
//! Presentation names and incomplete or competing ownership chains cannot select a target.

use super::*;
use rootlight_ir::{
    CoverageRecord, CoverageScope, FactDomain, SkippedRegion, SkippedRegionReason,
    derive_coverage_record_id, derive_skipped_region_id,
};

fn fixture() -> (Fixture, SymbolId) {
    let mut fixture = Fixture::new();
    fixture.document.files[0].path = "client.groovy".to_owned();
    let client = fixture.primary_file;
    let declaration = fixture.add_file(2, "definition.groovy");
    let client_root = fixture.add_entity(10, "client.groovy", client, EntityKind::Module, None);
    let root = fixture.add_entity(
        11,
        "definition.groovy",
        declaration,
        EntityKind::Module,
        None,
    );
    let package = fixture.add_entity(
        12,
        "sample.types",
        declaration,
        EntityKind::Module,
        Some(root),
    );
    let target = fixture.add_entity(13, "Widget", declaration, EntityKind::Class, Some(package));
    fixture.add_occurrence(
        20,
        "sample.types.Widget",
        client,
        OccurrenceRole::ImportUse,
        Some(client_root),
    );
    fixture.document.occurrences[0].syntax_kind = "groovy.type_import.reference".to_owned();
    for file in &mut fixture.document.files {
        file.language = "groovy".to_owned();
    }
    for entity in &mut fixture.document.entities {
        entity.language = "groovy".to_owned();
        if let Some(ContainerRef::File(file)) = entity.container {
            entity.evidence.source = Some(source_ref(
                fixture.document.repository,
                fixture.document.generation,
                file,
                fixture.content_hash,
                0,
                SOURCE_BYTES,
            ));
        }
    }
    fixture.document.provenance[0].language = "groovy".to_owned();
    refresh_coverage(&mut fixture);
    fixture.validate();
    (fixture, target)
}

fn refresh_coverage(fixture: &mut Fixture) {
    fixture.document.coverage_records.clear();
    for file in &fixture.document.files {
        let count = fixture
            .document
            .entities
            .iter()
            .filter(|entity| {
                entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == file.id)
            })
            .count();
        for (domain, indexed) in [
            (FactDomain::Files, 1),
            (FactDomain::Entities, u64::try_from(count).unwrap()),
        ] {
            let mut record = CoverageRecord {
                id: FactId::from_bytes([0; 20]),
                repository: fixture.document.repository,
                generation: fixture.document.generation,
                scope: CoverageScope::File(file.id),
                domain,
                tier: AnalysisTier::TierB,
                status: CoverageStatus::Complete,
                discovered: indexed,
                indexed,
                skipped: 0,
                provenance: file.provenance,
                evidence: file.evidence.clone(),
            };
            record.id = derive_coverage_record_id(&record).unwrap();
            fixture.document.coverage_records.push(record);
        }
    }
}

fn assert_unresolved(fixture: &Fixture) {
    fixture.validate();
    let engine = ResolutionEngine::default();
    let cancellation = Cancellation::new();
    assert_eq!(
        engine
            .estimate_work(&fixture.document, &cancellation)
            .unwrap()
            .required,
        0
    );
    assert!(
        engine
            .resolve(&fixture.document, &cancellation)
            .unwrap()
            .decisions
            .is_empty()
    );
    let context = ResolverFactContext::new(fixture.content_hash);
    let applied = engine
        .apply(fixture.document.clone(), context, &cancellation)
        .unwrap();
    let streamed = engine
        .apply_document(fixture.document.clone(), context, &cancellation)
        .unwrap();
    let (bounded, _) = engine
        .apply_document_bounded(fixture.document.clone(), context, &cancellation)
        .unwrap();
    assert_eq!(applied.document, streamed);
    assert_eq!(streamed, bounded);
    assert!(
        bounded
            .occurrences
            .iter()
            .all(|occurrence| matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }))
    );
    assert!(bounded.relations.is_empty());
}

#[test]
fn groovy_import_proofs_preserve_owners_and_coverage_across_application_paths() {
    let (fixture, target) = fixture();
    let engine = ResolutionEngine::new(ResolutionLimits::with_work_limit(1, 1).unwrap());
    let cancellation = Cancellation::new();
    let context = ResolverFactContext::new(fixture.content_hash);
    let expected = engine
        .apply(fixture.document.clone(), context, &cancellation)
        .unwrap()
        .document;
    for reversed in [false, true] {
        let mut document = fixture.document.clone();
        if reversed {
            document.files.reverse();
            document.entities.reverse();
            document.coverage_records.reverse();
        }
        assert_eq!(
            engine
                .estimate_work(&document, &cancellation)
                .unwrap()
                .required,
            1
        );
        let streamed = engine
            .apply_document(document.clone(), context, &cancellation)
            .unwrap();
        let (bounded, estimate) = engine
            .apply_document_bounded(document, context, &cancellation)
            .unwrap();
        assert_eq!(estimate.required, 1);
        assert_eq!(streamed, expected);
        assert_eq!(bounded, expected);
        assert_eq!(
            bounded.occurrences[0].target,
            OccurrenceTarget::Resolved { symbol: target }
        );
        let provenance = bounded
            .provenance
            .iter()
            .find(|record| record.id == bounded.occurrences[0].provenance)
            .unwrap();
        assert_eq!(
            provenance.rule.as_deref(),
            Some("groovy-type-import-v1.indexed_snapshot")
        );
        for owner in [11, 12, 13] {
            assert!(
                provenance
                    .derivation_parents
                    .contains(&FactRef::Entity(SymbolId::from_bytes([owner; 20])))
            );
        }
        for coverage in &fixture.document.coverage_records {
            assert!(
                provenance
                    .derivation_parents
                    .contains(&FactRef::Fact(coverage.id))
            );
        }
        assert_eq!(bounded.relations.len(), 1);
        assert_eq!(bounded.relations[0].predicate, RelationPredicate::Imports);
        assert_eq!(
            bounded.relations[0].object,
            RelationEndpoint::Entity(target)
        );
    }
}

#[test]
fn groovy_type_uses_require_exact_native_binding_envelopes() {
    use rootlight_ir::{GroovyImportedType, SourceSpan, new_groovy_binding_envelope};
    let (mut fixture, target) = fixture();
    let site = &mut fixture.document.occurrences[0];
    site.role = OccurrenceRole::TypeUse;
    site.syntax_kind = "groovy.type_identifier.reference".to_owned();
    site.syntactic_text_hash = content_hash(b"Selected");
    site.target = OccurrenceTarget::Unresolved {
        text_hash: site.syntactic_text_hash,
    };
    let source = site.source.clone();
    let binding = GroovyImportedType {
        import: SourceSpan::new(source.span().file(), 0, 10).unwrap(),
        target: content_hash(b"sample.types.Widget"),
    };
    let envelope = new_groovy_binding_envelope(
        fixture.document.repository,
        fixture.document.generation,
        site.provenance,
        source.clone(),
        binding,
    )
    .unwrap();
    assert_unresolved(&fixture);
    fixture.document.extensions.push(envelope.clone());
    fixture.validate();
    let engine = ResolutionEngine::default();
    let context = ResolverFactContext::new(fixture.content_hash);
    let cancellation = Cancellation::new();
    let applied = engine
        .apply(fixture.document.clone(), context, &cancellation)
        .unwrap()
        .document;
    assert_eq!(
        applied.occurrences[0].target,
        OccurrenceTarget::Resolved { symbol: target }
    );
    assert_eq!(applied.relations[0].predicate, RelationPredicate::UsesType);
    let proof = applied
        .provenance
        .iter()
        .find(|record| record.id == applied.occurrences[0].provenance)
        .unwrap();
    assert!(
        proof
            .derivation_parents
            .contains(&FactRef::Fact(envelope.id))
    );
    assert!(
        proof
            .input_sources
            .iter()
            .any(|source| source.span() == binding.import)
    );
    assert_eq!(
        applied,
        engine
            .apply_document(fixture.document.clone(), context, &cancellation)
            .unwrap()
    );
    assert_eq!(
        applied,
        engine
            .apply_document_bounded(fixture.document.clone(), context, &cancellation)
            .unwrap()
            .0
    );
    for wrong_span in [
        SourceSpan::new(source.span().file(), 25, 26).unwrap(),
        SourceSpan::new(source.span().file(), 16, 25).unwrap(),
    ] {
        let wrong_source = SourceRef::new(
            source.repository(),
            source.generation(),
            wrong_span,
            source.content_hash(),
            None,
        );
        fixture.document.extensions[0] = new_groovy_binding_envelope(
            fixture.document.repository,
            fixture.document.generation,
            envelope.provenance,
            wrong_source,
            binding,
        )
        .unwrap();
        assert_unresolved(&fixture);
    }
    fixture.document.extensions[0] = new_groovy_binding_envelope(
        fixture.document.repository,
        fixture.document.generation,
        envelope.provenance,
        source,
        GroovyImportedType {
            target: content_hash(b"missing.Widget"),
            ..binding
        },
    )
    .unwrap();
    assert_unresolved(&fixture);
}

#[test]
fn groovy_imports_ignore_presentation_names_and_refuse_duplicate_declarations() {
    let (mut fixture, target) = fixture();
    let declaration = fixture.document.files[1].id;
    let package = fixture.document.entities[2].id;
    let other = fixture.add_entity(14, "Other", declaration, EntityKind::Class, Some(package));
    let entity = fixture.document.entities.last_mut().unwrap();
    entity.language = "groovy".to_owned();
    entity.display_name = "Widget".to_owned();
    entity.qualified_name = "sample.types.Widget".to_owned();
    refresh_coverage(&mut fixture);
    let batch = ResolutionEngine::default()
        .resolve(&fixture.document, &Cancellation::new())
        .unwrap();
    assert!(
        matches!(batch.decisions[0].outcome, ResolutionOutcome::Resolved { symbol, .. } if symbol == target && symbol != other)
    );
    fixture.document.entities.last_mut().unwrap().canonical_name = "Widget".to_owned();
    assert_unresolved(&fixture);
}

#[test]
fn groovy_imports_refuse_missing_partial_or_duplicate_inventory_proofs() {
    for domain in [FactDomain::Files, FactDomain::Entities] {
        for file_index in 0..2 {
            for mutation in 0..3 {
                let (mut fixture, _) = fixture();
                let file = fixture.document.files[file_index].id;
                let index = fixture
                    .document
                    .coverage_records
                    .iter()
                    .position(|record| {
                        record.scope == CoverageScope::File(file) && record.domain == domain
                    })
                    .unwrap();
                match mutation {
                    0 => {
                        fixture.document.coverage_records.remove(index);
                    }
                    1 => {
                        let record = &mut fixture.document.coverage_records[index];
                        record.status = CoverageStatus::Bounded;
                        record.discovered += 1;
                        record.skipped = 1;
                        record.id = derive_coverage_record_id(record).unwrap();
                    }
                    _ => {
                        let mut duplicate = fixture.document.coverage_records[index].clone();
                        duplicate.tier = AnalysisTier::TierD;
                        duplicate.id = derive_coverage_record_id(&duplicate).unwrap();
                        fixture.document.coverage_records.push(duplicate);
                    }
                }
                assert_unresolved(&fixture);
            }
        }
    }
}

#[test]
fn groovy_imports_do_not_promote_static_star_lexical_or_call_sites() {
    for syntax in [
        "groovy.static_import.reference",
        "groovy.static_star_import.reference",
        "groovy.package_star_import.reference",
        "groovy.identifier.reference",
    ] {
        let (mut fixture, _) = fixture();
        fixture.document.occurrences[0].syntax_kind = syntax.to_owned();
        assert_unresolved(&fixture);
    }
    for role in [OccurrenceRole::Reference, OccurrenceRole::CallSite] {
        let (mut fixture, _) = fixture();
        fixture.document.occurrences[0].role = role;
        assert_unresolved(&fixture);
    }
}

#[test]
fn groovy_imports_require_parser_owned_type_and_file_module_chains() {
    for mutation in 0..3 {
        let (mut fixture, _) = fixture();
        match mutation {
            0 => fixture.document.provenance[0].producer_kind = ProducerKind::Rule,
            1 => fixture.document.entities[1].canonical_name = "unrelated.groovy".to_owned(),
            _ => fixture.document.entities[2].kind = EntityKind::Function,
        }
        assert_unresolved(&fixture);
    }
}

#[test]
fn groovy_imports_bind_default_package_and_nested_type_ownership() {
    for nested in [false, true] {
        let (mut fixture, target) = fixture();
        let name = if nested {
            fixture.document.entities[2].kind = EntityKind::Class;
            fixture.document.entities[2].canonical_name = "Outer".to_owned();
            "Outer.Widget"
        } else {
            let root = fixture.document.entities[1].id;
            fixture.document.entities[3].container = Some(ContainerRef::Entity(root));
            fixture.document.entities.remove(2);
            "Widget"
        };
        let occurrence = &mut fixture.document.occurrences[0];
        occurrence.syntactic_text_hash = content_hash(name.as_bytes());
        occurrence.target = OccurrenceTarget::Unresolved {
            text_hash: occurrence.syntactic_text_hash,
        };
        refresh_coverage(&mut fixture);
        fixture.validate();
        let batch = ResolutionEngine::default()
            .resolve(&fixture.document, &Cancellation::new())
            .unwrap();
        assert!(
            matches!(batch.decisions[0].outcome, ResolutionOutcome::Resolved { symbol, .. } if symbol == target)
        );
        if nested {
            let root = fixture.document.entities[1].id;
            let file = fixture.document.files[1].id;
            let package = fixture.add_entity(14, "Outer", file, EntityKind::Module, Some(root));
            fixture.add_entity(15, "Widget", file, EntityKind::Class, Some(package));
            for entity in &mut fixture.document.entities {
                entity.language = "groovy".to_owned();
            }
            refresh_coverage(&mut fixture);
            assert_unresolved(&fixture);
        }
    }
}

#[test]
fn groovy_imports_refuse_inventory_counts_without_retained_declarations() {
    let (mut fixture, _) = fixture();
    let record = fixture
        .document
        .coverage_records
        .iter_mut()
        .find(|record| {
            record.domain == FactDomain::Entities
                && record.scope == CoverageScope::File(FileId::from_bytes([2; 20]))
        })
        .unwrap();
    record.indexed += 1;
    record.discovered += 1;
    record.id = derive_coverage_record_id(record).unwrap();
    assert_unresolved(&fixture);
}

#[test]
fn groovy_resolved_import_gaps_retain_only_unverified_build_context() {
    let (mut fixture, _) = fixture();
    for end in [8, SOURCE_BYTES] {
        let source = source_ref(
            fixture.document.repository,
            fixture.document.generation,
            fixture.primary_file,
            fixture.content_hash,
            0,
            end,
        );
        let mut gap = SkippedRegion {
            id: FactId::from_bytes([0; 20]),
            repository: fixture.document.repository,
            generation: fixture.document.generation,
            source: source.clone(),
            domain: FactDomain::Relations,
            reason: SkippedRegionReason::UnsupportedConstruct,
            detail: "unresolved-import-target".to_owned(),
            provenance: fixture.provenance,
            evidence: FactEvidence {
                source: Some(source),
                derivation: Vec::new(),
            },
        };
        gap.id = derive_skipped_region_id(&gap).unwrap();
        fixture.document.skipped_regions.push(gap);
    }
    fixture.validate();
    let engine = ResolutionEngine::default();
    let context = ResolverFactContext::new(fixture.content_hash);
    let cancellation = Cancellation::new();
    let applied = engine
        .apply(fixture.document.clone(), context, &cancellation)
        .unwrap()
        .document;
    let streamed = engine
        .apply_document(fixture.document.clone(), context, &cancellation)
        .unwrap();
    let (bounded, _) = engine
        .apply_document_bounded(fixture.document.clone(), context, &cancellation)
        .unwrap();
    assert_eq!(applied, streamed);
    assert_eq!(applied, bounded);
    fixture
        .document
        .coverage_records
        .sort_unstable_by_key(|record| record.id);
    assert_eq!(applied.coverage_records, fixture.document.coverage_records);
    assert_eq!(applied.skipped_regions.len(), 2);
    for gap in &applied.skipped_regions {
        if gap.source.span().end_byte() == SOURCE_BYTES {
            assert_eq!(gap.detail, "groovy-import-build-context-unverified");
            assert_eq!(gap.reason, SkippedRegionReason::MissingBuildContext);
            assert_eq!(gap.provenance, applied.occurrences[0].provenance);
        } else {
            assert_eq!(gap.detail, "unresolved-import-target");
            assert_eq!(gap.provenance, fixture.provenance);
        }
        assert_eq!(gap.evidence.source.as_ref(), Some(&gap.source));
        assert_eq!(gap.id, derive_skipped_region_id(gap).unwrap());
    }
}
