//! Collection contracts are exercised through native containers and durable queries.
//! Authored receivers distinguish standard type identity from lexical names,
//! runtime contents and invalid calls or writes.

use super::*;

#[test]
fn standard_collection_requires_namespace_evidence_for_unqualified_types() {
    fn counted(out: &mut Vec<u8>, text: &[u8]) {
        out.extend(u32::try_from(text.len()).unwrap().to_le_bytes());
        out.extend(text);
    }
    let root = tempdir();
    let source = b"Attribute VB_Name = \"Client\"\r\nPublic Sub Inspect(ByVal items As Collection, ByVal exact As VBA.Collection)\r\nDim value As Long\r\nvalue = items.Count\r\nvalue = exact.Count\r\nEnd Sub\r\n";
    for label in ["VBA", "Collection", "Other"] {
        let mut records = Vec::new();
        records.extend(0x0016_u16.to_le_bytes());
        counted(&mut records, label.as_bytes());
        records.extend(0x003e_u16.to_le_bytes());
        counted(
            &mut records,
            &label
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        records.extend(0x000d_u16.to_le_bytes());
        records.extend(0_u32.to_le_bytes());
        counted(&mut records, b"*\\G{01234567-89AB-CDEF-1032-547698BADCFE}#1.2#409#C:\\unavailable\\model.tlb#Library");
        records.extend([0; 6]);
        fs::write(
            root.path().join("book.xlsm"),
            fixture::native_book_with_references(
                &[(
                    "Client",
                    source,
                    rootlight_source::vba_project::ProjectModuleKind::Procedural,
                )],
                "\"Project\"",
                &records,
            ),
        )
        .unwrap();
        let mut service = FirstSliceService::new(4).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        for rule in [
            "vba-collection-type-v1.indexed_snapshot",
            "vba-collection-call-v1.indexed_snapshot",
        ] {
            assert_eq!(
                document
                    .occurrences
                    .iter()
                    .filter(|site| document
                        .provenance
                        .iter()
                        .any(|p| p.id == site.provenance && p.rule.as_deref() == Some(rule)))
                    .count(),
                1,
                "label={label}; rule={rule}"
            );
        }
    }
}

#[test]
fn standard_collection_types_and_calls_survive_reopen_and_receiver_mutation() {
    let root = tempdir();
    let storage = tempdir();
    let state = storage.path().join("state");
    rootlight_runtime::RuntimePaths::new(state.clone(), storage.path().join("runtime"))
        .unwrap()
        .prepare_owner()
        .unwrap();
    let calls = [
        ("value = values.Count", "values.Count", "Count"),
        ("value = values.Count()", "values.Count()", "Count"),
        ("value = values.Item(1)", "values.Item(1)", "Item"),
        ("value = values(1)", "values(1)", "Item"),
        (
            "value = values.Item(Index:=\"first\")",
            "values.Item(Index:=\"first\")",
            "Item",
        ),
        ("values.Add value", "values.Add value", "Add"),
        (
            "Call values.Add(Item:=value, Key:=\"first\")",
            "Call values.Add(Item:=value, Key:=\"first\")",
            "Add",
        ),
        ("values.Add value, , 1", "values.Add value, , 1", "Add"),
        ("values.Remove Index:=1", "values.Remove Index:=1", "Remove"),
        ("Call values.Remove(1)", "Call values.Remove(1)", "Remove"),
        (
            "With values\r\nvalue = .Count\r\nEnd With",
            ".Count",
            "Count",
        ),
        ("With values\r\n.Add value\r\nEnd With", ".Add value", "Add"),
    ];
    let invalid = [
        "value = values.Item()",
        "value = values.Count(1)",
        "values.Add",
        "values.Add Key:=\"missing\"",
        "values.Remove",
        "Call values.Remove()",
        "Call values.Remove(1, 2)",
        "values.Add value, Key:=\"a\", key:=\"b\"",
        "value = values.Add(1)",
        "value = values.Remove(1)",
        "values.Count = 1",
        "Set values.Item(1) = value",
        "value = values.Unknown",
        "value = values()",
    ];
    for typed in [true, false, true] {
        let kind = if typed { "Collection" } else { "Object" };
        let mut text = format!(
            "Attribute VB_Name = \"Client\"\r\nPublic Sub Inspect(ByVal values As {kind})\r\nDim value As Variant\r\nDim created As New VBA.Collection\r\nSet created = New [vBa].[Collection]\r\n"
        );
        let mut expected = BTreeMap::new();
        for (statement, expression, name) in calls {
            let start = text.len() + statement.find(expression).unwrap();
            if typed {
                expected.insert(
                    (
                        u64::try_from(start).unwrap(),
                        u64::try_from(start + expression.len()).unwrap(),
                    ),
                    format!("VBA.Collection.{name}"),
                );
            }
            text.push_str(statement);
            text.push_str("\r\n");
        }
        for statement in invalid {
            text.push_str(statement);
            text.push_str("\r\n");
        }
        text.push_str("End Sub\r\n");
        fs::write(
            root.path().join("book.xlsm"),
            fixture::typed_book(&[("Client", text.as_bytes(), false)]),
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
        let mut sources = Vec::new();
        let mut symbols = BTreeSet::new();
        let mut type_count = 0;
        let mut construction_count = 0;
        for site in &document.occurrences {
            let Some(provenance) = document.provenance.iter().find(|p| {
                p.id == site.provenance
                    && p.rule
                        .as_deref()
                        .is_some_and(|rule| rule.starts_with("vba-collection-"))
            }) else {
                continue;
            };
            let OccurrenceTarget::Resolved { symbol } = site.target else {
                panic!("collection proof must resolve")
            };
            let target = document.entities.iter().find(|e| e.id == symbol).unwrap();
            assert_eq!(target.kind, EntityKind::ExternalSymbol);
            assert!(target.evidence.source.is_none());
            assert!(target.flags.contains(&EntityFlag::External));
            let span = site.source.span();
            match provenance.rule.as_deref() {
                Some("vba-collection-type-v1.indexed_snapshot") => type_count += 1,
                Some("vba-collection-construction-v1.indexed_snapshot") => construction_count += 1,
                _ => {}
            }
            let retired_gap = match provenance.rule.as_deref().unwrap() {
                "vba-collection-type-v1.indexed_snapshot" => "vba-type-target-unavailable",
                "vba-collection-construction-v1.indexed_snapshot" => {
                    "vba-construction-semantics-unavailable"
                }
                "vba-collection-call-v1.indexed_snapshot"
                    if site.syntax_kind == "vba.application.reference" =>
                {
                    "vba-call-or-index-target-unavailable"
                }
                _ => "vba-member-target-unavailable",
            };
            assert!(
                !document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source == site.source && gap.detail == retired_gap),
                "resolved gap: {}",
                &text[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()]
            );
            if provenance.rule.as_deref() != Some("vba-collection-call-v1.indexed_snapshot") {
                continue;
            }
            assert_eq!(site.role, rootlight_ir::OccurrenceRole::CallSite);
            assert_eq!(
                document
                    .relations
                    .iter()
                    .filter(|edge| edge.subject == RelationEndpoint::Occurrence(site.id)
                        && edge.predicate == RelationPredicate::Calls
                        && edge.object == RelationEndpoint::Entity(symbol))
                    .count(),
                1
            );
            assert!(
                actual
                    .insert(
                        (span.start_byte(), span.end_byte()),
                        target.qualified_name.clone()
                    )
                    .is_none()
            );
            sources.push(site.source.clone());
            symbols.insert(symbol);
        }
        assert_eq!(type_count, if typed { 3 } else { 2 });
        assert_eq!(construction_count, 1);
        assert_eq!(
            actual,
            expected,
            "typed={typed}; actual texts={:?}",
            actual
                .keys()
                .map(|&(a, b)| &text[usize::try_from(a).unwrap()..usize::try_from(b).unwrap()])
                .collect::<Vec<_>>()
        );
        if typed {
            let relationships = service
                .symbol_relationships(
                    receipt.generation,
                    symbols,
                    vec![RelationFamily::Calls],
                    Some(RelationDirection::Inbound),
                    0,
                    64,
                    0,
                    &deadline(),
                )
                .unwrap();
            assert_eq!(
                usize::try_from(relationships.data.returned_edges).unwrap(),
                calls.len()
            );
            assert!(!relationships.data.truncated);
            let read = service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    sources,
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            let mut bytes: Vec<_> = read
                .data
                .chunks
                .iter()
                .map(|chunk| {
                    assert!(chunk.extracted_from.is_some());
                    chunk.bytes.clone()
                })
                .collect();
            bytes.sort();
            let mut expected_bytes: Vec<_> = calls
                .iter()
                .map(|(_, expression, _)| expression.as_bytes().to_vec())
                .collect();
            expected_bytes.sort();
            assert_eq!(bytes, expected_bytes);
        }
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
}

#[test]
fn collection_type_shadowing_stays_with_its_own_native_project() {
    let root = tempdir();
    let client = b"Attribute VB_Name = \"Client\"\r\nPublic Sub Inspect(ByVal items As Collection, ByVal exact As VBA.Collection)\r\nDim result As Long\r\nresult = items.Count\r\nresult = exact.Count\r\nEnd Sub\r\n";
    let own_class = b"Attribute VB_Name = \"Collection\"\r\nPublic Function Count() As Long\r\nCount = 7\r\nEnd Function\r\n";
    let namespace = b"Attribute VB_Name = \"VBA\"\r\nPublic Sub Marker()\r\nEnd Sub\r\n";
    for shadow in ["none", "class", "namespace", "local", "none"] {
        let local = if shadow == "local" {
            String::from_utf8(client.to_vec())
                .unwrap()
                .replacen(
                    "Public Sub",
                    "Private Type Collection\r\nCount As Long\r\nEnd Type\r\nPublic Sub",
                    1,
                )
                .into_bytes()
        } else {
            client.to_vec()
        };
        let mut modules = vec![("Client", local.as_slice(), false)];
        if shadow == "class" {
            modules.push(("Collection", own_class.as_slice(), true));
        }
        if shadow == "namespace" {
            modules.push(("VBA", namespace.as_slice(), false));
        }
        fs::write(
            root.path().join("scoped.xlsm"),
            fixture::typed_book(&modules),
        )
        .unwrap();
        fs::write(
            root.path().join("independent.xlsm"),
            fixture::typed_book(&[("Client", client.as_slice(), false)]),
        )
        .unwrap();
        let mut service = FirstSliceService::new(4).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let mut types = BTreeMap::<_, Vec<_>>::new();
        let mut calls = BTreeMap::<_, Vec<_>>::new();
        for site in &document.occurrences {
            let Some(rule) = document
                .provenance
                .iter()
                .find(|p| p.id == site.provenance)
                .and_then(|p| p.rule.as_deref())
            else {
                continue;
            };
            if !matches!(
                rule,
                "vba-collection-type-v1.indexed_snapshot"
                    | "vba-collection-call-v1.indexed_snapshot"
            ) {
                continue;
            }
            let path = &document
                .files
                .iter()
                .find(|f| f.id == site.file)
                .unwrap()
                .path;
            let independent = path.starts_with("independent.xlsm/");
            let text = if independent {
                client.as_slice()
            } else {
                local.as_slice()
            };
            let span = site.source.span();
            let text = std::str::from_utf8(
                &text[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()],
            )
            .unwrap()
            .to_owned();
            if rule == "vba-collection-type-v1.indexed_snapshot" {
                types.entry(independent).or_default().push(text);
            } else {
                calls.entry(independent).or_default().push(text);
            }
        }
        for (independent, values) in &mut types {
            values.sort();
            assert_eq!(
                values,
                &if *independent || shadow == "none" {
                    vec!["Collection", "VBA.Collection"]
                } else if shadow == "namespace" {
                    vec!["Collection"]
                } else {
                    vec!["VBA.Collection"]
                },
                "shadow={shadow}; independent={independent}"
            );
        }
        assert_eq!(types.len(), 2);
        for (independent, values) in &mut calls {
            values.sort();
            assert_eq!(
                values,
                &if *independent || shadow == "none" {
                    vec!["exact.Count", "items.Count"]
                } else if shadow == "namespace" {
                    vec!["items.Count"]
                } else {
                    vec!["exact.Count"]
                },
                "shadow={shadow}; independent={independent}"
            );
        }
        assert_eq!(calls.len(), 2);
    }
}

#[test]
fn collection_receivers_require_valid_calls_and_do_not_infer_variant_or_array_elements() {
    let root = tempdir();
    let client = b"Attribute VB_Name = \"Client\"\r\nPublic Sub Inspect(ByVal values() As Collection, ByVal holder As Holder)\r\nDim value As Variant\r\nvalue = Factory.Create(7).Count\r\nvalue = Factory.Create(7)(1)\r\nvalue = Factory.Create(7).Item(1)\r\nFactory.Create(7).Remove 1\r\nvalue = holder.Values.Count\r\nvalue = Factory.Create().Count\r\nvalue = Factory.Create(7).Item(1).Count\r\nvalue = values(1).Count\r\nEnd Sub\r\n";
    let factory = b"Attribute VB_Name = \"Factory\"\r\nPublic Function Create(ByVal seed As Long) As Collection\r\nSet Create = New Collection\r\nEnd Function\r\n";
    let holder = b"Attribute VB_Name = \"Holder\"\r\nPublic Property Get Values() As Collection\r\nSet Values = New Collection\r\nEnd Property\r\n";
    fs::write(
        root.path().join("book.xlsm"),
        fixture::typed_book(&[
            ("Client", client, false),
            ("Factory", factory, false),
            ("Holder", holder, true),
        ]),
    )
    .unwrap();
    let mut service = FirstSliceService::new(4).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let mut actual = Vec::new();
    for site in &document.occurrences {
        if !document.provenance.iter().any(|p| {
            p.id == site.provenance
                && p.rule.as_deref() == Some("vba-collection-call-v1.indexed_snapshot")
        }) {
            continue;
        }
        let span = site.source.span();
        actual.push(
            std::str::from_utf8(
                &client[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()],
            )
            .unwrap(),
        );
    }
    actual.sort();
    let mut expected = vec![
        "Factory.Create(7).Count",
        "Factory.Create(7)(1)",
        "Factory.Create(7).Item(1)",
        "Factory.Create(7).Item(1)",
        "Factory.Create(7).Remove 1",
        "holder.Values.Count",
    ];
    expected.sort();
    assert_eq!(actual, expected);
    for expression in [
        "Factory.Create().Count",
        "Factory.Create(7).Item(1).Count",
        "values(1).Count",
    ] {
        let start = u64::try_from(
            client
                .windows(expression.len())
                .position(|part| part == expression.as_bytes())
                .unwrap(),
        )
        .unwrap();
        let end = start + u64::try_from(expression.len()).unwrap();
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().start_byte() == start
                    && gap.source.span().end_byte() == end
                    && gap.detail == "vba-member-target-unavailable"),
            "{expression}"
        );
    }
}
