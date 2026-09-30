//! Authored ordered-multiset and work witnesses for retained occurrence views.
//! Ambiguous records remain separate inputs even when their IDs or spans match.

use super::*;
use rootlight_ids::{FactId, FileId, GenerationId, RepositoryId, SymbolId, content_hash};
use rootlight_ir::{Confidence, CoverageStatus, FactEvidence, SourceRef, SourceSpan};

fn unresolved() -> OccurrenceTarget {
    OccurrenceTarget::Unresolved {
        text_hash: content_hash(b"item"),
    }
}

fn resolved() -> OccurrenceTarget {
    OccurrenceTarget::Resolved {
        symbol: SymbolId::from_bytes([7; 20]),
    }
}

fn candidates() -> OccurrenceTarget {
    OccurrenceTarget::Candidates {
        symbols: vec![SymbolId::from_bytes([7; 20]), SymbolId::from_bytes([8; 20])],
        total_count: 2,
        completeness: CoverageStatus::Complete,
    }
}

fn site(
    id: usize,
    start: u64,
    role: OccurrenceRole,
    target: OccurrenceTarget,
    syntax: &str,
) -> OccurrenceRecord {
    let repository = RepositoryId::from_bytes([1; 16]);
    let generation = GenerationId::from_bytes([2; 20]);
    let file = FileId::from_bytes([3; 20]);
    let mut fact = [0; 20];
    fact[..8].copy_from_slice(&u64::try_from(id).unwrap().to_le_bytes());
    OccurrenceRecord {
        id: FactId::from_bytes(fact),
        repository,
        generation,
        file,
        source: SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, start, start + 4).unwrap(),
            content_hash(b"item item item item item item item item item item"),
            None,
        ),
        role,
        enclosing: Some(SymbolId::from_bytes([9; 20])),
        target,
        syntactic_text_hash: content_hash(b"item"),
        syntax_kind: syntax.to_owned(),
        provenance: FactId::from_bytes([6; 20]),
        confidence: Confidence::new(0).unwrap(),
        evidence: FactEvidence {
            source: None,
            derivation: Vec::new(),
        },
    }
}

fn mixed_sites() -> Vec<OccurrenceRecord> {
    use OccurrenceRole::{CallSite, Definition, ImplementationUse, Reference, TypeUse, Write};

    let rows = [
        (Reference, unresolved(), "vba.procedure_pointer.reference"),
        (TypeUse, unresolved(), "vba.procedure_pointer.reference"),
        (
            TypeUse,
            unresolved(),
            "vba.direct_type_identifier.reference",
        ),
        (
            Reference,
            resolved(),
            "vba.direct_type_identifier.reference",
        ),
        (
            ImplementationUse,
            unresolved(),
            "vba.implemented_type.reference",
        ),
        (TypeUse, unresolved(), "vba.implemented_type.reference"),
        (Definition, resolved(), "authored.definition"),
        (
            TypeUse,
            resolved(),
            "vba.constructed_type_identifier.reference",
        ),
        (CallSite, candidates(), "vba.application.reference"),
        (TypeUse, unresolved(), "vba.application.reference"),
        (Reference, resolved(), "vba.member.reference"),
        (Write, resolved(), "vba.get_member.reference"),
        (Definition, resolved(), "vba.invoked_member.reference"),
        (Reference, unresolved(), "vba.nonvalue_identifier.reference"),
        (Reference, unresolved(), "vba.construction.reference"),
        (Reference, unresolved(), "vba.value_identifier.reference"),
    ];
    let mut sites: Vec<_> = rows
        .into_iter()
        .enumerate()
        .map(|(position, (role, target, syntax))| {
            site(
                100 - position,
                40 - u64::try_from(position).unwrap(),
                role,
                target,
                syntax,
            )
        })
        .collect();
    sites.push(sites[6].clone());
    let mut same_span = sites[10].clone();
    same_span.id = FactId::from_bytes([99; 20]);
    sites.push(same_span);
    sites.push(site(
        3,
        1,
        TypeUse,
        unresolved(),
        "vba.constructed_type_identifier.reference",
    ));
    sites.push(site(
        2,
        0,
        Definition,
        resolved(),
        "vba.direct_type_identifier.reference",
    ));
    sites.push(site(1, 0, Reference, candidates(), "authored.ordinary"));
    sites.push(site(
        4,
        5,
        Reference,
        resolved(),
        "vba.value_identifier.reference",
    ));
    sites.push(site(
        5,
        10,
        Reference,
        resolved(),
        "vba.identifier.reference",
    ));
    sites.push(site(6, 15, Write, resolved(), "vba.application.reference"));
    sites.push(sites[21].clone());
    let mut same_receiver_span = sites[22].clone();
    same_receiver_span.id = FactId::from_bytes([98; 20]);
    sites.push(same_receiver_span);
    sites
}

