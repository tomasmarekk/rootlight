//! Authored library qualifiers retain declaration identity through durable indexing.
//! Procedural module membership constrains lookup; these tests do not evaluate
//! library functions or infer host-reference precedence for unqualified modules.

use super::*;

#[test]
fn module_qualified_functions_keep_flattened_library_identity_after_reopen() {
    let calls = [
        ("VBA.Strings.Chr(65)", "VBA.Chr"),
        ("VBA.Chr(65)", "VBA.Chr"),
        ("VBA.Conversion.CStr(65)", "VBA.CStr"),
        ("VBA.CStr(65)", "VBA.CStr"),
        ("VBA.Information.IsNumeric(65)", "VBA.IsNumeric"),
        ("VBA.IsNumeric(65)", "VBA.IsNumeric"),
        ("VBA.Interaction.IIf(True, 1, 2)", "VBA.IIf"),
        ("VBA.IIf(True, 1, 2)", "VBA.IIf"),
        ("[vBa].[sTrInGs].[cHr](CharCode:=65)", "VBA.Chr"),
        ("Call VBA.Strings.Chr$(CharCode:=65)", "VBA.Chr$"),
        ("VBA.Conversion.CStr(Expression:=1)", "VBA.CStr"),
        ("VBA.Information.IsNumeric(Arg:=1)", "VBA.IsNumeric"),
        (
            "VBA.Interaction.IIf(Expression:=True, TruePart:=1, FalsePart:=2)",
            "VBA.IIf",
        ),
        (
            "VBA.Strings.StrConv(String:=\"sample\", Conversion:=1, LCID:=1033)",
            "VBA.StrConv",
        ),
        (
            "VBA.Strings.FormatDateTime(Expression:=0, NamedFormat:=0)",
            "VBA.FormatDateTime",
        ),
        ("VBA.Conversion.Error$(ErrorNumber:=5)", "VBA.Error$"),
        ("VBA.Strings.Chr(VBA.Conversion.CInt(65))", "VBA.Chr"),
        (
            "VBA.Interaction.IIf(VBA.Information.IsNumeric(1), VBA.Strings.Chr$(65), VBA.Conversion.CStr(0))",
            "VBA.IIf",
        ),
        ("VBA. _\r\nStrings. _\r\nChr(66)", "VBA.Chr"),
    ];
    let nested = [
        ("VBA.Conversion.CInt(65)", "VBA.CInt"),
        ("VBA.Information.IsNumeric(1)", "VBA.IsNumeric"),
        ("VBA.Strings.Chr$(65)", "VBA.Chr$"),
        ("VBA.Conversion.CStr(0)", "VBA.CStr"),
    ];
    let text = client_source("", "", calls.iter().map(|(expression, _)| *expression));
    let expected: Vec<_> = calls.into_iter().chain(nested).collect();
    assert_namespace_fixture(
        &text,
        fixture::book(&[("Client", text.as_bytes())]),
        &expected,
        &[],
        &[],
    );
}

