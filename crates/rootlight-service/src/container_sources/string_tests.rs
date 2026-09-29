//! Authored string-library calls exercise durable native VBA indexing.
//! These checks distinguish declaration-backed dependencies from runtime
//! evaluation and retain exact source evidence through reopen and rebuild.

use super::*;

#[test]
fn standard_character_calls_keep_declared_names_and_string_identities() {
    let valid = [
        "VBA.Asc(\"a\")",
        "VBA.AscB(\"a\")",
        "VBA.AscW(\"a\")",
        "VBA.Chr(65)",
        "VBA.Chr$(65)",
        "VBA.ChrB(65)",
        "VBA.ChrB$(65)",
        "VBA.ChrW(65)",
        "VBA.ChrW$(65)",
        "VBA.Asc(StringValue:=\"a\")",
        "VBA.AscB(StringValue:=\"a\")",
        "VBA.AscW(StringValue:=\"a\")",
        "VBA.Chr(CharCode:=65)",
        "VBA.Chr$(CharCode:=65)",
        "VBA.ChrB(CharCode:=65)",
        "VBA.ChrB$(CharCode:=65)",
        "VBA.ChrW(CharCode:=65)",
        "VBA.ChrW$(CharCode:=65)",
        "Call VBA.Chr$(65)",
        "[vBa].[aScW](\"a\")",
        "VBA.ChrW$(VBA.AscW(\"a\"))",
    ];
    let invalid = [
        "VBA.Asc()",
        "VBA.AscB(\"a\", 1)",
        "VBA.AscW(String:=\"a\")",
        "VBA.Asc(StringValue:=\"a\", stringvalue:=\"b\")",
        "VBA.AscB(ByVal \"a\")",
        "VBA.Asc$(\"a\")",
        "VBA.AscB$(\"a\")",
        "VBA.AscW$(\"a\")",
        "VBA.Chr()",
        "VBA.Chr$(65, 66)",
        "VBA.ChrB(Number:=65)",
        "VBA.ChrB$(CharCode:=65, charcode:=66)",
        "VBA.ChrW(CharCode:=65, 66)",
        "VBA.ChrW$(ByVal 65)",
        "VBA.Chr%(65)",
        "VBA.ChrW$(65,)",
    ];
    assert_standard_string_calls(&valid, &invalid, &["VBA.AscW(\"a\")"], 9);
}

#[test]
fn standard_format_calls_preserve_named_and_omitted_declaration_arguments() {
    let valid = [
        "VBA.Format(1)",
        "VBA.Format$(1)",
        "VBA.Format(1, \"0\", 1, 1)",
        "VBA.Format$(1, , , 2)",
        "VBA.Format(FirstWeekOfYear:=2, Expression:=1)",
        "VBA.Format$(Format:=\"0\", Expression:=1, FirstDayOfWeek:=2)",
        "VBA.FormatCurrency(1)",
        "VBA.FormatCurrency(1, 2, -1, 0, 0)",
        "VBA.FormatCurrency(1, , , , 0)",
        "VBA.FormatCurrency(Expression:=1, GroupDigits:=0)",
        "VBA.FormatNumber(1)",
        "VBA.FormatNumber(1, 2, -1, 0, 0)",
        "VBA.FormatNumber(UseParensForNegativeNumbers:=0, Expression:=1)",
        "VBA.FormatPercent(1)",
        "VBA.FormatPercent(1, , , 0)",
        "VBA.FormatPercent(IncludeLeadingDigit:=0, NumDigitsAfterDecimal:=2, Expression:=1)",
        "VBA.FormatDateTime(1)",
        "VBA.FormatDateTime(1, 2)",
        "VBA.FormatDateTime(1, NamedFormat:=2)",
        "VBA.FormatDateTime(Expression:=1, NamedFormat:=2)",
        "VBA.MonthName(1)",
        "VBA.MonthName(Month:=1, Abbreviate:=True)",
        "VBA.WeekdayName(1)",
        "VBA.WeekdayName(1, , 2)",
        "VBA.WeekdayName(FirstDayOfWeek:=2, Weekday:=1, Abbreviate:=True)",
        "Call VBA.Format$(1)",
        "[vBa].[FormatCurrency](1)",
        "VBA.Format$(VBA.FormatNumber(1))",
    ];
    let invalid = [
        "VBA.Format()",
        "VBA.Format$(Format:=\"0\")",
        "VBA.Format(1, \"0\", 1, 1, 1)",
        "VBA.Format(Expression:=1, Expression:=2)",
        "VBA.Format$(1, FirstDayOfWeek:=1, 2)",
        "VBA.Format%(1)",
        "VBA.FormatCurrency()",
        "VBA.FormatCurrency(1, 2, 0, 0, 0, 0)",
        "VBA.FormatCurrency(Expression:=1, DecimalPlaces:=2)",
        "VBA.FormatCurrency$(1)",
        "VBA.FormatNumber(NumDigitsAfterDecimal:=2)",
        "VBA.FormatNumber(Expression:=1, GroupDigits:=0, groupdigits:=1)",
        "VBA.FormatNumber$(1)",
        "VBA.FormatPercent(ByVal 1)",
        "VBA.FormatPercent(1,)",
        "VBA.FormatPercent$(1)",
        "VBA.FormatDateTime()",
        "VBA.FormatDateTime(1, 2, 3)",
        "VBA.FormatDateTime(1, Format:=2)",
        // The formal declaration names Expression. Rejecting an unverified
        // Date alias here does not establish what an installed Office runtime accepts.
        "VBA.FormatDateTime(Date:=1)",
        "VBA.FormatDateTime$(1)",
        "VBA.MonthName()",
        "VBA.MonthName(1, False, 2)",
        "VBA.MonthName(Number:=1)",
        "VBA.MonthName$(1)",
        "VBA.WeekdayName(Abbreviate:=True)",
        "VBA.WeekdayName(1, False, 2, 3)",
        "VBA.WeekdayName(Day:=1)",
        "VBA.WeekdayName$(1)",
    ];
    assert_standard_string_calls(&valid, &invalid, &["VBA.FormatNumber(1)"], 8);
}

