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

fn qualified_header_fixture() -> (Fixture, SymbolId) {
    let (mut fixture, target) = fixture();
    let root = fixture.document.entities[0].id;
    let owner = fixture.add_entity(
        30,
        "Holder",
        fixture.primary_file,
        EntityKind::Class,
        Some(root),
    );
    let owner_entity = fixture.document.entities.last_mut().unwrap();
    owner_entity.language = "groovy".to_owned();
    owner_entity.evidence.source = Some(source_ref(
        fixture.document.repository,
        fixture.document.generation,
        fixture.primary_file,
        fixture.content_hash,
        0,
        SOURCE_BYTES,
    ));
    let owner_span = owner_entity.evidence.source.as_ref().unwrap().span();
    let site = &mut fixture.document.occurrences[0];
    site.role = OccurrenceRole::InheritanceUse;
    site.syntax_kind = "groovy.extends_type.reference".to_owned();
    site.enclosing = Some(owner);
    fixture.document.extensions.push(
        rootlight_ir::new_groovy_type_lookup_envelope(
            fixture.document.repository,
            fixture.document.generation,
            fixture.provenance,
            site.source.clone(),
            rootlight_ir::GroovyTypeLookup {
                owner: owner_span,
                name: "sample.types.Widget".to_owned(),
                fallback_import: None,
                fallback_local: None,
            },
        )
        .unwrap(),
    );
    refresh_coverage(&mut fixture);
    fixture.validate();
    (fixture, target)
}

#[test]
fn groovy_qualified_headers_require_exact_native_context() {
    let (fixture, target) = qualified_header_fixture();
    let engine = ResolutionEngine::new(ResolutionLimits::with_work_limit(1, 1).unwrap());
    let cancellation = Cancellation::new();
    let context = ResolverFactContext::new(fixture.content_hash);
    let applied = engine
        .apply(fixture.document.clone(), context, &cancellation)
        .unwrap()
        .document;
    assert_eq!(
        applied,
        engine
            .apply_document(fixture.document.clone(), context, &cancellation)
            .unwrap()
    );
    let (bounded, estimate) = engine
        .apply_document_bounded(fixture.document.clone(), context, &cancellation)
        .unwrap();
    assert_eq!(applied, bounded);
    assert_eq!(estimate.required, 1);
    assert_eq!(
        bounded.occurrences[0].target,
        OccurrenceTarget::Resolved { symbol: target }
    );
    assert_eq!(bounded.relations[0].predicate, RelationPredicate::Extends);
    let proof = bounded
        .provenance
        .iter()
        .find(|item| item.id == bounded.occurrences[0].provenance)
        .unwrap();
    assert_eq!(
        proof.rule.as_deref(),
        Some("groovy-qualified-header-v1.indexed_snapshot")
    );
    assert!(
        proof
            .derivation_parents
            .contains(&FactRef::Fact(fixture.document.extensions[0].id))
    );
    assert!(
        proof
            .derivation_parents
            .contains(&FactRef::Entity(SymbolId::from_bytes([30; 20])))
    );

    for mutation in 0..7 {
        let (mut fixture, _) = qualified_header_fixture();
        match mutation {
            0 => fixture.document.extensions.clear(),
            1 => fixture.document.extensions[0].version = "2".to_owned(),
            2 => fixture.document.occurrences[0].enclosing = None,
            3 => fixture.document.occurrences[0].role = OccurrenceRole::TypeUse,
            4 => fixture.document.entities.last_mut().unwrap().kind = EntityKind::Function,
            5 => {
                fixture.document.extensions[0].payload = fixture.document.extensions[0]
                    .payload
                    .replace("sample.types.Widget", "another.Type")
            }
            _ => {
                fixture.document.occurrences[0].syntactic_text_hash = content_hash(b"another.Type")
            }
        }
        assert_unresolved(&fixture);
    }
}

#[test]
fn groovy_type_uses_require_exact_native_binding_envelopes() {
    assert_type_use_requires_native_binding("groovy.type_identifier.reference");
}

#[test]
fn groovy_qualified_type_uses_require_exact_native_binding_envelopes() {
    assert_type_use_requires_native_binding("groovy.qualified_type.reference");
}

