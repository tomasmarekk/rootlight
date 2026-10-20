//! Native Modelica binding barriers survive the service's generic resolver.
//! Exact adapter-certified input reads retain their original source identity.

use super::*;

#[test]
fn rejected_native_bindings_do_not_gain_generic_spelling_candidates() {
    for (case, source, marker, marker_offset) in [
        (
            "loop",
            "function f input Real x; output Real y; algorithm for x in 1:2 loop y := x; end for; end f;",
            ":= x",
            3,
        ),
        (
            "local",
            "function f input Real x; Real x; output Real y; algorithm y := x; end f;",
            ":= x",
            3,
        ),
        (
            "output",
            "function f input Real x; output Real x; algorithm x := x; end f;",
            ":= x",
            3,
        ),
        (
            "duplicate",
            "function f input Real x; input Real x; output Real y; algorithm y := x; end f;",
            ":= x",
            3,
        ),
        (
            "reduction",
            "function f input Real x; output Real y; algorithm y := sum(x for x in 1:3); end f;",
            "sum(x",
            4,
        ),
    ] {
        let directory = TempDir::new().unwrap();
        fs::write(directory.path().join("source.mo"), source).unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository(directory.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let offset = u64::try_from(source.find(marker).unwrap() + marker_offset).unwrap();
        let sites = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == offset
                    && site.source.span().end_byte() == offset + 1
            })
            .collect::<Vec<_>>();
        assert_eq!(sites.len(), 1, "{case}: exact authored read is retained");
        let site = sites[0];
        assert_eq!(
            site.syntax_kind, "modelica.unresolved_read.reference",
            "{case}"
        );
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{case}: a rejected native binding must not become a same-spelling candidate: {:?}",
            site.target
        );
        assert_eq!(site.source.repository(), receipt.repository);
        assert_eq!(site.source.generation(), receipt.generation);
        assert_eq!(site.source.content_hash(), content_hash(source.as_bytes()));
        assert!(
            document.skipped_regions.iter().any(|gap| {
                gap.detail == "modelica-input-binding-unavailable"
                    && gap.source.span() == site.source.span()
                    && gap.source.repository() == site.source.repository()
                    && gap.source.generation() == site.source.generation()
                    && gap.source.content_hash() == site.source.content_hash()
            }),
            "{case}: exact binding gap survives"
        );
    }
}

#[test]
fn exact_native_input_read_is_unchanged_by_generic_resolution_barrier() {
    let source = "function f input Real x; output Real y; algorithm y := x; end f;";
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("source.mo"), source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let input = document
        .entities
        .iter()
        .find(|entity| {
            entity.language == "modelica"
                && entity.kind == EntityKind::Parameter
                && entity.canonical_name == "x"
        })
        .unwrap();
    let offset = u64::try_from(source.find(":= x").unwrap() + 3).unwrap();
    let read = document
        .occurrences
        .iter()
        .find(|site| site.role == OccurrenceRole::Read && site.source.span().start_byte() == offset)
        .unwrap();
    assert_eq!(read.syntax_kind, "modelica.input_read.reference");
    assert_eq!(read.target, OccurrenceTarget::Resolved { symbol: input.id });
    assert_eq!(read.source.repository(), receipt.repository);
    assert_eq!(read.source.generation(), receipt.generation);
    assert_eq!(read.source.content_hash(), content_hash(source.as_bytes()));
    assert_eq!(read.source.span().end_byte(), offset + 1);
}
