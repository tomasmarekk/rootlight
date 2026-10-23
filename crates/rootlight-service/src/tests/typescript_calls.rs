//! Native module-call proofs must survive strict generation identity verification.

use super::*;
use rootlight_ir::{FactRef, RelationEndpoint, derive_relation_record_id};

#[test]
fn typescript_module_call_survives_merge_query_and_exact_source_read() {
    let source = include_bytes!(
        "../../../rootlight-adapter-treesitter/tests/fixtures/typescript_calls/vue-util.ts"
    );
    let fixture = TempDir::new().unwrap();
    fs::write(fixture.path().join("util.ts"), source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let function = |name: &str| {
        document
            .entities
            .iter()
            .find(|row| {
                row.language == "typescript"
                    && row.kind == EntityKind::Function
                    && row.canonical_name == name
            })
            .unwrap()
    };
    let owner = function("isPromise");
    let target = function("isDef");
    let occurrence = document
        .occurrences
        .iter()
        .find(|row| {
            row.role == OccurrenceRole::CallSite
                && row.source.span().start_byte() == 2018
                && row.enclosing == Some(owner.id)
                && row.target == (OccurrenceTarget::Resolved { symbol: target.id })
        })
        .expect("the exact written call remains resolved after identity-verified merge");
    let relation = document
        .relations
        .iter()
        .find(|row| {
            row.predicate == RelationPredicate::Calls
                && row.subject == RelationEndpoint::Entity(owner.id)
                && row.object == RelationEndpoint::Entity(target.id)
                && row.evidence.source.as_ref() == Some(&occurrence.source)
        })
        .unwrap();
    let mut parents = [
        FactRef::Entity(owner.id),
        FactRef::Fact(occurrence.id),
        FactRef::Entity(target.id),
    ];
    parents.sort_unstable();
    assert_eq!(relation.evidence.derivation, parents);
    assert_eq!(relation.id, derive_relation_record_id(relation).unwrap());
    let relationships = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([owner.id]),
            vec![RelationFamily::Calls],
            Some(rootlight_query::RelationDirection::Outbound),
            0,
            10,
            0,
            &deadline(),
        )
        .unwrap();
    assert!(relationships.data.groups.iter().any(|group| {
        group
            .items
            .iter()
            .any(|row| row.symbol == target.id && row.source_refs.contains(&occurrence.source))
    }));
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![occurrence.source.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].bytes, b"isDef");
}

const WRITTEN_VALUE_ORIGINAL: &[u8] =
    include_bytes!("../../tests/fixtures/typescript_value_uses/annotation-tests.ts");