#[test]
fn wrong_procedural_modules_and_non_namespace_receivers_remain_unresolved() {
    let rejected = [
        "VBA.Conversion.Chr(65)",
        "VBA.Information.Chr(65)",
        "VBA.Interaction.Chr(65)",
        "VBA.Strings.CStr(1)",
        "VBA.Information.CStr(1)",
        "VBA.Interaction.CStr(1)",
        "VBA.Strings.IsNumeric(1)",
        "VBA.Conversion.IsNumeric(1)",
        "VBA.Interaction.IsNumeric(1)",
        "VBA.Strings.IIf(True, 1, 2)",
        "VBA.Conversion.IIf(True, 1, 2)",
        "VBA.Information.IIf(True, 1, 2)",
        "VBA.Conversion.StrConv(\"sample\", 1)",
        "VBA.Strings.Error$(5)",
        "VBA.DateTime.FormatDateTime(0)",
        "VBA.Fake.Strings.Chr(65)",
        "VBA.Strings.Fake.Chr(65)",
        "VBA.Strings.Strings.Chr(65)",
        "VBA.Strings.Constants.vbTab()",
        "VBA.Strings.Err.Number()",
        "VBA.Strings.Collection.Count()",
        "VBA.Strings.vbTab()",
        "VBA.Strings.Err()",
        "VBA.Strings.Collection()",
        "VBA.Conversion.Raise(5)",
        "VBA.Strings().Chr(65)",
        "VBA().Strings.Chr(65)",
        "VBA$.Strings.Chr(65)",
        "VBA.Strings$.Chr(65)",
        "VBA.Strings.Chr%(65)",
        "Strings.Chr(65)",
    ];
    let retained = [
        ("VBA.Strings.Chr()", "VBA.Chr"),
        ("VBA.Conversion.CStr(Unknown:=1)", "VBA.CStr"),
        ("VBA.Information.IsNumeric()", "VBA.IsNumeric"),
        ("VBA.Interaction.IIf(True, 1)", "VBA.IIf"),
        ("VBA.Strings.Chr(CharCode:=65, charcode:=66)", "VBA.Chr"),
        ("VBA.Strings.Chr(CharCode:=65, 66)", "VBA.Chr"),
    ];
    let declarations = "Private stringsType As VBA.Strings\r\nPrivate conversionType As VBA.Conversion\r\nPrivate informationType As VBA.Information\r\nPrivate interactionType As VBA.Interaction\r\n";
    let mut text = client_source(
        "",
        declarations,
        rejected
            .iter()
            .copied()
            .chain(retained.iter().map(|(expression, _)| *expression)),
    );
    text = text.replace("End Sub\r\n", "Set value = New VBA.Strings\r\nSet value = New VBA.Conversion\r\nSet value = New VBA.Information\r\nSet value = New VBA.Interaction\r\nEnd Sub\r\n");
    let invalid: Vec<_> = rejected
        .into_iter()
        .chain(retained.iter().map(|(expression, _)| *expression))
        .collect();
    assert_namespace_fixture(
        &text,
        fixture::book(&[("Client", text.as_bytes())]),
        &[],
        &invalid,
        &retained,
    );
}

#[test]
fn project_qualifier_shadowing_stops_module_lookup_but_local_module_names_do_not() {
    let calls = [
        ("VBA.Strings.Chr(65)", "VBA.Chr"),
        ("VBA.Conversion.CStr(65)", "VBA.CStr"),
        ("VBA.Information.IsNumeric(65)", "VBA.IsNumeric"),
        ("VBA.Interaction.IIf(True, 1, 2)", "VBA.IIf"),
    ];
    let expressions: Vec<_> = calls.iter().map(|(expression, _)| *expression).collect();
    for (parameter, declarations) in [
        ("ByVal VBA As Object", ""),
        ("", "Private VBA As Object\r\n"),
    ] {
        let text = client_source(parameter, declarations, expressions.iter().copied());
        assert_namespace_fixture(
            &text,
            fixture::book(&[("Client", text.as_bytes())]),
            &[],
            &expressions,
            &[],
        );
    }
    let text = client_source("", "", expressions.iter().copied());
    let sibling = b"Attribute VB_Name = \"VBA\"\r\nPublic Sub Placeholder()\r\nEnd Sub\r\n";
    assert_namespace_fixture(
        &text,
        fixture::book(&[("Client", text.as_bytes()), ("VBA", sibling)]),
        &[],
        &expressions,
        &[],
    );
    let book = fixture::native_book_with_directory_values(
        &[(
            "Client",
            text.as_bytes(),
            rootlight_source::vba_project::ProjectModuleKind::Procedural,
        )],
        "\"VBA\"",
        "VBA",
        (1, 0),
        &[],
    );
    assert_namespace_fixture(&text, book, &[], &expressions, &[]);
    for (parameter, declarations) in [
        ("ByVal Strings As Object", ""),
        ("", "Private Strings As Object\r\n"),
    ] {
        let text = client_source(parameter, declarations, expressions.iter().copied());
        assert_namespace_fixture(
            &text,
            fixture::book(&[("Client", text.as_bytes())]),
            &calls,
            &[],
            &[],
        );
    }
}

