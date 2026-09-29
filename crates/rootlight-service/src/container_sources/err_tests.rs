//! The standard Err model is exercised through native source extraction and
//! durable indexing. Fixtures separate target identity from argument validity
//! and keep runtime error-state semantics outside static binding claims.

use super::*;

#[test]
fn standard_err_contracts_preserve_shadowing_and_durable_lifecycles() {
    let root = tempdir();
    let storage = tempdir();
    let state = storage.path().join("state");
    rootlight_runtime::RuntimePaths::new(state.clone(), storage.path().join("runtime"))
        .unwrap()
        .prepare_owner()
        .unwrap();
    let calls = [
        "Err.Clear",
        "Call Err.Clear()",
        "Err.Raise 7",
        "Call Err.Raise(8)",
        "Err.Raise Number:=9, Description:=\"failure\"",
        "Err.Raise 10, , \"failure\"",
        "VBA.Err.Clear",
        "Call [vBa].[eRr].[Raise](Number:=11, HelpContext:=4)",
    ];
    let properties = [
        ("value = Err.Number", "Err.Number", "Number"),
        ("value = Err.Description", "Err.Description", "Description"),
        ("value = Err.Source", "Err.Source", "Source"),
        ("value = Err.HelpFile", "Err.HelpFile", "HelpFile"),
        ("value = Err.HelpContext", "Err.HelpContext", "HelpContext"),
        (
            "value = Err.LastDllError",
            "Err.LastDllError",
            "LastDllError",
        ),
        ("Err.Number = 12", "Err.Number", "Number"),
        (
            "Err.Description = \"failure\"",
            "Err.Description",
            "Description",
        ),
        ("Err.Source = \"component\"", "Err.Source", "Source"),
        ("Err.HelpFile = \"help.chm\"", "Err.HelpFile", "HelpFile"),
        ("Err.HelpContext = 3", "Err.HelpContext", "HelpContext"),
        ("value = VBA.Err.Number", "VBA.Err.Number", "Number"),
        ("VBA.Err.Source = \"component\"", "VBA.Err.Source", "Source"),
    ];
    let invalid = [
        "Err.Raise(1, 2)",
        "Err.Raise(Number:=1)",
        "Err.Clear()",
        "Call Err.Raise()",
        "Err.Raise Description:=\"missing number\"",
        "Err.Raise Number:=1, number:=2",
        "Err.Raise Missing:=1",
        "Err.Raise 1, 2, 3, 4, 5, 6",
        "Err.Clear 1",
        "Call Err.Clear(1)",
        "value = Err.Raise(1)",
        "value = Err.Clear()",
        "Err.LastDllError = 1",
        "Set Err.Source = value",
        "Err.Clear = 1",
        "value = Err.Unknown",
        "value = Err.Number$",
        "value = Err$.Number",
        "value = Err.Number(1)",
        "value = Err",
        "With Err\r\nvalue = .Number\r\nEnd With",
    ];
    for (shadow_err, shadow_vba) in [
        (false, false),
        (true, false),
        (false, true),
        (true, true),
        (false, false),
    ] {
        let parameters = [
            (shadow_err, "ByVal Err As Object"),
            (shadow_vba, "ByVal VBA As Object"),
        ]
        .into_iter()
        .filter_map(|(enabled, text)| enabled.then_some(text))
        .collect::<Vec<_>>()
        .join(", ");
        let mut text = format!(
            "Attribute VB_Name = \"Client\"\r\nPublic Sub Run({parameters})\r\nDim value As Variant\r\n"
        );
        let mut expected = BTreeMap::new();
        for expression in calls {
            let start = u64::try_from(text.len()).unwrap();
            text.push_str(expression);
            text.push_str("\r\n");
            let qualified = expression.contains("VBA.") || expression.contains("[vBa]");
            if !(if qualified { shadow_vba } else { shadow_err }) {
                let member = if expression.to_ascii_lowercase().contains("clear") {
                    "Clear"
                } else {
                    "Raise"
                };
                expected.insert(
                    (start, start + u64::try_from(expression.len()).unwrap()),
                    (
                        "vba-err-call-v1.indexed_snapshot",
                        format!("VBA.Err.{member}"),
                    ),
                );
            }
        }
        for (statement, expression, member) in properties {
            let start = u64::try_from(text.len() + statement.find(expression).unwrap()).unwrap();
            text.push_str(statement);
            text.push_str("\r\n");
            if !(if expression.starts_with("VBA.") {
                shadow_vba
            } else {
                shadow_err
            }) {
                expected.insert(
                    (start, start + u64::try_from(expression.len()).unwrap()),
                    (
                        "vba-err-property-v1.indexed_snapshot",
                        format!("VBA.Err.{member}"),
                    ),
                );
            }
        }
        for statement in invalid {
            text.push_str(statement);
            text.push_str("\r\n");
        }
        text.push_str("End Sub\r\n");
        fs::write(
            root.path().join("book.xlsm"),
            fixture::book(&[("Client", text.as_bytes())]),
        )
        .unwrap();
        let mut service = FirstSliceService::new_durable(4, &state, &deadline()).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_eq!(
            receipt,
            service.index_repository(root.path(), &deadline()).unwrap()
        );
        drop(service);
        let mut service = FirstSliceService::new_durable(4, &state, &deadline()).unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let mut actual = BTreeMap::new();
        let mut call_symbols = BTreeSet::new();
        let mut call_count = 0;
        for site in &document.occurrences {
            let rule = document
                .provenance
                .iter()
                .find(|p| p.id == site.provenance)
                .unwrap()
                .rule
                .as_deref()
                .unwrap_or_default();
            if !matches!(
                rule,
                "vba-err-call-v1.indexed_snapshot" | "vba-err-property-v1.indexed_snapshot"
            ) {
                continue;
            }
            let OccurrenceTarget::Resolved { symbol } = site.target else {
                panic!("modeled member must resolve")
            };
            let target = document.entities.iter().find(|e| e.id == symbol).unwrap();
            assert_eq!(target.kind, EntityKind::ExternalSymbol);
            assert!(target.evidence.source.is_none());
            let span = site.source.span();
            assert!(
                actual
                    .insert(
                        (span.start_byte(), span.end_byte()),
                        (rule, target.qualified_name.clone())
                    )
                    .is_none()
            );
            let predicate = if rule == "vba-err-call-v1.indexed_snapshot" {
                assert_eq!(site.role, rootlight_ir::OccurrenceRole::CallSite);
                call_symbols.insert(symbol);
                call_count += 1;
                RelationPredicate::Calls
            } else {
                RelationPredicate::RefersTo
            };
            assert_eq!(
                document
                    .relations
                    .iter()
                    .filter(|r| r.subject == RelationEndpoint::Occurrence(site.id)
                        && r.object == RelationEndpoint::Entity(symbol)
                        && r.predicate == predicate
                        && r.evidence.source.as_ref() == Some(&site.source))
                    .count(),
                1
            );
            assert!(
                !document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source.span() == span
                        && matches!(
                            gap.detail.as_str(),
                            "vba-member-target-unavailable"
                                | "vba-call-or-index-target-unavailable"
                        ))
            );
        }
        assert_eq!(
            actual, expected,
            "Err shadow={shadow_err}, VBA shadow={shadow_vba}"
        );
        if !call_symbols.is_empty() {
            let relationships = service
                .symbol_relationships(
                    receipt.generation,
                    call_symbols,
                    vec![RelationFamily::Calls],
                    Some(RelationDirection::Inbound),
                    0,
                    64,
                    0,
                    &deadline(),
                )
                .unwrap();
            assert_eq!(relationships.data.returned_edges, call_count);
            assert!(!relationships.data.truncated);
        }
        let prepared = service
            .prepare_repository_with_options(
                root.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            receipt.logical_snapshot,
            service
                .publish_prepared(prepared, &deadline())
                .unwrap()
                .logical_snapshot
        );
    }
}