fn typescript_complete_original_value_probe(
    role: OccurrenceRole,
    start: u64,
    end: u64,
    name: &str,
    definition_start: u64,
    definition_end: u64,
) {
    assert_eq!(WRITTEN_VALUE_ORIGINAL.len(), 856);
    assert_eq!(
        WRITTEN_VALUE_ORIGINAL
            .windows(2)
            .filter(|bytes| *bytes == b"\r\n")
            .count(),
        31
    );
    assert_eq!(&WRITTEN_VALUE_ORIGINAL[160..177], b"validateTestSuite");
    assert_eq!(&WRITTEN_VALUE_ORIGINAL[490..495], b"suite");
    let fixture = TempDir::new().unwrap();
    fs::write(
        fixture.path().join("annotation-tests.ts"),
        WRITTEN_VALUE_ORIGINAL,
    )
    .unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let definition = |name: &str, start: u64, end: u64| {
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect();
        assert_eq!(sites.len(), 1, "written local Definition {name}");
        let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
            panic!("written Definition resolves")
        };
        let entities: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.id == symbol)
            .collect();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].language, "typescript");
        assert_eq!(entities[0].kind, EntityKind::Variable);
        assert_eq!(entities[0].canonical_name, name);
        assert_eq!(sites[0].source.repository(), receipt.repository);
        assert_eq!(sites[0].source.generation(), receipt.generation);
        assert_eq!(
            sites[0].source.content_hash(),
            content_hash(WRITTEN_VALUE_ORIGINAL)
        );
        symbol
    };
    let validator = definition("validateTestSuite", 160, 177);
    let suite = definition("suite", 490, 495);
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(validator)
                && edge.object == RelationEndpoint::Entity(suite))
    );
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 589
                && site.source.span().end_byte() == 594)
    );
    for symbol in [validator, suite] {
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls
                    && edge.object == RelationEndpoint::Entity(symbol))
        );
    }
    let file = document
        .files
        .iter()
        .find(|file| file.language == "typescript")
        .unwrap();
    let full = rootlight_ir::SourceRef::new(
        receipt.repository,
        receipt.generation,
        rootlight_ir::SourceSpan::new(file.id, 0, 856).unwrap(),
        content_hash(WRITTEN_VALUE_ORIGINAL),
        None,
    );
    let full_read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![full],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(full_read.data.chunks.len(), 1);
    assert_eq!(full_read.data.chunks[0].bytes, WRITTEN_VALUE_ORIGINAL);
    let target = definition(name, definition_start, definition_end);
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == role
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
                && site.target == (OccurrenceTarget::Resolved { symbol: target })
        })
        .collect();
    assert_eq!(sites.len(), 1, "exact local {role:?} {name}@{start}..{end}");
    let site = sites[0];
    assert_eq!(site.source.repository(), receipt.repository);
    assert_eq!(site.source.generation(), receipt.generation);
    assert_eq!(
        site.source.content_hash(),
        content_hash(WRITTEN_VALUE_ORIGINAL)
    );
    assert_eq!(site.source.span().file(), file.id);
    assert_eq!(site.evidence.source.as_ref(), Some(&site.source));
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![site.source.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        read.data.chunks[0].bytes,
        &WRITTEN_VALUE_ORIGINAL[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()]
    );
}

#[test]
fn typescript_complete_original_callee_name_references_local_variable_without_callable_claim() {
    typescript_complete_original_value_probe(
        OccurrenceRole::Reference,
        538,
        555,
        "validateTestSuite",
        160,
        177,
    );
}

#[test]
fn typescript_complete_original_argument_reads_its_block_const_binding() {
    typescript_complete_original_value_probe(OccurrenceRole::Read, 556, 561, "suite", 490, 495);
}

#[test]
fn typescript_complete_original_native_value_use_context_is_reported() {
    let fixture = TempDir::new().unwrap();
    fs::write(
        fixture.path().join("annotation-tests.ts"),
        WRITTEN_VALUE_ORIGINAL,
    )
    .unwrap();
    let repository = derive_repository(b"typescript-value-context").id();
    let generation = GenerationId::from_bytes([67; 20]);
    let root = RepositoryRoot::open(repository, fixture.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("annotation-tests.ts")).unwrap(),
            65536,
        )
        .unwrap();
    let service = FirstSliceService::new(2).unwrap();
    let request = structural_analysis_request(
        repository,
        generation,
        &snapshot,
        false,
        "typescript",
        "utf-8",
        &service.analysis_limits,
    )
    .unwrap();
    let parsed = rootlight_adapter_sdk::execute_parse(
        service.structural_parsers["typescript"].as_ref(),
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let mut selected = BTreeSet::new();
    for (start, end) in [(160, 177), (490, 495), (538, 555), (556, 561)] {
        let matches: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.span().start_byte() == start
                    && fact.span().end_byte() == end
                    && fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Occurrence
            })
            .collect();
        assert!(!matches.is_empty(), "native source witness {start}..{end}");
        for fact in matches {
            let mut current = Some(fact.local_id());
            for _ in 0..parsed.facts().len() {
                let Some(id) = current else { break };
                if !selected.insert(id) {
                    break;
                }
                current = parsed
                    .facts()
                    .iter()
                    .find(|fact| fact.local_id() == id)
                    .unwrap()
                    .parent();
            }
        }
    }
    let rows: Vec<_> = parsed.facts().iter().filter(|fact| selected.contains(&fact.local_id())).map(|fact| serde_json::json!({"local_id":fact.local_id(),"parent":fact.parent(),"kind":format!("{:?}",fact.kind()),"syntax":fact.syntax_kind().as_str(),"depth":fact.depth(),"start":fact.span().start_byte(),"end":fact.span().end_byte(),"text":std::str::from_utf8(&WRITTEN_VALUE_ORIGINAL[usize::try_from(fact.span().start_byte()).unwrap()..usize::try_from(fact.span().end_byte()).unwrap()]).unwrap()})).collect();
    println!(
        "{}",
        serde_json::json!({"schema":"typescript-complete-original-native-value-context/1","bytes":856,"coverage":format!("{:?}",parsed.report().coverage().status()),"facts":rows})
    );
}