#[test]
fn standard_string_conversion_uses_optional_lcid_without_evaluating_runtime_rules() {
    let valid = [
        "VBA.StrConv(\"sample\", 1)",
        "VBA.StrConv(\"sample\", 1, 1033)",
        "VBA.StrConv(String:=\"sample\", Conversion:=1)",
        "VBA.StrConv(LCID:=1033, Conversion:=1, String:=\"sample\")",
        "VBA.StrConv(\"sample\", 1, LCID:=1033)",
        "Call VBA.StrConv(\"sample\", 1)",
        "[vBa].[StrConv](\"sample\", 1)",
        // Declared dependencies do not prove locale availability, coercion
        // or successful runtime evaluation of a conversion flag.
        "VBA.StrConv(Null, 1048576)",
        "VBA.StrConv(VBA.StrConv(\"sample\", 1), 2)",
    ];
    let invalid = [
        "VBA.StrConv()",
        "VBA.StrConv(\"sample\")",
        "VBA.StrConv(\"sample\", , 1033)",
        "VBA.StrConv(Conversion:=1)",
        "VBA.StrConv(String:=\"sample\")",
        "VBA.StrConv(\"sample\", 1, 1033, 0)",
        "VBA.StrConv(\"sample\", 1, LocaleID:=1033)",
        "VBA.StrConv(String:=\"sample\", Conversion:=1, LCID:=1033, lcid:=1031)",
        "VBA.StrConv(String:=\"sample\", 1)",
        "VBA.StrConv(ByVal \"sample\", 1)",
        "VBA.StrConv$(\"sample\", 1)",
    ];
    assert_standard_string_calls(&valid, &invalid, &["VBA.StrConv(\"sample\", 1)"], 1);
}