fn assert_type_use_requires_native_binding(syntax: &str) {
    use rootlight_ir::{GroovyImportedType, SourceSpan, new_groovy_binding_envelope};
    let (mut fixture, target) = fixture();
    let site = &mut fixture.document.occurrences[0];
    site.role = OccurrenceRole::TypeUse;
    site.syntax_kind = syntax.to_owned();
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
fn groovy_qualified_headers_do_not_skip_ambiguous_preferred_depths() {
    for reversed in [false, true] {
        for duplicate in [false, true] {
            let (mut fixture, target) = qualified_header_fixture();
            let file = fixture.document.files[1].id;
            let root = fixture.document.entities[1].id;
            let package = fixture.add_entity(40, "sample", file, EntityKind::Module, Some(root));
            let outer = fixture.add_entity(41, "types", file, EntityKind::Class, Some(package));
            fixture.add_entity(42, "Widget", file, EntityKind::Class, Some(outer));
            if duplicate {
                let preferred_package = fixture.document.entities[2].id;
                fixture.add_entity(
                    43,
                    "Widget",
                    file,
                    EntityKind::Class,
                    Some(preferred_package),
                );
            }
            for entity in &mut fixture.document.entities {
                entity.language = "groovy".to_owned();
            }
            refresh_coverage(&mut fixture);
            if reversed {
                fixture.document.entities.reverse();
                fixture.document.files.reverse();
                fixture.document.coverage_records.reverse();
            }
            fixture.validate();
            if duplicate {
                assert_unresolved(&fixture);
            } else {
                let engine = ResolutionEngine::default();
                let context = ResolverFactContext::new(fixture.content_hash);
                let applied = engine
                    .apply_document(fixture.document, context, &Cancellation::new())
                    .unwrap();
                assert_eq!(
                    applied.occurrences[0].target,
                    OccurrenceTarget::Resolved { symbol: target }
                );
                assert_eq!(applied.relations.len(), 1);
                assert_eq!(
                    applied.relations[0].object,
                    RelationEndpoint::Entity(target)
                );
            }
        }
    }
}

#[test]
fn groovy_inherited_types_preserve_work_bounds_and_application_identity() {
    let (fixture, target) = inherited_type_fixture();
    let cancellation = Cancellation::new();
    let context = ResolverFactContext::new(fixture.content_hash);
    let engine = ResolutionEngine::default();
    let estimate = engine
        .estimate_work(&fixture.document, &cancellation)
        .unwrap();
    assert!(estimate.required > fixture.document.occurrences.len());
    let exact =
        ResolutionEngine::new(ResolutionLimits::with_work_limit(8, estimate.required).unwrap());
    let expected = exact
        .apply(fixture.document.clone(), context, &cancellation)
        .unwrap()
        .document;
    assert_eq!(
        expected,
        exact
            .apply_document(fixture.document.clone(), context, &cancellation)
            .unwrap()
    );
    assert_eq!(
        expected,
        exact
            .apply_document_bounded(fixture.document.clone(), context, &cancellation)
            .unwrap()
            .0
    );
    let uses: Vec<_> = expected
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::TypeUse)
        .collect();
    assert_eq!(uses.len(), 2);
    assert!(
        uses.iter()
            .all(|site| site.target == OccurrenceTarget::Resolved { symbol: target })
    );
    for site in uses {
        let proof = expected
            .provenance
            .iter()
            .find(|proof| proof.id == site.provenance)
            .unwrap();
        assert_eq!(
            proof.rule.as_deref(),
            Some("groovy-inherited-type-v1.indexed_snapshot")
        );
        assert!(proof.evidence_sources.contains(&site.source));
        assert!(proof.derivation_parents.contains(&FactRef::Entity(target)));
    }
    let reapplied = exact
        .apply_document(expected.clone(), context, &cancellation)
        .unwrap();
    assert_eq!(reapplied, expected);

    let insufficient =
        ResolutionEngine::new(ResolutionLimits::with_work_limit(8, estimate.required - 1).unwrap());
    assert!(matches!(
        insufficient.resolve(&fixture.document, &cancellation),
        Err(ResolutionError::WorkLimit { .. })
    ));
    assert!(matches!(
        insufficient.apply_document(fixture.document.clone(), context, &cancellation),
        Err(ResolutionError::WorkLimit { .. })
    ));
    let limited = ResolutionEngine::new(ResolutionLimits::with_work_limit(8, 1).unwrap());
    let (partial, partial_estimate) = limited
        .apply_document_bounded(fixture.document.clone(), context, &cancellation)
        .unwrap();
    assert_eq!(partial_estimate.required, estimate.required);
    assert!(!partial_estimate.fits());
    assert_eq!(partial.relations.len(), 1);
    assert_eq!(partial.relations[0].predicate, RelationPredicate::Extends);
    assert!(
        partial
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::TypeUse)
            .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
    let per_use = (estimate.required - 1) / 2;
    let staged = ResolutionEngine::new(ResolutionLimits::with_work_limit(8, per_use).unwrap());
    let (middle, _) = staged
        .apply_document_bounded(partial, context, &cancellation)
        .unwrap();
    assert_eq!(
        middle
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::TypeUse
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
            .count(),
        1
    );
    let resumed = exact
        .apply_document(middle, context, &cancellation)
        .unwrap();
    assert!(
        resumed
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::TypeUse)
            .all(|site| site.target == OccurrenceTarget::Resolved { symbol: target })
    );
}

