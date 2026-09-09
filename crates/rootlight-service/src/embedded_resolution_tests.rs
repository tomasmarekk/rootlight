//! Native embedded-source evidence composed with the production resolver.
//! Required-only artifact replay must retain example ownership independently
//! of optional references and the generation in which facts are materialized.

use super::*;
use rootlight_resolve::ResolutionOutcome;
use std::fs;

#[test]
fn embedded_native_resolution_and_replay_preserve_source_scopes() {
    for (host, source, expected_calls) in [
        (
            "markdown",
            concat!(
                "# value\n\n",
                "~~~rust\nfn value() {}\nfn caller() { value(); }\n~~~\n\n",
                "~~~rust\nfn value() {}\nfn caller() { value(); }\n~~~\n\n",
                "~~~rust\nfn alone() { value(); }\n~~~\n\n",
                "~~~yaml\nname: &entry first\ncopy: *entry\n~~~\n\n",
                "~~~yaml\nname: &entry second\ncopy: *entry\n~~~\n\n",
                "~~~toml\n[config]\nname = 'first'\n~~~\n\n",
                "~~~toml\n[config]\nname = 'second'\n~~~\n",
                "\n~~~lua\nlocal stored = 1\nreturn stored\n~~~\n",
                "\n~~~lua\nlocal stored = 2\nreturn stored\n~~~\n",
                "\n~~~lua\nreturn stored\n~~~\n",
            ),
            3,
        ),
        (
            "html",
            "<main><script>function value() { return 1; } value();</script></main>",
            1,
        ),
    ] {
        let fixture = tempfile::tempdir().unwrap();
        fs::write(fixture.path().join("example"), source).unwrap();
        let repository = derive_repository(b"embedded-source-resolution").id();
        let root = RepositoryRoot::open(repository, fixture.path()).unwrap();
        let snapshot = root
            .snapshot(&RelativePath::parse(Path::new("example")).unwrap(), 4096)
            .unwrap();
        let service = FirstSliceService::new(2).unwrap();
        let analyzer = service.analyzers.get(host).unwrap();
        let memory = MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback;
        let cancellation = Cancellation::with_deadline(Instant::now() + Duration::from_secs(30));
        let request = structural_analysis_request(
            repository,
            GenerationId::from_bytes([1; 20]),
            &snapshot,
            false,
            host,
            &service.analysis_limits,
        )
        .unwrap();
        let (full, artifact) = analyzer
            .analyze_and_capture(&request, ExtensionSupport::default(), memory, &cancellation)
            .unwrap();
        let document = full.document();
        let engine = ResolutionEngine::default();
        let batch = engine.resolve(document, &cancellation).unwrap();
        let mut call_count = 0;
        let mut local_targets = BTreeSet::new();
        let mut calls: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::CallSite
                    && occurrence.syntactic_text_hash == content_hash(b"value")
            })
            .collect();
        calls.sort_by_key(|call| call.source.span().start_byte());
        for call in calls {
            call_count += 1;
            let decision = batch
                .decisions
                .iter()
                .find(|entry| entry.occurrence == call.id)
                .unwrap();
            if host == "markdown" && call_count == 3 {
                assert!(
                    matches!(decision.outcome, ResolutionOutcome::Unresolved { .. }),
                    "{decision:?}"
                );
                continue;
            }
            let ResolutionOutcome::Candidates {
                symbols,
                total_count: 1,
                ..
            } = &decision.outcome
            else {
                panic!("native syntax must retain its one local candidate: {decision:?}");
            };
            let target = document
                .entities
                .iter()
                .find(|entity| entity.id == symbols[0])
                .unwrap();
            assert_eq!(
                target.language,
                if host == "markdown" {
                    "rust"
                } else {
                    "javascript"
                }
            );
            assert!(
                target.evidence.source.as_ref().unwrap().span().end_byte()
                    <= call.source.span().start_byte()
            );
            local_targets.insert(target.id);
        }
        assert_eq!(call_count, expected_calls);
        assert_eq!(local_targets.len(), if host == "markdown" { 2 } else { 1 });
        if host == "markdown" {
            let mut references: Vec<_> = document
                .occurrences
                .iter()
                .filter(|item| item.syntax_kind == "lua.identifier.reference")
                .collect();
            references.sort_by_key(|item| item.source.span().start_byte());
            assert_eq!(references.len(), 3);
            let mut targets = BTreeSet::new();
            for reference in &references[..2] {
                let rootlight_ir::OccurrenceTarget::Resolved { symbol } = reference.target else {
                    panic!("native Lua binding must be exact: {reference:?}");
                };
                targets.insert(symbol);
                assert!(
                    !batch
                        .decisions
                        .iter()
                        .any(|item| item.occurrence == reference.id)
                );
                assert!(document.relations.iter().any(|relation| {
                    relation.predicate == rootlight_ir::RelationPredicate::RefersTo
                        && relation.subject
                            == rootlight_ir::RelationEndpoint::Occurrence(reference.id)
                        && relation.object == rootlight_ir::RelationEndpoint::Entity(symbol)
                        && relation.evidence.source.as_ref() == Some(&reference.source)
                }));
            }
            assert_eq!(targets.len(), 2);
            let missing = batch
                .decisions
                .iter()
                .find(|item| item.occurrence == references[2].id)
                .unwrap();
            assert!(matches!(
                missing.outcome,
                ResolutionOutcome::Unresolved { .. }
            ));
        }
        let required = artifact.required_syntax_fact_count(&cancellation).unwrap();
        let limits =
            analysis_limits_with_syntax_records(&service.analysis_limits, required).unwrap();
        let request = structural_analysis_request(
            repository,
            GenerationId::from_bytes([2; 20]),
            &snapshot,
            false,
            host,
            &limits,
        )
        .unwrap();
        let (bounded, artifact) = analyzer
            .analyze_and_capture(&request, ExtensionSupport::default(), memory, &cancellation)
            .unwrap();
        let request = structural_analysis_request(
            repository,
            GenerationId::from_bytes([3; 20]),
            &snapshot,
            false,
            host,
            &limits,
        )
        .unwrap();
        let (fresh, _) = analyzer
            .analyze_and_capture(&request, ExtensionSupport::default(), memory, &cancellation)
            .unwrap();
        let replay = analyzer
            .analyze_from_artifact(
                &request,
                &artifact,
                ExtensionSupport::default(),
                memory,
                &cancellation,
            )
            .unwrap();
        assert_eq!(fresh.document(), replay.document());
        assert_eq!(fresh.report(), replay.report());
        assert_eq!(
            document
                .entities
                .iter()
                .map(|entity| entity.id)
                .collect::<BTreeSet<_>>(),
            bounded
                .document()
                .entities
                .iter()
                .map(|entity| entity.id)
                .collect::<BTreeSet<_>>()
        );
        for entity in &replay.document().entities {
            let reference = entity.evidence.source.as_ref().unwrap();
            assert_eq!(reference.generation(), GenerationId::from_bytes([3; 20]));
            assert_eq!(reference.content_hash(), snapshot.content_hash());
        }
    }
}