fn record_all(
    sites: &[OccurrenceRecord],
    cancellation: &Cancellation,
    work: &mut ResolutionWorkBudget,
) -> Result<OccurrenceViews, ResolutionError> {
    let mut views = OccurrenceViews::default();
    for (position, site) in sites.iter().enumerate() {
        views.record(position, site, cancellation, work)?;
    }
    Ok(views)
}

#[test]
fn occurrence_views_preserve_exact_order_multiplicity_and_overlapping_unions() {
    use OccurrenceView::{
        Calls, ConstructedTypes, Constructions, MemberApplications, Members, Pointers,
        TypeAssociation, TypeLookup,
    };

    let sites = mixed_sites();
    let unchanged = sites.clone();
    let mut work = ResolutionWorkBudget::new(63);
    let views = record_all(&sites, &Cancellation::new(), &mut work).unwrap();
    assert_eq!(work.consumed(), 63);
    let expected: [(OccurrenceView, &[usize]); VIEW_COUNT] = [
        (Pointers, &[0]),
        (TypeLookup, &[2, 4, 5, 18]),
        (TypeAssociation, &[2, 3, 6, 12, 16, 19, 21, 22, 23, 24, 25]),
        (ConstructedTypes, &[7, 18]),
        (Constructions, &[14]),
        (Calls, &[6, 8, 9, 10, 12, 13, 16, 17, 19, 23]),
        (Members, &[10, 11, 12, 17]),
        (MemberApplications, &[9, 10, 15, 17]),
    ];
    for (view, positions) in expected {
        assert_eq!(views.positions[view.index()], positions, "{view:?}");
        let borrowed: Vec<_> = views.iter(view, &sites).collect();
        assert_eq!(borrowed.len(), positions.len());
        for (site, position) in borrowed.into_iter().zip(positions) {
            assert!(std::ptr::eq(site, &sites[*position]));
        }
    }
    assert_eq!(sites, unchanged);
    assert_eq!(sites[6], sites[16]);
    assert_eq!(sites[10].source, sites[17].source);
    assert_ne!(sites[10].id, sites[17].id);
    assert_eq!(sites[21], sites[24]);
    assert_eq!(sites[22].source, sites[25].source);
    assert_ne!(sites[22].id, sites[25].id);
}

#[test]
fn occurrence_views_precharge_entries_before_mutation_and_propagate_cancellation() {
    let sites = mixed_sites();
    let member = &sites[10];
    for maximum in [0, 1, 2, 3] {
        let mut views = OccurrenceViews::default();
        let mut seed_work = ResolutionWorkBudget::new(2);
        views
            .record(0, &sites[0], &Cancellation::new(), &mut seed_work)
            .unwrap();
        let before = views.positions.clone();
        let mut work = ResolutionWorkBudget::new(maximum);
        assert!(matches!(
            views.record(1, member, &Cancellation::new(), &mut work),
            Err(ResolutionError::WorkLimit { maximum: actual }) if actual == maximum
        ));
        assert_eq!(views.positions, before);
        assert_eq!(work.consumed(), usize::from(maximum != 0));
    }
    let mut exact = ResolutionWorkBudget::new(4);
    let mut views = OccurrenceViews::default();
    views
        .record(0, member, &Cancellation::new(), &mut exact)
        .unwrap();
    assert_eq!(exact.consumed(), 4);
    assert_eq!(views.positions[OccurrenceView::Calls.index()], [0]);
    assert_eq!(views.positions[OccurrenceView::Members.index()], [0]);
    assert_eq!(
        views.positions[OccurrenceView::MemberApplications.index()],
        [0]
    );
    let cancelled = Cancellation::new();
    cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    let before = views.positions.clone();
    let mut work = ResolutionWorkBudget::new(4);
    assert!(matches!(
        views.record(1, member, &cancelled, &mut work),
        Err(ResolutionError::Cancelled(_))
    ));
    assert_eq!(work.consumed(), 0);
    assert_eq!(views.positions, before);
    let empty = record_all(&[], &Cancellation::new(), &mut ResolutionWorkBudget::new(0)).unwrap();
    for view in OccurrenceView::ALL {
        assert_eq!(empty.iter(view, &[]).count(), 0);
    }
}