fn assert_standard_string_calls(
    valid: &[&str],
    invalid: &[&str],
    nested: &[&str],
    distinct_targets: usize,
) {
    assert!(valid.len() + nested.len() <= 64);
    assert_standard_function_calls(valid, invalid, nested, distinct_targets);
    let root = tempdir();
    let storage = tempdir();
    let state = storage.path().join("state");
    rootlight_runtime::RuntimePaths::new(state.clone(), storage.path().join("runtime"))
        .unwrap()
        .prepare_owner()
        .unwrap();
    let mut text = String::from(
        "Attribute VB_Name = \"Client\"\r\nPublic Sub Run()\r\nDim value As Variant\r\n",
    );
    for expression in valid.iter().chain(invalid) {
        if !expression.starts_with("Call ") {
            text.push_str("value = ");
        }
        text.push_str(expression);
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
    let service = FirstSliceService::new_durable(4, &state, &deadline()).unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let mut sources = Vec::new();
    let mut expected_bytes = Vec::new();
    for call in &document.occurrences {
        if !document.provenance.iter().any(|proof| {
            proof.id == call.provenance
                && proof.rule.as_deref() == Some("vba-standard-call-v1.indexed_snapshot")
        }) {
            continue;
        }
        let OccurrenceTarget::Resolved { symbol } = call.target else {
            panic!("proved call must resolve");
        };
        let span = call.source.span();
        let expression = text
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap();
        assert!(
            valid
                .iter()
                .chain(nested)
                .any(|expected| *expected == expression)
        );
        let unprefixed = expression.strip_prefix("Call ").unwrap_or(expression);
        let callee = unprefixed.split_once('(').unwrap().0;
        let start = span.start_byte() + u64::try_from(expression.len() - unprefixed.len()).unwrap();
        let end = start + u64::try_from(callee.len()).unwrap();
        let mut references = document.occurrences.iter().filter(|site| {
            site.source.span().file() == call.source.span().file()
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
                && site.role == rootlight_ir::OccurrenceRole::Reference
                && site.target == OccurrenceTarget::Resolved { symbol }
                && document.provenance.iter().any(|proof| {
                    proof.id == site.provenance
                        && proof.rule.as_deref()
                            == Some("vba-standard-function-v1.indexed_snapshot")
                })
        });
        let reference = references.next().unwrap_or_else(|| {
            panic!("call must retain its separate callee binding: {expression}");
        });
        assert!(references.next().is_none(), "{expression}");
        for (site, predicate) in [
            (call, RelationPredicate::Calls),
            (reference, RelationPredicate::RefersTo),
        ] {
            let mut edges = document.relations.iter().filter(|edge| {
                edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.predicate == predicate
                    && edge.object == RelationEndpoint::Entity(symbol)
            });
            let edge = edges.next().unwrap_or_else(|| {
                panic!("dependency must retain its exact relation: {expression}");
            });
            assert!(edges.next().is_none(), "{expression}");
            assert_eq!(edge.evidence.source.as_ref(), Some(&site.source));
            assert_eq!(edge.provenance, site.provenance);
            assert_eq!(edge.generation, receipt.generation);
            assert_eq!(edge.repository, site.repository);
        }
        sources.push(reference.source.clone());
        expected_bytes.push(callee.as_bytes().to_vec());
    }
    assert_eq!(sources.len(), valid.len() + nested.len());
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
}

#[test]
fn standard_qualifiers_materialize_without_type_uses_after_reopen() {
    let root = tempdir();
    let storage = tempdir();
    let state = storage.path().join("state");
    rootlight_runtime::RuntimePaths::new(state.clone(), storage.path().join("runtime"))
        .unwrap()
        .prepare_owner()
        .unwrap();
    let text = "Attribute VB_Name = \"Client\"\r\nPublic Sub Run()\r\nCall VBA.Len(\"sample\")\r\nCall VBA.Len(VBA.vbTab)\r\nEnd Sub\r\n";
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
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| site.role != rootlight_ir::OccurrenceRole::TypeUse),
        "qualifier-only modules must not depend on an unrelated type declaration"
    );
    let mut calls = Vec::new();
    let mut references = Vec::new();
    let mut call_symbols = BTreeSet::new();
    let mut reference_symbols = BTreeSet::new();
    let mut sources = Vec::new();
    for site in &document.occurrences {
        let proof = document
            .provenance
            .iter()
            .find(|proof| proof.id == site.provenance)
            .unwrap();
        let (qualified_name, predicate) = match proof.rule.as_deref() {
            Some("vba-standard-call-v1.indexed_snapshot") => {
                assert_eq!(site.role, rootlight_ir::OccurrenceRole::CallSite);
                ("VBA.Len", RelationPredicate::Calls)
            }
            Some("vba-standard-constant-v1.indexed_snapshot") => {
                assert_eq!(site.role, rootlight_ir::OccurrenceRole::Reference);
                ("VBA.Constants.vbTab", RelationPredicate::RefersTo)
            }
            _ => continue,
        };
        let OccurrenceTarget::Resolved { symbol } = site.target else {
            panic!("standard dependencies must resolve");
        };
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(entity.kind, EntityKind::ExternalSymbol);
        assert!(entity.flags.contains(&EntityFlag::External));
        assert!(entity.flags.contains(&EntityFlag::Synthetic));
        assert!(entity.evidence.source.is_none());
        assert_eq!(entity.qualified_name, qualified_name);
        let span = site.source.span();
        let expression = text
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap();
        match predicate {
            RelationPredicate::Calls => {
                calls.push(expression);
                call_symbols.insert(symbol);
            }
            RelationPredicate::RefersTo => {
                references.push(expression);
                reference_symbols.insert(symbol);
            }
            _ => unreachable!(),
        }
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.predicate == predicate
                    && edge.object == RelationEndpoint::Entity(symbol))
                .count(),
            1
        );
        sources.push(site.source.clone());
    }
    calls.sort();
    assert_eq!(
        calls,
        ["Call VBA.Len(\"sample\")", "Call VBA.Len(VBA.vbTab)"]
    );
    assert_eq!(references, ["VBA.vbTab"]);
    assert_eq!(call_symbols.len(), 1);
    assert_eq!(reference_symbols.len(), 1);
    let relationships = service
        .symbol_relationships(
            receipt.generation,
            call_symbols,
            vec![RelationFamily::Calls],
            Some(RelationDirection::Inbound),
            0,
            8,
            0,
            &deadline(),
        )
        .unwrap();
    assert_eq!(relationships.data.returned_edges, 2);
    assert!(!relationships.data.truncated);
    let relationships = service
        .symbol_relationships(
            receipt.generation,
            reference_symbols,
            vec![RelationFamily::References],
            Some(RelationDirection::Inbound),
            0,
            8,
            0,
            &deadline(),
        )
        .unwrap();
    assert_eq!(relationships.data.returned_edges, 1);
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
    assert_eq!(
        bytes,
        [
            b"Call VBA.Len(\"sample\")".to_vec(),
            b"Call VBA.Len(VBA.vbTab)".to_vec(),
            b"VBA.vbTab".to_vec(),
        ]
    );
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
