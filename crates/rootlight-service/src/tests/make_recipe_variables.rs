//! Literal Make recipe variable uses retain source-owned lexical references.
//! Rule ownership and definition leaves are independent of shell execution,
//! expanded values and ambiguous assignments, which remain bounded analysis.

use super::*;
use rootlight_ir::{FactRef, RelationEndpoint};

#[test]
fn make_recipe_variable_uses_have_exact_source_and_rule_owner() {
    let source = b"FRONTEND := frontend\nSTAMP := stamp\nbuild: $(STAMP)\n\tnpm --prefix $(FRONTEND) run build\nother:\n\t@echo $$(FRONTEND) $(PREFIX_$(KEY)) $(shell echo BAD) ${FRONTEND}\n";
    let fixture = TempDir::new().unwrap();
    let path = fixture.path().join("Makefile");
    fs::write(&path, source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let frontend = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "FRONTEND")
        .unwrap();
    let build = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::BuildTarget && entity.canonical_name == "build")
        .unwrap();
    let other = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::BuildTarget && entity.canonical_name == "other")
        .unwrap();
    let references: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == "make.variable.reference"
                && site.target
                    == (OccurrenceTarget::Resolved {
                        symbol: frontend.id,
                    })
        })
        .collect();
    assert_eq!(references.len(), 2);
    for owner in [build, other] {
        let owned: Vec<_> = references
            .iter()
            .copied()
            .filter(|site| site.enclosing == Some(owner.id))
            .collect();
        assert_eq!(owned.len(), 1);
        let reference = owned[0].source.clone();
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.content_hash(), content_hash(source));
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Entity(owner.id)
                    && edge.object == RelationEndpoint::Entity(frontend.id)
            })
            .collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].evidence.source.as_ref(), Some(&reference));
        for proof in [
            FactRef::Entity(owner.id),
            FactRef::Fact(owned[0].id),
            FactRef::Entity(frontend.id),
        ] {
            assert!(edges[0].evidence.derivation.contains(&proof));
        }
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![reference],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, b"FRONTEND");
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| { gap.detail == "make-recipe-not-analyzed" })
    );
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| { matches!(entity.canonical_name.as_str(), "PREFIX_" | "KEY" | "BAD") })
    );
    let before_build = build.id;
    let before_frontend = frontend.id;
    let mut changed = source.to_vec();
    changed.extend_from_slice(b"\t@echo another shell command\n");
    fs::write(&path, changed).unwrap();
    let updated = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    assert_ne!(updated.generation, receipt.generation);
    let updated_snapshot = service
        .loaded_generation_snapshot(updated.generation)
        .unwrap();
    for (name, id) in [("build", before_build), ("FRONTEND", before_frontend)] {
        assert!(
            updated_snapshot
                .document()
                .entities
                .iter()
                .any(|entity| { entity.canonical_name == name && entity.id == id })
        );
    }
}

#[test]
fn make_ambiguous_variable_assignments_do_not_gain_resolved_recipe_edges() {
    let source = b"APP := first\nAPP += second\nbuild:\n\t@echo $(APP)\n";
    let fixture = TempDir::new().unwrap();
    fs::write(fixture.path().join("Makefile"), source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let build = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::BuildTarget && entity.canonical_name == "build")
        .unwrap();
    assert!(document.occurrences.iter().any(|site| {
        site.syntax_kind == "make.variable.reference"
            && !matches!(site.target, OccurrenceTarget::Resolved { .. })
    }));
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::RefersTo
            && edge.subject == RelationEndpoint::Entity(build.id)
    }));
}

// The complete source retains its original CRLF bytes and all existing recipes.
const ORIGINAL_MAKE_PREREQUISITES: &[u8] =
    include_bytes!("../../tests/fixtures/make_prerequisites/Makefile");

fn prerequisite_target<'a>(
    document: &'a rootlight_ir::NormalizedIrDocument,
    name: &str,
) -> &'a rootlight_ir::EntityRecord {
    let matches: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::BuildTarget
                && entity.language == "make"
                && entity.canonical_name == name
        })
        .collect();
    assert_eq!(matches.len(), 1, "{name}");
    matches[0]
}