#[test]
fn occurrence_views_keep_resolved_receiver_and_native_result_tails() {
    use OccurrenceRole::{CallSite, Definition, Read, Reference, TypeUse, Write};

    let mut sites = vec![
        site(
            1,
            0,
            Reference,
            resolved(),
            "vba.value_identifier.reference",
        ),
        site(2, 5, Reference, resolved(), "vba.identifier.reference"),
        site(3, 10, Read, resolved(), "vba.application.reference"),
        site(4, 15, Definition, resolved(), "vba.application.reference"),
        site(5, 20, TypeUse, resolved(), "vba.application.reference"),
        site(6, 0, Write, resolved(), "vba.value_identifier.reference"),
        site(7, 5, TypeUse, resolved(), "vba.identifier.reference"),
        site(
            8,
            0,
            Reference,
            unresolved(),
            "vba.value_identifier.reference",
        ),
        site(9, 5, Reference, candidates(), "vba.identifier.reference"),
        site(10, 10, CallSite, candidates(), "vba.application.reference"),
        site(11, 20, TypeUse, unresolved(), "vba.application.reference"),
    ];
    sites.push(sites[0].clone());
    let mut same_span = sites[1].clone();
    same_span.id = FactId::from_bytes([97; 20]);
    sites.push(same_span);
    let prefix_only: Vec<_> = sites
        .iter()
        .enumerate()
        .filter_map(|(position, site)| {
            ((site.role == Definition && matches!(site.target, OccurrenceTarget::Resolved { .. }))
                || matches!(
                    site.syntax_kind.as_str(),
                    "vba.direct_type_identifier.reference"
                        | "vba.automatic_type_identifier.reference"
                        | "vba.type_identifier.reference"
                ))
            .then_some(position)
        })
        .collect();
    assert_eq!(prefix_only, [3]);
    let views = record_all(
        &sites,
        &Cancellation::new(),
        &mut ResolutionWorkBudget::new(50),
    )
    .unwrap();
    let expected = [0, 1, 2, 3, 4, 11, 12];
    assert_eq!(
        views.positions[OccurrenceView::TypeAssociation.index()],
        expected
    );
    for (actual, position) in views
        .iter(OccurrenceView::TypeAssociation, &sites)
        .zip(expected)
    {
        assert!(std::ptr::eq(actual, &sites[position]));
    }
    assert_eq!(
        views.positions[OccurrenceView::TypeAssociation.index()]
            .iter()
            .filter(|position| **position == 3)
            .count(),
        1
    );
}