#[test]
fn standard_err_model_respects_project_and_conditional_declarations() {
    let root = tempdir();
    for (header, sibling, expected) in [
        ("", "Other", 1),
        ("Private Err As Object\r\n", "Other", 0),
        (
            "#If Unknown Then\r\nPrivate Err As Object\r\n#End If\r\n",
            "Other",
            0,
        ),
        ("", "Err", 0),
    ] {
        let text = format!(
            "Attribute VB_Name = \"Client\"\r\n{header}Public Sub Run()\r\nErr.Clear\r\nEnd Sub\r\n"
        );
        let other =
            format!("Attribute VB_Name = \"{sibling}\"\r\nPublic Sub Empty()\r\nEnd Sub\r\n");
        fs::write(
            root.path().join("book.xlsm"),
            fixture::typed_book(&[
                ("Client", text.as_bytes(), false),
                (sibling, other.as_bytes(), false),
            ]),
        )
        .unwrap();
        let mut service = FirstSliceService::new(4).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let count = document
            .occurrences
            .iter()
            .filter(|site| {
                document.provenance.iter().any(|p| {
                    p.id == site.provenance
                        && p.rule.as_deref() == Some("vba-err-call-v1.indexed_snapshot")
                })
            })
            .count();
        assert_eq!(count, expected, "header={header}, sibling={sibling}");
    }
}