fn const_value_document(raw: &[u8]) -> rootlight_ir::NormalizedIrDocument {
    let fixture = TempDir::new().unwrap();
    fs::write(fixture.path().join("written.ts"), raw).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.language == "typescript")
        .unwrap();
    let full = rootlight_ir::SourceRef::new(
        receipt.repository,
        receipt.generation,
        rootlight_ir::SourceSpan::new(file.id, 0, u64::try_from(raw.len()).unwrap()).unwrap(),
        content_hash(raw),
        None,
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![full],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].bytes, raw);
    document.clone()
}

#[test]
fn typescript_const_value_reads_keep_lexical_reference_and_no_callable_target() {
    for source in [
        "const value = 1; const output = consume(value);",
        "const value = 1; if (true) { const output = consume(value); }",
        "// const value = ignored;\nconst value = 'value'; const output = consume(value);",
    ] {
        let document = const_value_document(source.as_bytes());
        let definition = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte()
                        == u64::try_from(source.rfind("const value =").unwrap() + 6).unwrap()
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = definition.target else {
            panic!("const owns its source definition")
        };
        assert!(
            document
                .entities
                .iter()
                .any(|entity| entity.id == symbol && entity.kind == EntityKind::Variable)
        );
        let start = u64::try_from(source.rfind("value").unwrap()).unwrap();
        let mut ids = BTreeSet::new();
        for role in [OccurrenceRole::Reference, OccurrenceRole::Read] {
            let rows: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == role
                        && site.source.span().start_byte() == start
                        && site.source.span().end_byte() == start + 5
                        && site.target == (OccurrenceTarget::Resolved { symbol })
                })
                .collect();
            assert_eq!(rows.len(), 1, "{source} {role:?}");
            assert!(ids.insert(rows[0].id));
            assert_eq!(
                rows[0].source.content_hash(),
                content_hash(source.as_bytes())
            );
            assert!(document.relations.iter().any(|edge| edge.predicate
                == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(rows[0].id)
                && edge.object == RelationEndpoint::Entity(symbol)
                && edge.evidence.source.as_ref() == Some(&rows[0].source)));
        }
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls
                    && edge.object == RelationEndpoint::Entity(symbol))
        );
    }
}

#[test]
fn typescript_const_proof_refuses_rebinding_shadowing_and_unmodeled_value_positions() {
    for source in [
        "let value = 1; const output = consume(value);",
        "var value = 1; const output = consume(value);",
        "const value = 1; { const value = 2; const output = consume(value); }",
        "const value = 1; value = 2; const output = consume(value);",
        "const value = 1; value++; const output = consume(value);",
        "const value = 1; for (value of source) {} const output = consume(value);",
        "const value = 1; function other(value:unknown){} const output = consume(value);",
        "const value = 1; const output = (() => value);",
        "const value = 1; const output = receiver.consume(value);",
        "const value = 1; const output = consume?.(value);",
        "const value = 1; const output = consume({value});",
        "const value = 1; const output = consume(value.member);",
        "const value = 1; const output = consume(value[index]);",
        "const value = 1; eval('value = 2'); const output = consume(value);",
        r"const value = 1; const val\u0075e = 2; const output = consume(value);",
        "const value = 1; const output = consume(value, value = 2);",
        "const output = consume(value); const value = 1;",
        "const value = consume(value);",
        "{ const value = 1; } const output = consume(value);",
        "const value = 1, other = 2; const output = consume(value);",
        "const value: number = 1; const output = consume(value);",
        "const value = 1; const output = consume(value;",
    ] {
        let document = const_value_document(source.as_bytes());
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && site.syntax_kind == "typescript.const_value_read.read"),
            "{source}"
        );
    }
}