fn original_accepts(view: OccurrenceView, site: &OccurrenceRecord) -> bool {
    let unresolved = matches!(site.target, OccurrenceTarget::Unresolved { .. });
    let definition = site.role == OccurrenceRole::Definition
        && matches!(site.target, OccurrenceTarget::Resolved { .. });
    let member = matches!(
        site.syntax_kind.as_str(),
        "vba.member.reference"
            | "vba.invoked_member.reference"
            | "vba.get_member.reference"
            | "vba.let_member.reference"
            | "vba.set_member.reference"
            | "vba.invoked_get_member.reference"
            | "vba.invoked_let_member.reference"
            | "vba.invoked_set_member.reference"
    );
    match view {
        OccurrenceView::Pointers => {
            site.role == OccurrenceRole::Reference
                && unresolved
                && matches!(
                    site.syntax_kind.as_str(),
                    "vba.procedure_pointer.reference" | "vba.qualified_procedure_pointer.reference"
                )
        }
        OccurrenceView::TypeLookup => {
            let implementation = site.role == OccurrenceRole::ImplementationUse
                && site.syntax_kind == "vba.implemented_type.reference";
            (implementation || site.role == OccurrenceRole::TypeUse)
                && unresolved
                && matches!(
                    site.syntax_kind.as_str(),
                    "vba.direct_type_identifier.reference"
                        | "vba.implemented_type.reference"
                        | "vba.type_identifier.reference"
                        | "vba.automatic_type_identifier.reference"
                        | "vba.constructed_type_identifier.reference"
                        | "vba.tested_type_identifier.reference"
                )
        }
        OccurrenceView::TypeAssociation => {
            definition
                || matches!(
                    site.syntax_kind.as_str(),
                    "vba.direct_type_identifier.reference"
                        | "vba.automatic_type_identifier.reference"
                        | "vba.type_identifier.reference"
                )
                || (site.role == OccurrenceRole::Reference
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })
                    && matches!(
                        site.syntax_kind.as_str(),
                        "vba.value_identifier.reference" | "vba.identifier.reference"
                    ))
                || (site.syntax_kind == "vba.application.reference"
                    && matches!(site.target, OccurrenceTarget::Resolved { .. }))
        }
        OccurrenceView::ConstructedTypes => {
            site.role == OccurrenceRole::TypeUse
                && site.syntax_kind == "vba.constructed_type_identifier.reference"
        }
        OccurrenceView::Constructions => {
            site.role == OccurrenceRole::Reference
                && unresolved
                && site.syntax_kind == "vba.construction.reference"
        }
        OccurrenceView::Calls => {
            definition
                || site.syntax_kind == "vba.application.reference"
                || (site.role == OccurrenceRole::Reference
                    && unresolved
                    && site.syntax_kind == "vba.nonvalue_identifier.reference")
                || matches!(
                    site.syntax_kind.as_str(),
                    "vba.invoked_member.reference"
                        | "vba.invoked_get_member.reference"
                        | "vba.invoked_let_member.reference"
                        | "vba.invoked_set_member.reference"
                        | "vba.invoked_get_identifier.reference"
                        | "vba.invoked_let_identifier.reference"
                        | "vba.invoked_set_identifier.reference"
                        | "vba.invoked_identifier.reference"
                        | "vba.member.reference"
                )
        }
        OccurrenceView::Members => member,
        OccurrenceView::MemberApplications => {
            (site.syntax_kind == "vba.application.reference" && unresolved)
                || (site.syntax_kind == "vba.value_identifier.reference"
                    && site.role == OccurrenceRole::Reference
                    && unresolved)
                || (member && site.role == OccurrenceRole::Reference)
        }
    }
}