#[test]
fn procedural_qualifiers_materialize_without_unrelated_type_uses() {
    let calls = [
        ("Call VBA.Strings.Chr(65)", "VBA.Chr"),
        ("Call VBA.Conversion.CStr(65)", "VBA.CStr"),
        ("Call VBA.Information.IsNumeric(65)", "VBA.IsNumeric"),
        ("Call VBA.Interaction.IIf(True, 1, 2)", "VBA.IIf"),
    ];
    let mut text = String::from("Attribute VB_Name = \"Client\"\r\nPublic Sub Run()\r\n");
    for (expression, _) in calls {
        text.push_str(expression);
        text.push_str("\r\n");
    }
    text.push_str("End Sub\r\n");
    assert_namespace_fixture(
        &text,
        fixture::book(&[("Client", text.as_bytes())]),
        &calls,
        &[],
        &[],
    );
}

#[test]
fn procedural_namespaces_are_not_values_callables_or_types() {
    let rejected = [
        "VBA.Strings()",
        "VBA.Conversion()",
        "VBA.Information()",
        "VBA.Interaction()",
    ];
    let declarations = "Private stringsType As VBA.Strings\r\nPrivate conversionType As VBA.Conversion\r\nPrivate informationType As VBA.Information\r\nPrivate interactionType As VBA.Interaction\r\n";
    let mut text = client_source("", declarations, rejected);
    let mut standalone = String::new();
    for module in ["Strings", "Conversion", "Information", "Interaction"] {
        standalone.push_str(&format!(
            "value = VBA.{module}\r\nVBA.{module} = value\r\nSet value = New VBA.{module}\r\n"
        ));
    }
    text = text.replace("End Sub\r\n", &format!("{standalone}End Sub\r\n"));
    assert_namespace_fixture(
        &text,
        fixture::book(&[("Client", text.as_bytes())]),
        &[],
        &rejected,
        &[],
    );
}

fn client_source<'a>(
    parameter: &str,
    declarations: &str,
    expressions: impl IntoIterator<Item = &'a str>,
) -> String {
    let mut text = format!(
        "Attribute VB_Name = \"Client\"\r\n{declarations}Public Sub Run({parameter})\r\nDim value As Variant\r\n"
    );
    for expression in expressions {
        if !expression.starts_with("Call ") {
            text.push_str("value = ");
        }
        text.push_str(expression);
        text.push_str("\r\n");
    }
    text.push_str("End Sub\r\n");
    text
}

fn callee(expression: &str) -> &str {
    expression
        .strip_prefix("Call ")
        .unwrap_or(expression)
        .split_once('(')
        .unwrap()
        .0
}