#[test]
fn make_complete_original_keeps_exact_written_prerequisite_dependency() {
    assert_eq!(ORIGINAL_MAKE_PREREQUISITES.len(), 895);
    assert_eq!(&ORIGINAL_MAKE_PREREQUISITES[73..87], b"default : test");
    let fixture = TempDir::new().unwrap();
    fs::write(fixture.path().join("Makefile"), ORIGINAL_MAKE_PREREQUISITES).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let owner = prerequisite_target(document, "default");
    let target = prerequisite_target(document, "test");
    for (entity, start, end) in [(owner, 73, 80), (target, 91, 95)] {
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
                        && site.source.span().start_byte() == start
                        && site.source.span().end_byte() == end
                })
                .count(),
            1
        );
    }
    let references: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.syntax_kind == "make.target.reference"
                && site.enclosing == Some(owner.id)
                && site.target == OccurrenceTarget::Resolved { symbol: target.id }
                && site.source.span().start_byte() == 83
                && site.source.span().end_byte() == 87
        })
        .collect();
    assert_eq!(references.len(), 1);
    let reference = references[0];
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(reference.id)
                    && edge.object == RelationEndpoint::Entity(target.id)
            })
            .count(),
        1
    );
    let dependencies: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::DependsOn
                && edge.subject == RelationEndpoint::Entity(owner.id)
                && edge.object == RelationEndpoint::Entity(target.id)
        })
        .collect();
    assert_eq!(dependencies.len(), 1);
    let edge = dependencies[0];
    assert_eq!(edge.evidence.source.as_ref(), Some(&reference.source));
    assert_eq!(reference.source.repository(), receipt.repository);
    assert_eq!(reference.source.generation(), receipt.generation);
    assert_eq!(
        reference.source.content_hash(),
        content_hash(ORIGINAL_MAKE_PREREQUISITES)
    );
    for parent in [
        FactRef::Entity(owner.id),
        FactRef::Fact(reference.id),
        FactRef::Entity(target.id),
    ] {
        assert!(edge.evidence.derivation.contains(&parent));
    }
    assert!(!document.relations.iter().any(|edge| {
        (edge.subject == RelationEndpoint::Entity(target.id)
            && edge.object == RelationEndpoint::Entity(owner.id)
            && edge.predicate == RelationPredicate::DependsOn)
            || (edge.subject == RelationEndpoint::Entity(owner.id)
                && edge.object == RelationEndpoint::Entity(target.id)
                && edge.predicate == RelationPredicate::Contains)
    }));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "make-recipe-not-analyzed")
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "make-variable-value-not-analyzed")
    );
    let file = document
        .files
        .iter()
        .find(|file| file.path == "Makefile")
        .unwrap();
    assert_eq!(file.language, "make");
    assert_eq!(file.content_hash, content_hash(ORIGINAL_MAKE_PREREQUISITES));
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![file.evidence.source.clone().unwrap()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(rootlight_source::SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].bytes, ORIGINAL_MAKE_PREREQUISITES);
}

#[test]
fn make_renamed_normal_prerequisites_do_not_promote_other_header_or_recipe_forms() {
    for (source, expected) in [
        ("package: verify\nverify:\n", 1),
        ("package : verify # literal\nverify:\n", 1),
        ("package: verify | cache\nverify:\ncache:\n", 1),
        ("package: | verify\nverify:\n", 0),
        ("package:: verify\nverify:\n", 0),
        ("package other: verify\nverify:\n", 0),
        ("package: X = verify\nverify:\n", 0),
        ("package: $(NAME) verify\nverify:\n", 0),
        ("package:\n\t@echo verify\nverify:\n", 0),
        ("package: verify\nverify:\nverify:\n", 0),
        ("package: verify\npackage: verify\nverify:\n", 0),
        ("package: missing\n", 0),
        (".PHONY: verify\nverify:\n", 0),
        ("package: package\n", 0),
    ] {
        let fixture = TempDir::new().unwrap();
        fs::write(fixture.path().join("Makefile"), source).unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let dependencies: Vec<_> = snapshot
            .document()
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::DependsOn)
            .collect();
        assert_eq!(dependencies.len(), expected, "{source}");
        if expected == 1 {
            let document = snapshot.document();
            assert_eq!(
                dependencies[0].subject,
                RelationEndpoint::Entity(prerequisite_target(document, "package").id)
            );
            assert_eq!(
                dependencies[0].object,
                RelationEndpoint::Entity(prerequisite_target(document, "verify").id)
            );
            let evidence = dependencies[0].evidence.source.as_ref().unwrap();
            let start = u64::try_from(source.find("verify").unwrap()).unwrap();
            assert_eq!(evidence.span().start_byte(), start);
            assert_eq!(evidence.span().end_byte(), start + 6);
        }
    }
}