#[test]
fn occurrence_views_match_original_predicates_across_roles_targets_and_syntaxes() {
    use OccurrenceRole::*;
    let roles = [
        Definition,
        Declaration,
        Reference,
        CallSite,
        TypeUse,
        ImportUse,
        Write,
        Read,
        InheritanceUse,
        ImplementationUse,
        DecoratorUse,
        MacroUse,
        RouteUse,
        TestUse,
        Documentation,
        StringEvidence,
    ];
    let syntaxes = [
        "authored.ordinary",
        "vba.procedure_pointer.reference",
        "vba.qualified_procedure_pointer.reference",
        "vba.direct_type_identifier.reference",
        "vba.implemented_type.reference",
        "vba.type_identifier.reference",
        "vba.automatic_type_identifier.reference",
        "vba.constructed_type_identifier.reference",
        "vba.tested_type_identifier.reference",
        "vba.construction.reference",
        "vba.nonvalue_identifier.reference",
        "vba.application.reference",
        "vba.member.reference",
        "vba.get_member.reference",
        "vba.let_member.reference",
        "vba.set_member.reference",
        "vba.invoked_member.reference",
        "vba.invoked_get_member.reference",
        "vba.invoked_let_member.reference",
        "vba.invoked_set_member.reference",
        "vba.invoked_get_identifier.reference",
        "vba.invoked_let_identifier.reference",
        "vba.invoked_set_identifier.reference",
        "vba.invoked_identifier.reference",
        "vba.value_identifier.reference",
        "vba.identifier.reference",
    ];
    let mut sites = Vec::new();
    for syntax in syntaxes {
        for role in roles {
            for target in [unresolved(), resolved(), candidates()] {
                sites.push(site(sites.len(), 0, role, target, syntax));
            }
        }
    }
    let views = record_all(
        &sites,
        &Cancellation::new(),
        &mut ResolutionWorkBudget::new(20_000),
    )
    .unwrap();
    for view in OccurrenceView::ALL {
        let expected: Vec<_> = sites
            .iter()
            .enumerate()
            .filter_map(|(position, site)| original_accepts(view, site).then_some(position))
            .collect();
        assert_eq!(views.positions[view.index()], expected, "{view:?}");
    }
}

#[test]
fn occurrence_views_preserve_seven_sparse_pass_results_with_bounded_work() {
    let mut sites: Vec<_> = (0..2048)
        .map(|id| {
            site(
                id,
                0,
                OccurrenceRole::Read,
                unresolved(),
                "authored.ordinary",
            )
        })
        .collect();
    sites.extend(mixed_sites());
    let mut measured = ResolutionWorkBudget::new(30_000);
    let mut views = OccurrenceViews::default();
    for (position, site) in sites.iter().enumerate() {
        measured.consume().unwrap();
        views
            .record(position, site, &Cancellation::new(), &mut measured)
            .unwrap();
    }
    let mut results = Vec::new();
    for view in OccurrenceView::ALL.into_iter().take(7) {
        let mut ids = Vec::new();
        for site in views.iter(view, &sites) {
            measured.consume().unwrap();
            ids.push(site.id);
        }
        results.push(ids);
    }
    let required = measured.consumed();
    assert!(required < sites.len() * 8);
    let old = |work: &mut ResolutionWorkBudget| -> Result<Vec<Vec<FactId>>, ResolutionError> {
        for _ in &sites {
            work.consume()?;
        }
        let mut results = Vec::new();
        for view in OccurrenceView::ALL.into_iter().take(7) {
            let mut ids = Vec::new();
            for site in &sites {
                work.consume()?;
                if original_accepts(view, site) {
                    ids.push(site.id);
                }
            }
            results.push(ids);
        }
        Ok(results)
    };
    assert_eq!(
        old(&mut ResolutionWorkBudget::new(sites.len() * 8)).unwrap(),
        results
    );
    assert!(
        matches!(old(&mut ResolutionWorkBudget::new(required)), Err(ResolutionError::WorkLimit { maximum }) if maximum == required)
    );
    let execute = |work: &mut ResolutionWorkBudget| -> Result<Vec<Vec<FactId>>, ResolutionError> {
        let mut views = OccurrenceViews::default();
        for (position, site) in sites.iter().enumerate() {
            work.consume()?;
            views.record(position, site, &Cancellation::new(), work)?;
        }
        let mut results = Vec::new();
        for view in OccurrenceView::ALL.into_iter().take(7) {
            let mut ids = Vec::new();
            for site in views.iter(view, &sites) {
                work.consume()?;
                ids.push(site.id);
            }
            results.push(ids);
        }
        Ok(results)
    };
    let mut exact = ResolutionWorkBudget::new(required);
    assert_eq!(execute(&mut exact).unwrap(), results);
    assert_eq!(exact.remaining(), 0);
    assert!(
        matches!(execute(&mut ResolutionWorkBudget::new(required - 1)), Err(ResolutionError::WorkLimit { maximum }) if maximum == required - 1)
    );
}
