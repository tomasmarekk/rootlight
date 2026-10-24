//! Exercise CUDA proof preservation through production bounded resolution.
//! CUDA-backed headers retain their existing ordinary Cpp resolver admission.

use super::*;
use rootlight_ir::OccurrenceTarget;
use rootlight_resolve::{RejectedCandidate, RejectionReason, ResolutionOutcome, UnresolvedReason};
use std::fs;

const TEXT: &str = "namespace other { void missing(int value) {} }\n__global__ void kernel(int value) {}\nvoid launch(int scalar) { int next = scalar * 2; kernel<<<1, 1>>>(next); missing(next); }\n";

#[test]
fn cuda_service_resolution_preserves_proven_facets_and_unproved_original_sites() {
    for (language, path) in [("cuda", "sample.cu"), ("cpp", "sample.cuh")] {
        let fixture = tempfile::tempdir().unwrap();
        fs::write(fixture.path().join(path), TEXT).unwrap();
        let repository = derive_repository(b"cuda-service-proof-preservation").id();
        let root = RepositoryRoot::open(repository, fixture.path()).unwrap();
        let snapshot = root
            .snapshot(&RelativePath::parse(Path::new(path)).unwrap(), 4096)
            .unwrap();
        let service = FirstSliceService::new(2).unwrap();
        let analyzer = service.analyzers.get(language).unwrap();
        let cancel = Cancellation::with_deadline(Instant::now() + Duration::from_secs(30));
        let request = structural_analysis_request(
            repository,
            GenerationId::from_bytes([1; 20]),
            &snapshot,
            false,
            language,
            "utf-8",
            &service.analysis_limits,
        )
        .unwrap();
        let (output, artifact) = analyzer
            .analyze_and_capture(
                &request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )
            .unwrap();
        let replayed = analyzer
            .analyze_from_artifact(
                &request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )
            .unwrap();
        for document in [output.document(), replayed.document()] {
            let applied = apply_bounded_resolution_with_native_projects(
                document.clone(),
                ResolutionLimits::default(),
                ResolverFactContext::new(snapshot.content_hash()),
                &[],
                &[],
                &cancel,
            )
            .unwrap();
            for label in ["cuda.scalar_formal.read", "cuda.kernel_launch.call"] {
                let before: Vec<_> = document
                    .occurrences
                    .iter()
                    .filter(|site| site.syntax_kind == label)
                    .collect();
                assert!(
                    !before.is_empty(),
                    "native positive premise for {language}/{label}"
                );
                for site in before {
                    assert!(matches!(site.target, OccurrenceTarget::Resolved { .. }));
                    assert!(
                        applied
                            .occurrences
                            .iter()
                            .any(|after| after.source == site.source
                                && after.syntax_kind == label
                                && after.target == site.target)
                    );
                }
            }
            let engine = ResolutionEngine::default();
            let decisions = engine.resolve(document, &cancel).unwrap();
            let ordinary: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.syntax_kind == format!("{language}.identifier.reference")
                        && matches!(site.target, OccurrenceTarget::Unresolved { .. })
                })
                .collect();
            assert!(!ordinary.is_empty());
            if language == "cuda" {
                for site in ordinary {
                    assert!(
                        !decisions
                            .decisions
                            .iter()
                            .any(|decision| decision.occurrence == site.id)
                    );
                    assert!(
                        applied.occurrences.iter().any(|after| after == site),
                        "the service retains the complete original unresolved CUDA fact"
                    );
                }
            } else {
                let scalar = *ordinary
                    .iter()
                    .find(|site| site.syntactic_text_hash == content_hash(b"scalar"))
                    .unwrap();
                let scalar_decisions: Vec<_> = decisions
                    .decisions
                    .iter()
                    .filter(|decision| decision.occurrence == scalar.id)
                    .collect();
                assert_eq!(
                    scalar_decisions.len(),
                    1,
                    "ordinary Cpp retains generic resolution work"
                );
                let decision = scalar_decisions[0];
                let parameters: Vec<_> = document
                    .entities
                    .iter()
                    .filter(|entity| {
                        entity.language == "cpp"
                            && entity.kind == EntityKind::Parameter
                            && entity.canonical_name == "scalar"
                    })
                    .collect();
                assert_eq!(parameters.len(), 1);
                assert!(
                    matches!(
                        &decision.outcome,
                        ResolutionOutcome::Unresolved {
                            reason: UnresolvedReason::NoCandidate,
                            confidence,
                        } if confidence.get() == 0
                    ),
                    "{decision:?}"
                );
                assert!(decision.explanation.candidates.is_empty());
                assert_eq!(decision.explanation.rejected_total, 1);
                assert_eq!(
                    decision.explanation.rejected_candidates,
                    [RejectedCandidate {
                        symbol: parameters[0].id,
                        reason: RejectionReason::MissingLexicalEvidence,
                    }]
                );
                let retained: Vec<_> = applied
                    .occurrences
                    .iter()
                    .filter(|after| {
                        after.source == scalar.source
                            && after.syntax_kind == "cpp.identifier.reference"
                    })
                    .collect();
                assert_eq!(retained, [scalar]);
                eprintln!(
                    "ordinary Cpp scalar refusal: decision={decision:?}; retained={:?}",
                    retained[0]
                );
            }
        }
    }
}