#[test]
fn make_written_dependencies_reject_variable_target_labels_and_withheld_duplicate_definitions() {
    use rootlight_adapter_sdk::{
        SyntaxFact, SyntaxFactKind, SyntaxKindLabel, testkit::MockParseProvider,
    };
    let fixture = TempDir::new().unwrap();
    let path = fixture.path().join("Makefile");
    let repository = derive_repository(b"make-prerequisite-endpoint-proof").id();
    let generation = GenerationId::from_bytes([65; 20]);
    let root = RepositoryRoot::open(repository, fixture.path()).unwrap();
    let service = FirstSliceService::new(2).unwrap();
    let native = MakeParseProvider::new(65536, 65536).unwrap();
    let native_limits =
        provider_analysis_limits(&service.analysis_limits, native.capabilities()).unwrap();
    for (source, mode) in [
        (b"package: verify\nverify:\n".as_slice(), 0),
        (b"package: verify\nverify := stored\n".as_slice(), 1),
        (b"package: verify\nverify:\nverify:\n".as_slice(), 2),
        (b"package: verify\nholder:\n\tverify:\n".as_slice(), 3),
        (b"package: verify\n# masked \\\nverify:\n".as_slice(), 5),
        (
            b"package: verify\ndefine TEMPLATE\nverify:\nendef\n".as_slice(),
            4,
        ),
    ] {
        fs::write(&path, source).unwrap();
        let snapshot = root
            .snapshot(&RelativePath::parse(Path::new("Makefile")).unwrap(), 65536)
            .unwrap();
        let input = structural_analysis_request(
            repository,
            generation,
            &snapshot,
            false,
            "make",
            "utf-8",
            &native_limits,
        )
        .unwrap();
        let parsed = rootlight_adapter_sdk::execute_parse(
            &native,
            &input.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        let mut facts = parsed.facts().to_vec();
        if mode == 1 {
            let variable = facts
                .iter_mut()
                .find(|fact| {
                    fact.kind() == SyntaxFactKind::Declaration
                        && fact.syntax_kind().as_str() == "make.variable.declaration"
                })
                .unwrap();
            *variable = SyntaxFact::new(
                variable.local_id(),
                variable.parent(),
                variable.kind(),
                variable.span(),
                variable.depth(),
                SyntaxKindLabel::new("make.target.declaration").unwrap(),
            );
        } else if mode == 2 {
            let last = facts
                .iter()
                .filter(|fact| {
                    fact.kind() == SyntaxFactKind::Declaration
                        && fact.syntax_kind().as_str() == "make.target.declaration"
                })
                .max_by_key(|fact| fact.span().start_byte())
                .unwrap()
                .local_id();
            facts.retain(|fact| {
                !(fact.parent() == Some(last)
                    && fact.syntax_kind().as_str() == "make.name.definition")
            });
        }
        if matches!(mode, 3 | 4) {
            let start = std::str::from_utf8(source)
                .unwrap()
                .rfind("verify:")
                .unwrap() as u64;
            let id = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
            let file = input.source().source_ref().span().file();
            facts.push(SyntaxFact::new(
                id,
                Some(1),
                SyntaxFactKind::Declaration,
                rootlight_ir::SourceSpan::new(file, start, start + 7).unwrap(),
                1,
                SyntaxKindLabel::new("make.target.declaration").unwrap(),
            ));
            facts.push(SyntaxFact::new(
                id + 1,
                Some(id),
                SyntaxFactKind::Occurrence,
                rootlight_ir::SourceSpan::new(file, start, start + 6).unwrap(),
                2,
                SyntaxKindLabel::new("make.name.definition").unwrap(),
            ));
        }
        let mock = MockParseProvider::new(
            native.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let analyzer = TreeSitterAnalyzer::new(
            Arc::new(mock),
            ProducerIdentity::new("make-endpoint-proof", "1.0", first_slice_build_context())
                .unwrap(),
            LanguageId::new("make").unwrap(),
            "make-endpoint-proof",
            MakeParseProvider::binary_digest(),
        )
        .unwrap();
        let result = rootlight_adapter_sdk::execute_analysis(
            &analyzer,
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        let document = result.document();
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::DependsOn)
            .collect();
        assert_eq!(edges.len(), usize::from(mode == 0), "mode {mode}");
        if mode == 0 {
            let owner = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "package")
                .unwrap();
            let target = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "verify")
                .unwrap();
            assert_eq!(edges[0].subject, RelationEndpoint::Entity(owner.id));
            assert_eq!(edges[0].object, RelationEndpoint::Entity(target.id));
            assert_eq!(
                edges[0]
                    .evidence
                    .source
                    .as_ref()
                    .unwrap()
                    .span()
                    .start_byte(),
                9
            );
            assert_eq!(
                edges[0].evidence.source.as_ref().unwrap().span().end_byte(),
                15
            );
        }
        assert!(
            document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo)
        );
        rootlight_ir::validate_ir_document(
            document,
            service.analysis_limits.ir(),
            &ExtensionSupport::default(),
        )
        .unwrap();
    }
}