#[test]
fn groovy_inherited_types_reject_incomplete_source_inventories() {
    for mutation in 0..7 {
        let (mut fixture, _) = inherited_type_fixture();
        match mutation {
            0 => fixture
                .document
                .coverage_records
                .retain(|record| record.domain != FactDomain::Occurrences),
            1 => {
                let record = fixture
                    .document
                    .coverage_records
                    .iter_mut()
                    .find(|record| {
                        record.domain == FactDomain::Occurrences
                            && record.scope == CoverageScope::File(fixture.primary_file)
                    })
                    .unwrap();
                record.indexed += 1;
                record.discovered += 1;
                record.id = derive_coverage_record_id(record).unwrap();
            }
            2 => fixture.document.extensions.retain(|envelope| {
                envelope
                    .evidence
                    .source
                    .as_ref()
                    .unwrap()
                    .span()
                    .start_byte()
                    == 16
            }),
            3 => {
                for site in fixture
                    .document
                    .occurrences
                    .iter_mut()
                    .filter(|site| site.role == OccurrenceRole::TypeUse)
                {
                    site.enclosing = Some(SymbolId::from_bytes([13; 20]));
                }
            }
            4 => {
                for site in fixture
                    .document
                    .occurrences
                    .iter_mut()
                    .filter(|site| site.role == OccurrenceRole::TypeUse)
                {
                    site.syntactic_text_hash = content_hash(b"Other");
                    site.target = OccurrenceTarget::Unresolved {
                        text_hash: site.syntactic_text_hash,
                    };
                }
            }
            5 => {
                fixture.document.occurrences[0].syntax_kind =
                    "groovy.identifier.reference".to_owned()
            }
            _ => fixture
                .document
                .entities
                .retain(|entity| entity.canonical_name != "Item"),
        }
        fixture.validate();
        let engine = ResolutionEngine::default();
        let context = ResolverFactContext::new(fixture.content_hash);
        let cancellation = Cancellation::new();
        for document in [
            engine
                .apply(fixture.document.clone(), context, &cancellation)
                .unwrap()
                .document,
            engine
                .apply_document(fixture.document.clone(), context, &cancellation)
                .unwrap(),
            engine
                .apply_document_bounded(fixture.document.clone(), context, &cancellation)
                .unwrap()
                .0,
        ] {
            assert!(
                document
                    .occurrences
                    .iter()
                    .filter(|site| site.role == OccurrenceRole::TypeUse)
                    .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. })),
                "mutation {mutation}"
            );
            assert!(
                !document
                    .relations
                    .iter()
                    .any(|relation| relation.predicate == RelationPredicate::UsesType)
            );
        }
    }
}

#[test]
fn groovy_inherited_types_validate_resumed_provenance_and_cancellation() {
    let (fixture, target) = inherited_type_fixture();
    let context = ResolverFactContext::new(fixture.content_hash);
    let limited = ResolutionEngine::new(ResolutionLimits::with_work_limit(8, 1).unwrap());
    let (partial, _) = limited
        .apply_document_bounded(fixture.document.clone(), context, &Cancellation::new())
        .unwrap();
    for mutation in 0..9 {
        let mut document = partial.clone();
        let target_source = document
            .entities
            .iter()
            .find(|entity| entity.id == target)
            .unwrap()
            .evidence
            .source
            .clone()
            .unwrap();
        let header = document
            .occurrences
            .iter_mut()
            .find(|site| site.role == OccurrenceRole::InheritanceUse)
            .unwrap();
        let record = document
            .provenance
            .iter_mut()
            .find(|record| record.id == header.provenance)
            .unwrap();
        let old_id = record.id;
        let old_site = header.id;
        match mutation {
            0 => record.rule = Some("unknown-rule".to_owned()),
            1 => {
                record.producer = ProducerIdentity::new(
                    "foreign-resolver",
                    env!("CARGO_PKG_VERSION"),
                    fixture.content_hash,
                )
                .unwrap()
            }
            2 => record.frontend_version = Some("unknown-version".to_owned()),
            3 => {
                record
                    .evidence_sources
                    .retain(|source| source != &header.source);
                record.input_sources = record.evidence_sources.clone();
            }
            4 => record
                .derivation_parents
                .retain(|parent| !matches!(parent, FactRef::Fact(_))),
            5 => header.target = OccurrenceTarget::Resolved { symbol: target },
            6 => record.language = "foreign".to_owned(),
            7 => record.id = FactId::from_bytes([99; 20]),
            _ => {
                let OccurrenceTarget::Resolved { symbol: previous } = header.target else {
                    unreachable!()
                };
                header.target = OccurrenceTarget::Resolved { symbol: target };
                for parent in &mut record.derivation_parents {
                    if *parent == FactRef::Entity(previous) {
                        *parent = FactRef::Entity(target);
                    }
                }
                record.derivation_parents.sort_unstable();
                record.derivation_parents.dedup();
                record.evidence_sources.push(target_source);
                record.evidence_sources.sort_unstable();
                record.evidence_sources.dedup();
                record.input_sources = record.evidence_sources.clone();
            }
        }
        if mutation != 7 {
            record.id = rootlight_ir::derive_provenance_record_id(record).unwrap();
        }
        header.provenance = record.id;
        header.id = rootlight_ir::derive_occurrence_record_id(header).unwrap();
        for relation in &mut document.relations {
            for parent in &mut relation.evidence.derivation {
                if *parent == FactRef::Fact(old_site) {
                    *parent = FactRef::Fact(header.id);
                }
            }
            if relation.provenance == old_id {
                relation.provenance = record.id;
                relation.id = rootlight_ir::derive_relation_record_id(relation).unwrap();
            }
        }
        validate_ir_document(
            &document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .unwrap();
        let resumed = ResolutionEngine::default()
            .apply_document(document, context, &Cancellation::new())
            .unwrap();
        assert!(
            resumed
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::TypeUse)
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. })),
            "mutation {mutation}"
        );
    }
    let engine = ResolutionEngine::default();
    let cancellation = Cancellation::new();
    assert!(cancellation.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        engine.estimate_work(&fixture.document, &cancellation),
        Err(ResolutionError::Cancelled(_))
    ));
    assert!(matches!(
        engine.resolve(&fixture.document, &cancellation),
        Err(ResolutionError::Cancelled(_))
    ));
    assert!(matches!(
        engine.apply(fixture.document.clone(), context, &cancellation),
        Err(ResolutionError::Cancelled(_))
    ));
    assert!(matches!(
        engine.apply_document(fixture.document.clone(), context, &cancellation),
        Err(ResolutionError::Cancelled(_))
    ));
    assert!(matches!(
        engine.apply_document_bounded(fixture.document, context, &cancellation),
        Err(ResolutionError::Cancelled(_))
    ));
}

