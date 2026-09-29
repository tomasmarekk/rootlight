//! Simple-name constants use the same external identities as qualified reads,
//! while source spelling and declaration barriers survive durable publication.

use super::*;

const RULE: &str = "vba-standard-implicit-constant-v1.indexed_snapshot";

#[test]
fn implicit_constants_preserve_exact_sources_and_shadowing_across_reopen() {
    let root = tempdir();
    let storage = tempdir();
    let state = storage.path().join("state");
    rootlight_runtime::RuntimePaths::new(state.clone(), storage.path().join("runtime"))
        .unwrap()
        .prepare_owner()
        .unwrap();
    let constants = [
        "vbBack",
        "vbCr",
        "vbCrLf",
        "vbFormFeed",
        "vbLf",
        "vbNewLine",
        "vbNullChar",
        "vbTab",
        "vbVerticalTab",
        "vbNullString",
        "vbObjectError",
    ];
    for shadow in [false, true, false] {
        let parameter = if shadow { "ByVal VBTAB As String" } else { "" };
        let mut text = format!(
            "Attribute VB_Name = \"Client\"\r\nPublic Sub Run({parameter})\r\nDim value As Variant\r\n"
        );
        let mut expected = BTreeMap::new();
        for name in constants {
            for spelling in [name.to_owned(), format!("[{}]", name.to_ascii_uppercase())] {
                text.push_str("value = ");
                let start = u64::try_from(text.len()).unwrap();
                text.push_str(&spelling);
                let end = u64::try_from(text.len()).unwrap();
                text.push_str("\r\n");
                if !shadow || name != "vbTab" {
                    expected.insert((start, end), format!("VBA.Constants.{name}"));
                }
            }
        }
        text.push_str("value = VBA.Constants.vbTab\r\nEnd Sub\r\n");
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
        let mut symbols = BTreeSet::new();
        let mut sources = Vec::new();
        for site in &document.occurrences {
            if !document
                .provenance
                .iter()
                .any(|p| p.id == site.provenance && p.rule.as_deref() == Some(RULE))
            {
                continue;
            }
            let OccurrenceTarget::Resolved { symbol } = site.target else {
                panic!("constant must resolve")
            };
            let target = document.entities.iter().find(|e| e.id == symbol).unwrap();
            assert_eq!(target.kind, EntityKind::ExternalSymbol);
            assert!(target.flags.contains(&EntityFlag::External));
            assert!(target.evidence.source.is_none());
            assert_eq!(site.role, rootlight_ir::OccurrenceRole::Reference);
            let span = site.source.span();
            assert!(
                actual
                    .insert(
                        (span.start_byte(), span.end_byte()),
                        target.qualified_name.clone()
                    )
                    .is_none()
            );
            assert_eq!(
                document
                    .relations
                    .iter()
                    .filter(|edge| edge.subject == RelationEndpoint::Occurrence(site.id)
                        && edge.predicate == RelationPredicate::RefersTo
                        && edge.object == RelationEndpoint::Entity(symbol)
                        && edge.evidence.source.as_ref() == Some(&site.source))
                    .count(),
                1
            );
            assert!(
                !document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source == site.source
                        && gap.detail == "vba-binding-target-unavailable")
            );
            sources.push(site.source.clone());
            symbols.insert(symbol);
        }
        assert_eq!(actual, expected, "shadow={shadow}");
        let relationships = service
            .symbol_relationships(
                receipt.generation,
                symbols,
                vec![RelationFamily::References],
                Some(RelationDirection::Inbound),
                0,
                64,
                0,
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            relationships.data.returned_edges,
            u32::try_from(expected.len()).unwrap() + u32::from(!shadow)
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
        let mut actual_bytes: Vec<_> = read
            .data
            .chunks
            .iter()
            .map(|chunk| {
                assert!(chunk.extracted_from.is_some());
                chunk.bytes.clone()
            })
            .collect();
        let mut expected_bytes: Vec<_> = expected
            .keys()
            .map(|&(start, end)| {
                text.as_bytes()[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()]
                    .to_vec()
            })
            .collect();
        actual_bytes.sort();
        expected_bytes.sort();
        assert_eq!(actual_bytes, expected_bytes);
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
fn implicit_constants_require_read_syntax_and_unshadowed_namespaces() {
    let root = tempdir();
    for (header, sibling, sibling_body, expected) in [
        ("", "Other", "", 1),
        ("Private vbTab As String\r\n", "Other", "", 0),
        ("Private Const vbTab = 1\r\n", "Other", "", 0),
        (
            "#If Unknown Then\r\nPrivate vbTab As String\r\n#End If\r\n",
            "Other",
            "",
            0,
        ),
        (
            "Private Enum Codes\r\nvbTab = 1\r\nEnd Enum\r\n",
            "Other",
            "",
            0,
        ),
        ("", "vbTab", "", 0),
        ("", "Other", "Public Const vbTab = 1\r\n", 0),
        ("", "Other", "Private Const vbTab = 1\r\n", 1),
        (
            "",
            "Other",
            "Public Enum Codes\r\nvbTab = 1\r\nEnd Enum\r\n",
            0,
        ),
        ("#Const vbTab = 1\r\n", "Other", "", 1),
    ] {
        let text = format!(
            "Attribute VB_Name = \"Client\"\r\n{header}#If vbTab Then\r\n#End If\r\nPublic Sub Run()\r\nDim value As Variant\r\nvalue = vbTab\r\nvbTab = value\r\nSet vbTab = value\r\nCall vbTab\r\nvalue = vbTab(0)\r\nvalue = vbTab.Unknown\r\nvalue = vbTab$\r\nvalue = vbTabUnknown\r\nDim item As vbTab\r\nEnd Sub\r\nPublic Sub Local(ByVal vbTab As String)\r\nDim value As String\r\nvalue = vbTab\r\nEnd Sub\r\n"
        );
        let other = format!("Attribute VB_Name = \"{sibling}\"\r\n{sibling_body}");
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
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                document
                    .provenance
                    .iter()
                    .any(|p| p.id == site.provenance && p.rule.as_deref() == Some(RULE))
            })
            .collect();
        assert_eq!(
            sites.len(),
            expected,
            "header={header}, sibling={sibling}, body={sibling_body}"
        );
        if let Some(site) = sites.first() {
            assert_eq!(
                site.source.span().start_byte(),
                u64::try_from(text.find("value = vbTab").unwrap() + "value = ".len()).unwrap()
            );
        }
    }
}