fn assert_namespace_fixture(
    text: &str,
    book: Vec<u8>,
    expected_calls: &[(&str, &str)],
    rejected_calls: &[&str],
    retained_callees: &[(&str, &str)],
) {
    let root = tempdir();
    let storage = tempdir();
    let state = storage.path().join("state");
    rootlight_runtime::RuntimePaths::new(state.clone(), storage.path().join("runtime"))
        .unwrap()
        .prepare_owner()
        .unwrap();
    fs::write(root.path().join("book.xlsm"), book).unwrap();
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
    if expected_calls.is_empty() && retained_callees.is_empty() {
        assert!(
            document.occurrences.iter().all(|site| {
                !document.provenance.iter().any(|proof| {
                    proof.id == site.provenance
                        && proof.rule.as_deref()
                            == Some("vba-standard-namespace-v1.indexed_snapshot")
                }) || matches!(site.target, OccurrenceTarget::Resolved { symbol }
                    if document.entities.iter().any(|entity|
                        entity.id == symbol && entity.qualified_name == "VBA"))
            }),
            "procedural modules are not proven value, call or type dependencies"
        );
    }
    if !text.contains(" As ") {
        assert!(
            document
                .occurrences
                .iter()
                .all(|site| site.role != rootlight_ir::OccurrenceRole::TypeUse)
        );
    }
    let mut calls = Vec::new();
    let mut references = Vec::new();
    let mut targets = BTreeMap::new();
    let mut sources = Vec::new();
    let mut expected_bytes = Vec::new();
    for site in &document.occurrences {
        let proof = document
            .provenance
            .iter()
            .find(|proof| proof.id == site.provenance)
            .unwrap();
        let predicate = match proof.rule.as_deref() {
            Some("vba-standard-call-v1.indexed_snapshot") => {
                assert_eq!(site.role, rootlight_ir::OccurrenceRole::CallSite);
                RelationPredicate::Calls
            }
            Some("vba-standard-function-v1.indexed_snapshot") => {
                assert_eq!(site.role, rootlight_ir::OccurrenceRole::Reference);
                RelationPredicate::RefersTo
            }
            Some("vba-standard-namespace-v1.indexed_snapshot") => {
                assert_eq!(site.role, rootlight_ir::OccurrenceRole::Reference);
                let OccurrenceTarget::Resolved { symbol } = site.target else {
                    panic!("namespace must resolve")
                };
                let target = document
                    .entities
                    .iter()
                    .find(|entity| entity.id == symbol)
                    .unwrap();
                assert_eq!(target.kind, EntityKind::ExternalSymbol);
                assert!(target.flags.contains(&EntityFlag::External));
                assert!(target.flags.contains(&EntityFlag::Synthetic));
                assert!(target.evidence.source.is_none());
                assert_eq!(site.source.generation(), receipt.generation);
                assert_eq!(site.source.repository(), site.repository);
                assert!(matches!(
                    target.qualified_name.as_str(),
                    "VBA"
                        | "VBA.Strings"
                        | "VBA.Conversion"
                        | "VBA.Information"
                        | "VBA.Interaction"
                        | "VBA.Constants"
                ));
                continue;
            }
            _ => {
                if let OccurrenceTarget::Resolved { symbol } = site.target {
                    let target = document
                        .entities
                        .iter()
                        .find(|entity| entity.id == symbol)
                        .unwrap();
                    assert!(
                        target.kind != EntityKind::ExternalSymbol
                            || !target.qualified_name.starts_with("VBA."),
                        "unexpected non-namespace library dependency: {} ({:?})",
                        target.qualified_name,
                        proof.rule,
                    );
                }
                continue;
            }
        };
        let OccurrenceTarget::Resolved { symbol } = site.target else {
            panic!("proved function dependency must resolve")
        };
        let target = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(target.kind, EntityKind::ExternalSymbol);
        assert!(target.flags.contains(&EntityFlag::External));
        assert!(target.flags.contains(&EntityFlag::Synthetic));
        assert!(target.evidence.source.is_none());
        if let Some(previous) = targets.insert(target.qualified_name.clone(), symbol) {
            assert_eq!(
                previous, symbol,
                "module and project qualifiers name one declaration"
            );
        }
        let span = site.source.span();
        let expression = text
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap();
        assert_eq!(site.source.generation(), receipt.generation);
        assert_eq!(site.source.repository(), site.repository);
        let file = document
            .files
            .iter()
            .find(|file| file.id == site.file)
            .unwrap();
        assert_eq!(site.source.content_hash(), file.content_hash);
        let pair = (expression, target.qualified_name.as_str());
        if predicate == RelationPredicate::Calls {
            calls.push(pair);
        } else {
            references.push(pair);
        }
        let mut edges = document.relations.iter().filter(|edge| {
            edge.subject == RelationEndpoint::Occurrence(site.id)
                && edge.object == RelationEndpoint::Entity(symbol)
        });
        let edge = edges.next().unwrap();
        assert!(
            edges.next().is_none(),
            "one role-specific dependency per occurrence: {expression}"
        );
        assert_eq!(edge.predicate, predicate);
        assert_eq!(edge.evidence.source.as_ref(), Some(&site.source));
        assert_eq!(edge.provenance, site.provenance);
        assert_eq!(edge.generation, receipt.generation);
        assert_eq!(edge.repository, site.repository);
        sources.push(site.source.clone());
        expected_bytes.push(expression.as_bytes().to_vec());
        if predicate != RelationPredicate::Calls {
            continue;
        }
        let unprefixed = expression.strip_prefix("Call ").unwrap_or(expression);
        let callee = callee(expression);
        let start = span.start_byte() + u64::try_from(expression.len() - unprefixed.len()).unwrap();
        let end = start + u64::try_from(callee.len()).unwrap();
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|reference| reference.source.span().file() == span.file()
                    && reference.source.span().start_byte() == start
                    && reference.source.span().end_byte() == end
                    && reference.role == rootlight_ir::OccurrenceRole::Reference
                    && reference.target == OccurrenceTarget::Resolved { symbol }
                    && document
                        .provenance
                        .iter()
                        .any(|proof| proof.id == reference.provenance
                            && proof.rule.as_deref()
                                == Some("vba-standard-function-v1.indexed_snapshot")))
                .count(),
            1,
            "separate callee binding: {expression}"
        );
        if let Some((namespace, _)) = callee.rsplit_once('.') {
            let normalized = namespace.replace(['[', ']', ' ', '_', '\r', '\n'], "");
            if normalized.contains('.') {
                assert_eq!(document.occurrences.iter().filter(|qualifier| qualifier.source.span().file() == span.file() && qualifier.source.span().start_byte() == start && qualifier.source.span().end_byte() == start + u64::try_from(namespace.len()).unwrap() && document.provenance.iter().any(|proof| proof.id == qualifier.provenance && proof.rule.as_deref() == Some("vba-standard-namespace-v1.indexed_snapshot")) && matches!(qualifier.target, OccurrenceTarget::Resolved { symbol } if document.entities.iter().any(|entity| entity.id == symbol && entity.qualified_name.eq_ignore_ascii_case(&normalized)))).count(), 1, "intermediate module namespace: {expression}");
            }
        }
    }
    calls.sort();
    let mut expected = expected_calls.to_vec();
    expected.sort();
    assert_eq!(calls, expected);
    references.sort();
    let mut expected_references: Vec<_> = expected_calls
        .iter()
        .chain(retained_callees)
        .map(|(expression, target)| (callee(expression), *target))
        .collect();
    expected_references.sort();
    assert_eq!(references, expected_references);
    let expected_targets: BTreeSet<_> = expected_calls
        .iter()
        .chain(retained_callees)
        .map(|(_, name)| *name)
        .collect();
    assert_eq!(
        targets.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        expected_targets
    );
    for expression in rejected_calls {
        let prefix = if expression.starts_with("Call ") {
            ""
        } else {
            "value = "
        };
        let statement = format!("{prefix}{expression}\r\n");
        let start = u64::try_from(text.find(&statement).unwrap() + prefix.len()).unwrap();
        let end = start + u64::try_from(expression.len()).unwrap();
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "vba-call-or-index-target-unavailable"
                    && gap.source.span().start_byte() == start
                    && gap.source.span().end_byte() == end),
            "unproved full call retains scoped coverage: {expression}"
        );
    }
    if !targets.is_empty() {
        for (family, expected_edges) in [
            (RelationFamily::Calls, expected_calls.len()),
            (RelationFamily::References, expected_references.len()),
        ] {
            assert!(expected_edges <= 64);
            let relationships = service
                .symbol_relationships(
                    receipt.generation,
                    targets.values().copied().collect(),
                    vec![family],
                    Some(RelationDirection::Inbound),
                    0,
                    64,
                    0,
                    &deadline(),
                )
                .unwrap();
            assert_eq!(
                usize::try_from(relationships.data.returned_edges).unwrap(),
                expected_edges
            );
            assert!(!relationships.data.truncated);
        }
    }
    let mut bytes = Vec::new();
    for batch in sources.chunks(16) {
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                batch.to_vec(),
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        bytes.extend(read.data.chunks.iter().map(|chunk| {
            assert!(chunk.extracted_from.is_some());
            chunk.bytes.clone()
        }));
    }
    bytes.sort();
    expected_bytes.sort();
    assert_eq!(bytes, expected_bytes);
    let prepared = service
        .prepare_repository_with_options(
            root.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
            &deadline(),
        )
        .unwrap();
    let rebuilt = service.publish_prepared(prepared, &deadline()).unwrap();
    assert!(receipt.logical_snapshot.is_some());
    assert_eq!(receipt.logical_snapshot, rebuilt.logical_snapshot);
}