fn inherited_type_fixture() -> (Fixture, SymbolId) {
    let (mut fixture, parent) = qualified_header_fixture();
    let owner = fixture.document.occurrences[0].enclosing.unwrap();
    let owner_span = fixture
        .document
        .entities
        .iter()
        .find(|entity| entity.id == owner)
        .unwrap()
        .evidence
        .source
        .as_ref()
        .unwrap()
        .span();
    let file = fixture.document.files[1].id;
    let target = fixture.add_entity(31, "Item", file, EntityKind::Class, Some(parent));
    let entity = fixture.document.entities.last_mut().unwrap();
    entity.language = "groovy".to_owned();
    entity.visibility = EntityVisibility::Public;
    entity.evidence.source = Some(source_ref(
        fixture.document.repository,
        fixture.document.generation,
        file,
        fixture.content_hash,
        8,
        16,
    ));
    for (identity, start) in [(32, 32), (33, 40)] {
        fixture.add_occurrence(
            identity,
            "Item",
            fixture.primary_file,
            OccurrenceRole::TypeUse,
            Some(owner),
        );
        let source = source_ref(
            fixture.document.repository,
            fixture.document.generation,
            fixture.primary_file,
            fixture.content_hash,
            start,
            start + 4,
        );
        let site = fixture.document.occurrences.last_mut().unwrap();
        site.source = source.clone();
        site.evidence.source = Some(source.clone());
        site.syntax_kind = "groovy.type_identifier.reference".to_owned();
        fixture.document.extensions.push(
            rootlight_ir::new_groovy_type_lookup_envelope(
                fixture.document.repository,
                fixture.document.generation,
                fixture.provenance,
                source,
                rootlight_ir::GroovyTypeLookup {
                    owner: owner_span,
                    name: "Item".to_owned(),
                    fallback_import: None,
                    fallback_local: None,
                },
            )
            .unwrap(),
        );
    }
    refresh_coverage(&mut fixture);
    for file in &fixture.document.files {
        let count = fixture
            .document
            .occurrences
            .iter()
            .filter(|site| site.file == file.id)
            .count();
        let mut record = CoverageRecord {
            id: FactId::from_bytes([0; 20]),
            repository: fixture.document.repository,
            generation: fixture.document.generation,
            scope: CoverageScope::File(file.id),
            domain: FactDomain::Occurrences,
            tier: AnalysisTier::TierB,
            status: CoverageStatus::Complete,
            discovered: u64::try_from(count).unwrap(),
            indexed: u64::try_from(count).unwrap(),
            skipped: 0,
            provenance: file.provenance,
            evidence: file.evidence.clone(),
        };
        record.id = derive_coverage_record_id(&record).unwrap();
        fixture.document.coverage_records.push(record);
    }
    fixture.validate();
    (fixture, target)
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