#[test]
fn implicit_constants_require_referenced_namespace_evidence() {
    fn text(out: &mut Vec<u8>, value: &[u8]) {
        out.extend(u32::try_from(value.len()).unwrap().to_le_bytes());
        out.extend(value);
    }
    let root = tempdir();
    for alias in ["Other", "vBtAb"] {
        let mut records = Vec::new();
        records.extend(0x0016u16.to_le_bytes());
        text(&mut records, alias.as_bytes());
        records.extend(0x003eu16.to_le_bytes());
        text(
            &mut records,
            &alias
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        records.extend(0x000du16.to_le_bytes());
        records.extend(0u32.to_le_bytes());
        text(&mut records, b"*\\G{01234567-89AB-CDEF-1032-547698BADCFE}#1.2#409#C:\\unavailable\\model.tlb#Library");
        records.extend([0; 6]);
        let source = b"Attribute VB_Name = \"Client\"\r\nPublic Sub Run()\r\nDim value As String\r\nvalue = vbTab\r\nEnd Sub\r\n";
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
        fs::write(
            root.path().join("independent.xlsm"),
            fixture::typed_book(&[("Client", source, false)]),
        )
        .unwrap();
        let mut service = FirstSliceService::new(4).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        assert!(
            document
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::ExternalSymbol
                    && entity.flags.contains(&EntityFlag::External)
                    && entity.qualified_name.eq_ignore_ascii_case(alias))
        );
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|site| document
                    .provenance
                    .iter()
                    .any(|p| p.id == site.provenance && p.rule.as_deref() == Some(RULE)))
                .count(),
            1,
            "alias={alias}"
        );
    }
}