#[test]
fn typescript_renamed_const_callee_and_argument_keep_variables_and_exact_relations() {
    let source = "const apply = factory; const sample = payload; const report = apply(sample);";
    let document = const_value_document(source.as_bytes());
    for (name, role) in [
        ("apply", OccurrenceRole::Reference),
        ("sample", OccurrenceRole::Read),
    ] {
        let definition_start = u64::try_from(source.find(name).unwrap()).unwrap();
        let use_start = u64::try_from(source.rfind(name).unwrap()).unwrap();
        let width = u64::try_from(name.len()).unwrap();
        let definition = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == definition_start
                    && site.source.span().end_byte() == definition_start + width
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = definition.target else {
            panic!("const owns its exact definition")
        };
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(entity.kind, EntityKind::Variable);
        assert_eq!(entity.canonical_name, name);
        let uses: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == role
                    && site.source.span().start_byte() == use_start
                    && site.source.span().end_byte() == use_start + width
                    && site.target == OccurrenceTarget::Resolved { symbol }
            })
            .collect();
        assert_eq!(uses.len(), 1, "{name} {role:?}");
        assert!(document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(uses[0].id)
                && edge.object == RelationEndpoint::Entity(symbol)
                && edge.evidence.source.as_ref() == Some(&uses[0].source)
        }));
        assert!(!document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.object == RelationEndpoint::Entity(symbol)
        }));
    }
}

#[test]
fn typescript_complete_original_const_uses_preserve_four_physical_encodings_and_noop() {
    let text = std::str::from_utf8(WRITTEN_VALUE_ORIGINAL).unwrap();
    for encoding in [0, 1, 2, 3] {
        let (raw, bom, width) = match encoding {
            0 => (WRITTEN_VALUE_ORIGINAL.to_vec(), 0, 1),
            1 => (
                [b"\xef\xbb\xbf".as_slice(), WRITTEN_VALUE_ORIGINAL].concat(),
                3,
                1,
            ),
            2 | 3 => {
                let mut bytes = if encoding == 2 {
                    vec![0xff, 0xfe]
                } else {
                    vec![0xfe, 0xff]
                };
                for unit in text.encode_utf16() {
                    bytes.extend(if encoding == 2 {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    });
                }
                (bytes, 2, 2)
            }
            _ => unreachable!(),
        };
        let fixture = TempDir::new().unwrap();
        fs::write(fixture.path().join("original.ts"), &raw).unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let first = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let document = service
            .loaded_generation_snapshot(first.generation)
            .unwrap()
            .document()
            .clone();
        for (start, end, definition_start, definition_end) in
            [(538, 555, 160, 177), (556, 561, 490, 495)]
        {
            let definitions: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.source.span().start_byte() == bom + width * definition_start
                        && site.source.span().end_byte() == bom + width * definition_end
                })
                .collect();
            assert_eq!(definitions.len(), 1, "encoding {encoding}");
            let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
                panic!("definition resolves")
            };
            for role in [OccurrenceRole::Reference, OccurrenceRole::Read] {
                let rows: Vec<_> = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.role == role
                            && site.source.span().start_byte() == bom + width * start
                            && site.source.span().end_byte() == bom + width * end
                            && site.target == (OccurrenceTarget::Resolved { symbol })
                    })
                    .collect();
                assert_eq!(rows.len(), 1, "encoding {encoding} {role:?}");
                assert_eq!(rows[0].source.content_hash(), content_hash(&raw));
                assert_eq!(rows[0].source.repository(), first.repository);
                assert_eq!(rows[0].source.generation(), first.generation);
                let read = service
                    .source_read_with_options_and_budget(
                        first.generation,
                        vec![rows[0].source.clone()],
                        SourceReadOptions::new()
                            .with_context_lines_before(0)
                            .with_context_lines_after(0)
                            .with_encoding(SourceEncoding::Bytes),
                        FirstSliceBudget::default(),
                        &deadline(),
                    )
                    .unwrap();
                assert_eq!(
                    read.data.chunks[0].bytes,
                    raw[usize::try_from(bom + width * start).unwrap()
                        ..usize::try_from(bom + width * end).unwrap()]
                );
            }
        }
        let second = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        assert_eq!(second.generation, first.generation);
        assert_eq!(
            service
                .loaded_generation_snapshot(second.generation)
                .unwrap()
                .document(),
            &document
        );
    }
}
