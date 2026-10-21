//! Durable F# profile selection, authored declarations and exact source identity.
//! Structural declarations do not establish cross-file binding or execution.

use super::tests::logical_lexical_envelopes;
use super::*;
use rootlight_ir::{ContainerRef, EntityKind, OccurrenceRole, OccurrenceTarget, RelationEndpoint};
use std::fs;

const IMPLEMENTATION: &str = "module Sample.Root\nopen System\nmodule Nested =\n    let value = 3\n    let ``value with spaces`` = value\n    let add x y = x + y\n    let rec first n = n + 1\n    and second n = n + 2\n    let repeat limit =\n        for _ = 1 to limit do\n            ()\ntype Record = { Name: string; mutable Count: int }\ntype Choice = | Empty | Named of name: string\ntype Sample =\n      First = 0\n    | Next = 1\ntype Alias = System.String\ntype Constructed<'T> = Container<'T>.Builder\ntype Container(seed: int) =\n    member this.Read amount = seed + amount\n    member this.Value = seed\n    new () = Container(0)\n";
const SIGNATURE: &str = "module Sample.Root\nopen System\nmodule Nested =\n    val value: int\n    val ``value with spaces``: int\n    val add: int -> int -> int\n    val repeat: int -> unit\ntype Record = { Name: string; mutable Count: int }\ntype Choice = | Empty | Named of name: string\ntype Sample =\n      First = 0\n    | Next = 1\ntype Alias = System.String\ntype Constructed<'T> = Container<'T>.Builder\n[<AbstractClass>]\ntype Container =\n    new: seed:int -> Container\n    abstract member Read: unit -> int\n    abstract member Value: int\n    abstract member ``abstract``: int\n    abstract member FunctionValue: (int -> int)\n";

fn assert_signature_mask_gap_preserves_structure(
    document: &NormalizedIrDocument,
    file: &rootlight_ir::FileRecord,
) {
    use rootlight_ir::{CoverageScope, FactDomain};
    assert_eq!(file.language, "fsharp-signature");
    let source = file.evidence.source.as_ref().unwrap();
    let gaps = document
        .skipped_regions
        .iter()
        .filter(|gap| {
            gap.source.span().file() == file.id && gap.detail == "fsharp-signature-mask-unavailable"
        })
        .collect::<Vec<_>>();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].domain, FactDomain::Extensions);
    assert_eq!(gaps[0].reason, SkippedRegionReason::UnsupportedConstruct);
    assert_eq!(&gaps[0].source, source);
    assert_eq!(gaps[0].evidence.source.as_ref(), Some(source));
    assert_eq!(gaps[0].provenance, file.provenance);
    for domain in [
        FactDomain::Files,
        FactDomain::Entities,
        FactDomain::Occurrences,
    ] {
        let records = document
            .coverage_records
            .iter()
            .filter(|coverage| {
                coverage.scope == CoverageScope::File(file.id) && coverage.domain == domain
            })
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].status,
            CoverageStatus::Complete,
            "structural {domain:?}"
        );
        assert_eq!(records[0].skipped, 0);
        assert_eq!(records[0].discovered, records[0].indexed);
    }
    let extension = document
        .coverage_records
        .iter()
        .find(|coverage| {
            coverage.scope == CoverageScope::File(file.id)
                && coverage.domain == FactDomain::Extensions
        })
        .unwrap();
    assert_eq!(extension.status, CoverageStatus::Bounded);
    assert_eq!(extension.skipped, 1);
    assert_eq!(extension.discovered, extension.indexed + extension.skipped);
    assert!(
        !document
            .extensions
            .iter()
            .filter(|envelope| { envelope.namespace == rootlight_ir::FSHARP_BINDING_NAMESPACE })
            .any(|envelope| {
                let claim = rootlight_ir::decode_fsharp_binding_envelope(envelope).unwrap();
                claim.scope().file() == file.id
                    && matches!(
                        claim.binding(),
                        rootlight_ir::FSharpBinding::Context
                            | rootlight_ir::FSharpBinding::SignatureValue { .. }
                            | rootlight_ir::FSharpBinding::SignatureType { .. }
                            | rootlight_ir::FSharpBinding::SignatureAlias { .. }
                    )
            })
    );
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

fn policy() -> FirstSliceStoragePolicy {
    FirstSliceStoragePolicy::resolve_user_config(
        Some("version='1.3'\n[storage]\nretained_generations=4\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nfs='fsharp'\n"),
        4,
    )
    .unwrap()
}

fn index(
    service: &mut FirstSliceService,
    repository: &Path,
    options: FirstSliceIndexOptions,
) -> FirstSliceIndexCommit {
    let cancellation = deadline();
    let prepared = service
        .prepare_repository_with_options(repository, options, &cancellation)
        .unwrap();
    service
        .publish_prepared_with_metrics(prepared, &cancellation)
        .unwrap()
}

fn exact_source(
    service: &FirstSliceService,
    reference: &SourceRef,
    language: &str,
    expected: &[u8],
) {
    let read = service
        .source_read_with_options_and_budget(
            reference.generation(),
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(rootlight_source::SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.generation, reference.generation());
    assert_eq!(read.data.chunks.len(), 1);
    let chunk = &read.data.chunks[0];
    assert_eq!(&chunk.reference, reference);
    assert_eq!(chunk.content_hash, reference.content_hash());
    assert_eq!(chunk.start_byte, reference.span().start_byte());
    assert_eq!(chunk.end_byte, reference.span().end_byte());
    assert_eq!(chunk.language, language);
    assert_eq!(chunk.encoding, rootlight_query::SourceChunkEncoding::Bytes);
    assert_eq!(chunk.bytes, expected);
}

#[test]
fn fsharp_match_bindings_preserve_scrutinee_and_arm_queries() {
    let source = "module Match\nlet echo input =\n    match input with\n    | value -> value\n";
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    fs::write(repository.path().join("Match.fs"), source).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy(), paths.state_dir(), &deadline())
            .unwrap();
    let indexed = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
    );
    let receipt = indexed.receipt();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = &document.files[0];
    assert_eq!(
        (file.path.as_str(), file.language.as_str()),
        ("Match.fs", "fsharp")
    );
    let formal = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "input")
        .unwrap();
    let scrutinee_start = source.find("match input").unwrap() + "match ".len();
    let expected = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(
            file.id,
            u64::try_from(scrutinee_start).unwrap(),
            u64::try_from(scrutinee_start + "input".len()).unwrap(),
        )
        .unwrap(),
        file.content_hash,
        None,
    );
    let reads = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Reference && site.source == expected)
        .collect::<Vec<_>>();
    assert_eq!(
        reads.len(),
        1,
        "ordinary match scrutinee reference must be retained"
    );
    assert_eq!(
        reads[0].target,
        OccurrenceTarget::Resolved { symbol: formal.id }
    );
    exact_source(&service, &expected, "fsharp", b"input");
    let relationships = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([formal.id]),
            vec![RelationFamily::References],
            Some(RelationDirection::Inbound),
            0,
            16,
            0,
            &deadline(),
        )
        .unwrap();
    assert_eq!(relationships.data.returned_edges, 1);
    assert!(!relationships.data.truncated);
    assert_eq!(
        relationships.data.groups[0].items[0].source_refs,
        vec![expected]
    );
    let binder_start = source.find("| value ->").unwrap() + "| ".len();
    let binder = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "value")
        .unwrap();
    let explained = service
        .symbol_explain(receipt.generation, binder.id, &deadline())
        .unwrap();
    let definition = explained
        .data
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Definition
                && site.target == (OccurrenceTarget::Resolved { symbol: binder.id })
        })
        .unwrap();
    assert_eq!(
        definition.source.span().start_byte(),
        u64::try_from(binder_start).unwrap()
    );
    exact_source(&service, &definition.source, "fsharp", b"value");
    let located = service
        .code_locate_with_filters_and_budget(
            receipt.generation,
            "value".to_owned(),
            LocateMode::Exact,
            vec!["fsharp".to_owned()],
            vec!["Match.fs".to_owned()],
            16,
            0,
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert!(
        located
            .data
            .hits
            .iter()
            .any(|hit| hit.symbol == Some(binder.id))
    );
    let incoming = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([binder.id]),
            vec![RelationFamily::References],
            Some(RelationDirection::Inbound),
            0,
            16,
            0,
            &deadline(),
        )
        .unwrap();
    assert_eq!(incoming.data.returned_edges, 1);
    assert!(!incoming.data.truncated);
    exact_source(
        &service,
        &incoming.data.groups[0].items[0].source_refs[0],
        "fsharp",
        b"value",
    );
}

#[test]
fn fsharp_match_shadowing_survives_mutation_and_durable_reopen() {
    let source = "module Matches\r\n// ☃ 🧭\r\nlet echo value = value\r\nlet choose input =\r\n    match input with\r\n    | input -> echo input\r\n    | _ -> input\r\nlet siblings input =\r\n    match input with\r\n    | value -> value\r\n    | value -> value\r\nlet resume input =\r\n    let selected = match input with | input -> input\r\n    input + selected\r\nlet outside () = value\r\n";
    let body_changed = source.replacen("| input -> echo input", "| input -> echo value", 1);
    let binder_changed = source.replacen("| input -> echo input", "| local -> echo input", 1);
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let encode = |text: &str| {
            let mut bytes = if encoding == "utf-8" {
                Vec::new()
            } else {
                typed_formal_leaf_bytes("\u{feff}", encoding)
            };
            bytes.extend(typed_formal_leaf_bytes(text, encoding));
            bytes
        };
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let mut service =
            FirstSliceService::new_durable_with_policy(policy(), paths.state_dir(), &deadline())
                .unwrap();
        let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
        let texts = [source, body_changed.as_str(), binder_changed.as_str()];
        let bytes = texts.map(encode);
        let mut receipts = Vec::new();
        let mut symbols = Vec::new();
        let mut retained = Vec::new();
        for (phase, (text, raw)) in texts.iter().zip(&bytes).enumerate() {
            fs::write(repository.path().join("Matches.fs"), raw).unwrap();
            let indexed = index(&mut service, repository.path(), options);
            assert_eq!(indexed.evidence().changed_files, 1);
            assert_eq!(indexed.evidence().rebuilt_files, 1);
            let receipt = indexed.receipt().clone();
            let (identities, full) =
                assert_match_generation(&service, &receipt, text, raw, encoding, phase);
            if phase == 1 {
                assert_eq!(identities, symbols[0], "body-only mutation: {encoding}");
            } else if phase == 2 {
                for (key, symbol) in &symbols[0] {
                    if *key != "choose.arm" {
                        assert_eq!(identities[key], *symbol, "{key}: {encoding}");
                    }
                }
                assert_ne!(identities["choose.arm"], symbols[0]["choose.arm"]);
            }
            receipts.push(receipt);
            symbols.push(identities);
            retained.push(full);
        }
        for (reference, raw) in retained.iter().zip(&bytes) {
            exact_source(&service, reference, "fsharp", raw);
        }
        let clean = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
        );
        assert_eq!(
            clean.receipt().logical_snapshot,
            receipts[2].logical_snapshot
        );
        let last = clean.receipt().clone();
        assert_eq!(
            assert_match_generation(&service, &last, texts[2], &bytes[2], encoding, 2).0,
            symbols[2]
        );
        drop(service);
        let mut reopened =
            FirstSliceService::new_durable_with_policy(policy(), paths.state_dir(), &deadline())
                .unwrap();
        for (phase, ((receipt, text), raw)) in receipts.iter().zip(texts).zip(&bytes).enumerate() {
            assert_eq!(
                assert_match_generation(&reopened, receipt, text, raw, encoding, phase).0,
                symbols[phase]
            );
        }
        let noop = index(&mut reopened, repository.path(), options);
        assert_eq!(noop.receipt(), &last);
        assert_eq!(
            (noop.evidence().rebuilt_files, noop.evidence().rebuilt_facts),
            (0, 0)
        );
    }
}

fn assert_match_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    raw: &[u8],
    encoding: &str,
    phase: usize,
) -> (BTreeMap<&'static str, SymbolId>, SourceRef) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(
        (
            file.path.as_str(),
            file.language.as_str(),
            file.encoding.as_str()
        ),
        ("Matches.fs", "fsharp", encoding)
    );
    let reference = |start: usize, text: &str| {
        assert_eq!(source.get(start..start + text.len()), Some(text));
        let start = typed_formal_leaf_bytes(source.get(..start).unwrap(), encoding).len()
            + usize::from(encoding != "utf-8") * 2;
        SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                file.id,
                u64::try_from(start).unwrap(),
                u64::try_from(start + typed_formal_leaf_bytes(text, encoding).len()).unwrap(),
            )
            .unwrap(),
            file.content_hash,
            None,
        )
    };
    let full = file.evidence.source.as_ref().unwrap();
    assert_eq!(file.byte_length, u64::try_from(raw.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(raw));
    assert_eq!(full.repository(), receipt.repository);
    assert_eq!(full.generation(), receipt.generation);
    assert_eq!(full.content_hash(), file.content_hash);
    assert_eq!(
        full.span(),
        SourceSpan::new(file.id, 0, file.byte_length).unwrap()
    );
    exact_source(service, full, "fsharp", raw);
    let mut symbols = BTreeMap::new();
    let mut definitions = Vec::new();
    for (key, kind, needle, offset, written) in [
        ("echo", EntityKind::Function, "echo value =", 0, "echo"),
        (
            "echo.value",
            EntityKind::Parameter,
            "echo value =",
            "echo ".len(),
            "value",
        ),
        (
            "choose",
            EntityKind::Function,
            "choose input =",
            0,
            "choose",
        ),
        (
            "choose.input",
            EntityKind::Parameter,
            "choose input =",
            "choose ".len(),
            "input",
        ),
        (
            "choose.arm",
            EntityKind::Variable,
            if phase == 2 {
                "| local ->"
            } else {
                "| input -> echo"
            },
            "| ".len(),
            if phase == 2 { "local" } else { "input" },
        ),
        (
            "siblings",
            EntityKind::Function,
            "siblings input =",
            0,
            "siblings",
        ),
        (
            "siblings.input",
            EntityKind::Parameter,
            "siblings input =",
            "siblings ".len(),
            "input",
        ),
        (
            "resume",
            EntityKind::Function,
            "resume input =",
            0,
            "resume",
        ),
        (
            "resume.input",
            EntityKind::Parameter,
            "resume input =",
            "resume ".len(),
            "input",
        ),
        (
            "outside",
            EntityKind::Function,
            "outside () =",
            0,
            "outside",
        ),
        (
            "selected",
            EntityKind::Variable,
            "selected = match",
            0,
            "selected",
        ),
        (
            "resume.arm",
            EntityKind::Variable,
            "with | input -> input",
            "with | ".len(),
            "input",
        ),
    ] {
        definitions.push((key, kind, source.find(needle).unwrap() + offset, written));
    }
    for (ordinal, (start, _)) in source.match_indices("| value -> value").enumerate() {
        definitions.push((
            if ordinal == 0 {
                "sibling.first"
            } else {
                "sibling.second"
            },
            EntityKind::Variable,
            start + "| ".len(),
            "value",
        ));
    }
    for (key, kind, start, name) in definitions {
        let expected = reference(start, name);
        let sites = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Definition && site.source == expected)
            .collect::<Vec<_>>();
        assert_eq!(sites.len(), 1, "definition {key}");
        let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
            panic!("unresolved definition {key}");
        };
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!((entity.kind, entity.canonical_name.as_str()), (kind, name));
        symbols.insert(key, symbol);
        exact_source(
            service,
            &expected,
            "fsharp",
            &typed_formal_leaf_bytes(name, encoding),
        );
        let explained = service
            .symbol_explain(receipt.generation, symbol, &deadline())
            .unwrap();
        assert!(
            explained
                .data
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Definition && site.source == expected)
        );
        let located = service
            .code_locate_with_filters_and_budget(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                vec!["fsharp".to_owned()],
                vec!["Matches.fs".to_owned()],
                32,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert!(
            located
                .data
                .hits
                .iter()
                .any(|hit| hit.symbol == Some(symbol)),
            "locate {key}"
        );
    }
    assert_ne!(symbols["sibling.first"], symbols["sibling.second"]);
    assert_ne!(symbols["choose.arm"], symbols["choose.input"]);
    for (binder, owner) in [
        ("choose.arm", "choose"),
        ("sibling.first", "siblings"),
        ("sibling.second", "siblings"),
        ("resume.arm", "selected"),
    ] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbols[binder])
            .unwrap();
        assert_eq!(
            entity.container,
            Some(ContainerRef::Entity(symbols[owner])),
            "{binder} container"
        );
    }
    let matches = source
        .match_indices("match input with")
        .map(|(start, _)| start + "match ".len())
        .collect::<Vec<_>>();
    let mut reads = vec![
        (
            "echo",
            source.find("echo value = value").unwrap() + "echo value = ".len(),
            "value",
            Some("echo.value"),
        ),
        ("choose", matches[0], "input", Some("choose.input")),
        ("siblings", matches[1], "input", Some("siblings.input")),
        ("selected", matches[2], "input", Some("resume.input")),
        (
            "choose",
            source.find("-> echo ").unwrap() + "-> ".len(),
            "echo",
            Some("echo"),
        ),
        (
            "choose",
            source.find("-> echo ").unwrap() + "-> echo ".len(),
            if phase == 1 { "value" } else { "input" },
            if phase == 1 {
                None
            } else if phase == 2 {
                Some("choose.input")
            } else {
                Some("choose.arm")
            },
        ),
        (
            "choose",
            source.find("| _ -> input").unwrap() + "| _ -> ".len(),
            "input",
            Some("choose.input"),
        ),
        (
            "selected",
            source.find("with | input -> input").unwrap() + "with | input -> ".len(),
            "input",
            Some("resume.arm"),
        ),
        (
            "resume",
            source.find("    input + selected").unwrap() + "    ".len(),
            "input",
            Some("resume.input"),
        ),
        (
            "resume",
            source.find("    input + selected").unwrap() + "    input + ".len(),
            "selected",
            Some("selected"),
        ),
        (
            "outside",
            source.find("outside () = value").unwrap() + "outside () = ".len(),
            "value",
            None,
        ),
    ];
    for (ordinal, (start, _)) in source.match_indices("| value -> value").enumerate() {
        reads.push((
            "siblings",
            start + "| value -> ".len(),
            "value",
            Some(if ordinal == 0 {
                "sibling.first"
            } else {
                "sibling.second"
            }),
        ));
    }
    let mut incoming = BTreeMap::<SymbolId, Vec<(SymbolId, SourceRef)>>::new();
    for (owner, start, name, target) in reads {
        let expected = reference(start, name);
        let sites = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference && site.source == expected)
            .collect::<Vec<_>>();
        assert_eq!(sites.len(), 1, "read {name} at {start}");
        assert_eq!(
            sites[0].enclosing,
            Some(symbols[owner]),
            "read owner {name} at {start}"
        );
        let edges = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(sites[0].id)
            })
            .collect::<Vec<_>>();
        if let Some(target) = target {
            assert_eq!(
                sites[0].target,
                OccurrenceTarget::Resolved {
                    symbol: symbols[target]
                },
                "read {name} at {start}"
            );
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].object, RelationEndpoint::Entity(symbols[target]));
            assert_eq!(edges[0].evidence.source.as_ref(), Some(&expected));
            incoming
                .entry(symbols[target])
                .or_default()
                .push((symbols[owner], expected.clone()));
        } else {
            assert!(matches!(
                sites[0].target,
                OccurrenceTarget::Unresolved { .. }
            ));
            assert!(edges.is_empty());
        }
        exact_source(
            service,
            &expected,
            "fsharp",
            &typed_formal_leaf_bytes(name, encoding),
        );
    }
    for symbol in symbols.values().copied() {
        let result = service
            .symbol_relationships(
                receipt.generation,
                BTreeSet::from([symbol]),
                vec![RelationFamily::References],
                Some(RelationDirection::Inbound),
                0,
                32,
                0,
                &deadline(),
            )
            .unwrap();
        assert!(!result.data.truncated);
        let mut actual = result
            .data
            .groups
            .iter()
            .flat_map(|group| {
                group.items.iter().map(|item| {
                    assert_eq!(item.source_refs.len(), 1);
                    (item.symbol, item.source_refs[0].clone())
                })
            })
            .collect::<Vec<_>>();
        let mut expected = incoming.get(&symbol).cloned().unwrap_or_default();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected);
    }
    let calls = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([symbols["choose"]]),
            vec![RelationFamily::Calls],
            Some(RelationDirection::Outbound),
            0,
            16,
            0,
            &deadline(),
        )
        .unwrap();
    assert_eq!(calls.data.returned_edges, 1);
    assert_eq!(calls.data.groups[0].items[0].symbol, symbols["echo"]);
    let application = if phase == 1 {
        "echo value"
    } else {
        "echo input"
    };
    assert_eq!(
        calls.data.groups[0].items[0].source_refs,
        vec![reference(
            source.find("-> echo ").unwrap() + "-> ".len(),
            application
        )]
    );
    (symbols, full.clone())
}

fn assert_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    implementation: &str,
    signature: &str,
) -> (BTreeSet<SymbolId>, SourceRef) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 2);
    assert!(!document.diagnostics.iter().any(|diagnostic| {
        matches!(
            diagnostic.code.as_str(),
            "unsupported-language" | "unsupported-encoding"
        )
    }));
    assert!(!document.skipped_regions.iter().any(|gap| {
        matches!(
            gap.reason,
            SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
        )
    }));
    let mut symbols = BTreeSet::new();
    let mut implementation_reference = None;
    for (path, language, source) in [
        ("Library.fs", "fsharp", implementation),
        ("Library.fsi", "fsharp-signature", signature),
    ] {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, language);
        assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
        assert_eq!(file.content_hash, content_hash(source.as_bytes()));
        let full = file.evidence.source.as_ref().unwrap();
        assert_eq!(full.repository(), receipt.repository);
        assert_eq!(full.generation(), receipt.generation);
        assert_eq!(full.span().file(), file.id);
        assert_eq!(full.span().start_byte(), 0);
        assert_eq!(full.span().end_byte(), file.byte_length);
        exact_source(service, full, language, source.as_bytes());
        if language == "fsharp" {
            implementation_reference = Some(full.clone());
        }
        let provenance = document
            .provenance
            .iter()
            .find(|record| record.id == file.provenance)
            .unwrap();
        assert_eq!(provenance.producer_kind, ProducerKind::Parser);
        assert_eq!(provenance.language, language);
        let coverage = service
            .source_file_coverage_until(receipt.generation, file.id, &deadline())
            .unwrap();
        assert_eq!(coverage.language, language);
        assert_eq!(coverage.tier, AnalysisTier::TierD);
        assert_ne!(coverage.status, CoverageStatus::Complete);

        let expected = [
            ("value", EntityKind::Variable),
            ("``value with spaces``", EntityKind::Variable),
            ("Record", EntityKind::Struct),
            ("Choice", EntityKind::Union),
            ("Sample", EntityKind::Enum),
            ("First", EntityKind::Constant),
            ("Next", EntityKind::Constant),
            ("Alias", EntityKind::TypeAlias),
            ("Constructed", EntityKind::TypeAlias),
            ("Name", EntityKind::Field),
            ("Count", EntityKind::Field),
            ("Empty", EntityKind::Constructor),
            ("Named", EntityKind::Constructor),
            ("Container", EntityKind::Class),
            ("Read", EntityKind::Method),
            ("Value", EntityKind::Property),
            ("new", EntityKind::Constructor),
        ];
        for (name, kind) in expected
            .into_iter()
            .chain(["add", "repeat"].map(|name| {
                (
                    name,
                    if language == "fsharp" {
                        EntityKind::Function
                    } else {
                        EntityKind::Variable
                    },
                )
            }))
            .chain(
                [
                    ("``abstract``", EntityKind::Property),
                    ("FunctionValue", EntityKind::Property),
                ]
                .into_iter()
                .filter(|_| language == "fsharp-signature"),
            )
        {
            let matches: Vec<_> = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.language == language
                        && entity.canonical_name == name
                        && entity.kind == kind
                        && entity
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|source| source.span().file() == file.id)
                })
                .collect();
            assert_eq!(
                matches.len(),
                1,
                "{language}: exact authored declaration {name}: {:?}",
                document
                    .entities
                    .iter()
                    .map(|entity| (&entity.language, &entity.canonical_name, entity.kind))
                    .collect::<Vec<_>>()
            );
            let entity = matches[0];
            assert!(symbols.insert(entity.id));
            if language == "fsharp-signature" {
                let written = match name {
                    "Read" => Some("Read: unit -> int"),
                    "Value" => Some("Value: int"),
                    "``abstract``" => Some("``abstract``: int"),
                    "FunctionValue" => Some("FunctionValue: (int -> int)"),
                    _ => None,
                };
                if let Some(written) = written {
                    let reference = entity.evidence.source.as_ref().unwrap();
                    let start = source.find(written).unwrap();
                    assert_eq!(reference.repository(), receipt.repository);
                    assert_eq!(reference.generation(), receipt.generation);
                    assert_eq!(reference.content_hash(), file.content_hash);
                    assert_eq!(
                        reference.span(),
                        SourceSpan::new(
                            file.id,
                            u64::try_from(start).unwrap(),
                            u64::try_from(start + written.len()).unwrap(),
                        )
                        .unwrap()
                    );
                    exact_source(service, reference, language, written.as_bytes());
                }
            }
            let definitions: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.file == file.id
                        && site.role == OccurrenceRole::Definition
                        && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                })
                .collect();
            assert_eq!(definitions.len(), 1, "{language}: definition {name}");
            exact_source(service, &definitions[0].source, language, name.as_bytes());
            let located = service
                .code_locate(
                    receipt.generation,
                    name.to_owned(),
                    LocateMode::Exact,
                    20,
                    0,
                    &deadline(),
                )
                .unwrap();
            assert!(
                located
                    .data
                    .hits
                    .iter()
                    .any(|hit| hit.symbol == Some(entity.id) && hit.file == file.id)
            );
        }
        let enumeration = document
            .entities
            .iter()
            .find(|entity| entity.language == language && entity.canonical_name == "Sample")
            .unwrap();
        for name in ["First", "Next"] {
            let case = document
                .entities
                .iter()
                .find(|entity| entity.language == language && entity.canonical_name == name)
                .unwrap();
            assert_eq!(case.container, Some(ContainerRef::Entity(enumeration.id)));
            assert!(document.relations.iter().any(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(enumeration.id)
                    && relation.object == RelationEndpoint::Entity(case.id)
            }));
        }
        let mut formal_reads = 0;
        let mut earlier_value_reads = 0;
        for site in document
            .occurrences
            .iter()
            .filter(|site| site.file == file.id)
        {
            assert_eq!(site.source.generation(), receipt.generation);
            assert_eq!(site.source.content_hash(), full.content_hash());
            let span = site.source.span();
            let written = source
                .as_bytes()
                .get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap(),
                )
                .unwrap();
            exact_source(service, &site.source, language, written);
            if !matches!(
                site.role,
                OccurrenceRole::Definition | OccurrenceRole::Declaration
            ) {
                if site.role == OccurrenceRole::Reference && language == "fsharp" {
                    let OccurrenceTarget::Resolved { symbol } = site.target else {
                        panic!("The admitted fixture lexical read must resolve")
                    };
                    let target = document
                        .entities
                        .iter()
                        .find(|entity| entity.id == symbol)
                        .unwrap();
                    if target.kind == EntityKind::Parameter {
                        assert_eq!(target.container, site.enclosing.map(ContainerRef::Entity));
                        formal_reads += 1;
                    } else {
                        assert_eq!(target.kind, EntityKind::Variable);
                        assert_eq!(target.canonical_name, "value");
                        let consumer = document
                            .entities
                            .iter()
                            .find(|entity| {
                                entity.language == "fsharp"
                                    && entity.canonical_name == "``value with spaces``"
                            })
                            .unwrap();
                        assert_eq!(site.enclosing, Some(consumer.id));
                        assert_eq!(target.container, consumer.container);
                        let start = source.find("``value with spaces`` = value").unwrap()
                            + "``value with spaces`` = ".len();
                        assert_eq!(
                            site.source.span(),
                            SourceSpan::new(
                                file.id,
                                u64::try_from(start).unwrap(),
                                u64::try_from(start + "value".len()).unwrap(),
                            )
                            .unwrap()
                        );
                        earlier_value_reads += 1;
                    }
                    assert_eq!(target.canonical_name.as_bytes(), written);
                    symbols.insert(target.id);
                    let references = document
                        .relations
                        .iter()
                        .filter(|relation| {
                            relation.subject == RelationEndpoint::Occurrence(site.id)
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(references.len(), 1);
                    assert_eq!(references[0].predicate, RelationPredicate::RefersTo);
                    assert_eq!(references[0].object, RelationEndpoint::Entity(symbol));
                    assert_eq!(references[0].evidence.source.as_ref(), Some(&site.source));
                } else {
                    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
                    assert!(document.relations.iter().all(|relation| relation.subject
                        != rootlight_ir::RelationEndpoint::Occurrence(site.id)));
                }
            }
        }
        assert_eq!(formal_reads, if language == "fsharp" { 4 } else { 0 });
        assert_eq!(earlier_value_reads, usize::from(language == "fsharp"));
    }
    assert!(document.relations.iter().all(|relation| matches!(
        relation.predicate,
        RelationPredicate::Contains
            | RelationPredicate::Declares
            | RelationPredicate::DefinesAt
            | RelationPredicate::RefersTo
    )));
    (symbols, implementation_reference.unwrap())
}

#[test]
fn fsharp_embedded_examples_preserve_host_sources_across_recovery() {
    let fixture = concat!(
        "# Examples\n\n",
        "~~~fsharp\nmodule Shared\nlet pick value = value + 1\n~~~\n\n",
        "~~~fsharp-signature\nmodule Shared\nval pick : int -> int\n~~~\n\n",
        "~~~fsharp\nmodule Shared\nlet pick value = value + 2\n~~~\n",
    );
    for newline in ["\n", "\r\n"] {
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let source = fixture.replace('\n', newline);
        fs::write(repository.path().join("examples.md"), &source).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let state = paths.state_dir();
        let mut service =
            FirstSliceService::new_durable_with_policy(policy(), state, &deadline()).unwrap();
        let commit = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        let receipt = commit.receipt().clone();
        assert_eq!(receipt.discovered_inputs, 1);
        assert_eq!(receipt.indexed_files, 1);
        let check = |service: &FirstSliceService| {
            let snapshot = service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap();
            let document = snapshot.document();
            let file = document
                .files
                .iter()
                .find(|file| file.path == "examples.md")
                .unwrap();
            assert_eq!(file.language, "markdown");
            exact_source(
                service,
                file.evidence.source.as_ref().unwrap(),
                "markdown",
                source.as_bytes(),
            );
            let symbols = document
                .entities
                .iter()
                .filter(|entity| entity.canonical_name == "pick")
                .collect::<Vec<_>>();
            assert_eq!(symbols.len(), 3);
            assert_eq!(
                symbols
                    .iter()
                    .map(|entity| entity.id)
                    .collect::<BTreeSet<_>>()
                    .len(),
                3
            );
            for entity in symbols {
                let definition = document
                    .occurrences
                    .iter()
                    .find(|occurrence| {
                        occurrence.role == OccurrenceRole::Definition
                            && occurrence.target
                                == (OccurrenceTarget::Resolved { symbol: entity.id })
                    })
                    .unwrap();
                exact_source(service, &definition.source, "markdown", b"pick");
            }
            assert!(
                !document
                    .extensions
                    .iter()
                    .filter(|envelope| envelope.namespace == rootlight_ir::FSHARP_BINDING_NAMESPACE)
                    .any(|envelope| {
                        let claim = rootlight_ir::decode_fsharp_binding_envelope(envelope).unwrap();
                        claim.scope().file() == file.id
                            && matches!(claim.binding(), rootlight_ir::FSharpBinding::Context)
                    })
            );
            assert_eq!(
                document
                    .skipped_regions
                    .iter()
                    .filter(|gap| {
                        gap.domain == rootlight_ir::FactDomain::Relations
                            && gap.detail == "markdown-code-semantics-unavailable"
                    })
                    .count(),
                3
            );
        };
        check(&service);
        drop(service);
        let mut reopened =
            FirstSliceService::new_durable_with_policy(policy(), state, &deadline()).unwrap();
        check(&reopened);
        let noop = index(
            &mut reopened,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        assert_eq!(noop.receipt(), &receipt);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
    }
}

#[test]
fn fsharp_profiles_preserve_source_and_identity_across_recovery() {
    for newline in ["\n", "\r\n"] {
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let implementation = IMPLEMENTATION.replace('\n', newline);
        let signature = SIGNATURE.replace('\n', newline);
        fs::write(repository.path().join("Library.fs"), &implementation).unwrap();
        fs::write(repository.path().join("Library.fsi"), &signature).unwrap();
        let mut service =
            FirstSliceService::new_durable_with_policy(policy(), paths.state_dir(), &deadline())
                .unwrap();
        let first = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        let initial = first.receipt().clone();
        assert_eq!(initial.discovered_inputs, 2);
        assert_eq!(initial.indexed_files, 2);
        assert!(initial.discovery_complete);
        assert_eq!(first.evidence().rebuilt_files, 2);
        let (symbols, original_source) =
            assert_generation(&service, &initial, &implementation, &signature);
        let noop = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        assert_eq!(noop.receipt(), &initial);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);

        assert_eq!(implementation.matches("n + 1").count(), 1);
        let changed = implementation.replace("n + 1", "n + 4");
        fs::write(repository.path().join("Library.fs"), &changed).unwrap();
        let mutation = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        let updated = mutation.receipt().clone();
        assert_ne!(updated.generation, initial.generation);
        assert_eq!(updated.parent, Some(initial.generation));
        assert_eq!(mutation.evidence().changed_files, 1);
        assert_eq!(mutation.evidence().rebuilt_files, 1);
        let (updated_symbols, updated_source) =
            assert_generation(&service, &updated, &changed, &signature);
        assert_eq!(updated_symbols, symbols);
        assert_ne!(
            updated_source.content_hash(),
            original_source.content_hash()
        );
        exact_source(
            &service,
            &original_source,
            "fsharp",
            implementation.as_bytes(),
        );
        assert!(
            service
                .source_read_with_options_and_budget(
                    updated.generation,
                    vec![original_source.clone()],
                    SourceReadOptions::new(),
                    FirstSliceBudget::default(),
                    &deadline()
                )
                .is_err()
        );
        drop(service);

        let mut reopened =
            FirstSliceService::new_durable_with_policy(policy(), paths.state_dir(), &deadline())
                .unwrap();
        assert_eq!(
            assert_generation(&reopened, &initial, &implementation, &signature).0,
            symbols
        );
        assert_eq!(
            assert_generation(&reopened, &updated, &changed, &signature).0,
            symbols
        );
        let noop = index(
            &mut reopened,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        assert_eq!(noop.receipt(), &updated);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
        let clean = index(
            &mut reopened,
            repository.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
        );
        assert!(updated.logical_snapshot.is_some());
        assert_eq!(clean.receipt().logical_snapshot, updated.logical_snapshot);
        assert_eq!(
            assert_generation(&reopened, clean.receipt(), &changed, &signature).0,
            symbols
        );
    }
}

#[test]
fn fsharp_parenthesized_enum_sources_survive_durable_reopen() {
    for newline in ["\n", "\r\n"] {
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let sources = [
            ("Values.fs", "fsharp", "let following = 30"),
            ("Values.fsi", "fsharp-signature", "val following: int"),
        ]
        .map(|(path, language, following)| {
            let source = format!(
                "module EnumValues\ntype Flags =\n    | First = (1u)\n    | Next = (1u <<< 0)\n{following}\n"
            )
            .replace('\n', newline);
            fs::write(repository.path().join(path), &source).unwrap();
            (path, language, source)
        });
        let mut original = None;
        for lifetime in 0..2 {
            let mut service = FirstSliceService::new_durable_with_policy(
                policy(),
                paths.state_dir(),
                &deadline(),
            )
            .unwrap();
            let commit = index(
                &mut service,
                repository.path(),
                FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
            );
            let receipt = commit.receipt();
            assert_eq!(receipt.discovered_inputs, 2);
            assert_eq!(receipt.indexed_files, 2);
            assert!(receipt.discovery_complete);
            if lifetime == 0 {
                assert_eq!(commit.evidence().rebuilt_files, 2);
                original = Some(receipt.clone());
            } else {
                assert_eq!(Some(receipt), original.as_ref());
                assert_eq!(commit.evidence().rebuilt_files, 0);
                assert_eq!(commit.evidence().rebuilt_facts, 0);
            }
            let snapshot = service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap();
            let document = snapshot.document();
            assert_eq!(document.files.len(), 2);
            assert!(!document.skipped_regions.iter().any(|gap| {
                matches!(
                    gap.reason,
                    SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
                )
            }));
            for (path, language, source) in &sources {
                let file = document
                    .files
                    .iter()
                    .find(|file| file.path == *path)
                    .unwrap();
                assert_eq!(file.language, *language);
                let full = file.evidence.source.as_ref().unwrap();
                assert_eq!(full.repository(), receipt.repository);
                assert_eq!(full.generation(), receipt.generation);
                assert_eq!(full.content_hash(), content_hash(source.as_bytes()));
                exact_source(&service, full, language, source.as_bytes());
                let coverage = service
                    .source_file_coverage_until(receipt.generation, file.id, &deadline())
                    .unwrap();
                assert_eq!(coverage.language, *language);
                // Complete written declarations remain structural tier D;
                // this fixture does not prove enum evaluation or name resolution.
                assert_eq!(coverage.tier, AnalysisTier::TierD);
                if *language == "fsharp-signature" {
                    assert_eq!(coverage.status, CoverageStatus::Bounded);
                    assert_signature_mask_gap_preserves_structure(document, file);
                } else {
                    assert_eq!(coverage.status, CoverageStatus::Complete);
                }
                assert_eq!(coverage.reason, None);
                let unique = |name: &str, kind| {
                    let entities: Vec<_> = document
                        .entities
                        .iter()
                        .filter(|entity| {
                            entity.language == *language
                                && entity.canonical_name == name
                                && entity.kind == kind
                                && entity.evidence.source.as_ref().unwrap().span().file() == file.id
                        })
                        .collect();
                    assert_eq!(entities.len(), 1, "{language}: {name}");
                    entities[0]
                };
                let enumeration = unique("Flags", EntityKind::Enum);
                for (name, written) in [("First", "First = (1u)"), ("Next", "Next = (1u <<< 0)")] {
                    let case = unique(name, EntityKind::Constant);
                    assert_eq!(case.container, Some(ContainerRef::Entity(enumeration.id)));
                    assert_eq!(
                        case.evidence.source.as_ref().unwrap().generation(),
                        receipt.generation
                    );
                    exact_source(
                        &service,
                        case.evidence.source.as_ref().unwrap(),
                        language,
                        written.as_bytes(),
                    );
                    let definitions: Vec<_> = document
                        .occurrences
                        .iter()
                        .filter(|site| {
                            site.file == file.id
                                && site.role == OccurrenceRole::Definition
                                && site.target == (OccurrenceTarget::Resolved { symbol: case.id })
                        })
                        .collect();
                    assert_eq!(definitions.len(), 1);
                    assert_eq!(definitions[0].enclosing, Some(enumeration.id));
                    exact_source(&service, &definitions[0].source, language, name.as_bytes());
                    assert!(document.relations.iter().any(|relation| {
                        relation.predicate == RelationPredicate::Contains
                            && relation.subject == RelationEndpoint::Entity(enumeration.id)
                            && relation.object == RelationEndpoint::Entity(case.id)
                    }));
                    let located = service
                        .code_locate(
                            receipt.generation,
                            name.to_owned(),
                            LocateMode::Exact,
                            20,
                            0,
                            &deadline(),
                        )
                        .unwrap();
                    assert!(
                        located
                            .data
                            .hits
                            .iter()
                            .any(|hit| hit.symbol == Some(case.id) && hit.file == file.id)
                    );
                }
                unique("following", EntityKind::Variable);
            }
            assert!(document.relations.iter().all(|relation| matches!(
                relation.predicate,
                RelationPredicate::Contains
                    | RelationPredicate::Declares
                    | RelationPredicate::DefinesAt
            )));
        }
    }
}

#[test]
fn fsharp_script_suffix_preserves_queries_and_recovery() {
    let source = "// ☃ 🧭\r\nlet seed = 7\r\nlet add (value: int) = value + seed\r\nlet run (input: int) = add input\r\n";
    let changed_source = source.replace("= add input", "= missing input");
    assert_eq!(source.matches("= add input").count(), 1);
    let peer = b": twice dup + ;\n";
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let encode = |text: &str| {
            let mut bytes = if encoding == "utf-8" {
                Vec::new()
            } else {
                typed_formal_leaf_bytes("\u{feff}", encoding)
            };
            bytes.extend(typed_formal_leaf_bytes(text, encoding));
            bytes
        };
        let bytes = encode(source);
        let changed_bytes = encode(&changed_source);
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let script = repository.path().join("Routine.fsscript");
        let unrecognized = repository.path().join("Routine.data");
        fs::write(&script, &bytes).unwrap();
        fs::write(repository.path().join("Peer.fs"), peer).unwrap();
        fs::write(repository.path().join("Binary.fsscript"), b"\0binary").unwrap();
        let policy = FirstSliceStoragePolicy::resolve_user_config(
            Some("version='1.3'\n[storage]\nretained_generations=4\n"),
            4,
        )
        .unwrap();
        let mut service = FirstSliceService::new_durable_with_policy(
            policy.clone(),
            paths.state_dir(),
            &deadline(),
        )
        .unwrap();
        let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
        let cold = index(&mut service, repository.path(), options);
        assert_eq!(cold.evidence().rebuilt_files, 1);
        let initial = cold.receipt().clone();
        assert_eq!(initial.indexed_files, 2);
        assert_eq!(initial.binary_inputs, 1);
        assert!(initial.discovery_complete);
        let (symbols, original) =
            assert_script_suffix_generation(&service, &initial, source, &bytes, encoding, true);
        assert_project_member(&service, &initial, "Peer.fs", peer, "unknown", None);
        let noop = index(&mut service, repository.path(), options);
        assert_eq!(noop.receipt(), &initial);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);

        fs::write(&script, &changed_bytes).unwrap();
        let mutation = index(&mut service, repository.path(), options);
        assert_eq!(mutation.evidence().changed_files, 1);
        assert_eq!(mutation.evidence().rebuilt_files, 1);
        let changed = mutation.receipt().clone();
        assert_eq!(changed.parent, Some(initial.generation));
        let (changed_symbols, updated) = assert_script_suffix_generation(
            &service,
            &changed,
            &changed_source,
            &changed_bytes,
            encoding,
            false,
        );
        assert_eq!(changed_symbols, symbols);
        assert_eq!(updated.span().file(), original.span().file());
        assert_ne!(updated.content_hash(), original.content_hash());
        exact_source(&service, &original, "fsharp", &bytes);
        assert!(
            service
                .source_read_with_options_and_budget(
                    changed.generation,
                    vec![original.clone()],
                    SourceReadOptions::new(),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .is_err()
        );

        fs::rename(&script, &unrecognized).unwrap();
        let rerouted = index(&mut service, repository.path(), options);
        assert_eq!(rerouted.evidence().rebuilt_files, 0);
        let unknown = rerouted.receipt().clone();
        assert_eq!(unknown.parent, Some(changed.generation));
        let unknown_file = assert_project_member(
            &service,
            &unknown,
            "Routine.data",
            &changed_bytes,
            "unknown",
            None,
        );
        assert_ne!(unknown_file, original.span().file());
        // Unambiguous suffixes use path and provider-set invalidation; per-file
        // route components are reserved for ambiguous routing evidence.
        for (generation, file) in [
            (unknown.generation, unknown_file),
            (changed.generation, original.span().file()),
        ] {
            assert_eq!(
                service
                    .incremental_inputs
                    .get(&generation)
                    .unwrap()
                    .value(InputKey::FileParserRoute(file)),
                None
            );
        }
        let absent = service
            .code_locate_with_filters_and_budget(
                unknown.generation,
                "add".to_owned(),
                LocateMode::Exact,
                vec!["fsharp".to_owned()],
                vec!["Routine.fsscript".to_owned()],
                8,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert!(absent.data.hits.is_empty());

        fs::rename(&unrecognized, &script).unwrap();
        fs::write(&script, &bytes).unwrap();
        let restored = index(&mut service, repository.path(), options);
        assert_eq!(restored.evidence().rebuilt_files, 1);
        let last = restored.receipt().clone();
        assert_eq!(last.parent, Some(unknown.generation));
        assert_eq!(
            assert_script_suffix_generation(&service, &last, source, &bytes, encoding, true,).0,
            symbols
        );
        assert_eq!(last.logical_snapshot, initial.logical_snapshot);
        drop(service);
        let mut reopened =
            FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline())
                .unwrap();
        assert_eq!(
            assert_script_suffix_generation(&reopened, &initial, source, &bytes, encoding, true,).0,
            symbols
        );
        assert_eq!(
            assert_script_suffix_generation(
                &reopened,
                &changed,
                &changed_source,
                &changed_bytes,
                encoding,
                false,
            )
            .0,
            symbols
        );
        assert_eq!(
            assert_project_member(
                &reopened,
                &unknown,
                "Routine.data",
                &changed_bytes,
                "unknown",
                None,
            ),
            unknown_file
        );
        assert_eq!(
            assert_script_suffix_generation(&reopened, &last, source, &bytes, encoding, true,).0,
            symbols
        );
        exact_source(&reopened, &original, "fsharp", &bytes);
        assert_project_member(&reopened, &last, "Peer.fs", peer, "unknown", None);
        let noop = index(&mut reopened, repository.path(), options);
        assert_eq!(noop.receipt(), &last);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
    }
}

fn assert_script_suffix_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    bytes: &[u8],
    encoding: &str,
    resolved_call: bool,
) -> (BTreeMap<String, SymbolId>, SourceRef) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == "Routine.fsscript")
        .unwrap();
    assert_eq!(
        (file.language.as_str(), file.encoding.as_str()),
        ("fsharp", encoding)
    );
    assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(bytes));
    assert!(
        !document
            .files
            .iter()
            .any(|file| file.path == "Binary.fsscript")
    );
    let reference = |start: usize, written: &str| {
        assert_eq!(source.get(start..start + written.len()), Some(written));
        let start = typed_formal_leaf_bytes(source.get(..start).unwrap(), encoding).len()
            + usize::from(encoding != "utf-8") * 2;
        SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                file.id,
                u64::try_from(start).unwrap(),
                u64::try_from(start + typed_formal_leaf_bytes(written, encoding).len()).unwrap(),
            )
            .unwrap(),
            file.content_hash,
            None,
        )
    };
    let check_source = |reference: &SourceRef| {
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), file.content_hash);
        exact_source(
            service,
            reference,
            "fsharp",
            bytes
                .get(
                    usize::try_from(reference.span().start_byte()).unwrap()
                        ..usize::try_from(reference.span().end_byte()).unwrap(),
                )
                .unwrap(),
        );
    };
    let full = file.evidence.source.as_ref().unwrap();
    assert_eq!(
        full.span(),
        SourceSpan::new(file.id, 0, file.byte_length).unwrap()
    );
    check_source(full);
    let mut symbols = BTreeMap::new();
    for (name, kind, owner) in [
        ("seed", EntityKind::Variable, None),
        ("add", EntityKind::Function, None),
        ("run", EntityKind::Function, None),
        ("value", EntityKind::Parameter, Some("add")),
        ("input", EntityKind::Parameter, Some("run")),
    ] {
        let entities = document
            .entities
            .iter()
            .filter(|entity| {
                entity.canonical_name == name
                    && entity.kind == kind
                    && entity.language == "fsharp"
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|reference| reference.span().file() == file.id)
                    && owner.is_none_or(|owner| {
                        entity.container == Some(ContainerRef::Entity(symbols[owner]))
                    })
            })
            .collect::<Vec<_>>();
        assert_eq!(entities.len(), 1, "{name}");
        let entity = entities[0];
        symbols.insert(name.to_owned(), entity.id);
        let explained = service
            .symbol_explain(receipt.generation, entity.id, &deadline())
            .unwrap();
        assert_eq!(explained.data.generation, receipt.generation);
        assert_eq!(explained.data.entity.id, entity.id);
        check_source(explained.data.entity.evidence.source.as_ref().unwrap());
        let definitions = explained
            .data
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        let start = source
            .find(&if let Some(owner) = owner {
                format!("let {owner} ({name}: int)")
            } else {
                format!("let {name}")
            })
            .unwrap()
            + if let Some(owner) = owner {
                format!("let {owner} (").len()
            } else {
                "let ".len()
            };
        assert_eq!(definitions[0].source, reference(start, name));
        check_source(&definitions[0].source);
        let located = service
            .code_locate_with_filters_and_budget(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                vec!["fsharp".to_owned()],
                vec!["Routine.fsscript".to_owned()],
                8,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(located.data.generation, receipt.generation);
        let hit = located
            .data
            .hits
            .iter()
            .find(|hit| hit.symbol == Some(entity.id))
            .unwrap();
        assert_eq!(hit.file, file.id);
        assert_eq!(hit.source, explained.data.entity.evidence.source);
        check_source(hit.source.as_ref().unwrap());
        assert_eq!(hit.signature, explained.data.signature);
        assert!(explained.data.signature.is_some());
        for coverage in [&located.data.coverage, &explained.data.coverage] {
            assert!(coverage.iter().all(|record| {
                record.repository == receipt.repository && record.generation == receipt.generation
                    && !matches!(record.scope, rootlight_ir::CoverageScope::File(other) if other != file.id)
            }));
            assert!(coverage.iter().any(|record| {
                record.scope == rootlight_ir::CoverageScope::File(file.id)
                    && record.domain == IrFactDomain::Relations
                    && record.tier == AnalysisTier::TierD
                    && record.status != CoverageStatus::Complete
            }));
        }
    }
    let body_start = |owner: &str| {
        let start = source.find(&format!("let {owner} ")).unwrap();
        start + source.get(start..).unwrap().find(" = ").unwrap() + " = ".len()
    };
    let callee = if resolved_call { "add" } else { "missing" };
    for (owner, offset, name, target) in [
        ("add", 0, "value", Some("value")),
        ("add", "value + ".len(), "seed", Some("seed")),
        ("run", 0, callee, resolved_call.then_some("add")),
        ("run", callee.len() + 1, "input", Some("input")),
    ] {
        let expected = reference(body_start(owner) + offset, name);
        let reads = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference && site.source == expected)
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 1, "{owner}: {name}");
        assert_eq!(reads[0].enclosing, Some(symbols[owner]));
        check_source(&expected);
        let edges = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(reads[0].id)
            })
            .collect::<Vec<_>>();
        if let Some(target) = target {
            assert_eq!(
                reads[0].target,
                OccurrenceTarget::Resolved {
                    symbol: symbols[target]
                }
            );
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].object, RelationEndpoint::Entity(symbols[target]));
            assert_eq!(edges[0].evidence.source.as_ref(), Some(&expected));
        } else {
            assert!(matches!(
                reads[0].target,
                OccurrenceTarget::Unresolved { .. }
            ));
            assert!(edges.is_empty());
            assert!(document.skipped_regions.iter().any(|gap| {
                gap.source == expected
                    && gap.domain == IrFactDomain::Relations
                    && gap.reason == SkippedRegionReason::UnsupportedConstruct
            }));
        }
    }
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| { site.file == file.id && site.role == OccurrenceRole::Reference })
            .count(),
        4
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo)
            .count(),
        if resolved_call { 4 } else { 3 }
    );
    let application = format!("{callee} input");
    let call_source = reference(body_start("run"), &application);
    check_source(&call_source);
    let call_sites = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::CallSite)
        .collect::<Vec<_>>();
    assert_eq!(call_sites.len(), 1);
    assert_eq!(call_sites[0].source, call_source);
    assert_eq!(call_sites[0].enclosing, Some(symbols["run"]));
    let calls = document
        .relations
        .iter()
        .filter(|edge| edge.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), usize::from(resolved_call));
    if resolved_call {
        assert_eq!(calls[0].subject, RelationEndpoint::Entity(symbols["run"]));
        assert_eq!(calls[0].object, RelationEndpoint::Entity(symbols["add"]));
        assert_eq!(calls[0].evidence.source.as_ref(), Some(&call_source));
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::DispatchCandidate)
    );
    for (seed, family, direction, target) in [
        (
            "run",
            RelationFamily::Calls,
            RelationDirection::Outbound,
            "add",
        ),
        (
            "add",
            RelationFamily::CalledBy,
            RelationDirection::Inbound,
            "run",
        ),
    ] {
        let relationships = service
            .symbol_relationships(
                receipt.generation,
                BTreeSet::from([symbols[seed]]),
                vec![family],
                Some(direction),
                0,
                8,
                0,
                &deadline(),
            )
            .unwrap();
        assert_eq!(relationships.data.generation, receipt.generation);
        assert!(!relationships.data.truncated);
        assert!(!relationships.data.exact);
        assert_eq!(relationships.data.returned_edges, u32::from(resolved_call));
        assert_eq!(relationships.data.total_edges, u32::from(resolved_call));
        let items = relationships
            .data
            .groups
            .iter()
            .flat_map(|group| {
                assert_eq!(
                    (group.seed, group.family, group.direction),
                    (symbols[seed], family, direction)
                );
                &group.items
            })
            .collect::<Vec<_>>();
        assert_eq!(items.len(), usize::from(resolved_call));
        if resolved_call {
            assert_eq!(items[0].symbol, symbols[target]);
            assert_eq!(items[0].source_refs, vec![call_source.clone()]);
        }
    }
    let references = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([symbols["run"]]),
            vec![RelationFamily::References],
            Some(RelationDirection::Outbound),
            0,
            8,
            0,
            &deadline(),
        )
        .unwrap();
    assert_eq!(references.data.generation, receipt.generation);
    assert!(!references.data.truncated);
    assert!(!references.data.exact);
    let mut expected = vec![(
        symbols["input"],
        reference(body_start("run") + callee.len() + 1, "input"),
    )];
    if resolved_call {
        expected.push((symbols["add"], reference(body_start("run"), callee)));
    }
    assert_eq!(
        references.data.returned_edges,
        u32::try_from(expected.len()).unwrap()
    );
    assert_eq!(
        references.data.total_edges,
        u32::try_from(expected.len()).unwrap()
    );
    let mut actual = Vec::new();
    for group in &references.data.groups {
        assert_eq!(
            (group.seed, group.family, group.direction),
            (
                symbols["run"],
                RelationFamily::References,
                RelationDirection::Outbound
            )
        );
        for item in &group.items {
            assert_eq!(item.source_refs.len(), 1);
            check_source(&item.source_refs[0]);
            actual.push((item.symbol, item.source_refs[0].clone()));
        }
    }
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected);
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(
        (coverage.language.as_str(), coverage.tier),
        ("fsharp", AnalysisTier::TierD)
    );
    assert_eq!(coverage.reason, None);
    assert_ne!(coverage.status, CoverageStatus::Complete);
    (symbols, full.clone())
}

const QUALIFIED_LIBRARY: &str =
    "module Library\nlet offset = 7\nlet carry value = value + offset\n";
const QUALIFIED_CONSUMER: &str =
    "module Consumer\nlet read () = Library.offset\nlet invoke input = Library.carry input\n";
const QUALIFIED_APPLICATION: &str = "Library.carry input";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QualifiedLink {
    Closed,
    ReferenceOnly,
    Call,
}

fn qualified_project(paths: &[&str]) -> String {
    let items = paths
        .iter()
        .map(|path| format!("<Compile Include=\"{path}\"/>"))
        .collect::<String>();
    format!(
        "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup><ItemGroup>{items}</ItemGroup></Project>"
    )
}

fn write_qualified_modules(repository: &Path, library: &str, consumer: &str) {
    fs::write(repository.join("Library.fs"), library).unwrap();
    fs::write(repository.join("Consumer.fs"), consumer).unwrap();
    fs::write(
        repository.join("Sample.fsproj"),
        qualified_project(&["Library.fs", "Consumer.fs"]),
    )
    .unwrap();
}

#[test]
fn fsharp_open_import_values_resolve_exact_authored_sources() {
    let library = "module Library\nlet offset = 7\n";
    let consumer = "module Consumer\nopen Library\nlet choose () = offset\n";
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    write_qualified_modules(repository.path(), library, consumer);
    let mut service = FirstSliceService::new(2).unwrap();
    let indexed = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
    );
    let receipt = indexed.receipt();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let library_file = document
        .files
        .iter()
        .find(|file| file.path == "Library.fs")
        .unwrap();
    let consumer_file = document
        .files
        .iter()
        .find(|file| file.path == "Consumer.fs")
        .unwrap();
    assert_eq!(library_file.content_hash, content_hash(library.as_bytes()));
    assert_eq!(
        consumer_file.content_hash,
        content_hash(consumer.as_bytes())
    );
    let reference = |file: &rootlight_ir::FileRecord, source: &str, name: &str| {
        let start = source.find(name).unwrap();
        SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                file.id,
                u64::try_from(start).unwrap(),
                u64::try_from(start + name.len()).unwrap(),
            )
            .unwrap(),
            file.content_hash,
            None,
        )
    };
    let definition = reference(library_file, library, "offset");
    let definitions = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Definition && site.source == definition)
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1);
    let OccurrenceTarget::Resolved { symbol: target } = definitions[0].target else {
        panic!("authored library definition must be resolved");
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == target)
        .unwrap();
    assert_eq!(
        (entity.kind, entity.canonical_name.as_str()),
        (EntityKind::Variable, "offset")
    );
    exact_source(&service, &definition, "fsharp", b"offset");
    let owner_definition = reference(consumer_file, consumer, "choose");
    let owner = document
        .occurrences
        .iter()
        .find_map(|site| {
            if site.role == OccurrenceRole::Definition && site.source == owner_definition {
                match site.target {
                    OccurrenceTarget::Resolved { symbol } => Some(symbol),
                    _ => None,
                }
            } else {
                None
            }
        })
        .unwrap();
    let usage = reference(consumer_file, consumer, "offset");
    exact_source(&service, &usage, "fsharp", b"offset");
    let reads = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Reference && site.source == usage)
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 1, "ordinary open-import read must be retained");
    assert_eq!(reads[0].enclosing, Some(owner));
    assert_eq!(
        reads[0].target,
        OccurrenceTarget::Resolved { symbol: target },
        "ordinary open-import reference must resolve to authored library definition"
    );
    let edges = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(reads[0].id)
        })
        .collect::<Vec<_>>();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].object, RelationEndpoint::Entity(target));
    assert_eq!(edges[0].evidence.source.as_ref(), Some(&usage));
    let public = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([owner]),
            vec![RelationFamily::References],
            Some(RelationDirection::Outbound),
            0,
            16,
            0,
            &deadline(),
        )
        .unwrap();
    assert_eq!(public.data.returned_edges, 1);
    assert!(!public.data.truncated);
    assert_eq!(public.data.groups.len(), 1);
    assert_eq!(public.data.groups[0].items.len(), 1);
    assert_eq!(public.data.groups[0].items[0].symbol, target);
    assert_eq!(public.data.groups[0].items[0].source_refs, vec![usage]);
}

const OPEN_CONSUMER: &str =
    "module Consumer\nopen Library\nlet read () = offset\nlet invoke input = carry input\n";
const OPEN_APPLICATION: &str = "carry input";

#[test]
fn fsharp_open_import_written_names_preserve_encoded_exact_sources() {
    for written in ["δ", "``value with spaces``"] {
        let library = format!("module Library\r\nlet {written} = 7\r\n");
        let consumer = format!("module Consumer\r\nopen Library\r\nlet choose () = {written}\r\n");
        for encoding in ["utf-8", "utf-16le", "utf-16be"] {
            let encode = |text: &str| {
                let mut bytes = if encoding == "utf-8" {
                    Vec::new()
                } else {
                    typed_formal_leaf_bytes("\u{feff}", encoding)
                };
                bytes.extend(typed_formal_leaf_bytes(text, encoding));
                bytes
            };
            let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
            write_qualified_modules(repository.path(), &library, &consumer);
            let library_bytes = encode(&library);
            let consumer_bytes = encode(&consumer);
            fs::write(repository.path().join("Library.fs"), &library_bytes).unwrap();
            fs::write(repository.path().join("Consumer.fs"), &consumer_bytes).unwrap();
            let mut service = FirstSliceService::new(2).unwrap();
            let indexed = index(
                &mut service,
                repository.path(),
                FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
            );
            let receipt = indexed.receipt();
            let snapshot = service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap();
            let document = snapshot.document();
            let mut references = BTreeMap::new();
            for (path, source, bytes) in [
                ("Library.fs", &library, &library_bytes),
                ("Consumer.fs", &consumer, &consumer_bytes),
            ] {
                let file = document
                    .files
                    .iter()
                    .find(|file| file.path == path)
                    .unwrap();
                assert_eq!(file.encoding, encoding);
                assert_eq!(file.content_hash, content_hash(bytes));
                let full = file.evidence.source.as_ref().unwrap();
                assert_eq!(
                    full.span(),
                    SourceSpan::new(file.id, 0, u64::try_from(bytes.len()).unwrap()).unwrap()
                );
                exact_source(&service, full, "fsharp", bytes);
                let start = source.find(written).unwrap();
                let start = typed_formal_leaf_bytes(source.get(..start).unwrap(), encoding).len()
                    + usize::from(encoding != "utf-8") * 2;
                let reference = SourceRef::new(
                    receipt.repository,
                    receipt.generation,
                    SourceSpan::new(
                        file.id,
                        u64::try_from(start).unwrap(),
                        u64::try_from(start + typed_formal_leaf_bytes(written, encoding).len())
                            .unwrap(),
                    )
                    .unwrap(),
                    file.content_hash,
                    None,
                );
                exact_source(
                    &service,
                    &reference,
                    "fsharp",
                    &typed_formal_leaf_bytes(written, encoding),
                );
                references.insert(path, reference);
            }
            let definitions = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.source == references["Library.fs"]
                })
                .collect::<Vec<_>>();
            assert_eq!(definitions.len(), 1);
            let OccurrenceTarget::Resolved { symbol: target } = definitions[0].target else {
                panic!("authored opened definition")
            };
            let entity = document
                .entities
                .iter()
                .find(|entity| entity.id == target)
                .unwrap();
            assert_eq!(
                (entity.kind, entity.canonical_name.as_str()),
                (EntityKind::Variable, written)
            );
            let reads = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Reference
                        && site.source == references["Consumer.fs"]
                })
                .collect::<Vec<_>>();
            assert_eq!(reads.len(), 1, "{written}: {encoding}");
            assert_eq!(
                reads[0].target,
                OccurrenceTarget::Resolved { symbol: target },
                "{written}: {encoding}"
            );
            let owner_start = consumer.find("choose").unwrap();
            let owner_start =
                typed_formal_leaf_bytes(consumer.get(..owner_start).unwrap(), encoding).len()
                    + usize::from(encoding != "utf-8") * 2;
            let owner_source = SourceRef::new(
                receipt.repository,
                receipt.generation,
                SourceSpan::new(
                    references["Consumer.fs"].span().file(),
                    u64::try_from(owner_start).unwrap(),
                    u64::try_from(owner_start + typed_formal_leaf_bytes("choose", encoding).len())
                        .unwrap(),
                )
                .unwrap(),
                content_hash(&consumer_bytes),
                None,
            );
            let owner = document
                .occurrences
                .iter()
                .find_map(|site| {
                    if site.role == OccurrenceRole::Definition && site.source == owner_source {
                        match site.target {
                            OccurrenceTarget::Resolved { symbol } => Some(symbol),
                            _ => None,
                        }
                    } else {
                        None
                    }
                })
                .unwrap();
            assert_eq!(reads[0].enclosing, Some(owner));
            let public = service
                .symbol_relationships(
                    receipt.generation,
                    BTreeSet::from([target]),
                    vec![RelationFamily::References],
                    Some(RelationDirection::Inbound),
                    0,
                    16,
                    0,
                    &deadline(),
                )
                .unwrap();
            assert!(!public.data.truncated);
            assert_eq!(public.data.returned_edges, 1);
            assert_eq!(public.data.groups[0].items[0].symbol, owner);
            assert_eq!(
                public.data.groups[0].items[0].source_refs,
                vec![references["Consumer.fs"].clone()]
            );
        }
    }
}

#[test]
fn fsharp_open_imports_require_owned_prior_visible_contexts() {
    for (case, opens, order, signature, private, proved) in [
        ("ordinary", "open Library\n", 0, None, false, true),
        (
            "repeated",
            "open Library\nopen Library\n",
            0,
            None,
            false,
            true,
        ),
        ("missing-project", "open Library\n", 1, None, false, false),
        ("later-library", "open Library\n", 2, None, false, false),
        ("absent-open", "", 0, None, false, false),
        ("later-open", "", 0, None, false, false),
        (
            "unknown-open",
            "open Library\nopen External\n",
            0,
            None,
            false,
            false,
        ),
        ("private", "open Library\n", 0, None, true, false),
        (
            "signature-exposed",
            "open Library\n",
            0,
            Some("module Library\nval offset: int\nval carry: int -> int\n"),
            false,
            true,
        ),
        (
            "signature-hidden",
            "open Library\n",
            0,
            Some("module Library\nval unrelated: int\n"),
            false,
            false,
        ),
    ] {
        let library = if private {
            QUALIFIED_LIBRARY.replace("let ", "let private ")
        } else {
            QUALIFIED_LIBRARY.to_owned()
        };
        let library = if case == "signature-hidden" {
            format!("{library}let unrelated = 0\n")
        } else {
            library
        };
        let consumer = format!(
            "module Consumer\n{opens}let read () = offset\nlet invoke input = carry input\n{}",
            if case == "later-open" {
                "open Library\n"
            } else {
                ""
            }
        );
        let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        write_qualified_modules(repository.path(), &library, &consumer);
        if let Some(signature) = signature {
            fs::write(repository.path().join("Library.fsi"), signature).unwrap();
            fs::write(
                repository.path().join("Sample.fsproj"),
                qualified_project(&["Library.fsi", "Library.fs", "Consumer.fs"]),
            )
            .unwrap();
        } else if order == 1 {
            fs::remove_file(repository.path().join("Sample.fsproj")).unwrap();
        } else if order == 2 {
            fs::write(
                repository.path().join("Sample.fsproj"),
                qualified_project(&["Consumer.fs", "Library.fs"]),
            )
            .unwrap();
        }
        let mut service = FirstSliceService::new(2).unwrap();
        let indexed = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        assert_qualified_module_generation(
            &service,
            indexed.receipt(),
            &library,
            &consumer,
            OPEN_APPLICATION,
            if proved {
                [
                    Some(QualifiedLink::ReferenceOnly),
                    Some(QualifiedLink::Call),
                ]
            } else {
                [Some(QualifiedLink::Closed), Some(QualifiedLink::Closed)]
            },
        );
    }
}

#[test]
fn fsharp_open_imports_rebind_unchanged_consumers_after_signature_changes() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let storage = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    write_qualified_modules(repository.path(), QUALIFIED_LIBRARY, OPEN_CONSUMER);
    fs::write(
        repository.path().join("Sample.fsproj"),
        qualified_project(&["Library.fsi", "Library.fs", "Consumer.fs"]),
    )
    .unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy(), paths.state_dir(), &deadline())
            .unwrap();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let mut generations = Vec::new();
    for (signature, expected) in [
        (
            "module Library\nval offset: int\nval carry: int -> int\n",
            [
                Some(QualifiedLink::ReferenceOnly),
                Some(QualifiedLink::Call),
            ],
        ),
        (
            "module Library\nval offset: int\n",
            [
                Some(QualifiedLink::ReferenceOnly),
                Some(QualifiedLink::Closed),
            ],
        ),
        (
            "module Library\nval offset: int\nval carry: int -> int\n",
            [
                Some(QualifiedLink::ReferenceOnly),
                Some(QualifiedLink::Call),
            ],
        ),
    ] {
        fs::write(repository.path().join("Library.fsi"), signature).unwrap();
        let indexed = index(&mut service, repository.path(), options);
        if !generations.is_empty() {
            assert_eq!(indexed.evidence().changed_files, 1);
            assert_eq!(indexed.evidence().rebuilt_files, 1);
        }
        let receipt = indexed.receipt().clone();
        let (symbols, _, source) = assert_qualified_module_generation(
            &service,
            &receipt,
            QUALIFIED_LIBRARY,
            OPEN_CONSUMER,
            OPEN_APPLICATION,
            expected,
        );
        if let Some((_, previous, _, _)) = generations.first() {
            assert_eq!(&symbols, previous);
        }
        generations.push((receipt, symbols, expected, source));
    }
    let clean = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
    );
    assert_eq!(
        clean.receipt().logical_snapshot,
        generations[2].0.logical_snapshot
    );
    let last = clean.receipt().clone();
    assert_qualified_module_generation(
        &service,
        &last,
        QUALIFIED_LIBRARY,
        OPEN_CONSUMER,
        OPEN_APPLICATION,
        generations[2].2,
    );
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy(), paths.state_dir(), &deadline())
            .unwrap();
    for (receipt, _, expected, source) in &generations {
        assert_qualified_module_generation(
            &reopened,
            receipt,
            QUALIFIED_LIBRARY,
            OPEN_CONSUMER,
            OPEN_APPLICATION,
            *expected,
        );
        exact_source(&reopened, source, "fsharp", OPEN_CONSUMER.as_bytes());
    }
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt(), &last);
    assert_eq!(
        (noop.evidence().rebuilt_files, noop.evidence().rebuilt_facts),
        (0, 0)
    );
}

#[test]
fn fsharp_open_imports_never_replace_authored_local_bindings() {
    for (body, kind) in [
        ("let choose offset = offset\n", EntityKind::Parameter),
        (
            "let choose () =\n    let offset = 3\n    offset\n",
            EntityKind::Variable,
        ),
        (
            "let choose input =\n    match input with\n    | offset -> offset\n",
            EntityKind::Variable,
        ),
    ] {
        let consumer = format!("module Consumer\nopen Library\n{body}");
        let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        write_qualified_modules(
            repository.path(),
            "module Library\nlet offset = 7\n",
            &consumer,
        );
        let mut service = FirstSliceService::new(2).unwrap();
        let indexed = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        let receipt = indexed.receipt();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let file = document
            .files
            .iter()
            .find(|file| file.path == "Consumer.fs")
            .unwrap();
        let reference = |start: usize, name: &str| {
            assert_eq!(consumer.get(start..start + name.len()), Some(name));
            SourceRef::new(
                receipt.repository,
                receipt.generation,
                SourceSpan::new(
                    file.id,
                    u64::try_from(start).unwrap(),
                    u64::try_from(start + name.len()).unwrap(),
                )
                .unwrap(),
                file.content_hash,
                None,
            )
        };
        let definition = reference(consumer.find("offset").unwrap(), "offset");
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Definition && site.source == definition)
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1, "{body}");
        let OccurrenceTarget::Resolved { symbol: target } = definitions[0].target else {
            panic!("authored local definition")
        };
        let target_entity = document
            .entities
            .iter()
            .find(|entity| entity.id == target)
            .unwrap();
        assert_eq!(target_entity.kind, kind);
        let owner_source = reference(consumer.find("choose").unwrap(), "choose");
        let owner = document
            .occurrences
            .iter()
            .find_map(|site| {
                if site.role == OccurrenceRole::Definition && site.source == owner_source {
                    match site.target {
                        OccurrenceTarget::Resolved { symbol } => Some(symbol),
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .unwrap();
        let usage = reference(consumer.rfind("offset").unwrap(), "offset");
        let reads = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference && site.source == usage)
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 1, "{body}");
        assert_eq!(reads[0].enclosing, Some(owner));
        assert_eq!(
            reads[0].target,
            OccurrenceTarget::Resolved { symbol: target }
        );
        let edges = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(reads[0].id)
            })
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].object, RelationEndpoint::Entity(target));
        assert_eq!(edges[0].evidence.source.as_ref(), Some(&usage));
        exact_source(&service, &definition, "fsharp", b"offset");
        exact_source(&service, &usage, "fsharp", b"offset");
        let public = service
            .symbol_relationships(
                receipt.generation,
                BTreeSet::from([owner]),
                vec![RelationFamily::References],
                Some(RelationDirection::Outbound),
                0,
                16,
                0,
                &deadline(),
            )
            .unwrap();
        assert!(!public.data.truncated);
        let items = public
            .data
            .groups
            .iter()
            .flat_map(|group| &group.items)
            .filter(|item| item.symbol == target)
            .collect::<Vec<_>>();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].source_refs, vec![usage]);
        assert!(
            public
                .data
                .groups
                .iter()
                .flat_map(|group| &group.items)
                .all(|item| {
                    document
                        .entities
                        .iter()
                        .find(|entity| entity.id == item.symbol)
                        .unwrap()
                        .evidence
                        .source
                        .as_ref()
                        .unwrap()
                        .span()
                        .file()
                        == file.id
                })
        );
    }
}

#[test]
fn fsharp_qualified_module_targets_cross_files() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    write_qualified_modules(repository.path(), QUALIFIED_LIBRARY, QUALIFIED_CONSUMER);
    let mut service = FirstSliceService::new(2).unwrap();
    let indexed = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
    );
    assert_qualified_module_generation(
        &service,
        indexed.receipt(),
        QUALIFIED_LIBRARY,
        QUALIFIED_CONSUMER,
        QUALIFIED_APPLICATION,
        [
            Some(QualifiedLink::ReferenceOnly),
            Some(QualifiedLink::Call),
        ],
    );
}

#[test]
fn fsharp_qualified_module_links_survive_ordinary_record_contexts() {
    assert_qualified_links_with_type_context(
        "type Receiver = { carry: int -> int }\n",
        EntityKind::Struct,
    );
}

#[test]
fn fsharp_qualified_module_links_survive_plain_alias_contexts() {
    assert_qualified_links_with_type_context(
        "type Receiver = System.Int32\n",
        EntityKind::TypeAlias,
    );
}

#[test]
fn fsharp_qualified_module_links_survive_plain_enum_contexts() {
    assert_qualified_links_with_type_context(
        "type Receiver = | First = 0 | Next = 1\n",
        EntityKind::Enum,
    );
}

#[test]
fn fsharp_qualified_module_links_survive_plain_union_contexts() {
    assert_qualified_links_with_type_context(
        "type Receiver = | Empty | Named of value:int\n",
        EntityKind::Union,
    );
}

#[test]
fn fsharp_qualified_module_links_survive_plain_class_contexts() {
    assert_qualified_links_with_type_context(
        "type Receiver(seed:int) =\n    member this.Read amount = seed + amount\n",
        EntityKind::Class,
    );
}

fn assert_qualified_links_with_type_context(declaration: &str, kind: EntityKind) {
    for location in ["consumer", "library", "separate"] {
        let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let library = if location == "library" {
            QUALIFIED_LIBRARY.replace(
                "module Library\n",
                &format!("module Library\n{declaration}"),
            )
        } else {
            QUALIFIED_LIBRARY.to_owned()
        };
        let consumer = if location == "consumer" {
            QUALIFIED_CONSUMER.replace(
                "module Consumer\n",
                &format!("module Consumer\n{declaration}"),
            )
        } else {
            QUALIFIED_CONSUMER.to_owned()
        };
        write_qualified_modules(repository.path(), &library, &consumer);
        let separate = format!("module Models\n{declaration}");
        let (type_path, type_source) = match location {
            "consumer" => ("Consumer.fs", consumer.as_str()),
            "library" => ("Library.fs", library.as_str()),
            _ => {
                fs::write(repository.path().join("Models.fs"), &separate).unwrap();
                fs::write(
                    repository.path().join("Sample.fsproj"),
                    qualified_project(&["Models.fs", "Library.fs", "Consumer.fs"]),
                )
                .unwrap();
                ("Models.fs", separate.as_str())
            }
        };
        let mut service = FirstSliceService::new(2).unwrap();
        let indexed = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        let receipt = indexed.receipt();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let file = document
            .files
            .iter()
            .find(|file| file.path == type_path)
            .unwrap();
        assert_eq!(file.language, "fsharp");
        assert_eq!(file.content_hash, content_hash(type_source.as_bytes()));
        exact_source(
            &service,
            file.evidence.source.as_ref().unwrap(),
            "fsharp",
            type_source.as_bytes(),
        );
        let type_entity = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == kind
                    && entity.canonical_name == "Receiver"
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file.id)
            })
            .unwrap();
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == (OccurrenceTarget::Resolved {
                            symbol: type_entity.id,
                        })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        exact_source(&service, &definitions[0].source, "fsharp", b"Receiver");
        assert_qualified_module_generation(
            &service,
            receipt,
            &library,
            &consumer,
            QUALIFIED_APPLICATION,
            [
                Some(QualifiedLink::ReferenceOnly),
                Some(QualifiedLink::Call),
            ],
        );
        let claims = qualified_file_claims(document, file.id);
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(claim.binding(), rootlight_ir::FSharpBinding::Context))
                .count(),
            1
        );
        assert!(claims.iter().any(|claim| matches!(claim.binding(), rootlight_ir::FSharpBinding::Container { roots, .. } if roots.contains(&content_hash(b"Receiver")))));
    }
}

#[test]
fn fsharp_qualified_type_contexts_conserve_typenames_not_enum_cases() {
    for (declaration, shadows_module) in [
        ("type Library = System.Int32\n", true),
        ("type Library = | First = 0 | Next = 1\n", true),
        ("type Receiver = | Library = 0 | Next = 1\n", false),
    ] {
        let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let consumer = QUALIFIED_CONSUMER.replace(
            "module Consumer\n",
            &format!("module Consumer\n{declaration}"),
        );
        write_qualified_modules(repository.path(), QUALIFIED_LIBRARY, &consumer);
        let mut service = FirstSliceService::new(2).unwrap();
        let indexed = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        let snapshot = service
            .loaded_generation_snapshot(indexed.receipt().generation)
            .unwrap();
        let document = snapshot.document();
        let file = document
            .files
            .iter()
            .find(|file| file.path == "Consumer.fs")
            .unwrap();
        let claims = qualified_file_claims(document, file.id);
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(claim.binding(), rootlight_ir::FSharpBinding::Context))
                .count(),
            1
        );
        let roots = claims
            .iter()
            .find_map(|claim| match claim.binding() {
                rootlight_ir::FSharpBinding::Container { path, roots }
                    if path == &[content_hash(b"Consumer")] =>
                {
                    Some(roots)
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(roots.contains(&content_hash(b"Library")), shadows_module);
        assert!(!roots.contains(&content_hash(b"Next")));
        assert_qualified_module_generation(
            &service,
            indexed.receipt(),
            QUALIFIED_LIBRARY,
            &consumer,
            QUALIFIED_APPLICATION,
            if shadows_module {
                [Some(QualifiedLink::Closed), Some(QualifiedLink::Closed)]
            } else {
                [
                    Some(QualifiedLink::ReferenceOnly),
                    Some(QualifiedLink::Call),
                ]
            },
        );
    }
}

#[test]
fn fsharp_qualified_union_class_roots_distinguish_cases_and_owned_names() {
    for (declaration, name, kind, shadows_module) in [
        (
            "type Library = | Empty | Named of int\n",
            "Library",
            EntityKind::Union,
            true,
        ),
        (
            "type Receiver = | Library | Named of int\n",
            "Receiver",
            EntityKind::Union,
            true,
        ),
        (
            "type Receiver = | Empty | Named of Library:int\n",
            "Receiver",
            EntityKind::Union,
            false,
        ),
        (
            "type Library(seed:int) =\n    member this.Read amount = seed + amount\n",
            "Library",
            EntityKind::Class,
            true,
        ),
        (
            "type Receiver(library:int) =\n    member this.Library = library\n",
            "Receiver",
            EntityKind::Class,
            false,
        ),
    ] {
        let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let consumer = QUALIFIED_CONSUMER.replace(
            "module Consumer\n",
            &format!("module Consumer\n{declaration}"),
        );
        write_qualified_modules(repository.path(), QUALIFIED_LIBRARY, &consumer);
        let mut service = FirstSliceService::new(2).unwrap();
        let indexed = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        let snapshot = service
            .loaded_generation_snapshot(indexed.receipt().generation)
            .unwrap();
        let document = snapshot.document();
        let file = document
            .files
            .iter()
            .find(|file| file.path == "Consumer.fs")
            .unwrap();
        assert_authored_type_definition(&service, document, file, name, kind);
        let claims = qualified_file_claims(document, file.id);
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(claim.binding(), rootlight_ir::FSharpBinding::Context))
                .count(),
            1,
        );
        let roots = claims
            .iter()
            .find_map(|claim| match claim.binding() {
                rootlight_ir::FSharpBinding::Container { path, roots }
                    if path == &[content_hash(b"Consumer")] =>
                {
                    Some(roots)
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(roots.contains(&content_hash(b"Library")), shadows_module);
        assert!(!roots.contains(&content_hash(b"library")));
        assert!(claims.iter().all(|claim| !matches!(claim.binding(), rootlight_ir::FSharpBinding::Export { path, .. }
            if path.last().is_some_and(|last| [content_hash(b"Empty"), content_hash(b"Named"), content_hash(b"Library"), content_hash(b"Receiver")].contains(last)))));
        assert_qualified_module_generation(
            &service,
            indexed.receipt(),
            QUALIFIED_LIBRARY,
            &consumer,
            QUALIFIED_APPLICATION,
            if shadows_module {
                [Some(QualifiedLink::Closed), Some(QualifiedLink::Closed)]
            } else {
                [
                    Some(QualifiedLink::ReferenceOnly),
                    Some(QualifiedLink::Call),
                ]
            },
        );
    }
}

#[test]
fn fsharp_qualified_union_root_mutations_rebind_and_recover_links() {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let storage_policy = policy();
    let consumer =
        QUALIFIED_CONSUMER.replace("module Consumer\n", "module Consumer\nopen Imports\n");
    let ordinary = "module Imports\ntype Choice = | Empty | Named of int\n";
    let shadowing = "module Imports\ntype Choice = | Library | Named of int\n";
    write_qualified_modules(repository.path(), QUALIFIED_LIBRARY, &consumer);
    fs::write(
        repository.path().join("Sample.fsproj"),
        qualified_project(&["Imports.fs", "Library.fs", "Consumer.fs"]),
    )
    .unwrap();
    let mut service = FirstSliceService::new_durable_with_policy(
        storage_policy.clone(),
        paths.state_dir(),
        &deadline(),
    )
    .unwrap();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let proved = [
        Some(QualifiedLink::ReferenceOnly),
        Some(QualifiedLink::Call),
    ];
    let closed = [Some(QualifiedLink::Closed), Some(QualifiedLink::Closed)];
    let mut generations = Vec::new();
    let mut identity = None;
    for (imports, links) in [(ordinary, proved), (shadowing, closed), (ordinary, proved)] {
        fs::write(repository.path().join("Imports.fs"), imports).unwrap();
        let indexed = index(&mut service, repository.path(), options);
        if !generations.is_empty() {
            assert_eq!(indexed.evidence().rebuilt_files, 1);
            assert!(indexed.evidence().reused_files >= 2);
        }
        let receipt = indexed.receipt().clone();
        let (symbols, library_source, caller_source) = assert_qualified_module_generation(
            &service,
            &receipt,
            QUALIFIED_LIBRARY,
            &consumer,
            QUALIFIED_APPLICATION,
            links,
        );
        let stable = (
            library_source.span().file(),
            library_source.content_hash(),
            caller_source.span().file(),
            caller_source.content_hash(),
            symbols["offset"],
            symbols["carry"],
        );
        if let Some(previous) = identity {
            assert_eq!(stable, previous);
        } else {
            identity = Some(stable);
        }
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let file = snapshot
            .document()
            .files
            .iter()
            .find(|file| file.path == "Imports.fs")
            .unwrap();
        let source = file.evidence.source.as_ref().unwrap().clone();
        exact_source(&service, &source, "fsharp", imports.as_bytes());
        assert_authored_type_definition(
            &service,
            snapshot.document(),
            file,
            "Choice",
            EntityKind::Union,
        );
        let claims = qualified_file_claims(snapshot.document(), file.id);
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(claim.binding(), rootlight_ir::FSharpBinding::Context))
                .count(),
            1
        );
        assert_eq!(claims.iter().filter(|claim| matches!(claim.binding(), rootlight_ir::FSharpBinding::Container { path, roots }
            if path == &[content_hash(b"Imports")] && roots.contains(&content_hash(b"Library")))).count(), usize::from(imports == shadowing));
        generations.push((receipt, imports, links, source));
    }
    let clean = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
    )
    .receipt()
    .clone();
    assert!(clean.logical_snapshot.is_some());
    assert_eq!(clean.logical_snapshot, generations[2].0.logical_snapshot);
    assert_qualified_module_generation(
        &service,
        &clean,
        QUALIFIED_LIBRARY,
        &consumer,
        QUALIFIED_APPLICATION,
        proved,
    );
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(storage_policy, paths.state_dir(), &deadline())
            .unwrap();
    for (receipt, imports, links, source) in &generations {
        assert_qualified_module_generation(
            &reopened,
            receipt,
            QUALIFIED_LIBRARY,
            &consumer,
            QUALIFIED_APPLICATION,
            *links,
        );
        exact_source(&reopened, source, "fsharp", imports.as_bytes());
    }
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt().generation, clean.generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
}

fn qualified_file_claims(
    document: &NormalizedIrDocument,
    file: FileId,
) -> Vec<rootlight_ir::FSharpBindingEvidence> {
    document
        .extensions
        .iter()
        .filter(|envelope| envelope.namespace == rootlight_ir::FSHARP_BINDING_NAMESPACE)
        .map(|envelope| rootlight_ir::decode_fsharp_binding_envelope(envelope).unwrap())
        .filter(|claim| claim.scope().file() == file)
        .collect()
}

fn assert_authored_type_definition(
    service: &FirstSliceService,
    document: &NormalizedIrDocument,
    file: &rootlight_ir::FileRecord,
    name: &str,
    kind: EntityKind,
) {
    let entity = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == kind
                && entity.canonical_name == name
                && entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == file.id)
        })
        .unwrap();
    let definitions = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1);
    exact_source(
        service,
        &definitions[0].source,
        &file.language,
        name.as_bytes(),
    );
}

#[test]
fn fsharp_unsupported_type_contexts_preserve_structure_without_guessed_links() {
    for (declaration, name, kind) in [
        (
            "type Choice<'T> = | Empty | Named of value:'T\n",
            "Choice",
            EntityKind::Union,
        ),
        (
            "type Receiver<'T>(seed:'T) =\n    member this.Read = seed\n",
            "Receiver",
            EntityKind::Class,
        ),
    ] {
        let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let consumer = QUALIFIED_CONSUMER.replace(
            "module Consumer\n",
            &format!("module Consumer\n{declaration}"),
        );
        write_qualified_modules(repository.path(), QUALIFIED_LIBRARY, &consumer);
        let mut service = FirstSliceService::new(2).unwrap();
        let indexed = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        let snapshot = service
            .loaded_generation_snapshot(indexed.receipt().generation)
            .unwrap();
        let document = snapshot.document();
        let file = document
            .files
            .iter()
            .find(|file| file.path == "Consumer.fs")
            .unwrap();
        assert_authored_type_definition(&service, document, file, name, kind);
        assert!(
            !qualified_file_claims(document, file.id)
                .iter()
                .any(|claim| matches!(claim.binding(), rootlight_ir::FSharpBinding::Context))
        );
        assert_qualified_module_generation(
            &service,
            indexed.receipt(),
            QUALIFIED_LIBRARY,
            &consumer,
            QUALIFIED_APPLICATION,
            [Some(QualifiedLink::Closed), Some(QualifiedLink::Closed)],
        );
    }
}

#[test]
fn fsharp_signature_records_preserve_unrelated_qualified_value_links() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let library = QUALIFIED_LIBRARY.replace(
        "module Library\n",
        "module Library\ntype Receiver = { Count: int }\n",
    );
    let signature =
        "module Library\ntype Receiver = { Count: int }\nval offset: int\nval carry: int -> int\n";
    write_qualified_modules(repository.path(), &library, QUALIFIED_CONSUMER);
    fs::write(repository.path().join("Library.fsi"), signature).unwrap();
    fs::write(
        repository.path().join("Sample.fsproj"),
        qualified_project(&["Library.fsi", "Library.fs", "Consumer.fs"]),
    )
    .unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let indexed = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
    );
    let snapshot = service
        .loaded_generation_snapshot(indexed.receipt().generation)
        .unwrap();
    let document = snapshot.document();
    for path in ["Library.fsi", "Library.fs"] {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_authored_type_definition(&service, document, file, "Receiver", EntityKind::Struct);
        assert_authored_type_definition(&service, document, file, "Count", EntityKind::Field);
        exact_source(
            &service,
            file.evidence.source.as_ref().unwrap(),
            &file.language,
            if path.ends_with("fsi") {
                signature.as_bytes()
            } else {
                library.as_bytes()
            },
        );
        let claims = qualified_file_claims(document, file.id);
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(claim.binding(), rootlight_ir::FSharpBinding::Context))
                .count(),
            1,
        );
        if path.ends_with("fsi") {
            assert_eq!(
                claims
                    .iter()
                    .filter(|claim| matches!(claim.binding(),
                    rootlight_ir::FSharpBinding::SignatureType { path, .. }
                        if path == &[content_hash(b"Library"), content_hash(b"Receiver")]))
                    .count(),
                1,
            );
        }
    }
    assert_qualified_module_generation(
        &service,
        indexed.receipt(),
        &library,
        QUALIFIED_CONSUMER,
        QUALIFIED_APPLICATION,
        [
            Some(QualifiedLink::ReferenceOnly),
            Some(QualifiedLink::Call),
        ],
    );
    let implementation_record = document
        .entities
        .iter()
        .find(|entity| {
            entity.language == "fsharp"
                && entity.kind == EntityKind::Struct
                && entity.canonical_name == "Receiver"
        })
        .unwrap();
    let record_source = implementation_record.evidence.source.as_ref().unwrap();
    let signature_type = document
        .extensions
        .iter()
        .find(|envelope| {
            envelope.namespace == rootlight_ir::FSHARP_BINDING_NAMESPACE
                && matches!(
                    rootlight_ir::decode_fsharp_binding_envelope(envelope)
                        .unwrap()
                        .binding(),
                    rootlight_ir::FSharpBinding::SignatureType { .. }
                )
        })
        .unwrap();
    let value_targets = document
        .entities
        .iter()
        .filter(|entity| {
            entity.language == "fsharp"
                && ["offset", "carry"].contains(&entity.canonical_name.as_str())
        })
        .map(|entity| entity.id)
        .collect::<BTreeSet<_>>();
    let proofs = document.relations.iter().filter(|relation| {
        matches!(relation.predicate, RelationPredicate::RefersTo | RelationPredicate::Calls)
            && matches!(relation.object, RelationEndpoint::Entity(symbol) if value_targets.contains(&symbol))
            && relation.evidence.source.as_ref().is_some_and(|source| source.span().file() != record_source.span().file())
    }).map(|relation| document.provenance.iter().find(|proof| proof.id == relation.provenance).unwrap()).collect::<Vec<_>>();
    assert_eq!(proofs.len(), 3);
    for proof in proofs {
        assert!(
            proof
                .derivation_parents
                .contains(&rootlight_ir::FactRef::Fact(signature_type.id))
        );
        assert!(
            proof
                .derivation_parents
                .contains(&rootlight_ir::FactRef::Entity(implementation_record.id))
        );
        assert!(proof.input_sources.contains(record_source));
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls
                && relation.object == RelationEndpoint::Entity(implementation_record.id))
    );
}

#[test]
fn fsharp_signature_record_roots_preserve_exposed_and_hidden_open_masks() {
    let library = QUALIFIED_LIBRARY.replace("module Library", "module library");
    let consumer = QUALIFIED_CONSUMER
        .replace("Library.", "library.")
        .replace("module Consumer\n", "module Consumer\nopen Imports\n");
    let application = QUALIFIED_APPLICATION.replace("Library.", "library.");
    let imports = "module Imports\ntype library = { Count: int }\nlet sentinel = 0\n";
    for (signature, exposed) in [
        ("module Imports\nval sentinel: int\n", false),
        (
            "module Imports\ntype library = { Count: int }\nval sentinel: int\n",
            true,
        ),
    ] {
        let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        write_qualified_modules(repository.path(), &library, &consumer);
        fs::write(repository.path().join("Imports.fs"), imports).unwrap();
        fs::write(repository.path().join("Imports.fsi"), signature).unwrap();
        fs::write(
            repository.path().join("Sample.fsproj"),
            qualified_project(&["Imports.fsi", "Imports.fs", "Library.fs", "Consumer.fs"]),
        )
        .unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let indexed = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        let snapshot = service
            .loaded_generation_snapshot(indexed.receipt().generation)
            .unwrap();
        let file = snapshot
            .document()
            .files
            .iter()
            .find(|file| file.path == "Imports.fsi")
            .unwrap();
        let claims = qualified_file_claims(snapshot.document(), file.id);
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(claim.binding(), rootlight_ir::FSharpBinding::Context))
                .count(),
            1
        );
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(
                    claim.binding(),
                    rootlight_ir::FSharpBinding::SignatureType { .. }
                ))
                .count(),
            usize::from(exposed)
        );
        assert_eq!(claims.iter().filter(|claim| matches!(claim.binding(), rootlight_ir::FSharpBinding::Container { path, roots }
            if path == &[content_hash(b"Imports")] && roots.contains(&content_hash(b"library")))).count(), usize::from(exposed));
        exact_source(
            &service,
            file.evidence.source.as_ref().unwrap(),
            "fsharp-signature",
            signature.as_bytes(),
        );
        assert_qualified_module_generation(
            &service,
            indexed.receipt(),
            &library,
            &consumer,
            &application,
            if exposed {
                [Some(QualifiedLink::Closed), Some(QualifiedLink::Closed)]
            } else {
                [
                    Some(QualifiedLink::ReferenceOnly),
                    Some(QualifiedLink::Call),
                ]
            },
        );
    }
}

#[test]
fn fsharp_signature_record_mutations_rebind_and_recover_value_links() {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let storage_policy = policy();
    let library = QUALIFIED_LIBRARY.replace(
        "module Library\n",
        "module Library\ntype Receiver = { Count: int }\n",
    );
    let matched =
        "module Library\ntype Receiver = { Count: int }\nval offset: int\nval carry: int -> int\n";
    let unmatched =
        "module Library\ntype Other = { Count: int }\nval offset: int\nval carry: int -> int\n";
    write_qualified_modules(repository.path(), &library, QUALIFIED_CONSUMER);
    fs::write(
        repository.path().join("Sample.fsproj"),
        qualified_project(&["Library.fsi", "Library.fs", "Consumer.fs"]),
    )
    .unwrap();
    let mut service = FirstSliceService::new_durable_with_policy(
        storage_policy.clone(),
        paths.state_dir(),
        &deadline(),
    )
    .unwrap();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let proved = [
        Some(QualifiedLink::ReferenceOnly),
        Some(QualifiedLink::Call),
    ];
    let closed = [Some(QualifiedLink::Closed), Some(QualifiedLink::Closed)];
    let mut generations = Vec::new();
    let mut identity = None;
    for (signature, links) in [(matched, proved), (unmatched, closed), (matched, proved)] {
        fs::write(repository.path().join("Library.fsi"), signature).unwrap();
        let indexed = index(&mut service, repository.path(), options);
        if !generations.is_empty() {
            assert_eq!(indexed.evidence().rebuilt_files, 1);
        }
        let receipt = indexed.receipt().clone();
        let (symbols, implementation, caller) = assert_qualified_module_generation(
            &service,
            &receipt,
            &library,
            QUALIFIED_CONSUMER,
            QUALIFIED_APPLICATION,
            links,
        );
        let stable = (
            implementation.span().file(),
            implementation.content_hash(),
            caller.span().file(),
            caller.content_hash(),
            symbols["offset"],
            symbols["carry"],
        );
        if let Some(previous) = identity {
            assert_eq!(stable, previous);
        } else {
            identity = Some(stable);
        }
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let file = snapshot
            .document()
            .files
            .iter()
            .find(|file| file.path == "Library.fsi")
            .unwrap();
        let source = file.evidence.source.as_ref().unwrap().clone();
        assert_authored_type_definition(
            &service,
            snapshot.document(),
            file,
            if signature == matched {
                "Receiver"
            } else {
                "Other"
            },
            EntityKind::Struct,
        );
        let claims = qualified_file_claims(snapshot.document(), file.id);
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(claim.binding(), rootlight_ir::FSharpBinding::Context))
                .count(),
            1
        );
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(
                    claim.binding(),
                    rootlight_ir::FSharpBinding::SignatureType { .. }
                ))
                .count(),
            1
        );
        exact_source(&service, &source, "fsharp-signature", signature.as_bytes());
        generations.push((receipt, signature, links, source));
    }
    let clean = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
    )
    .receipt()
    .clone();
    assert!(clean.logical_snapshot.is_some());
    assert_eq!(clean.logical_snapshot, generations[2].0.logical_snapshot);
    assert_qualified_module_generation(
        &service,
        &clean,
        &library,
        QUALIFIED_CONSUMER,
        QUALIFIED_APPLICATION,
        proved,
    );
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(storage_policy, paths.state_dir(), &deadline())
            .unwrap();
    for (receipt, signature, links, source) in &generations {
        assert_qualified_module_generation(
            &reopened,
            receipt,
            &library,
            QUALIFIED_CONSUMER,
            QUALIFIED_APPLICATION,
            *links,
        );
        exact_source(&reopened, source, "fsharp-signature", signature.as_bytes());
    }
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt().generation, clean.generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
}

#[test]
fn fsharp_signature_aliases_preserve_unrelated_value_links() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let library = QUALIFIED_LIBRARY.replace(
        "module Library\n",
        "module Library\ntype Receiver = System.Int32\n",
    );
    let signature =
        "module Library\ntype Receiver = System.Int32\nval offset: int\nval carry: int -> int\n";
    write_qualified_modules(repository.path(), &library, QUALIFIED_CONSUMER);
    fs::write(repository.path().join("Library.fsi"), signature).unwrap();
    fs::write(
        repository.path().join("Sample.fsproj"),
        qualified_project(&["Library.fsi", "Library.fs", "Consumer.fs"]),
    )
    .unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let indexed = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
    );
    let snapshot = service
        .loaded_generation_snapshot(indexed.receipt().generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == "Library.fsi")
        .unwrap();
    exact_source(
        &service,
        file.evidence.source.as_ref().unwrap(),
        "fsharp-signature",
        signature.as_bytes(),
    );
    for (name, kind) in [
        ("Receiver", EntityKind::TypeAlias),
        ("offset", EntityKind::Variable),
        ("carry", EntityKind::Variable),
    ] {
        assert_authored_type_definition(&service, document, file, name, kind);
    }
    assert_signature_alias_queries(
        &service,
        indexed.receipt(),
        "Library.fsi",
        "Receiver",
        signature,
    );
    let consumer_file = document
        .files
        .iter()
        .find(|file| file.path == "Consumer.fs")
        .unwrap();
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.source.span().file() == consumer_file.id
            && gap.detail == "fsharp-qualified-target-unavailable"
    }));
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.source.span().file() == consumer_file.id
            && gap.detail == "fsharp-project-build-closure-unverified"
            && gap.reason == SkippedRegionReason::MissingBuildContext
    }));
    assert_qualified_module_generation(
        &service,
        indexed.receipt(),
        &library,
        QUALIFIED_CONSUMER,
        QUALIFIED_APPLICATION,
        [
            Some(QualifiedLink::ReferenceOnly),
            Some(QualifiedLink::Call),
        ],
    );
}

fn assert_signature_alias_queries(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    name: &str,
    source: &str,
) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == path)
        .unwrap();
    let alias = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::TypeAlias
                && entity.canonical_name == name
                && entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == file.id)
        })
        .unwrap();
    assert_authored_type_definition(service, document, file, name, EntityKind::TypeAlias);
    let check_source = |reference: &SourceRef| {
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), file.content_hash);
        let expected = source
            .as_bytes()
            .get(
                usize::try_from(reference.span().start_byte()).unwrap()
                    ..usize::try_from(reference.span().end_byte()).unwrap(),
            )
            .unwrap();
        exact_source(service, reference, "fsharp-signature", expected);
    };
    let located = service
        .code_locate_with_filters_and_budget(
            receipt.generation,
            name.to_owned(),
            LocateMode::Exact,
            vec!["fsharp-signature".to_owned()],
            vec![path.to_owned()],
            8,
            0,
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(located.data.generation, receipt.generation);
    let hits = located
        .data
        .hits
        .iter()
        .filter(|hit| hit.symbol == Some(alias.id))
        .collect::<Vec<_>>();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].file, file.id);
    assert_eq!(hits[0].path, path);
    check_source(hits[0].source.as_ref().unwrap());
    let explained = service
        .symbol_explain(receipt.generation, alias.id, &deadline())
        .unwrap();
    assert_eq!(explained.data.generation, receipt.generation);
    assert_eq!(explained.data.entity, *alias);
    assert_eq!(hits[0].signature, explained.data.signature);
    check_source(explained.data.entity.evidence.source.as_ref().unwrap());
    let claims = qualified_file_claims(document, file.id);
    assert_eq!(
        claims
            .iter()
            .filter(|claim| matches!(claim.binding(), rootlight_ir::FSharpBinding::Context))
            .count(),
        1
    );
    assert_eq!(claims.iter().filter(|claim| matches!(claim.binding(), rootlight_ir::FSharpBinding::SignatureAlias { symbol, .. } if *symbol == alias.id)).count(), 1);
    assert!(!claims.iter().any(|claim| matches!(claim.binding(), rootlight_ir::FSharpBinding::Export { symbol, .. } if *symbol == alias.id)));
    assert!(!document.relations.iter().any(|relation| {
        matches!(
            relation.predicate,
            RelationPredicate::Calls | RelationPredicate::DispatchCandidate
        ) && (relation.subject == RelationEndpoint::Entity(alias.id)
            || relation.object == RelationEndpoint::Entity(alias.id))
    }));
}

#[test]
fn fsharp_signature_alias_changes_rebind_values_across_durable_recovery() {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let storage_policy = policy();
    let library = QUALIFIED_LIBRARY.replace(
        "module Library\n",
        "module Library\ntype Receiver = System.Int32\n",
    );
    let matched =
        "module Library\ntype Receiver = System.Int32\nval offset: int\nval carry: int -> int\n";
    let unmatched = matched.replace("Receiver", "Other");
    write_qualified_modules(repository.path(), &library, QUALIFIED_CONSUMER);
    fs::write(
        repository.path().join("Sample.fsproj"),
        qualified_project(&["Library.fsi", "Library.fs", "Consumer.fs"]),
    )
    .unwrap();
    let mut service = FirstSliceService::new_durable_with_policy(
        storage_policy.clone(),
        paths.state_dir(),
        &deadline(),
    )
    .unwrap();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let proved = [
        Some(QualifiedLink::ReferenceOnly),
        Some(QualifiedLink::Call),
    ];
    let closed = [Some(QualifiedLink::Closed), Some(QualifiedLink::Closed)];
    let mut generations = Vec::new();
    let mut identity = None;
    for (signature, name, links) in [
        (matched, "Receiver", proved),
        (unmatched.as_str(), "Other", closed),
        (matched, "Receiver", proved),
    ] {
        fs::write(repository.path().join("Library.fsi"), signature).unwrap();
        let indexed = index(&mut service, repository.path(), options);
        if !generations.is_empty() {
            assert_eq!(indexed.evidence().rebuilt_files, 1);
            assert!(indexed.evidence().reused_files >= 2);
            assert!(indexed.evidence().reused_facts > 0);
        }
        let receipt = indexed.receipt().clone();
        let (symbols, implementation, caller) = assert_qualified_module_generation(
            &service,
            &receipt,
            &library,
            QUALIFIED_CONSUMER,
            QUALIFIED_APPLICATION,
            links,
        );
        let stable = (
            implementation.span().file(),
            implementation.content_hash(),
            caller.span().file(),
            caller.content_hash(),
            symbols["offset"],
            symbols["carry"],
        );
        if let Some(previous) = identity {
            assert_eq!(stable, previous);
        } else {
            identity = Some(stable);
        }
        assert_signature_alias_queries(&service, &receipt, "Library.fsi", name, signature);
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let file = snapshot
            .document()
            .files
            .iter()
            .find(|file| file.path == "Library.fsi")
            .unwrap();
        let source = file.evidence.source.as_ref().unwrap().clone();
        exact_source(&service, &source, "fsharp-signature", signature.as_bytes());
        generations.push((receipt, signature.to_owned(), name, links, source));
    }
    let clean = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
    )
    .receipt()
    .clone();
    assert!(clean.logical_snapshot.is_some());
    assert_eq!(clean.logical_snapshot, generations[2].0.logical_snapshot);
    assert_qualified_module_generation(
        &service,
        &clean,
        &library,
        QUALIFIED_CONSUMER,
        QUALIFIED_APPLICATION,
        proved,
    );
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(storage_policy, paths.state_dir(), &deadline())
            .unwrap();
    for (receipt, signature, name, links, source) in &generations {
        assert_qualified_module_generation(
            &reopened,
            receipt,
            &library,
            QUALIFIED_CONSUMER,
            QUALIFIED_APPLICATION,
            *links,
        );
        assert_signature_alias_queries(&reopened, receipt, "Library.fsi", name, signature);
        exact_source(&reopened, source, "fsharp-signature", signature.as_bytes());
    }
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt().generation, clean.generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
}

#[test]
fn fsharp_signature_alias_roots_preserve_exposed_and_hidden_open_masks() {
    let library = QUALIFIED_LIBRARY.replace("module Library", "module library");
    let consumer = QUALIFIED_CONSUMER
        .replace("Library.", "library.")
        .replace("module Consumer\n", "module Consumer\nopen Imports\n");
    let application = QUALIFIED_APPLICATION.replace("Library.", "library.");
    let imports = "module Imports\ntype library = System.Int32\nlet sentinel = 0\n";
    for (signature, exposed) in [
        ("module Imports\nval sentinel: int\n", false),
        (
            "module Imports\ntype library = System.Int32\nval sentinel: int\n",
            true,
        ),
    ] {
        let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        write_qualified_modules(repository.path(), &library, &consumer);
        fs::write(repository.path().join("Imports.fs"), imports).unwrap();
        fs::write(repository.path().join("Imports.fsi"), signature).unwrap();
        fs::write(
            repository.path().join("Sample.fsproj"),
            qualified_project(&["Imports.fsi", "Imports.fs", "Library.fs", "Consumer.fs"]),
        )
        .unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let indexed = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        assert_qualified_module_generation(
            &service,
            indexed.receipt(),
            &library,
            &consumer,
            &application,
            if exposed {
                [Some(QualifiedLink::Closed), Some(QualifiedLink::Closed)]
            } else {
                [
                    Some(QualifiedLink::ReferenceOnly),
                    Some(QualifiedLink::Call),
                ]
            },
        );
        if exposed {
            assert_signature_alias_queries(
                &service,
                indexed.receipt(),
                "Imports.fsi",
                "library",
                signature,
            );
        }
    }
}

#[test]
fn fsharp_qualified_module_links_survive_owned_open_contexts() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let imports = "module Imports\nlet unrelated = 1\n";
    let consumer =
        QUALIFIED_CONSUMER.replace("module Consumer\n", "module Consumer\nopen Imports\n");
    write_qualified_modules(repository.path(), QUALIFIED_LIBRARY, &consumer);
    fs::write(repository.path().join("Imports.fs"), imports).unwrap();
    fs::write(
        repository.path().join("Sample.fsproj"),
        qualified_project(&["Imports.fs", "Library.fs", "Consumer.fs"]),
    )
    .unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let indexed = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
    );
    let receipt = indexed.receipt();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let imported_file = document
        .files
        .iter()
        .find(|file| file.path == "Imports.fs")
        .unwrap();
    assert_eq!(imported_file.language, "fsharp");
    assert_eq!(imported_file.content_hash, content_hash(imports.as_bytes()));
    exact_source(
        &service,
        imported_file.evidence.source.as_ref().unwrap(),
        "fsharp",
        imports.as_bytes(),
    );
    let imported_value = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Variable
                && entity.canonical_name == "unrelated"
                && entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == imported_file.id)
        })
        .collect::<Vec<_>>();
    assert_eq!(imported_value.len(), 1);
    let definitions = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.target
                    == (OccurrenceTarget::Resolved {
                        symbol: imported_value[0].id,
                    })
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1);
    exact_source(&service, &definitions[0].source, "fsharp", b"unrelated");
    assert_qualified_module_generation(
        &service,
        receipt,
        QUALIFIED_LIBRARY,
        &consumer,
        QUALIFIED_APPLICATION,
        [
            Some(QualifiedLink::ReferenceOnly),
            Some(QualifiedLink::Call),
        ],
    );
}

#[test]
fn fsharp_qualified_module_opens_keep_root_shadowing_and_source_order() {
    let current = std::env::current_dir().unwrap();
    let library = QUALIFIED_LIBRARY.replace("module Library", "module library");
    let plain = QUALIFIED_CONSUMER.replace("Library.", "library.");
    let application = QUALIFIED_APPLICATION.replace("Library.", "library.");
    let value_shadow = "module Imports\ntype Receiver = { offset: int; carry: int -> int }\nlet library = { offset = 0; carry = fun value -> value }\n";
    let module_shadow = "module Imports\nmodule library =\n    let offset = 0\n    let carry value = value\nlet unrelated = 1\n";
    let opened = plain.replace("module Consumer\n", "module Consumer\nopen Imports\n");
    let later = plain.replace("let invoke input", "open Imports\nlet invoke input");
    let external = plain.replace("module Consumer\n", "module Consumer\nopen System\n");
    for (imports, consumer, links, require_shadow_root) in [
        (
            value_shadow,
            opened.as_str(),
            [QualifiedLink::Closed, QualifiedLink::Closed],
            true,
        ),
        (
            module_shadow,
            opened.as_str(),
            [QualifiedLink::Closed, QualifiedLink::Closed],
            true,
        ),
        (
            value_shadow,
            later.as_str(),
            [QualifiedLink::ReferenceOnly, QualifiedLink::Closed],
            true,
        ),
        (
            "module Imports\nlet unrelated = 1\n",
            external.as_str(),
            [QualifiedLink::Closed, QualifiedLink::Closed],
            false,
        ),
    ] {
        let repository = tempfile::tempdir_in(&current).unwrap();
        write_qualified_modules(repository.path(), &library, consumer);
        fs::write(repository.path().join("Imports.fs"), imports).unwrap();
        fs::write(
            repository.path().join("Sample.fsproj"),
            qualified_project(&["Imports.fs", "Library.fs", "Consumer.fs"]),
        )
        .unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let indexed = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        let snapshot = service
            .loaded_generation_snapshot(indexed.receipt().generation)
            .unwrap();
        let document = snapshot.document();
        let file = document
            .files
            .iter()
            .find(|file| file.path == "Imports.fs")
            .unwrap();
        exact_source(
            &service,
            file.evidence.source.as_ref().unwrap(),
            "fsharp",
            imports.as_bytes(),
        );
        if require_shadow_root {
            let claims = document
                .extensions
                .iter()
                .filter(|envelope| envelope.namespace == rootlight_ir::FSHARP_BINDING_NAMESPACE)
                .map(|envelope| {
                    (
                        envelope,
                        rootlight_ir::decode_fsharp_binding_envelope(envelope).unwrap(),
                    )
                })
                .filter(|(_, claim)| claim.scope().file() == file.id)
                .collect::<Vec<_>>();
            assert_eq!(
                claims
                    .iter()
                    .filter(|(_, claim)| matches!(
                        claim.binding(),
                        rootlight_ir::FSharpBinding::Context
                    ))
                    .count(),
                1,
                "complete context for {imports}"
            );
            let containers = claims
                .iter()
                .filter(|(_, claim)| {
                    matches!(
                        claim.binding(),
                        rootlight_ir::FSharpBinding::Container { .. }
                    )
                })
                .collect::<Vec<_>>();
            let imported = containers
                .iter()
                .find(|(_, claim)| {
                    matches!(claim.binding(), rootlight_ir::FSharpBinding::Container { path, roots }
                if path == &[content_hash(b"Imports")] && roots.contains(&content_hash(b"library")))
                })
                .unwrap();
            let manifests = claims
                .iter()
                .filter(|(_, claim)| {
                    matches!(
                        claim.binding(),
                        rootlight_ir::FSharpBinding::ImportContext { .. }
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(manifests.len(), 1);
            assert_eq!(
                manifests[0].0.evidence.source.as_ref(),
                file.evidence.source.as_ref()
            );
            let mut container_spans = containers
                .iter()
                .map(|(envelope, _)| envelope.evidence.source.as_ref().unwrap().span())
                .collect::<Vec<_>>();
            container_spans.sort_unstable();
            assert!(
                matches!(manifests[0].1.binding(), rootlight_ir::FSharpBinding::ImportContext { containers, opens }
                if containers == &container_spans && containers.contains(&imported.0.evidence.source.as_ref().unwrap().span()) && opens.is_empty()),
                "complete imported root inventory for {imports}"
            );
        }
        assert_qualified_module_generation(
            &service,
            indexed.receipt(),
            &library,
            consumer,
            &application,
            links.map(Some),
        );
    }
}

#[test]
fn fsharp_qualified_module_opens_preserve_target_exports_and_inherited_contexts() {
    let harmless = "module Imports\nlet unrelated = 1\n";
    let library = QUALIFIED_LIBRARY.replace("module Library", "module library");
    let plain = QUALIFIED_CONSUMER.replace("Library.", "library.");
    let application = QUALIFIED_APPLICATION.replace("Library.", "library.");
    let library_open = library.replace("module library\n", "module library\nopen System\n");
    let nested = "module Consumer\nopen Imports\nmodule Uses =\n    let read () = library.offset\n    let invoke input = library.carry input\n";
    let shadowed = "module Consumer\nopen Imports\ntype Receiver = { offset: int; carry: int -> int }\nlet library = { offset = 0; carry = fun value -> value }\nmodule Uses =\n    let read () = library.offset\n    let invoke input = library.carry input\n";
    let proven = [
        Some(QualifiedLink::ReferenceOnly),
        Some(QualifiedLink::Call),
    ];
    for (library, consumer, links) in [
        (library_open.as_str(), plain.as_str(), proven),
        (library.as_str(), nested, proven),
        (library.as_str(), shadowed, [Some(QualifiedLink::Closed); 2]),
    ] {
        let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        write_qualified_modules(repository.path(), library, consumer);
        fs::write(repository.path().join("Imports.fs"), harmless).unwrap();
        fs::write(
            repository.path().join("Sample.fsproj"),
            qualified_project(&["Imports.fs", "Library.fs", "Consumer.fs"]),
        )
        .unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let indexed = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        if consumer == shadowed {
            let snapshot = service
                .loaded_generation_snapshot(indexed.receipt().generation)
                .unwrap();
            let document = snapshot.document();
            let caller = document
                .files
                .iter()
                .find(|file| file.path == "Consumer.fs")
                .unwrap();
            assert!(document.extensions.iter().any(|envelope| {
                envelope.namespace == rootlight_ir::FSHARP_BINDING_NAMESPACE
                    && envelope.evidence.source.as_ref() == caller.evidence.source.as_ref()
                    && matches!(
                        rootlight_ir::decode_fsharp_binding_envelope(envelope)
                            .unwrap()
                            .binding(),
                        rootlight_ir::FSharpBinding::Context
                    )
            }));
        }
        assert_qualified_module_generation(
            &service,
            indexed.receipt(),
            library,
            consumer,
            &application,
            links,
        );
    }
}

#[test]
fn fsharp_qualified_module_open_proofs_keep_original_encoded_coordinates() {
    let imports = "module Imports\nlet unrelated = 1\n";
    let consumer = format!(
        "// \u{1f680}\n{}",
        QUALIFIED_CONSUMER.replace("module Consumer\n", "module Consumer\nopen Imports\n")
    );
    for (encoding, marker) in [("utf-16le", [0xff, 0xfe]), ("utf-16be", [0xfe, 0xff])] {
        let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        write_qualified_modules(repository.path(), QUALIFIED_LIBRARY, &consumer);
        let bytes = [
            marker.to_vec(),
            typed_formal_leaf_bytes(&consumer, encoding),
        ]
        .concat();
        fs::write(repository.path().join("Consumer.fs"), &bytes).unwrap();
        fs::write(repository.path().join("Imports.fs"), imports).unwrap();
        fs::write(
            repository.path().join("Sample.fsproj"),
            qualified_project(&["Imports.fs", "Library.fs", "Consumer.fs"]),
        )
        .unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let indexed = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        let receipt = indexed.receipt();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let caller = document
            .files
            .iter()
            .find(|file| file.path == "Consumer.fs")
            .unwrap();
        let library = document
            .files
            .iter()
            .find(|file| file.path == "Library.fs")
            .unwrap();
        assert_eq!(caller.encoding, encoding);
        assert_eq!(caller.content_hash, content_hash(&bytes));
        exact_source(
            &service,
            caller.evidence.source.as_ref().unwrap(),
            "fsharp",
            &bytes,
        );
        let span_for = |written: &str| {
            let start = consumer.find(written).unwrap();
            let first = marker.len() + typed_formal_leaf_bytes(&consumer[..start], encoding).len();
            let end = first + typed_formal_leaf_bytes(written, encoding).len();
            SourceSpan::new(
                caller.id,
                u64::try_from(first).unwrap(),
                u64::try_from(end).unwrap(),
            )
            .unwrap()
        };
        let container_span = SourceSpan::new(
            caller.id,
            span_for("module Consumer").start_byte(),
            span_for(QUALIFIED_APPLICATION).end_byte(),
        )
        .unwrap();
        let open_span = span_for("open Imports");
        let claims = document
            .extensions
            .iter()
            .filter(|envelope| envelope.namespace == rootlight_ir::FSHARP_BINDING_NAMESPACE)
            .map(|envelope| {
                (
                    envelope,
                    rootlight_ir::decode_fsharp_binding_envelope(envelope).unwrap(),
                )
            })
            .filter(|(_, claim)| claim.scope().file() == caller.id)
            .collect::<Vec<_>>();
        let opens = claims
            .iter()
            .filter(|(_, claim)| {
                matches!(claim.binding(), rootlight_ir::FSharpBinding::Open { .. })
            })
            .collect::<Vec<_>>();
        assert_eq!(opens.len(), 1);
        assert_eq!(
            opens[0].0.evidence.source.as_ref().unwrap().span(),
            open_span
        );
        assert!(
            matches!(opens[0].1.binding(), rootlight_ir::FSharpBinding::Open { path, owner }
            if path == &[content_hash(b"Imports")] && *owner == container_span)
        );
        exact_source(
            &service,
            opens[0].0.evidence.source.as_ref().unwrap(),
            "fsharp",
            &typed_formal_leaf_bytes("open Imports", encoding),
        );
        let manifests = claims
            .iter()
            .filter(|(_, claim)| {
                matches!(
                    claim.binding(),
                    rootlight_ir::FSharpBinding::ImportContext { .. }
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(manifests.len(), 1);
        assert!(
            matches!(manifests[0].1.binding(), rootlight_ir::FSharpBinding::ImportContext { containers, opens }
            if containers == &[container_span] && opens == &[open_span])
        );
        assert_eq!(
            manifests[0].0.evidence.source.as_ref().unwrap(),
            caller.evidence.source.as_ref().unwrap()
        );
        for (written, name) in [("Library.offset", "offset"), ("Library.carry", "carry")] {
            let target = document
                .entities
                .iter()
                .find(|entity| {
                    entity.canonical_name == name
                        && entity
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|source| source.span().file() == library.id)
                })
                .unwrap();
            let read = document
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Reference
                        && site.source.span() == span_for(written)
                })
                .unwrap();
            assert_eq!(
                read.target,
                OccurrenceTarget::Resolved { symbol: target.id }
            );
            assert_eq!(read.source.generation(), receipt.generation);
            assert_eq!(read.source.content_hash(), content_hash(&bytes));
            exact_source(
                &service,
                &read.source,
                "fsharp",
                &typed_formal_leaf_bytes(written, encoding),
            );
            assert!(document.relations.iter().any(|edge| edge.predicate
                == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(read.id)
                && edge.object == RelationEndpoint::Entity(target.id)
                && edge.evidence.source.as_ref() == Some(&read.source)));
        }
        let calls = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Calls
                    && edge
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == caller.id)
            })
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 1);
        let invoke = document
            .entities
            .iter()
            .find(|entity| {
                entity.canonical_name == "invoke"
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == caller.id)
            })
            .unwrap();
        let carry = document
            .entities
            .iter()
            .find(|entity| {
                entity.canonical_name == "carry"
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == library.id)
            })
            .unwrap();
        assert_eq!(calls[0].subject, RelationEndpoint::Entity(invoke.id));
        assert_eq!(calls[0].object, RelationEndpoint::Entity(carry.id));
        let source = calls[0].evidence.source.as_ref().unwrap();
        assert_eq!(source.span(), span_for(QUALIFIED_APPLICATION));
        exact_source(
            &service,
            source,
            "fsharp",
            &typed_formal_leaf_bytes(QUALIFIED_APPLICATION, encoding),
        );
    }
}

#[test]
fn fsharp_qualified_module_open_surface_changes_rebind_unchanged_callers() {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let storage_policy = policy();
    let library = QUALIFIED_LIBRARY.replace("module Library", "module library");
    let application = QUALIFIED_APPLICATION.replace("Library.", "library.");
    let harmless =
        "module Imports\ntype Receiver = { offset: int; carry: int -> int }\nlet unrelated = 1\n";
    let shadowed = harmless.replace(
        "let unrelated = 1",
        "let library = { offset = 0; carry = fun value -> value }",
    );
    let consumer = QUALIFIED_CONSUMER
        .replace("Library.", "library.")
        .replace("module Consumer\n", "module Consumer\nopen Imports\n");
    write_qualified_modules(repository.path(), &library, &consumer);
    fs::write(repository.path().join("Imports.fs"), harmless).unwrap();
    fs::write(
        repository.path().join("Sample.fsproj"),
        qualified_project(&["Imports.fs", "Library.fs", "Consumer.fs"]),
    )
    .unwrap();
    let mut service = FirstSliceService::new_durable_with_policy(
        storage_policy.clone(),
        paths.state_dir(),
        &deadline(),
    )
    .unwrap();
    let proved = [
        Some(QualifiedLink::ReferenceOnly),
        Some(QualifiedLink::Call),
    ];
    let closed = [Some(QualifiedLink::Closed), Some(QualifiedLink::Closed)];
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let mut generations = Vec::new();
    let mut unchanged_identity = None;
    for (imports, links) in [
        (harmless, proved),
        (shadowed.as_str(), closed),
        (harmless, proved),
    ] {
        fs::write(repository.path().join("Imports.fs"), imports).unwrap();
        let receipt = index(&mut service, repository.path(), options)
            .receipt()
            .clone();
        let (symbols, library_source, consumer_source) = assert_qualified_module_generation(
            &service,
            &receipt,
            &library,
            &consumer,
            &application,
            links,
        );
        let identity = (
            library_source.span().file(),
            library_source.content_hash(),
            consumer_source.span().file(),
            consumer_source.content_hash(),
            symbols["offset"],
            symbols["carry"],
        );
        if let Some(previous) = unchanged_identity {
            assert_eq!(identity, previous);
        } else {
            unchanged_identity = Some(identity);
        }
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let source = snapshot
            .document()
            .files
            .iter()
            .find(|file| file.path == "Imports.fs")
            .unwrap()
            .evidence
            .source
            .as_ref()
            .unwrap()
            .clone();
        let imported_claims = snapshot
            .document()
            .extensions
            .iter()
            .filter(|envelope| envelope.namespace == rootlight_ir::FSHARP_BINDING_NAMESPACE)
            .map(|envelope| {
                (
                    envelope,
                    rootlight_ir::decode_fsharp_binding_envelope(envelope).unwrap(),
                )
            })
            .filter(|(_, claim)| claim.scope().file() == source.span().file())
            .collect::<Vec<_>>();
        assert_eq!(
            imported_claims
                .iter()
                .filter(
                    |(envelope, claim)| envelope.evidence.source.as_ref() == Some(&source)
                        && matches!(claim.binding(), rootlight_ir::FSharpBinding::Context)
                )
                .count(),
            1,
            "complete imported context for {imports}"
        );
        assert_eq!(imported_claims.iter().filter(|(_, claim)| matches!(claim.binding(), rootlight_ir::FSharpBinding::Container { path, roots }
            if path == &[content_hash(b"Imports")] && roots.contains(&content_hash(b"library")))).count(), usize::from(imports == shadowed));
        exact_source(&service, &source, "fsharp", imports.as_bytes());
        if let Some((previous, _, _, previous_source)) = generations.last() {
            let previous: &FirstSliceIndexReceipt = previous;
            let previous_source: &SourceRef = previous_source;
            assert_ne!(receipt.generation, previous.generation);
            assert_eq!(source.span().file(), previous_source.span().file());
            assert_ne!(source.content_hash(), previous_source.content_hash());
        }
        generations.push((receipt, imports, links, source));
    }
    let clean = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
    )
    .receipt()
    .clone();
    assert!(clean.logical_snapshot.is_some());
    assert_eq!(clean.logical_snapshot, generations[2].0.logical_snapshot);
    assert_qualified_module_generation(&service, &clean, &library, &consumer, &application, proved);
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(storage_policy, paths.state_dir(), &deadline())
            .unwrap();
    for (receipt, imports, links, source) in &generations {
        assert_qualified_module_generation(
            &reopened,
            receipt,
            &library,
            &consumer,
            &application,
            *links,
        );
        exact_source(&reopened, source, "fsharp", imports.as_bytes());
    }
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt().generation, clean.generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
}

fn assert_qualified_module_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    library: &str,
    consumer: &str,
    application: &str,
    expected_links: [Option<QualifiedLink>; 2],
) -> (BTreeMap<String, SymbolId>, SourceRef, SourceRef) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let mut files = BTreeMap::new();
    for (path, source) in [("Library.fs", library), ("Consumer.fs", consumer)] {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, "fsharp");
        assert_eq!(file.content_hash, content_hash(source.as_bytes()));
        let full = file.evidence.source.as_ref().unwrap();
        assert_eq!(full.repository(), receipt.repository);
        assert_eq!(full.generation(), receipt.generation);
        assert_eq!(full.content_hash(), file.content_hash);
        assert_eq!(
            full.span(),
            SourceSpan::new(file.id, 0, u64::try_from(source.len()).unwrap()).unwrap()
        );
        exact_source(service, full, "fsharp", source.as_bytes());
        files.insert(path, file);
    }
    assert_ne!(files["Library.fs"].id, files["Consumer.fs"].id);
    let reference = |path: &str, source: &str, written: &str| {
        let file = files[path];
        let start = source.find(written).unwrap();
        SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                file.id,
                u64::try_from(start).unwrap(),
                u64::try_from(start + written.len()).unwrap(),
            )
            .unwrap(),
            file.content_hash,
            None,
        )
    };
    let mut symbols = BTreeMap::new();
    for (path, source, name, kind) in [
        ("Library.fs", library, "offset", EntityKind::Variable),
        ("Library.fs", library, "carry", EntityKind::Function),
        ("Consumer.fs", consumer, "read", EntityKind::Function),
        ("Consumer.fs", consumer, "invoke", EntityKind::Function),
    ] {
        let entities = document
            .entities
            .iter()
            .filter(|entity| {
                entity.canonical_name == name
                    && entity.kind == kind
                    && entity.language == "fsharp"
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == files[path].id)
            })
            .collect::<Vec<_>>();
        assert_eq!(entities.len(), 1, "authored target {path}: {name}");
        let entity = entities[0];
        let authored = entity.evidence.source.as_ref().unwrap();
        assert_eq!(authored.repository(), receipt.repository);
        assert_eq!(authored.generation(), receipt.generation);
        assert_eq!(authored.content_hash(), files[path].content_hash);
        let written = source
            .as_bytes()
            .get(
                usize::try_from(authored.span().start_byte()).unwrap()
                    ..usize::try_from(authored.span().end_byte()).unwrap(),
            )
            .unwrap();
        exact_source(service, authored, "fsharp", written);
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1, "authored definition {name}");
        let mut line_start = 0;
        let definition_start = source
            .split_inclusive('\n')
            .find_map(|line| {
                let start = line_start;
                line_start += line.len();
                let authored = line.trim_start_matches([' ', '\t']);
                let indentation = line.len() - authored.len();
                ["let ", "let private "].into_iter().find_map(|prefix| {
                    authored
                        .strip_prefix(prefix)?
                        .strip_prefix(name)?
                        .starts_with(' ')
                        .then_some(start + indentation + prefix.len())
                })
            })
            .unwrap();
        assert_eq!(
            definitions[0].source,
            SourceRef::new(
                receipt.repository,
                receipt.generation,
                SourceSpan::new(
                    files[path].id,
                    u64::try_from(definition_start).unwrap(),
                    u64::try_from(definition_start + name.len()).unwrap(),
                )
                .unwrap(),
                files[path].content_hash,
                None,
            )
        );
        exact_source(service, &definitions[0].source, "fsharp", name.as_bytes());
        symbols.insert(name.to_owned(), entity.id);
    }
    let assert_outbound =
        |owner: SymbolId, target: SymbolId, family: RelationFamily, source: Option<&SourceRef>| {
            let result = service
                .symbol_relationships(
                    receipt.generation,
                    BTreeSet::from([owner]),
                    vec![family],
                    Some(RelationDirection::Outbound),
                    0,
                    16,
                    0,
                    &deadline(),
                )
                .unwrap();
            assert_eq!(result.data.generation, receipt.generation);
            assert!(!result.data.truncated);
            let mut found = Vec::new();
            for group in &result.data.groups {
                assert_eq!(group.seed, owner);
                assert_eq!(group.family, family);
                assert_eq!(group.direction, RelationDirection::Outbound);
                found.extend(group.items.iter().filter(|item| item.symbol == target));
            }
            assert_eq!(
                found.len(),
                usize::from(source.is_some()),
                "public {family:?}"
            );
            if let Some(source) = source {
                assert_eq!(
                    found[0].source_refs.as_slice(),
                    std::slice::from_ref(source)
                );
            }
            if family == RelationFamily::Calls {
                assert_eq!(result.data.total_edges, u32::from(source.is_some()));
                assert_eq!(result.data.returned_edges, u32::from(source.is_some()));
            }
        };
    let qualifier = application.rsplit_once('.').map(|(qualifier, _)| qualifier);
    for (owner, target, link) in [
        ("read", "offset", expected_links[0]),
        ("invoke", "carry", expected_links[1]),
    ] {
        let Some(link) = link else {
            continue;
        };
        let written = qualifier.map_or_else(
            || target.to_owned(),
            |qualifier| format!("{qualifier}.{target}"),
        );
        let expected = reference("Consumer.fs", consumer, &written);
        let reads = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference && site.source == expected)
            .collect::<Vec<_>>();
        if link == QualifiedLink::Closed {
            assert!(reads.iter().all(|read| read.target
                != (OccurrenceTarget::Resolved {
                    symbol: symbols[target]
                })));
            assert!(
                !document.relations.iter().any(|edge| {
                    edge.object == RelationEndpoint::Entity(symbols[target])
                        && match edge.subject {
                            RelationEndpoint::Entity(symbol) => symbol == symbols[owner],
                            RelationEndpoint::Occurrence(id) => {
                                document.occurrences.iter().any(|site| {
                                    site.id == id && site.enclosing == Some(symbols[owner])
                                })
                            }
                            _ => false,
                        }
                }),
                "closed qualified target {written}"
            );
            assert_outbound(
                symbols[owner],
                symbols[target],
                RelationFamily::References,
                None,
            );
            exact_source(service, &expected, "fsharp", written.as_bytes());
            continue;
        }
        assert_eq!(reads.len(), 1, "qualified reference {written}");
        let read = reads[0];
        assert_eq!(read.file, files["Consumer.fs"].id);
        assert_eq!(read.enclosing, Some(symbols[owner]));
        assert_eq!(
            read.target,
            OccurrenceTarget::Resolved {
                symbol: symbols[target]
            },
            "qualified target {written} in {consumer}"
        );
        exact_source(service, &read.source, "fsharp", written.as_bytes());
        let edges = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(read.id)
            })
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].object, RelationEndpoint::Entity(symbols[target]));
        assert_eq!(edges[0].evidence.source.as_ref(), Some(&expected));
        assert_outbound(
            symbols[owner],
            symbols[target],
            RelationFamily::References,
            Some(&expected),
        );
    }
    let application_source = reference("Consumer.fs", consumer, application);
    let calls = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Entity(symbols["invoke"])
        })
        .collect::<Vec<_>>();
    let resolved_call = expected_links[1] == Some(QualifiedLink::Call);
    assert_eq!(
        calls.len(),
        usize::from(resolved_call),
        "call edges: {:?}",
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .collect::<Vec<_>>()
    );
    if let Some(call) = calls.first() {
        assert_eq!(call.object, RelationEndpoint::Entity(symbols["carry"]));
        assert_eq!(call.evidence.source.as_ref(), Some(&application_source));
    }
    exact_source(
        service,
        &application_source,
        "fsharp",
        application.as_bytes(),
    );
    assert_outbound(
        symbols["invoke"],
        symbols["carry"],
        RelationFamily::Calls,
        resolved_call.then_some(&application_source),
    );
    (
        symbols,
        files["Library.fs"]
            .evidence
            .source
            .as_ref()
            .unwrap()
            .clone(),
        files["Consumer.fs"]
            .evidence
            .source
            .as_ref()
            .unwrap()
            .clone(),
    )
}

#[test]
fn fsharp_qualified_module_roots_do_not_override_value_shadowing() {
    for body in [
        "let invoke (Library: Receiver) input = Library.carry input\n",
        "let invoke input =\n    let Library = { carry = fun value -> value }\n    Library.carry input\n",
    ] {
        let consumer = format!(
            "module Consumer\ntype Receiver = {{ carry: int -> int }}\nlet read () = 0\n{body}"
        );
        let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        write_qualified_modules(repository.path(), QUALIFIED_LIBRARY, &consumer);
        let mut service = FirstSliceService::new(2).unwrap();
        let indexed = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        assert_qualified_module_generation(
            &service,
            indexed.receipt(),
            QUALIFIED_LIBRARY,
            &consumer,
            QUALIFIED_APPLICATION,
            [None, Some(QualifiedLink::Closed)],
        );
    }
}

#[test]
fn fsharp_qualified_module_references_do_not_prove_unsaturated_calls() {
    for (library, application) in [
        (
            QUALIFIED_LIBRARY.replace("carry value =", "carry value extra ="),
            QUALIFIED_APPLICATION,
        ),
        (QUALIFIED_LIBRARY.to_owned(), "Library.carry input input"),
    ] {
        let consumer = QUALIFIED_CONSUMER.replace(QUALIFIED_APPLICATION, application);
        let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        write_qualified_modules(repository.path(), &library, &consumer);
        let mut service = FirstSliceService::new(2).unwrap();
        let indexed = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        assert_qualified_module_generation(
            &service,
            indexed.receipt(),
            &library,
            &consumer,
            application,
            [
                Some(QualifiedLink::ReferenceOnly),
                Some(QualifiedLink::ReferenceOnly),
            ],
        );
    }
}

#[test]
fn fsharp_qualified_module_targets_follow_declared_signature_values() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let signature = "module Library\nval offset: int\nval carry: int -> int\n";
    write_qualified_modules(repository.path(), QUALIFIED_LIBRARY, QUALIFIED_CONSUMER);
    fs::write(repository.path().join("Library.fsi"), signature).unwrap();
    fs::write(
        repository.path().join("Sample.fsproj"),
        qualified_project(&["Library.fsi", "Library.fs", "Consumer.fs"]),
    )
    .unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let indexed = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
    );
    let receipt = indexed.receipt();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == "Library.fsi")
        .unwrap();
    assert_eq!(document.files.len(), 4);
    assert_eq!(file.language, "fsharp-signature");
    assert_eq!(file.content_hash, content_hash(signature.as_bytes()));
    exact_source(
        &service,
        file.evidence.source.as_ref().unwrap(),
        "fsharp-signature",
        signature.as_bytes(),
    );
    for name in ["offset", "carry"] {
        let entity = document
            .entities
            .iter()
            .find(|entity| {
                entity.canonical_name == name
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file.id)
            })
            .unwrap();
        assert_eq!(entity.kind, EntityKind::Variable);
        assert_eq!(entity.language, "fsharp-signature");
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        exact_source(
            &service,
            &definitions[0].source,
            "fsharp-signature",
            name.as_bytes(),
        );
    }
    assert_qualified_module_generation(
        &service,
        receipt,
        QUALIFIED_LIBRARY,
        QUALIFIED_CONSUMER,
        QUALIFIED_APPLICATION,
        [
            Some(QualifiedLink::ReferenceOnly),
            Some(QualifiedLink::Call),
        ],
    );
}

#[test]
fn fsharp_signature_nested_values_keep_implementation_targets() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let library = "module Library\nmodule Nested =\n    let offset = 7\n    let carry value = value + offset\n";
    let signature =
        "module Library\nmodule Nested =\n    val offset: int\n    val carry: int -> int\n";
    let consumer = QUALIFIED_CONSUMER.replace("Library.", "Library.Nested.");
    let application = QUALIFIED_APPLICATION.replace("Library.", "Library.Nested.");
    write_qualified_modules(repository.path(), library, &consumer);
    fs::write(repository.path().join("Library.fsi"), signature).unwrap();
    fs::write(
        repository.path().join("Sample.fsproj"),
        qualified_project(&["Library.fsi", "Library.fs", "Consumer.fs"]),
    )
    .unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let indexed = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
    );
    assert_qualified_module_generation(
        &service,
        indexed.receipt(),
        library,
        &consumer,
        &application,
        [
            Some(QualifiedLink::ReferenceOnly),
            Some(QualifiedLink::Call),
        ],
    );
}

#[test]
fn fsharp_signature_masks_imported_container_roots_without_losing_native_context() {
    let library = QUALIFIED_LIBRARY.replace("module Library", "module library");
    let consumer = QUALIFIED_CONSUMER
        .replace("Library.", "library.")
        .replace("module Consumer\n", "module Consumer\nopen Imports\n");
    let application = QUALIFIED_APPLICATION.replace("Library.", "library.");
    let imports = "module Imports\nmodule library =\n    let offset = 0\n    let carry value = value\nlet sentinel = 0\n";
    let hidden = "module Imports\nval sentinel: int\n";
    let exposed = "module Imports\nmodule library =\n    val offset: int\n    val carry: int -> int\nval sentinel: int\n";
    for (signature, links) in [
        (
            hidden,
            [
                Some(QualifiedLink::ReferenceOnly),
                Some(QualifiedLink::Call),
            ],
        ),
        (
            exposed,
            [Some(QualifiedLink::Closed), Some(QualifiedLink::Closed)],
        ),
    ] {
        let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        write_qualified_modules(repository.path(), &library, &consumer);
        fs::write(repository.path().join("Imports.fs"), imports).unwrap();
        fs::write(repository.path().join("Imports.fsi"), signature).unwrap();
        fs::write(
            repository.path().join("Sample.fsproj"),
            qualified_project(&["Imports.fsi", "Imports.fs", "Library.fs", "Consumer.fs"]),
        )
        .unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let indexed = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        let snapshot = service
            .loaded_generation_snapshot(indexed.receipt().generation)
            .unwrap();
        let file = snapshot
            .document()
            .files
            .iter()
            .find(|file| file.path == "Imports.fsi")
            .unwrap();
        exact_source(
            &service,
            file.evidence.source.as_ref().unwrap(),
            "fsharp-signature",
            signature.as_bytes(),
        );
        let claims = snapshot
            .document()
            .extensions
            .iter()
            .filter(|envelope| envelope.namespace == rootlight_ir::FSHARP_BINDING_NAMESPACE)
            .map(|envelope| rootlight_ir::decode_fsharp_binding_envelope(envelope).unwrap())
            .filter(|claim| claim.scope().file() == file.id)
            .collect::<Vec<_>>();
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(claim.binding(), rootlight_ir::FSharpBinding::Context))
                .count(),
            1
        );
        assert_eq!(claims.iter().filter(|claim| matches!(claim.binding(),
            rootlight_ir::FSharpBinding::Container { path, roots }
                if path == &[content_hash(b"Imports")] && roots.contains(&content_hash(b"library")))).count(),
            usize::from(signature == exposed));
        assert_qualified_module_generation(
            &service,
            indexed.receipt(),
            &library,
            &consumer,
            &application,
            links,
        );
    }
}

#[test]
fn fsharp_signature_surface_changes_rebind_unchanged_implementation_and_callers() {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let storage_policy = policy();
    let library = format!("{QUALIFIED_LIBRARY}let sentinel = 0\n");
    let exposed = "module Library\nval offset: int\nval carry: int -> int\nval sentinel: int\n";
    let hidden = "module Library\nval sentinel: int\n";
    write_qualified_modules(repository.path(), &library, QUALIFIED_CONSUMER);
    fs::write(
        repository.path().join("Sample.fsproj"),
        qualified_project(&["Library.fsi", "Library.fs", "Consumer.fs"]),
    )
    .unwrap();
    let mut service = FirstSliceService::new_durable_with_policy(
        storage_policy.clone(),
        paths.state_dir(),
        &deadline(),
    )
    .unwrap();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let proved = [
        Some(QualifiedLink::ReferenceOnly),
        Some(QualifiedLink::Call),
    ];
    let closed = [Some(QualifiedLink::Closed), Some(QualifiedLink::Closed)];
    let mut unchanged_identity = None;
    let mut generations = Vec::new();
    for (signature, links) in [(exposed, proved), (hidden, closed), (exposed, proved)] {
        fs::write(repository.path().join("Library.fsi"), signature).unwrap();
        let indexed = index(&mut service, repository.path(), options);
        let receipt = indexed.receipt().clone();
        if !generations.is_empty() {
            assert_eq!(indexed.evidence().rebuilt_files, 1);
        }
        let (symbols, implementation, caller) = assert_qualified_module_generation(
            &service,
            &receipt,
            &library,
            QUALIFIED_CONSUMER,
            QUALIFIED_APPLICATION,
            links,
        );
        let identity = (
            implementation.span().file(),
            implementation.content_hash(),
            caller.span().file(),
            caller.content_hash(),
            symbols["offset"],
            symbols["carry"],
        );
        if let Some(previous) = unchanged_identity {
            assert_eq!(identity, previous);
        } else {
            unchanged_identity = Some(identity);
        }
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let source = snapshot
            .document()
            .files
            .iter()
            .find(|file| file.path == "Library.fsi")
            .unwrap()
            .evidence
            .source
            .as_ref()
            .unwrap()
            .clone();
        let claims = snapshot
            .document()
            .extensions
            .iter()
            .filter(|envelope| envelope.namespace == rootlight_ir::FSHARP_BINDING_NAMESPACE)
            .map(|envelope| {
                (
                    envelope,
                    rootlight_ir::decode_fsharp_binding_envelope(envelope).unwrap(),
                )
            })
            .filter(|(_, claim)| claim.scope().file() == source.span().file())
            .collect::<Vec<_>>();
        assert_eq!(
            claims
                .iter()
                .filter(|(envelope, claim)| {
                    envelope.evidence.source.as_ref() == Some(&source)
                        && matches!(claim.binding(), rootlight_ir::FSharpBinding::Context)
                })
                .count(),
            1,
            "complete signature mask for {signature}"
        );
        assert_eq!(
            claims
                .iter()
                .filter(|(_, claim)| matches!(
                    claim.binding(),
                    rootlight_ir::FSharpBinding::SignatureValue { .. }
                ))
                .count(),
            if signature == hidden { 1 } else { 3 }
        );
        exact_source(&service, &source, "fsharp-signature", signature.as_bytes());
        if let Some((previous, _, _, previous_source)) = generations.last() {
            let previous: &FirstSliceIndexReceipt = previous;
            let previous_source: &SourceRef = previous_source;
            assert_ne!(receipt.generation, previous.generation);
            assert_eq!(source.span().file(), previous_source.span().file());
            assert_ne!(source.content_hash(), previous_source.content_hash());
        }
        generations.push((receipt, signature, links, source));
    }
    let clean = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
    )
    .receipt()
    .clone();
    assert!(clean.logical_snapshot.is_some());
    assert_eq!(clean.logical_snapshot, generations[2].0.logical_snapshot);
    assert_qualified_module_generation(
        &service,
        &clean,
        &library,
        QUALIFIED_CONSUMER,
        QUALIFIED_APPLICATION,
        proved,
    );
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(storage_policy, paths.state_dir(), &deadline())
            .unwrap();
    for (receipt, signature, links, source) in &generations {
        assert_qualified_module_generation(
            &reopened,
            receipt,
            &library,
            QUALIFIED_CONSUMER,
            QUALIFIED_APPLICATION,
            *links,
        );
        exact_source(&reopened, source, "fsharp-signature", signature.as_bytes());
    }
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt().generation, clean.generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
}

#[test]
fn fsharp_qualified_module_targets_require_project_order_and_visibility() {
    for case in ["reversed", "missing", "unrelated", "private", "signature"] {
        let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let library = match case {
            "missing" => QUALIFIED_LIBRARY.replace("module Library", "module Different"),
            "private" => QUALIFIED_LIBRARY
                .replace("let offset", "let private offset")
                .replace("let carry", "let private carry"),
            "signature" => format!("{QUALIFIED_LIBRARY}let sentinel = 0\n"),
            _ => QUALIFIED_LIBRARY.to_owned(),
        };
        write_qualified_modules(repository.path(), &library, QUALIFIED_CONSUMER);
        match case {
            "reversed" => fs::write(
                repository.path().join("Sample.fsproj"),
                qualified_project(&["Consumer.fs", "Library.fs"]),
            )
            .unwrap(),
            "unrelated" => {
                fs::write(
                    repository.path().join("Sample.fsproj"),
                    qualified_project(&["Library.fs"]),
                )
                .unwrap();
                fs::write(
                    repository.path().join("Consumer.fsproj"),
                    qualified_project(&["Consumer.fs"]),
                )
                .unwrap();
            }
            "signature" => {
                fs::write(
                    repository.path().join("Library.fsi"),
                    "module Library\nval sentinel: int\n",
                )
                .unwrap();
                fs::write(
                    repository.path().join("Sample.fsproj"),
                    qualified_project(&["Library.fsi", "Library.fs", "Consumer.fs"]),
                )
                .unwrap();
            }
            _ => {}
        }
        let mut service = FirstSliceService::new(2).unwrap();
        let indexed = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        assert_qualified_module_generation(
            &service,
            indexed.receipt(),
            &library,
            QUALIFIED_CONSUMER,
            QUALIFIED_APPLICATION,
            [Some(QualifiedLink::Closed), Some(QualifiedLink::Closed)],
        );
    }
}

#[test]
fn fsharp_qualified_module_record_contexts_follow_durable_root_shadowing() {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let storage_policy = policy();
    let consumer = QUALIFIED_CONSUMER.replace(
        "module Consumer\n",
        "module Consumer\ntype Receiver = { carry: int -> int }\n",
    );
    let shadowed = consumer.replace(
        "let invoke input = Library.carry input",
        "let invoke input =\n    let Library = { carry = fun value -> value }\n    Library.carry input",
    );
    write_qualified_modules(repository.path(), QUALIFIED_LIBRARY, &consumer);
    let mut service = FirstSliceService::new_durable_with_policy(
        storage_policy.clone(),
        paths.state_dir(),
        &deadline(),
    )
    .unwrap();
    let assert_record =
        |service: &FirstSliceService, receipt: &FirstSliceIndexReceipt, source: &SourceRef| {
            let snapshot = service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap();
            let document = snapshot.document();
            let records =
                document
                    .entities
                    .iter()
                    .filter(|entity| {
                        entity.kind == EntityKind::Struct
                            && entity.canonical_name == "Receiver"
                            && entity.evidence.source.as_ref().is_some_and(|authored| {
                                authored.span().file() == source.span().file()
                            })
                    })
                    .collect::<Vec<_>>();
            assert_eq!(records.len(), 1);
            let definitions = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target
                            == (OccurrenceTarget::Resolved {
                                symbol: records[0].id,
                            })
                })
                .collect::<Vec<_>>();
            assert_eq!(definitions.len(), 1);
            let start = consumer.find("Receiver").unwrap();
            assert_eq!(
                definitions[0].source,
                SourceRef::new(
                    receipt.repository,
                    receipt.generation,
                    SourceSpan::new(
                        source.span().file(),
                        u64::try_from(start).unwrap(),
                        u64::try_from(start + "Receiver".len()).unwrap(),
                    )
                    .unwrap(),
                    source.content_hash(),
                    None,
                )
            );
            exact_source(service, &definitions[0].source, "fsharp", b"Receiver");
        };
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let proved = [
        Some(QualifiedLink::ReferenceOnly),
        Some(QualifiedLink::Call),
    ];
    let shadowed_links = [
        Some(QualifiedLink::ReferenceOnly),
        Some(QualifiedLink::Closed),
    ];
    let mut generations: Vec<(
        FirstSliceIndexReceipt,
        &str,
        [Option<QualifiedLink>; 2],
        SourceRef,
    )> = Vec::new();
    let mut library_identity = None;
    for (text, links) in [
        (consumer.as_str(), proved),
        (shadowed.as_str(), shadowed_links),
        (consumer.as_str(), proved),
    ] {
        fs::write(repository.path().join("Consumer.fs"), text).unwrap();
        let receipt = index(&mut service, repository.path(), options)
            .receipt()
            .clone();
        let (symbols, library_source, consumer_source) = assert_qualified_module_generation(
            &service,
            &receipt,
            QUALIFIED_LIBRARY,
            text,
            QUALIFIED_APPLICATION,
            links,
        );
        let current_identity = (
            library_source.span().file(),
            library_source.content_hash(),
            symbols["offset"],
            symbols["carry"],
        );
        if let Some(previous) = library_identity {
            assert_eq!(current_identity, previous);
        } else {
            library_identity = Some(current_identity);
        }
        assert_record(&service, &receipt, &consumer_source);
        if let Some((previous, _, _, previous_source)) = generations.last() {
            assert_ne!(receipt.generation, previous.generation);
            assert_eq!(consumer_source.span().file(), previous_source.span().file());
            assert_ne!(
                consumer_source.content_hash(),
                previous_source.content_hash()
            );
        }
        generations.push((receipt, text, links, consumer_source));
    }
    let restored = &generations[2].0;
    let clean = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
    )
    .receipt()
    .clone();
    assert!(restored.logical_snapshot.is_some());
    assert_eq!(clean.logical_snapshot, restored.logical_snapshot);
    let (_, _, clean_source) = assert_qualified_module_generation(
        &service,
        &clean,
        QUALIFIED_LIBRARY,
        &consumer,
        QUALIFIED_APPLICATION,
        proved,
    );
    assert_record(&service, &clean, &clean_source);
    for (_, text, _, source) in &generations {
        exact_source(&service, source, "fsharp", text.as_bytes());
    }
    assert!(
        service
            .source_read_with_options_and_budget(
                generations[1].0.generation,
                vec![generations[0].3.clone()],
                SourceReadOptions::new(),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .is_err()
    );
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(storage_policy, paths.state_dir(), &deadline())
            .unwrap();
    generations.push((clean.clone(), consumer.as_str(), proved, clean_source));
    for (receipt, text, links, source) in &generations {
        assert_qualified_module_generation(
            &reopened,
            receipt,
            QUALIFIED_LIBRARY,
            text,
            QUALIFIED_APPLICATION,
            *links,
        );
        assert_record(&reopened, receipt, source);
        exact_source(&reopened, source, "fsharp", text.as_bytes());
    }
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt().generation, clean.generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
}

#[test]
fn fsharp_qualified_module_links_follow_durable_source_and_project_changes() {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(
        Some("version='1.3'\n[storage]\nretained_generations=8\n"),
        8,
    )
    .unwrap();
    write_qualified_modules(repository.path(), QUALIFIED_LIBRARY, QUALIFIED_CONSUMER);
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let proved = [
        Some(QualifiedLink::ReferenceOnly),
        Some(QualifiedLink::Call),
    ];
    let reference_only = [
        Some(QualifiedLink::ReferenceOnly),
        Some(QualifiedLink::ReferenceOnly),
    ];
    let closed = [Some(QualifiedLink::Closed), Some(QualifiedLink::Closed)];
    let initial = index(&mut service, repository.path(), options)
        .receipt()
        .clone();
    let (symbols, original_library, original_consumer) = assert_qualified_module_generation(
        &service,
        &initial,
        QUALIFIED_LIBRARY,
        QUALIFIED_CONSUMER,
        QUALIFIED_APPLICATION,
        proved,
    );
    let noop = index(&mut service, repository.path(), options);
    assert_eq!(noop.receipt(), &initial);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    let consumer = QUALIFIED_CONSUMER.replace("= Library.offset", "= Library.offset + 1");
    fs::write(repository.path().join("Consumer.fs"), &consumer).unwrap();
    let body = index(&mut service, repository.path(), options)
        .receipt()
        .clone();
    assert_ne!(body.generation, initial.generation);
    let (body_symbols, _, body_source) = assert_qualified_module_generation(
        &service,
        &body,
        QUALIFIED_LIBRARY,
        &consumer,
        QUALIFIED_APPLICATION,
        proved,
    );
    assert_eq!(body_symbols, symbols);
    assert_eq!(body_source.span().file(), original_consumer.span().file());
    assert_ne!(body_source.content_hash(), original_consumer.content_hash());
    let library = QUALIFIED_LIBRARY.replace("carry value =", "carry value extra =");
    fs::write(repository.path().join("Library.fs"), &library).unwrap();
    let arity = index(&mut service, repository.path(), options)
        .receipt()
        .clone();
    assert_ne!(arity.generation, body.generation);
    let (_, arity_source, retained_consumer) = assert_qualified_module_generation(
        &service,
        &arity,
        &library,
        &consumer,
        QUALIFIED_APPLICATION,
        reference_only,
    );
    assert_eq!(retained_consumer.content_hash(), body_source.content_hash());
    assert_eq!(arity_source.span().file(), original_library.span().file());
    assert_ne!(arity_source.content_hash(), original_library.content_hash());
    fs::write(
        repository.path().join("Sample.fsproj"),
        qualified_project(&["Consumer.fs", "Library.fs"]),
    )
    .unwrap();
    let reordered = index(&mut service, repository.path(), options)
        .receipt()
        .clone();
    assert_ne!(reordered.generation, arity.generation);
    let reordered_sources = assert_qualified_module_generation(
        &service,
        &reordered,
        &library,
        &consumer,
        QUALIFIED_APPLICATION,
        closed,
    );
    assert_eq!(
        reordered_sources.1.content_hash(),
        arity_source.content_hash()
    );
    assert_eq!(
        reordered_sources.2.content_hash(),
        body_source.content_hash()
    );
    let clean = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
    )
    .receipt()
    .clone();
    assert!(reordered.logical_snapshot.is_some());
    assert_eq!(clean.logical_snapshot, reordered.logical_snapshot);
    assert_qualified_module_generation(
        &service,
        &clean,
        &library,
        &consumer,
        QUALIFIED_APPLICATION,
        closed,
    );
    exact_source(
        &service,
        &original_library,
        "fsharp",
        QUALIFIED_LIBRARY.as_bytes(),
    );
    exact_source(
        &service,
        &original_consumer,
        "fsharp",
        QUALIFIED_CONSUMER.as_bytes(),
    );
    assert!(
        service
            .source_read_with_options_and_budget(
                body.generation,
                vec![original_consumer.clone()],
                SourceReadOptions::new(),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .is_err()
    );
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    for (receipt, library, consumer, links) in [
        (&initial, QUALIFIED_LIBRARY, QUALIFIED_CONSUMER, proved),
        (&body, QUALIFIED_LIBRARY, consumer.as_str(), proved),
        (&arity, library.as_str(), consumer.as_str(), reference_only),
        (&reordered, library.as_str(), consumer.as_str(), closed),
        (&clean, library.as_str(), consumer.as_str(), closed),
    ] {
        assert_qualified_module_generation(
            &reopened,
            receipt,
            library,
            consumer,
            QUALIFIED_APPLICATION,
            links,
        );
    }
    exact_source(
        &reopened,
        &original_library,
        "fsharp",
        QUALIFIED_LIBRARY.as_bytes(),
    );
    exact_source(
        &reopened,
        &original_consumer,
        "fsharp",
        QUALIFIED_CONSUMER.as_bytes(),
    );
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt().generation, clean.generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
}

#[test]
fn fsharp_owned_pattern_annotations_preserve_neighbor_queries_after_reopen() {
    let prefix = "// ☃ 🧭\nmodule Ownership\ntype Parcel = Parcel of content: int\nlet echo (value: int) = value\nlet unwrap\n    (Parcel(content = content))\n    : int =\n    content\n";
    // A complete short source does not exercise the truncated-prefix conflict
    // veto; the owned annotation must precede the actual classification bound.
    let source = format!("{prefix}//{}\n", " ".repeat(8_192));
    let changed_source = source.replace("= value\n", "= value + 1\n");
    assert_eq!(source.matches("= value\n").count(), 1);
    let mixed_source = format!("{prefix}: duplicate dup ;\n//{}\n", " ".repeat(8_192));
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let encode = |text: &str| {
            let mut bytes = if encoding == "utf-8" {
                Vec::new()
            } else {
                typed_formal_leaf_bytes("\u{feff}", encoding)
            };
            bytes.extend(typed_formal_leaf_bytes(text, encoding));
            bytes
        };
        let bytes = encode(&source);
        let changed_bytes = encode(&changed_source);
        let mixed_bytes = encode(&mixed_source);
        assert!(bytes.len() > rootlight_discovery::MAX_CLASSIFICATION_BYTES);
        assert!(changed_bytes.len() > rootlight_discovery::MAX_CLASSIFICATION_BYTES);
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let path = repository.path().join("Owned.fs");
        fs::write(&path, &bytes).unwrap();
        fs::write(repository.path().join("Mixed.fs"), &mixed_bytes).unwrap();
        fs::write(repository.path().join("Binary.fs"), b"\0binary").unwrap();
        let policy = FirstSliceStoragePolicy::resolve_user_config(
            Some("version='1.3'\n[storage]\nretained_generations=2\n"),
            2,
        )
        .unwrap();
        let mut service = FirstSliceService::new_durable_with_policy(
            policy.clone(),
            paths.state_dir(),
            &deadline(),
        )
        .unwrap();
        let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
        let cold = index(&mut service, repository.path(), options);
        assert_eq!(cold.evidence().rebuilt_files, 1, "{encoding}");
        let initial = cold.receipt().clone();
        assert_eq!(initial.indexed_files, 2);
        assert_eq!(initial.binary_inputs, 1);
        assert!(initial.discovery_complete);
        let root = RepositoryRoot::open(initial.repository, repository.path()).unwrap();
        let manifest = rootlight_discovery::discover(
            &root,
            &service.config,
            &DiscoveryPolicy::build(Vec::new(), false).unwrap(),
            DiscoveryLimits::from_config(&service.config),
            &deadline(),
        )
        .unwrap();
        let input = manifest
            .inputs
            .iter()
            .find(|input| input.path == "Owned.fs")
            .unwrap();
        assert_eq!(input.content_hash, content_hash(&bytes));
        assert_eq!(input.encoding, encoding);
        assert_eq!(
            input.language_signals,
            vec![rootlight_discovery::LanguageSignal {
                language: "fsharp".to_owned(),
                evidence: rootlight_discovery::LanguageEvidence::Content,
            }]
        );
        assert!(input.project_language_witnesses.is_empty());
        let (identities, original) =
            assert_owned_annotation_neighbor(&service, &initial, &source, &bytes, encoding);
        assert_project_member(
            &service,
            &initial,
            "Mixed.fs",
            &mixed_bytes,
            "unknown",
            None,
        );
        let noop = index(&mut service, repository.path(), options);
        assert_eq!(noop.receipt(), &initial);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);

        fs::write(&path, &changed_bytes).unwrap();
        let mutation = index(&mut service, repository.path(), options);
        assert_eq!(mutation.evidence().changed_files, 1);
        assert_eq!(mutation.evidence().rebuilt_files, 1);
        let changed = mutation.receipt().clone();
        assert_eq!(changed.parent, Some(initial.generation));
        let (changed_identities, updated) = assert_owned_annotation_neighbor(
            &service,
            &changed,
            &changed_source,
            &changed_bytes,
            encoding,
        );
        assert_eq!(changed_identities, identities);
        assert_eq!(updated.span().file(), original.span().file());
        assert_ne!(updated.content_hash(), original.content_hash());
        exact_source(&service, &original, "fsharp", &bytes);
        assert!(
            service
                .source_read_with_options_and_budget(
                    changed.generation,
                    vec![original.clone()],
                    SourceReadOptions::new(),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .is_err()
        );
        drop(service);
        let mut reopened =
            FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline())
                .unwrap();
        assert_eq!(
            assert_owned_annotation_neighbor(&reopened, &initial, &source, &bytes, encoding,).0,
            identities
        );
        assert_eq!(
            assert_owned_annotation_neighbor(
                &reopened,
                &changed,
                &changed_source,
                &changed_bytes,
                encoding,
            )
            .0,
            identities
        );
        assert_project_member(
            &reopened,
            &changed,
            "Mixed.fs",
            &mixed_bytes,
            "unknown",
            None,
        );
        exact_source(&reopened, &original, "fsharp", &bytes);
        let noop = index(&mut reopened, repository.path(), options);
        assert_eq!(noop.receipt(), &changed);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
    }
}

fn assert_owned_annotation_neighbor(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    bytes: &[u8],
    encoding: &str,
) -> (BTreeSet<SymbolId>, SourceRef) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == "Owned.fs")
        .unwrap();
    assert_eq!(
        (file.language.as_str(), file.encoding.as_str()),
        ("fsharp", encoding)
    );
    assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(bytes));
    assert!(!document.files.iter().any(|file| file.path == "Binary.fs"));
    let reference = |start: usize, written: &str| {
        assert_eq!(source.get(start..start + written.len()), Some(written));
        let start = typed_formal_leaf_bytes(source.get(..start).unwrap(), encoding).len()
            + usize::from(encoding != "utf-8") * 2;
        SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                file.id,
                u64::try_from(start).unwrap(),
                u64::try_from(start + typed_formal_leaf_bytes(written, encoding).len()).unwrap(),
            )
            .unwrap(),
            file.content_hash,
            None,
        )
    };
    let check_source = |reference: &SourceRef| {
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), file.content_hash);
        exact_source(
            service,
            reference,
            "fsharp",
            bytes
                .get(
                    usize::try_from(reference.span().start_byte()).unwrap()
                        ..usize::try_from(reference.span().end_byte()).unwrap(),
                )
                .unwrap(),
        );
    };
    let full = file.evidence.source.as_ref().unwrap();
    assert_eq!(
        full.span(),
        SourceSpan::new(file.id, 0, file.byte_length).unwrap()
    );
    check_source(full);
    let mut symbols = BTreeMap::new();
    for (name, kind, prefix) in [
        ("echo", EntityKind::Function, "let "),
        ("value", EntityKind::Parameter, "let echo ("),
    ] {
        let entities = document
            .entities
            .iter()
            .filter(|entity| {
                entity.canonical_name == name
                    && entity.kind == kind
                    && entity.language == "fsharp"
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file.id)
            })
            .collect::<Vec<_>>();
        assert_eq!(entities.len(), 1, "authored neighbor {name}");
        let entity = entities[0];
        symbols.insert(name, entity.id);
        check_source(entity.evidence.source.as_ref().unwrap());
        let explained = service
            .symbol_explain(receipt.generation, entity.id, &deadline())
            .unwrap();
        assert_eq!(explained.data.generation, receipt.generation);
        let definitions = explained
            .data
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        assert_eq!(
            definitions[0].source,
            reference(
                source.find(&format!("{prefix}{name}")).unwrap() + prefix.len(),
                name
            )
        );
        check_source(&definitions[0].source);
        let located = service
            .code_locate_with_filters_and_budget(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                vec!["fsharp".to_owned()],
                vec!["Owned.fs".to_owned()],
                8,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(located.data.generation, receipt.generation);
        let hits = located
            .data
            .hits
            .iter()
            .filter(|hit| hit.file == file.id && hit.symbol == Some(entity.id))
            .collect::<Vec<_>>();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source, explained.data.entity.evidence.source);
        check_source(hits[0].source.as_ref().unwrap());
        for coverage in [&located.data.coverage, &explained.data.coverage] {
            assert!(
                coverage
                    .iter()
                    .all(|record| record.repository == receipt.repository
                        && record.generation == receipt.generation
                        && !matches!(record.scope, CoverageScope::File(other) if other != file.id))
            );
            assert!(
                coverage
                    .iter()
                    .any(|record| record.scope == CoverageScope::File(file.id)
                        && record.status != CoverageStatus::Complete)
            );
        }
    }
    let parameter = document
        .entities
        .iter()
        .find(|entity| entity.id == symbols["value"])
        .unwrap();
    assert_eq!(
        parameter.container,
        Some(ContainerRef::Entity(symbols["echo"]))
    );
    assert!(
        !document.entities.iter().any(
            |entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "content"
        )
    );
    let read_source = reference(
        source.find("let echo (value: int) = ").unwrap() + "let echo (value: int) = ".len(),
        "value",
    );
    let reads = document
        .occurrences
        .iter()
        .filter(|site| {
            site.enclosing == Some(symbols["echo"]) && site.role == OccurrenceRole::Reference
        })
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].source, read_source);
    assert_eq!(
        reads[0].target,
        OccurrenceTarget::Resolved {
            symbol: symbols["value"]
        }
    );
    check_source(&read_source);
    let edges = document
        .relations
        .iter()
        .filter(|edge| edge.subject == RelationEndpoint::Occurrence(reads[0].id))
        .collect::<Vec<_>>();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].predicate, RelationPredicate::RefersTo);
    assert_eq!(edges[0].object, RelationEndpoint::Entity(symbols["value"]));
    assert_eq!(edges[0].evidence.source.as_ref(), Some(&read_source));
    let relationships = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([symbols["echo"]]),
            vec![RelationFamily::References],
            Some(RelationDirection::Outbound),
            0,
            8,
            0,
            &deadline(),
        )
        .unwrap();
    assert_eq!(relationships.data.generation, receipt.generation);
    assert!(!relationships.data.truncated);
    assert!(!relationships.data.exact);
    assert_eq!(relationships.data.returned_edges, 1);
    assert_eq!(relationships.data.total_edges, 1);
    let items = relationships
        .data
        .groups
        .iter()
        .flat_map(|group| &group.items)
        .collect::<Vec<_>>();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].symbol, symbols["value"]);
    assert_eq!(items[0].source_refs, vec![read_source]);
    let pattern = "(Parcel(content = content))";
    let pattern_source = reference(source.find(pattern).unwrap(), pattern);
    let parameter_gaps = document
        .skipped_regions
        .iter()
        .filter(|gap| {
            gap.domain == IrFactDomain::Entities
                && gap.source == pattern_source
                && gap.detail == "fsharp-declaration-shape-unsupported"
        })
        .collect::<Vec<_>>();
    assert_eq!(parameter_gaps.len(), 1);
    assert_eq!(
        parameter_gaps[0].reason,
        SkippedRegionReason::UnsupportedConstruct
    );
    assert_eq!(
        parameter_gaps[0].evidence.source.as_ref(),
        Some(&pattern_source)
    );
    check_source(&pattern_source);
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.source.span().file() == file.id
                && matches!(
                    gap.reason,
                    SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
                ))
    );
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(
        (coverage.language.as_str(), coverage.tier, coverage.status),
        ("fsharp", AnalysisTier::TierD, CoverageStatus::Bounded)
    );
    assert_eq!(coverage.reason, None);
    (symbols.values().copied().collect(), full.clone())
}

#[test]
fn fsharp_shared_suffix_keeps_unproven_source_and_rejects_binary_input() {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(current).unwrap();
    let source = ": square dup * ;\n// module Sample.Root\n// let value = 1\n";
    fs::write(repository.path().join("Library.fs"), source).unwrap();
    fs::write(repository.path().join("binary.fsx"), b"\0binary").unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(receipt.indexed_files, 1);
    assert_eq!(receipt.binary_inputs, 1);
    let status = service.repository_status(receipt.repository, None).unwrap();
    assert!(status.coverage.iter().all(|entry| entry.indexed_files == 0));
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 1);
    assert!(document.entities.is_empty());
    assert!(document.files.iter().all(|file| file.path != "binary.fsx"));
    let file = &document.files[0];
    assert_eq!(file.path, "Library.fs");
    assert!(!matches!(
        file.language.as_str(),
        "fsharp" | "fsharp-signature"
    ));
    assert_eq!(file.encoding, "utf-8");
    let provenance = document
        .provenance
        .iter()
        .find(|record| record.id == file.provenance)
        .unwrap();
    assert_eq!(provenance.producer_kind, ProducerKind::Rule);
    assert_eq!(provenance.language, file.language);
    assert_eq!(provenance.tier, AnalysisTier::TierD);
    assert_eq!(provenance.rule.as_deref(), Some("unsupported-language"));
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(coverage.language, file.language);
    assert_eq!(coverage.tier, AnalysisTier::TierD);
    assert_eq!(coverage.status, CoverageStatus::Unknown);
    assert_eq!(
        coverage.reason,
        Some(FirstSliceSourceCoverageReason::UnsupportedLanguage)
    );
    assert!(document.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "unsupported-language"
            && diagnostic.provenance == file.provenance
            && diagnostic.source.as_ref() == file.evidence.source.as_ref()
            && diagnostic.coverage_effect == CoverageStatus::Unknown
    }));
    exact_source(
        &service,
        file.evidence.source.as_ref().unwrap(),
        &file.language,
        source.as_bytes(),
    );
}

#[test]
fn fsharp_conditional_scope_routes_preserve_authored_queries_after_reopen() {
    let prefix = "// ☃ 🧭\n#if FEATURE\nnamespace Variant.Primary\n#else\nnamespace Variant.Secondary\n#endif\nmodule Helpers =\n    type Entry = { Count: int }\n    let seed = 1\n    let echo (value: int) = value\n";
    let source = format!(
        "{prefix}//{}\n",
        " ".repeat(rootlight_discovery::MAX_CLASSIFICATION_BYTES)
    );
    let malformed = source.replace(
        "namespace Variant.Secondary",
        "namespace Variant..Secondary",
    );
    let mixed = format!(
        "{prefix}: duplicate dup ;\n//{}\n",
        " ".repeat(rootlight_discovery::MAX_CLASSIFICATION_BYTES)
    );
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let encode = |text: &str| {
            let mut bytes = if encoding == "utf-8" {
                Vec::new()
            } else {
                typed_formal_leaf_bytes("\u{feff}", encoding)
            };
            bytes.extend(typed_formal_leaf_bytes(text, encoding));
            bytes
        };
        let bytes = encode(&source);
        let malformed_bytes = encode(&malformed);
        let mixed_bytes = encode(&mixed);
        assert!(bytes.len() > rootlight_discovery::MAX_CLASSIFICATION_BYTES);
        let current = std::env::current_dir().unwrap();
        let control_repository = tempfile::tempdir_in(&current).unwrap();
        fs::write(control_repository.path().join("Scoped.fs"), &bytes).unwrap();
        let mut configured = FirstSliceService::new_with_storage_policy(
            2,
            MAX_RETAINED_SOURCE_BYTES,
            None,
            None,
            policy(),
        )
        .unwrap();
        let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
        let control = index(&mut configured, control_repository.path(), options);
        assert_eq!(control.evidence().rebuilt_files, 1);
        let control_proof = assert_conditional_scope_neighbors(
            &configured,
            control.receipt(),
            &source,
            &bytes,
            encoding,
        );
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        for (path, raw) in [
            ("Scoped.fs", bytes.as_slice()),
            ("Malformed.fs", malformed_bytes.as_slice()),
            ("Mixed.fs", mixed_bytes.as_slice()),
            ("Binary.fs", b"\0binary".as_slice()),
        ] {
            fs::write(repository.path().join(path), raw).unwrap();
        }
        let default_policy = FirstSliceStoragePolicy::resolve_user_config(
            Some("version='1.3'\n[storage]\nretained_generations=2\n"),
            2,
        )
        .unwrap();
        let mut service = FirstSliceService::new_durable_with_policy(
            default_policy.clone(),
            paths.state_dir(),
            &deadline(),
        )
        .unwrap();
        let cold = index(&mut service, repository.path(), options);
        assert_eq!(cold.evidence().rebuilt_files, 1, "{encoding}");
        let receipt = cold.receipt().clone();
        assert_eq!(receipt.indexed_files, 3);
        assert_eq!(receipt.binary_inputs, 1);
        assert!(receipt.discovery_complete);
        let root = RepositoryRoot::open(receipt.repository, repository.path()).unwrap();
        let manifest = rootlight_discovery::discover(
            &root,
            &service.config,
            &DiscoveryPolicy::build(Vec::new(), false).unwrap(),
            DiscoveryLimits::from_config(&service.config),
            &deadline(),
        )
        .unwrap();
        for (path, raw, recognized) in [
            ("Scoped.fs", bytes.as_slice(), true),
            ("Malformed.fs", malformed_bytes.as_slice(), false),
            ("Mixed.fs", mixed_bytes.as_slice(), false),
        ] {
            let input = manifest
                .inputs
                .iter()
                .find(|input| input.path == path)
                .unwrap();
            assert_eq!(input.encoding, encoding);
            assert_eq!(input.content_hash, content_hash(raw));
            assert_eq!(input.bytes, u64::try_from(raw.len()).unwrap());
            assert!(input.project_language_witnesses.is_empty());
            assert_eq!(
                input.language_signals,
                if recognized {
                    vec![rootlight_discovery::LanguageSignal {
                        language: "fsharp".to_owned(),
                        evidence: rootlight_discovery::LanguageEvidence::Content,
                    }]
                } else {
                    Vec::new()
                }
            );
        }
        let proof =
            assert_conditional_scope_neighbors(&service, &receipt, &source, &bytes, encoding);
        assert_eq!(proof.coverage, control_proof.coverage);
        assert_eq!(proof.warnings, control_proof.warnings);
        for (path, raw) in [
            ("Malformed.fs", malformed_bytes.as_slice()),
            ("Mixed.fs", mixed_bytes.as_slice()),
        ] {
            assert_project_member(&service, &receipt, path, raw, "unknown", None);
        }
        let noop = index(&mut service, repository.path(), options);
        assert_eq!(noop.receipt(), &receipt);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
        drop(service);
        let mut reopened = FirstSliceService::new_durable_with_policy(
            default_policy,
            paths.state_dir(),
            &deadline(),
        )
        .unwrap();
        let restored =
            assert_conditional_scope_neighbors(&reopened, &receipt, &source, &bytes, encoding);
        assert_eq!(restored.symbols, proof.symbols);
        assert_eq!(restored.coverage, proof.coverage);
        assert_eq!(restored.warnings, proof.warnings);
        for (path, raw) in [
            ("Malformed.fs", malformed_bytes.as_slice()),
            ("Mixed.fs", mixed_bytes.as_slice()),
        ] {
            assert_project_member(&reopened, &receipt, path, raw, "unknown", None);
        }
        let noop = index(&mut reopened, repository.path(), options);
        assert_eq!(noop.receipt(), &receipt);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
    }
}

struct ConditionalScopeProof {
    symbols: BTreeMap<String, SymbolId>,
    coverage: FirstSliceSourceCoverage,
    warnings: Vec<String>,
}

fn assert_conditional_scope_neighbors(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    bytes: &[u8],
    encoding: &str,
) -> ConditionalScopeProof {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let files = document
        .files
        .iter()
        .filter(|file| file.path == "Scoped.fs")
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 1);
    let file = files[0];
    assert_eq!(
        (file.language.as_str(), file.encoding.as_str()),
        ("fsharp", encoding)
    );
    assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(bytes));
    let reference = |start: usize, written: &str| {
        assert_eq!(source.get(start..start + written.len()), Some(written));
        let start = typed_formal_leaf_bytes(source.get(..start).unwrap(), encoding).len()
            + usize::from(encoding != "utf-8") * 2;
        SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                file.id,
                u64::try_from(start).unwrap(),
                u64::try_from(start + typed_formal_leaf_bytes(written, encoding).len()).unwrap(),
            )
            .unwrap(),
            file.content_hash,
            None,
        )
    };
    let check_source = |reference: &SourceRef| {
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), file.content_hash);
        let raw = bytes
            .get(
                usize::try_from(reference.span().start_byte()).unwrap()
                    ..usize::try_from(reference.span().end_byte()).unwrap(),
            )
            .unwrap();
        exact_source(service, reference, "fsharp", raw);
    };
    let full = file.evidence.source.as_ref().unwrap();
    assert_eq!(
        full.span(),
        SourceSpan::new(file.id, 0, file.byte_length).unwrap()
    );
    check_source(full);
    let mut symbols = BTreeMap::new();
    for (name, kind, declaration, offset) in [
        ("Entry", EntityKind::Struct, "Entry = { Count: int }", 0),
        ("seed", EntityKind::Variable, "seed = 1", 0),
        ("echo", EntityKind::Function, "echo (value: int) = value", 0),
        ("value", EntityKind::Parameter, "(value: int)", 1),
    ] {
        let start = source.find(declaration).unwrap();
        let definition = reference(start + offset, name);
        let entities = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == kind
                    && entity.canonical_name == name
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|evidence| evidence.span().file() == file.id)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            entities.len(),
            1,
            "configured/default authored neighbor {name}"
        );
        let entity = entities[0];
        let declared = if kind == EntityKind::Parameter {
            definition.clone()
        } else {
            reference(start, declaration)
        };
        assert_eq!(entity.evidence.source.as_ref(), Some(&declared));
        check_source(&declared);
        symbols.insert(name.to_owned(), entity.id);
        let explained = service
            .symbol_explain(receipt.generation, entity.id, &deadline())
            .unwrap();
        assert_eq!(explained.data.generation, receipt.generation);
        assert_eq!(explained.data.entity.id, entity.id);
        assert_eq!(
            explained.data.entity.evidence.source.as_ref(),
            Some(&declared)
        );
        let definitions = explained
            .data
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1, "{name}");
        assert_eq!(definitions[0].source, definition);
        check_source(&definition);
        let located = service
            .code_locate_with_filters_and_budget(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                vec!["fsharp".to_owned()],
                vec!["Scoped.fs".to_owned()],
                8,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(located.data.generation, receipt.generation);
        let hits = located
            .data
            .hits
            .iter()
            .filter(|hit| hit.symbol == Some(entity.id))
            .collect::<Vec<_>>();
        assert_eq!(hits.len(), 1, "{name}");
        assert_eq!(hits[0].source.as_ref(), Some(&declared));
        assert_eq!(hits[0].signature, explained.data.signature);
        check_source(hits[0].source.as_ref().unwrap());
        for coverage in [&explained.data.coverage, &located.data.coverage] {
            assert!(
                coverage
                    .iter()
                    .all(|record| record.repository == receipt.repository
                        && record.generation == receipt.generation
                        && !matches!(record.scope, CoverageScope::File(other) if other != file.id))
            );
        }
    }
    let value_read = reference(source.rfind("= value").unwrap() + "= ".len(), "value");
    let reads = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Reference && site.source == value_read)
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].enclosing, Some(symbols["echo"]));
    assert_eq!(reads[0].syntactic_text_hash, content_hash(b"value"));
    assert_eq!(
        reads[0].target,
        OccurrenceTarget::Resolved {
            symbol: symbols["value"]
        }
    );
    check_source(&value_read);
    let edges = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(reads[0].id)
        })
        .collect::<Vec<_>>();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].object, RelationEndpoint::Entity(symbols["value"]));
    assert_eq!(edges[0].evidence.source.as_ref(), Some(&value_read));
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.source == value_read && gap.detail == "fsharp-formal-binding-target-unavailable"
    }));
    assert!(!document.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "syntax-error-recovery"
            && diagnostic
                .source
                .as_ref()
                .is_some_and(|source| source.span().file() == file.id)
    }));
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.source.span().file() == file.id && gap.reason == SkippedRegionReason::ParseError
    }));
    // Authored conditional scopes do not establish an active namespace owner.
    for written in [
        "#if FEATURE\nnamespace Variant.Primary\n#else\nnamespace Variant.Secondary\n#endif",
        "#else\nnamespace Variant.Secondary",
    ] {
        let conditional = reference(source.find(written).unwrap(), written);
        check_source(&conditional);
        for (domain, detail) in [
            (
                rootlight_ir::FactDomain::Entities,
                "fsharp-conditional-namespace-owner-unavailable",
            ),
            (
                rootlight_ir::FactDomain::Relations,
                "fsharp-conditional-compilation-evaluation-unavailable",
            ),
        ] {
            assert_eq!(
                document
                    .skipped_regions
                    .iter()
                    .filter(|gap| {
                        gap.source == conditional
                            && gap.domain == domain
                            && gap.reason == SkippedRegionReason::UnsupportedConstruct
                            && gap.detail == detail
                    })
                    .count(),
                1
            );
        }
    }
    let mut warnings = Vec::new();
    for gap in document
        .skipped_regions
        .iter()
        .filter(|gap| gap.source.span().file() == file.id)
    {
        assert_eq!(gap.repository, receipt.repository);
        assert_eq!(gap.generation, receipt.generation);
        assert_eq!(gap.evidence.source.as_ref(), Some(&gap.source));
        assert!(matches!(
            gap.reason,
            SkippedRegionReason::ParseError | SkippedRegionReason::UnsupportedConstruct
        ));
        check_source(&gap.source);
        warnings.push(format!(
            "gap:{:?}:{:?}:{}:{}:{}",
            gap.domain,
            gap.reason,
            gap.source.span().start_byte(),
            gap.source.span().end_byte(),
            gap.detail
        ));
    }
    for diagnostic in document.diagnostics.iter().filter(|diagnostic| {
        diagnostic
            .source
            .as_ref()
            .is_some_and(|source| source.span().file() == file.id)
    }) {
        assert_eq!(diagnostic.repository, receipt.repository);
        assert_eq!(diagnostic.generation, receipt.generation);
        let reference = diagnostic.source.as_ref().unwrap();
        check_source(reference);
        warnings.push(format!(
            "diagnostic:{}:{:?}:{:?}:{}:{}:{}",
            diagnostic.code,
            diagnostic.severity,
            diagnostic.coverage_effect,
            reference.span().start_byte(),
            reference.span().end_byte(),
            diagnostic.message
        ));
    }
    warnings.sort();
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(
        (coverage.language.as_str(), coverage.tier),
        ("fsharp", AnalysisTier::TierD)
    );
    assert_eq!(coverage.reason, None);
    assert_eq!(coverage.status, CoverageStatus::Bounded);
    ConditionalScopeProof {
        symbols,
        coverage,
        warnings,
    }
}

#[test]
fn fsharp_content_routes_default_sources_without_promoting_mixed_suffix_peers() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let sources = [
        ("Full.fs", IMPLEMENTATION, "fsharp"),
        (
            "Library.fs",
            "module Example.Core\nlet value = 1\n",
            "fsharp",
        ),
        (
            "Upper.FS",
            "namespace Example.Core\ntype Entry = { Count: int }\n",
            "fsharp",
        ),
        (
            "MultilineValue.fs",
            "module Example.Core\nlet multilineValue =\n    1",
            "fsharp",
        ),
        (
            "MultilineFunction.fs",
            "module Example.Core\nlet identity input =\n    input\n",
            "fsharp",
        ),
        (
            "MultilineRecord.fs",
            "namespace Example.Core\r\ntype Entry =\r\n    { Count: int }",
            "fsharp",
        ),
        (
            "MultilineUnion.fs",
            "namespace Example.Core\ntype Choice =\n    | Empty\n    | Item of int\n",
            "fsharp",
        ),
        (
            "MultilineNested.fs",
            "module Example.Core\nmodule Inner =\n    let nestedValue =\n        2\n",
            "fsharp",
        ),
        (
            "Bom.fs",
            "\u{feff}module Example.Core\r\nlet bomValue = 1\r\n",
            "fsharp",
        ),
        ("Forth.fs", ": square dup * ;\n", "unknown"),
        (
            "Mixed.fs",
            "module Example.Core\nlet value = 1\n: square dup * ;\n",
            "unknown",
        ),
        (
            "InlineMixed.fs",
            "module Example.Core\nlet value = 1 : square dup * ;\n",
            "unknown",
        ),
        (
            "ConditionalMixed.fs",
            "module Example.Core\nlet value = 1\n0 [if] : square dup * ; [then]\n",
            "unknown",
        ),
        (
            "Annotated.fs",
            "module Example.Core\nlet value : int = 1 ;\n",
            "fsharp",
        ),
        (
            "Record.fs",
            "module Example.Core\ntype Entry = { Pair : int * int ; Name : string }\n",
            "fsharp",
        ),
        (
            "Postfix.fs",
            "module Example.Core\nlet value = [ 1 ] : int list ;\n",
            "fsharp",
        ),
        (
            "TypedRhs.fs",
            "module Example.Core\nlet value = (1 : int) ;\n",
            "fsharp",
        ),
        (
            "MemberContract.fs",
            "module Example.Core\nlet value = 1\ntype Contract =\n    abstract Pair : int * int\n",
            "fsharp",
        ),
        (
            "Escaped.fs",
            "module Example.Core\nlet ``value : square dup * ;`` = 1\n",
            "fsharp",
        ),
        ("Empty.fs", "", "unknown"),
        ("Words.fs", "module and namespace let type\n", "unknown"),
        (
            "Literal.fs",
            "s\" module Example.Core\nlet value = 1\"\n",
            "unknown",
        ),
        (
            "Backup.fs.bak",
            "module Example.Core\nlet value = 1\n",
            "unknown",
        ),
        (
            "Other.txt",
            "module Example.Core\nlet value = 1\n",
            "unknown",
        ),
        ("Script.fsx", "let scriptValue = 1\n", "fsharp"),
        (
            "Contract.fsi",
            "module Example.Core\nval declaredValue: int\n",
            "fsharp-signature",
        ),
        (
            "Broken.fs",
            "module Example.Core\nlet brokenValue = (\n",
            "fsharp",
        ),
    ];
    for (path, source, _) in sources {
        fs::write(repository.path().join(path), source).unwrap();
    }
    fs::write(
        repository.path().join("binary.fs"),
        b"\0module Example\nlet value = 1\n",
    )
    .unwrap();
    fs::write(repository.path().join("Example.fsproj"), "<Project />\n").unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(receipt.binary_inputs, 1);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert!(document.files.iter().all(|file| file.path != "binary.fs"));
    let mut configured = FirstSliceService::new_with_storage_policy(
        4,
        MAX_RETAINED_SOURCE_BYTES,
        None,
        None,
        policy(),
    )
    .unwrap();
    let configured_receipt = configured
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let configured_snapshot = configured
        .loaded_generation_snapshot(configured_receipt.generation)
        .unwrap();
    for (path, source, language) in sources {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, language, "{path}");
        assert_eq!(file.content_hash, content_hash(source.as_bytes()));
        assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
        let provenance = document
            .provenance
            .iter()
            .find(|record| record.id == file.provenance)
            .unwrap();
        assert_eq!(provenance.language, language);
        assert_eq!(provenance.tier, AnalysisTier::TierD);
        assert_eq!(
            provenance.producer_kind,
            if language == "unknown" {
                ProducerKind::Rule
            } else {
                ProducerKind::Parser
            }
        );
        let coverage = service
            .source_file_coverage_until(receipt.generation, file.id, &deadline())
            .unwrap();
        assert_eq!(coverage.language, language);
        assert_eq!(coverage.tier, AnalysisTier::TierD);
        if language == "unknown" {
            assert_eq!(provenance.rule.as_deref(), Some("unsupported-language"));
            assert_eq!(coverage.status, CoverageStatus::Unknown);
            assert_eq!(
                coverage.reason,
                Some(FirstSliceSourceCoverageReason::UnsupportedLanguage)
            );
            assert!(!document.entities.iter().any(|entity| {
                entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|reference| reference.span().file() == file.id)
            }));
        } else {
            let configured_file = configured_snapshot
                .document()
                .files
                .iter()
                .find(|file| file.path == path)
                .unwrap();
            let configured_coverage = configured
                .source_file_coverage_until(
                    configured_receipt.generation,
                    configured_file.id,
                    &deadline(),
                )
                .unwrap();
            assert_eq!(coverage.language, configured_coverage.language, "{path}");
            assert_eq!(coverage.tier, configured_coverage.tier, "{path}");
            assert_eq!(coverage.status, configured_coverage.status, "{path}");
            assert_eq!(coverage.reason, configured_coverage.reason, "{path}");
        }
        if path == "Broken.fs" {
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source.span().file() == file.id
                        && gap.reason == SkippedRegionReason::ParseError)
            );
        }
        let expected_declarations: &[(&str, EntityKind)] = match path {
            "Full.fs" => &[
                ("value", EntityKind::Variable),
                ("Record", EntityKind::Struct),
                ("Read", EntityKind::Method),
            ],
            "MultilineValue.fs" => &[("multilineValue", EntityKind::Variable)],
            "MultilineFunction.fs" => &[("identity", EntityKind::Function)],
            "MultilineRecord.fs" => &[("Entry", EntityKind::Struct)],
            "MultilineUnion.fs" => &[("Choice", EntityKind::Union)],
            "MultilineNested.fs" => &[
                ("Inner", EntityKind::Module),
                ("nestedValue", EntityKind::Variable),
            ],
            "Script.fsx" => &[("scriptValue", EntityKind::Variable)],
            _ => &[],
        };
        for (name, kind) in expected_declarations.iter().copied() {
            let entity = document
                .entities
                .iter()
                .find(|entity| {
                    entity.language == "fsharp"
                        && entity.canonical_name == name
                        && entity.kind == kind
                        && entity
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|reference| reference.span().file() == file.id)
                })
                .unwrap();
            let site = document
                .occurrences
                .iter()
                .find(|site| {
                    site.file == file.id
                        && site.role == OccurrenceRole::Definition
                        && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                })
                .unwrap();
            exact_source(&service, &site.source, "fsharp", name.as_bytes());
            assert_eq!(site.source.generation(), receipt.generation);
            let located = service
                .code_locate_with_filters_and_budget(
                    receipt.generation,
                    name.to_owned(),
                    LocateMode::Exact,
                    Vec::new(),
                    vec![path.to_owned()],
                    8,
                    0,
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            assert!(
                located
                    .data
                    .hits
                    .iter()
                    .any(|hit| hit.symbol == Some(entity.id) && hit.file == file.id)
            );
        }
        exact_source(
            &service,
            file.evidence.source.as_ref().unwrap(),
            language,
            source.as_bytes(),
        );
    }
}

#[test]
fn fsharp_headerless_content_preserves_authored_formals_and_scoped_coverage() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let implementation = "open System\nlet identity (value: int) = value\nlet answer = 1\n";
    let entry = "[<EntryPoint>]\nlet run _ = 0\n";
    let standalone = "let direct (value: int) = value\n";
    let peers = [
        ("Bare.fs", "let peer = 2\n"),
        ("Forth.fs", ": square dup * ;\n"),
        (
            "Mixed.fs",
            "open System\nlet identity (value: int) = value\n: square dup * ;\n",
        ),
    ];
    fs::write(repository.path().join("Library.fs"), implementation).unwrap();
    fs::write(repository.path().join("Program.fs"), entry).unwrap();
    fs::write(repository.path().join("Standalone.fs"), standalone).unwrap();
    for (path, source) in peers {
        fs::write(repository.path().join(path), source).unwrap();
    }
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(receipt.indexed_files, 6);
    assert!(receipt.discovery_complete);
    let status = service.repository_status(receipt.repository, None).unwrap();
    let supported = status
        .coverage
        .iter()
        .find(|entry| entry.language == "fsharp")
        .unwrap();
    assert_eq!(supported.discovered_files, 3);
    assert_eq!(supported.indexed_files, 3);
    let unknown = status
        .coverage
        .iter()
        .find(|entry| entry.language == "unknown")
        .unwrap();
    assert_eq!(unknown.discovered_files, 3);
    assert_eq!(unknown.indexed_files, 0);
    assert_headerless_fsharp_file(
        &service,
        &receipt,
        "Library.fs",
        implementation,
        &[
            ("identity", EntityKind::Function),
            ("value", EntityKind::Parameter),
            ("answer", EntityKind::Variable),
        ],
    );
    assert_headerless_fsharp_file(
        &service,
        &receipt,
        "Program.fs",
        entry,
        &[("run", EntityKind::Function)],
    );
    assert_headerless_fsharp_file(
        &service,
        &receipt,
        "Standalone.fs",
        standalone,
        &[
            ("direct", EntityKind::Function),
            ("value", EntityKind::Parameter),
        ],
    );
    for (path, source) in peers {
        assert_project_member(&service, &receipt, path, source.as_bytes(), "unknown", None);
    }
}

fn assert_headerless_fsharp_file(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    source: &str,
    declarations: &[(&str, EntityKind)],
) -> (FileId, BTreeSet<SymbolId>) {
    assert_headerless_fsharp_file_with_relation_gap(
        service,
        receipt,
        path,
        source,
        declarations,
        None,
    )
}

fn assert_headerless_fsharp_file_with_relation_gap(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    source: &str,
    declarations: &[(&str, EntityKind)],
    expected_relation_gap: Option<&str>,
) -> (FileId, BTreeSet<SymbolId>) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == path)
        .unwrap();
    assert_eq!(file.language, "fsharp", "{path}");
    assert_eq!(file.encoding, "utf-8");
    assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(source.as_bytes()));
    let check_source = |reference: &SourceRef| {
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), file.content_hash);
        let written = source
            .as_bytes()
            .get(
                usize::try_from(reference.span().start_byte()).unwrap()
                    ..usize::try_from(reference.span().end_byte()).unwrap(),
            )
            .unwrap();
        exact_source(service, reference, "fsharp", written);
    };
    let full = file.evidence.source.as_ref().unwrap();
    assert_eq!(
        full.span(),
        SourceSpan::new(file.id, 0, file.byte_length).unwrap()
    );
    check_source(full);
    assert!(!document.diagnostics.iter().any(|diagnostic| {
        matches!(
            diagnostic.code.as_str(),
            "unsupported-language" | "unsupported-encoding"
        ) && diagnostic
            .source
            .as_ref()
            .is_some_and(|reference| reference.span().file() == file.id)
    }));
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.source.span().file() == file.id
            && matches!(
                gap.reason,
                SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
            )
    }));
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(coverage.language, "fsharp");
    assert_eq!(coverage.tier, AnalysisTier::TierD);
    assert_eq!(coverage.reason, None);
    let unsupported_detail = if source.starts_with("open ") {
        Some("unresolved-import-target")
    } else if source.starts_with("[<") {
        Some("fsharp-attribute-binding-analysis-unavailable")
    } else {
        None
    };
    if let Some(written) = expected_relation_gap {
        assert_eq!(coverage.status, CoverageStatus::Bounded);
        assert_eq!(source.match_indices(written).count(), 1);
        let start = source.find(written).unwrap();
        let expected = SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                file.id,
                u64::try_from(start).unwrap(),
                u64::try_from(start + written.len()).unwrap(),
            )
            .unwrap(),
            file.content_hash,
            None,
        );
        let gaps = document
            .skipped_regions
            .iter()
            .filter(|gap| gap.source.span().file() == file.id)
            .collect::<Vec<_>>();
        assert_eq!(gaps.len(), 1, "{path}: exact scoped relation gap");
        let gap = gaps[0];
        assert_eq!(gap.domain, rootlight_ir::FactDomain::Relations);
        assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
        assert_eq!(
            gap.detail,
            "fsharp-body-binding-and-call-analysis-unavailable"
        );
        assert_eq!(gap.repository, receipt.repository);
        assert_eq!(gap.generation, receipt.generation);
        assert_eq!(gap.source, expected);
        assert_eq!(gap.evidence.source.as_ref(), Some(&expected));
        check_source(&gap.source);
    } else if let Some(detail) = unsupported_detail {
        assert_eq!(coverage.status, CoverageStatus::Bounded);
        let gaps = document
            .skipped_regions
            .iter()
            .filter(|gap| gap.source.span().file() == file.id && gap.detail == detail)
            .collect::<Vec<_>>();
        assert!(
            !gaps.is_empty(),
            "{path}: retained unsupported prologue evidence"
        );
        assert!(
            gaps.iter()
                .any(|gap| gap.domain == rootlight_ir::FactDomain::Relations)
        );
        for gap in gaps {
            assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
            assert_eq!(gap.repository, receipt.repository);
            assert_eq!(gap.generation, receipt.generation);
            assert_eq!(gap.evidence.source.as_ref(), Some(&gap.source));
            check_source(&gap.source);
        }
    } else {
        assert_eq!(
            coverage.status,
            CoverageStatus::Complete,
            "{path}: {:?}",
            document
                .skipped_regions
                .iter()
                .filter(|gap| gap.source.span().file() == file.id)
                .collect::<Vec<_>>()
        );
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().file() == file.id)
        );
    }
    let mut symbols = BTreeSet::new();
    let mut function = None;
    let mut parameter = None;
    for &(name, kind) in declarations {
        let entities = document
            .entities
            .iter()
            .filter(|entity| {
                entity.language == "fsharp"
                    && entity.canonical_name == name
                    && entity.kind == kind
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|reference| reference.span().file() == file.id)
            })
            .collect::<Vec<_>>();
        assert_eq!(entities.len(), 1, "{path}: authored {name}");
        let entity = entities[0];
        assert!(symbols.insert(entity.id));
        check_source(entity.evidence.source.as_ref().unwrap());
        if kind == EntityKind::Function {
            function = Some(entity);
        } else if kind == EntityKind::Parameter {
            parameter = Some(entity);
        }
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.file == file.id
                    && site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1, "{path}: definition {name}");
        check_source(&definitions[0].source);
        exact_source(service, &definitions[0].source, "fsharp", name.as_bytes());
        let located = service
            .code_locate_with_filters_and_budget(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                vec!["fsharp".to_owned()],
                vec![path.to_owned()],
                8,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(located.data.generation, receipt.generation);
        let hit = located
            .data
            .hits
            .iter()
            .find(|hit| hit.file == file.id && hit.symbol == Some(entity.id))
            .unwrap();
        assert_eq!(hit.path, path);
        assert_eq!(hit.language, "fsharp");
        check_source(hit.source.as_ref().unwrap());
        assert!(located.data.coverage.iter().all(|record| {
            !matches!(record.scope, CoverageScope::File(other) if other != file.id)
        }));
    }
    let function = function.unwrap();
    let Some(parameter) = parameter else {
        assert!(!document.relations.iter().any(|relation| {
            matches!(
                relation.predicate,
                RelationPredicate::RefersTo | RelationPredicate::Calls
            ) && relation
                .evidence
                .source
                .as_ref()
                .is_some_and(|reference| reference.span().file() == file.id)
        }));
        return (file.id, symbols);
    };
    assert_eq!(parameter.container, Some(ContainerRef::Entity(function.id)));
    let read_start = source.rfind(&parameter.canonical_name).unwrap();
    let read_span = SourceSpan::new(
        file.id,
        u64::try_from(read_start).unwrap(),
        u64::try_from(read_start + parameter.canonical_name.len()).unwrap(),
    )
    .unwrap();
    let reads = document
        .occurrences
        .iter()
        .filter(|site| {
            site.file == file.id
                && site.role == OccurrenceRole::Reference
                && site.source.span() == read_span
        })
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 1);
    let read = reads[0];
    assert_eq!(read.enclosing, Some(function.id));
    assert_eq!(
        read.target,
        OccurrenceTarget::Resolved {
            symbol: parameter.id
        }
    );
    check_source(&read.source);
    let relations = document
        .relations
        .iter()
        .filter(|relation| relation.subject == RelationEndpoint::Occurrence(read.id))
        .collect::<Vec<_>>();
    assert_eq!(relations.len(), 1);
    assert_eq!(relations[0].predicate, RelationPredicate::RefersTo);
    assert_eq!(relations[0].object, RelationEndpoint::Entity(parameter.id));
    assert_eq!(relations[0].evidence.source.as_ref(), Some(&read.source));
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.source == read.source && gap.detail == "fsharp-formal-binding-target-unavailable"
    }));
    let relationships = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([function.id]),
            vec![RelationFamily::References],
            Some(RelationDirection::Outbound),
            0,
            8,
            0,
            &deadline(),
        )
        .unwrap();
    assert_eq!(relationships.data.generation, receipt.generation);
    assert!(!relationships.data.truncated);
    let targets = relationships
        .data
        .groups
        .iter()
        .flat_map(|group| &group.items)
        .collect::<Vec<_>>();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].symbol, parameter.id);
    assert_eq!(targets[0].source_refs, vec![read.source.clone()]);
    (file.id, symbols)
}

#[test]
fn fsharp_headerless_content_routes_survive_zero_supported_transitions_and_reopen() {
    assert_headerless_content_transitions_and_reopen(
        "open System\nlet identity (value: int) = value\nlet answer = 1\n",
    );
}

#[test]
fn fsharp_plain_declaration_prefix_preserves_typed_neighbor_after_reopen() {
    assert_headerless_content_transitions_and_reopen(
        "let answer = 1\nlet identity (value: int) = value\n",
    );
}

#[test]
fn fsharp_native_function_routes_survive_zero_supported_transitions_and_reopen() {
    assert_headerless_content_transitions_and_reopen(
        "let identity value = value\nlet answer = 1\n",
    );
}

#[test]
fn fsharp_native_module_ownership_preserves_queries_and_recovery() {
    assert_native_owned_declarations(
        "module Sample\nlet identity(value) = value\n",
        "module Sample\nlet identity(value) = (value)\n",
        &[
            ("identity", EntityKind::Function),
            ("value", EntityKind::Parameter),
        ],
        None,
    );
}

#[test]
fn fsharp_native_recursive_ownership_preserves_queries_and_recovery() {
    assert_native_owned_declarations(
        "let rec first left = second left\nand second right = right\n",
        "let rec first left = second left\nand second right = (right)\n",
        &[
            ("first", EntityKind::Function),
            ("left", EntityKind::Parameter),
            ("second", EntityKind::Function),
            ("right", EntityKind::Parameter),
        ],
        Some("second left"),
    );
}

#[test]
fn fsharp_native_exception_prefix_preserves_queries_and_partiality_after_reopen() {
    assert_native_owned_declarations_with(
        "exception Failure of int\nlet identity value = value\n",
        "exception Failure of int\nlet identity value = (value)\n",
        assert_exception_prefixed_native_file,
    );
}

fn assert_native_owned_declarations(
    source: &str,
    changed_source: &str,
    declarations: &[(&str, EntityKind)],
    expected_relation_gap: Option<&str>,
) {
    assert_native_owned_declarations_with(source, changed_source, |service, receipt, text| {
        assert_native_owned_file(service, receipt, text, declarations, expected_relation_gap)
    });
}

fn assert_native_owned_declarations_with(
    source: &str,
    changed_source: &str,
    check_file: impl Fn(
        &FirstSliceService,
        &FirstSliceIndexReceipt,
        &str,
    ) -> (FileId, BTreeSet<SymbolId>),
) {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let path = repository.path().join("Owned.fs");
    let peer = "let peer = 1\n";
    let mixed = format!("{source}: square dup * ;\n");
    fs::write(&path, source).unwrap();
    fs::write(repository.path().join("Peer.fs"), peer).unwrap();
    fs::write(repository.path().join("Mixed.fs"), &mixed).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(
        Some("version='1.3'\n[storage]\nretained_generations=4\n"),
        4,
    )
    .unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let cold = index(&mut service, repository.path(), options);
    let initial = cold.receipt().clone();
    let root = RepositoryRoot::open(initial.repository, repository.path()).unwrap();
    let manifest = rootlight_discovery::discover(
        &root,
        &service.config,
        &DiscoveryPolicy::build(Vec::new(), false).unwrap(),
        DiscoveryLimits::from_config(&service.config),
        &deadline(),
    )
    .unwrap();
    let input = manifest
        .inputs
        .iter()
        .find(|input| input.path == "Owned.fs")
        .unwrap();
    assert!(input.language_signals.is_empty());
    assert!(input.project_language_witnesses.is_empty());
    assert_eq!(input.content_hash, content_hash(source.as_bytes()));
    assert_eq!(cold.evidence().rebuilt_files, 1);
    let identities = check_file(&service, &initial, source);
    for (peer_path, text) in [("Peer.fs", peer), ("Mixed.fs", mixed.as_str())] {
        assert_project_member(
            &service,
            &initial,
            peer_path,
            text.as_bytes(),
            "unknown",
            None,
        );
    }
    let route = retained_parser_route(&service, initial.generation, identities.0);
    let noop = index(&mut service, repository.path(), options);
    assert_eq!(noop.receipt(), &initial);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    fs::write(&path, changed_source).unwrap();
    let mutation = index(&mut service, repository.path(), options);
    assert_eq!(mutation.evidence().changed_files, 1);
    assert_eq!(mutation.evidence().rebuilt_files, 1);
    let changed = mutation.receipt().clone();
    assert_ne!(changed.generation, initial.generation);
    assert_eq!(check_file(&service, &changed, changed_source), identities);
    let clean = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
    );
    assert_eq!(clean.evidence().rebuilt_files, 1);
    assert!(changed.logical_snapshot.is_some());
    assert_eq!(clean.receipt().logical_snapshot, changed.logical_snapshot);
    let last = clean.receipt().clone();
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    for (receipt, text) in [
        (&initial, source),
        (&changed, changed_source),
        (&last, changed_source),
    ] {
        assert_eq!(check_file(&reopened, receipt, text), identities);
    }
    assert_eq!(
        retained_parser_route(&reopened, initial.generation, identities.0),
        route
    );
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt(), &last);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    let foreign = ": square dup * ;\n";
    fs::write(&path, foreign).unwrap();
    let removed = index(&mut reopened, repository.path(), options);
    assert_eq!(removed.evidence().changed_files, 1);
    assert_eq!(removed.evidence().rebuilt_files, 0);
    assert_eq!(
        assert_project_member(
            &reopened,
            removed.receipt(),
            "Owned.fs",
            foreign.as_bytes(),
            "unknown",
            None
        ),
        identities.0
    );
    assert_ne!(
        retained_parser_route(&reopened, removed.receipt().generation, identities.0),
        route
    );
    assert_eq!(check_file(&reopened, &initial, source), identities);
}

fn assert_exception_prefixed_native_file(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
) -> (FileId, BTreeSet<SymbolId>) {
    let function = assert_default_fsharp_declaration(
        service,
        receipt,
        "Owned.fs",
        source,
        "identity",
        EntityKind::Function,
    );
    let parameter = assert_default_fsharp_declaration(
        service,
        receipt,
        "Owned.fs",
        source,
        "value",
        EntityKind::Parameter,
    );
    assert_eq!(parameter.0, function.0);
    let file = function.0;
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert!(!document.diagnostics.iter().any(|diagnostic| {
        matches!(
            diagnostic.code.as_str(),
            "unsupported-language" | "unsupported-encoding"
        ) && diagnostic
            .source
            .as_ref()
            .is_some_and(|reference| reference.span().file() == file)
    }));
    let entities = document
        .entities
        .iter()
        .filter(|entity| {
            entity
                .evidence
                .source
                .as_ref()
                .is_some_and(|reference| reference.span().file() == file)
        })
        .collect::<Vec<_>>();
    assert_eq!(entities.len(), 3);
    assert!(
        !entities
            .iter()
            .any(|entity| entity.canonical_name == "Failure")
    );
    let parameter_entity = entities
        .iter()
        .find(|entity| entity.id == parameter.1)
        .unwrap();
    assert_eq!(
        parameter_entity.container,
        Some(ContainerRef::Entity(function.1))
    );
    let reference = |start: usize, written: &str| {
        SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                file,
                u64::try_from(start).unwrap(),
                u64::try_from(start + written.len()).unwrap(),
            )
            .unwrap(),
            content_hash(source.as_bytes()),
            None,
        )
    };
    let exception = "exception Failure of int";
    assert!(source.starts_with(exception));
    let gaps = document
        .skipped_regions
        .iter()
        .filter(|gap| gap.source.span().file() == file)
        .collect::<Vec<_>>();
    assert_eq!(gaps.len(), 2);
    for (start, written) in [(0, exception), (exception.len() - "int".len(), "int")] {
        let expected_gap = reference(start, written);
        let matching = gaps
            .iter()
            .filter(|gap| gap.source == expected_gap)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1);
        let gap = matching[0];
        assert_eq!(gap.domain, rootlight_ir::FactDomain::Entities);
        assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
        assert_eq!(gap.detail, "fsharp-declaration-shape-unsupported");
        assert_eq!(gap.repository, receipt.repository);
        assert_eq!(gap.generation, receipt.generation);
        assert_eq!(gap.evidence.source.as_ref(), Some(&expected_gap));
        exact_source(service, &expected_gap, "fsharp", written.as_bytes());
    }
    let coverage = service
        .source_file_coverage_until(receipt.generation, file, &deadline())
        .unwrap();
    assert_eq!(coverage.language, "fsharp");
    assert_eq!(coverage.tier, AnalysisTier::TierD);
    assert_eq!(coverage.status, CoverageStatus::Bounded);
    assert_eq!(coverage.reason, None);
    let expected_read = reference(source.rfind("value").unwrap(), "value");
    let reads = document
        .occurrences
        .iter()
        .filter(|site| {
            site.file == file
                && site.role == OccurrenceRole::Reference
                && site.source == expected_read
        })
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 1);
    let read = reads[0];
    assert_eq!(read.enclosing, Some(function.1));
    assert_eq!(
        read.target,
        OccurrenceTarget::Resolved {
            symbol: parameter.1
        }
    );
    let edges = document
        .relations
        .iter()
        .filter(|edge| edge.subject == RelationEndpoint::Occurrence(read.id))
        .collect::<Vec<_>>();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].predicate, RelationPredicate::RefersTo);
    assert_eq!(edges[0].object, RelationEndpoint::Entity(parameter.1));
    assert_eq!(edges[0].evidence.source.as_ref(), Some(&expected_read));
    exact_source(service, &expected_read, "fsharp", b"value");
    let relationships = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([function.1]),
            vec![RelationFamily::References],
            Some(RelationDirection::Outbound),
            0,
            8,
            0,
            &deadline(),
        )
        .unwrap();
    assert_eq!(relationships.data.generation, receipt.generation);
    assert!(!relationships.data.truncated);
    let targets = relationships
        .data
        .groups
        .iter()
        .flat_map(|group| &group.items)
        .collect::<Vec<_>>();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].symbol, parameter.1);
    assert_eq!(targets[0].source_refs, vec![expected_read]);
    (file, BTreeSet::from([function.1, parameter.1]))
}

fn assert_native_owned_file(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    declarations: &[(&str, EntityKind)],
    expected_relation_gap: Option<&str>,
) -> (FileId, BTreeSet<SymbolId>) {
    let identities = assert_headerless_fsharp_file_with_relation_gap(
        service,
        receipt,
        "Owned.fs",
        source,
        declarations,
        expected_relation_gap,
    );
    if expected_relation_gap.is_some() {
        assert_native_owned_recursive_relations(service, receipt, identities.0, source);
    }
    identities
}

fn assert_native_owned_recursive_relations(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    file: FileId,
    source: &str,
) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let entity = |name, kind| {
        let matches = document
            .entities
            .iter()
            .filter(|entity| {
                entity.canonical_name == name
                    && entity.kind == kind
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|reference| reference.span().file() == file)
            })
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1);
        matches[0]
    };
    let first = entity("first", EntityKind::Function);
    let second = entity("second", EntityKind::Function);
    let call_start = source.find("second left").unwrap();
    let reference = |start: usize, written: &str| {
        SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                file,
                u64::try_from(start).unwrap(),
                u64::try_from(start + written.len()).unwrap(),
            )
            .unwrap(),
            content_hash(source.as_bytes()),
            None,
        )
    };
    // The application-wide gap is conservative dispatch/evaluation coverage;
    // it must not erase independently proven formal reads or the direct call.
    for (owner, name, start) in [
        (first, "left", call_start + "second ".len()),
        (second, "right", source.rfind("right").unwrap()),
    ] {
        let parameter = entity(name, EntityKind::Parameter);
        assert_eq!(parameter.container, Some(ContainerRef::Entity(owner.id)));
        let expected = reference(start, name);
        let reads = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference && site.source == expected)
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 1);
        let read = reads[0];
        assert_eq!(read.enclosing, Some(owner.id));
        assert_eq!(
            read.target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            }
        );
        let edges = document
            .relations
            .iter()
            .filter(|edge| edge.subject == RelationEndpoint::Occurrence(read.id))
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].predicate, RelationPredicate::RefersTo);
        assert_eq!(edges[0].object, RelationEndpoint::Entity(parameter.id));
        assert_eq!(edges[0].evidence.source.as_ref(), Some(&expected));
        exact_source(service, &expected, "fsharp", name.as_bytes());
        let relationships = service
            .symbol_relationships(
                receipt.generation,
                BTreeSet::from([owner.id]),
                vec![RelationFamily::References],
                Some(RelationDirection::Outbound),
                0,
                8,
                0,
                &deadline(),
            )
            .unwrap();
        assert_eq!(relationships.data.generation, receipt.generation);
        assert!(!relationships.data.truncated);
        let targets = relationships
            .data
            .groups
            .iter()
            .flat_map(|group| &group.items)
            .filter(|item| item.symbol == parameter.id)
            .collect::<Vec<_>>();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].source_refs, vec![expected]);
    }
    let expected = reference(call_start, "second left");
    let calls = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|reference| reference.span().file() == file)
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].subject, RelationEndpoint::Entity(first.id));
    assert_eq!(calls[0].object, RelationEndpoint::Entity(second.id));
    assert_eq!(calls[0].evidence.source.as_ref(), Some(&expected));
    exact_source(service, &expected, "fsharp", b"second left");
    let relationships = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([first.id]),
            vec![RelationFamily::Calls],
            Some(RelationDirection::Outbound),
            0,
            8,
            0,
            &deadline(),
        )
        .unwrap();
    assert_eq!(relationships.data.generation, receipt.generation);
    assert!(!relationships.data.truncated);
    assert_eq!(relationships.data.returned_edges, 1);
    assert_eq!(relationships.data.total_edges, 1);
    let targets = relationships
        .data
        .groups
        .iter()
        .flat_map(|group| &group.items)
        .collect::<Vec<_>>();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].symbol, second.id);
    assert_eq!(targets[0].source_refs, vec![expected]);
}

fn assert_headerless_content_transitions_and_reopen(implementation: &str) {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let ambiguous = ": square dup * ;\n";
    let peer = "let peer = 2\n";
    let declarations = [
        ("identity", EntityKind::Function),
        ("value", EntityKind::Parameter),
        ("answer", EntityKind::Variable),
    ];
    let member_path = repository.path().join("Member.fs");
    fs::write(&member_path, ambiguous).unwrap();
    fs::write(repository.path().join("Peer.fs"), peer).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(
        Some("version='1.3'\n[storage]\nretained_generations=4\n"),
        4,
    )
    .unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let initial = index(&mut service, repository.path(), options);
    assert_eq!(initial.evidence().rebuilt_files, 0);
    let file = assert_project_member(
        &service,
        initial.receipt(),
        "Member.fs",
        ambiguous.as_bytes(),
        "unknown",
        None,
    );
    let unknown_route = retained_parser_route(&service, initial.receipt().generation, file);
    fs::write(&member_path, implementation).unwrap();
    let added = index(&mut service, repository.path(), options);
    assert_eq!(added.evidence().changed_files, 1);
    assert_eq!(added.evidence().rebuilt_files, 1);
    let identities = assert_headerless_fsharp_file(
        &service,
        added.receipt(),
        "Member.fs",
        implementation,
        &declarations,
    );
    assert_eq!(identities.0, file);
    let supported_route = retained_parser_route(&service, added.receipt().generation, file);
    assert_ne!(supported_route, unknown_route);
    assert_project_member(
        &service,
        added.receipt(),
        "Peer.fs",
        peer.as_bytes(),
        "unknown",
        None,
    );
    let retained = added.receipt().clone();
    let original_source = service
        .loaded_generation_snapshot(retained.generation)
        .unwrap()
        .document()
        .files
        .iter()
        .find(|record| record.id == file)
        .unwrap()
        .evidence
        .source
        .clone()
        .unwrap();
    let noop = index(&mut service, repository.path(), options);
    assert_eq!(noop.receipt().generation, retained.generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    assert_eq!(
        assert_headerless_fsharp_file(
            &reopened,
            &retained,
            "Member.fs",
            implementation,
            &declarations
        ),
        identities
    );
    assert_eq!(
        retained_parser_route(&reopened, retained.generation, file),
        supported_route
    );
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt().generation, retained.generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    fs::write(&member_path, ambiguous).unwrap();
    let removed = index(&mut reopened, repository.path(), options);
    assert_eq!(removed.evidence().changed_files, 1);
    assert_eq!(removed.evidence().rebuilt_files, 0);
    assert_eq!(
        assert_project_member(
            &reopened,
            removed.receipt(),
            "Member.fs",
            ambiguous.as_bytes(),
            "unknown",
            None
        ),
        file
    );
    assert_eq!(
        retained_parser_route(&reopened, removed.receipt().generation, file),
        unknown_route
    );
    assert_project_member(
        &reopened,
        removed.receipt(),
        "Peer.fs",
        peer.as_bytes(),
        "unknown",
        None,
    );
    exact_source(
        &reopened,
        &original_source,
        "fsharp",
        implementation.as_bytes(),
    );
    assert!(
        reopened
            .source_read_with_options_and_budget(
                removed.receipt().generation,
                vec![original_source],
                SourceReadOptions::new(),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .is_err()
    );
    let last = removed.receipt().clone();
    drop(reopened);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_eq!(
        assert_project_member(
            &reopened,
            &last,
            "Member.fs",
            ambiguous.as_bytes(),
            "unknown",
            None
        ),
        file
    );
    assert_eq!(
        assert_headerless_fsharp_file(
            &reopened,
            &retained,
            "Member.fs",
            implementation,
            &declarations
        ),
        identities
    );
    assert_eq!(
        retained_parser_route(&reopened, last.generation, file),
        unknown_route
    );
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt().generation, last.generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
}

#[test]
fn fsharp_headerless_content_respects_exact_configuration_priority() {
    assert_headerless_configuration_priority("open System\nlet identity (value: int) = value\n");
}

#[test]
fn fsharp_native_function_admission_respects_exact_configuration_priority() {
    assert_headerless_configuration_priority("let identity value = value\n");
}

fn assert_headerless_configuration_priority(source: &str) {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    for path in ["Default.fs", "Blocked.fs"] {
        fs::write(repository.path().join(path), source).unwrap();
    }
    let policy = FirstSliceStoragePolicy::resolve_user_config(Some("version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'Blocked.fs'='unsupported-language'\n"), 2).unwrap();
    let mut service = FirstSliceService::new_with_storage_policy(
        2,
        MAX_RETAINED_SOURCE_BYTES,
        None,
        None,
        policy,
    )
    .unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_headerless_fsharp_file(
        &service,
        &receipt,
        "Default.fs",
        source,
        &[
            ("identity", EntityKind::Function),
            ("value", EntityKind::Parameter),
        ],
    );
    assert_project_member(
        &service,
        &receipt,
        "Blocked.fs",
        source.as_bytes(),
        "unsupported-language",
        None,
    );
}

#[test]
fn fsharp_native_function_routes_preserve_encoded_sources_after_mutation_and_reopen() {
    let source = "// ☃ 🧭\r\nlet identity value = value\r\n";
    let changed_source = source.replace("= value\r\n", "= value + 1\r\n");
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let encode = |text: &str| {
            let mut bytes = if encoding == "utf-8" {
                Vec::new()
            } else {
                typed_formal_leaf_bytes("\u{feff}", encoding)
            };
            bytes.extend(typed_formal_leaf_bytes(text, encoding));
            bytes
        };
        let bytes = encode(source);
        let changed_bytes = encode(&changed_source);
        let peer = encode("let peer = 2\r\n");
        let mixed = encode("let identity value = value\r\n: square dup * ;\r\n");
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let path = repository.path().join("Native.fs");
        fs::write(&path, &bytes).unwrap();
        fs::write(repository.path().join("Peer.fs"), &peer).unwrap();
        fs::write(repository.path().join("Mixed.fs"), &mixed).unwrap();
        let policy = FirstSliceStoragePolicy::resolve_user_config(
            Some("version='1.3'\n[storage]\nretained_generations=2\n"),
            2,
        )
        .unwrap();
        let mut service = FirstSliceService::new_durable_with_policy(
            policy.clone(),
            paths.state_dir(),
            &deadline(),
        )
        .unwrap();
        let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
        let cold = index(&mut service, repository.path(), options);
        assert_eq!(cold.evidence().rebuilt_files, 1, "{encoding}");
        let initial = cold.receipt().clone();
        assert!(initial.discovery_complete);
        assert_eq!(initial.indexed_files, 3);
        let root = RepositoryRoot::open(initial.repository, repository.path()).unwrap();
        let manifest = rootlight_discovery::discover(
            &root,
            &service.config,
            &DiscoveryPolicy::build(Vec::new(), false).unwrap(),
            DiscoveryLimits::from_config(&service.config),
            &deadline(),
        )
        .unwrap();
        let input = manifest
            .inputs
            .iter()
            .find(|input| input.path == "Native.fs")
            .unwrap();
        assert_eq!(input.encoding, encoding);
        assert_eq!(input.bytes, u64::try_from(bytes.len()).unwrap());
        assert_eq!(input.content_hash, content_hash(&bytes));
        assert!(input.language_signals.is_empty());
        assert!(input.project_language_witnesses.is_empty());
        let identities =
            assert_native_function_generation(&service, &initial, source, &bytes, encoding);
        let initial_route = retained_parser_route(&service, initial.generation, identities.0);
        for (path, raw) in [("Peer.fs", &peer), ("Mixed.fs", &mixed)] {
            assert_project_member(&service, &initial, path, raw, "unknown", None);
        }
        let noop = index(&mut service, repository.path(), options);
        assert_eq!(noop.receipt(), &initial);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
        fs::write(&path, &changed_bytes).unwrap();
        let mutation = index(&mut service, repository.path(), options);
        assert_eq!(mutation.evidence().changed_files, 1);
        assert_eq!(mutation.evidence().rebuilt_files, 1);
        let changed = mutation.receipt().clone();
        assert_ne!(changed.generation, initial.generation);
        assert_eq!(
            assert_native_function_generation(
                &service,
                &changed,
                &changed_source,
                &changed_bytes,
                encoding,
            ),
            identities
        );
        let changed_route = retained_parser_route(&service, changed.generation, identities.0);
        drop(service);
        let mut reopened =
            FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline())
                .unwrap();
        for (receipt, text, raw, route) in [
            (&initial, source, &bytes, initial_route),
            (
                &changed,
                changed_source.as_str(),
                &changed_bytes,
                changed_route,
            ),
        ] {
            assert_eq!(
                assert_native_function_generation(&reopened, receipt, text, raw, encoding),
                identities
            );
            assert_eq!(
                retained_parser_route(&reopened, receipt.generation, identities.0),
                route
            );
            for (path, raw) in [("Peer.fs", &peer), ("Mixed.fs", &mixed)] {
                assert_project_member(&reopened, receipt, path, raw, "unknown", None);
            }
        }
        let noop = index(&mut reopened, repository.path(), options);
        assert_eq!(noop.receipt(), &changed);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
    }
}

fn assert_native_function_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    bytes: &[u8],
    encoding: &str,
) -> (FileId, BTreeSet<SymbolId>) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let record = document
        .files
        .iter()
        .find(|record| record.path == "Native.fs")
        .unwrap();
    let file = record.id;
    assert_eq!(record.language, "fsharp");
    assert_eq!(record.byte_length, u64::try_from(bytes.len()).unwrap());
    assert_eq!(record.content_hash, content_hash(bytes));
    assert_eq!(record.encoding, encoding);
    let reference = record.evidence.source.as_ref().unwrap();
    assert_eq!(reference.repository(), receipt.repository);
    assert_eq!(reference.generation(), receipt.generation);
    assert_eq!(reference.span().file(), file);
    assert_eq!(reference.span().start_byte(), 0);
    assert_eq!(reference.span().end_byte(), record.byte_length);
    assert_eq!(reference.content_hash(), record.content_hash);
    exact_source(service, reference, "fsharp", bytes);
    let body = source.rsplit_once(" = ").unwrap().1;
    let body = body.trim_end_matches(['\r', '\n']);
    let unsupported_body = match body {
        "value" => false,
        "value + 1" => true,
        other => panic!("unsupported fixture function body: {other}"),
    };
    let coverage = service
        .source_file_coverage_until(receipt.generation, file, &deadline())
        .unwrap();
    assert_eq!(coverage.language, "fsharp");
    assert_eq!(coverage.tier, AnalysisTier::TierD);
    assert_eq!(
        coverage.status,
        if unsupported_body {
            CoverageStatus::Bounded
        } else {
            CoverageStatus::Complete
        }
    );
    assert_eq!(coverage.reason, None);
    let entities = document
        .entities
        .iter()
        .filter(|entity| {
            entity
                .evidence
                .source
                .as_ref()
                .is_some_and(|reference| reference.span().file() == file)
        })
        .collect::<Vec<_>>();
    assert_eq!(entities.len(), 3);
    let module = entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Module)
        .unwrap();
    assert_eq!(module.canonical_name, "Native.fs");
    let function = entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function)
        .unwrap();
    let parameter = entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Parameter)
        .unwrap();
    assert_eq!(function.canonical_name, "identity");
    assert_eq!(parameter.canonical_name, "value");
    assert_eq!(function.container, Some(ContainerRef::Entity(module.id)));
    assert_eq!(parameter.container, Some(ContainerRef::Entity(function.id)));
    let check_source = |reference: &SourceRef| {
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.span().file(), file);
        assert_eq!(reference.content_hash(), content_hash(bytes));
        let written = bytes
            .get(
                usize::try_from(reference.span().start_byte()).unwrap()
                    ..usize::try_from(reference.span().end_byte()).unwrap(),
            )
            .unwrap();
        exact_source(service, reference, "fsharp", written);
    };
    check_source(module.evidence.source.as_ref().unwrap());
    for entity in [function, parameter] {
        check_source(entity.evidence.source.as_ref().unwrap());
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.file == file
                    && site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        check_source(&definitions[0].source);
        exact_source(
            service,
            &definitions[0].source,
            "fsharp",
            &typed_formal_leaf_bytes(&entity.canonical_name, encoding),
        );
        let located = service
            .code_locate_with_filters_and_budget(
                receipt.generation,
                entity.canonical_name.clone(),
                LocateMode::Exact,
                vec!["fsharp".to_owned()],
                vec!["Native.fs".to_owned()],
                8,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(located.data.generation, receipt.generation);
        let hit = located
            .data
            .hits
            .iter()
            .find(|hit| hit.file == file && hit.symbol == Some(entity.id))
            .unwrap();
        assert_eq!(hit.language, "fsharp");
        check_source(hit.source.as_ref().unwrap());
    }
    let read_start = source.rfind("value").unwrap();
    let start = typed_formal_leaf_bytes(source.get(..read_start).unwrap(), encoding)
        .len()
        .checked_add(if encoding == "utf-8" { 0 } else { 2 })
        .unwrap();
    let end = start
        .checked_add(typed_formal_leaf_bytes("value", encoding).len())
        .unwrap();
    let span = SourceSpan::new(
        file,
        u64::try_from(start).unwrap(),
        u64::try_from(end).unwrap(),
    )
    .unwrap();
    let reads = document
        .occurrences
        .iter()
        .filter(|site| site.file == file && site.role == OccurrenceRole::Reference)
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 1);
    let read = reads[0];
    assert_eq!(read.source.span(), span);
    assert_eq!(read.enclosing, Some(function.id));
    assert_eq!(
        read.target,
        OccurrenceTarget::Resolved {
            symbol: parameter.id
        }
    );
    check_source(&read.source);
    let gaps = document
        .skipped_regions
        .iter()
        .filter(|gap| gap.source.span().file() == file)
        .collect::<Vec<_>>();
    assert_eq!(gaps.len(), usize::from(unsupported_body));
    if let Some(gap) = gaps.first() {
        let written = typed_formal_leaf_bytes(body, encoding);
        let body_span = SourceSpan::new(
            file,
            u64::try_from(start).unwrap(),
            u64::try_from(start.checked_add(written.len()).unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            gap.detail,
            "fsharp-body-binding-and-call-analysis-unavailable"
        );
        assert_eq!(gap.domain, rootlight_ir::FactDomain::Relations);
        assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
        assert_eq!(gap.repository, receipt.repository);
        assert_eq!(gap.generation, receipt.generation);
        assert_eq!(gap.source.span(), body_span);
        assert_eq!(gap.evidence.source.as_ref(), Some(&gap.source));
        assert_eq!(gap.provenance, function.provenance);
        check_source(&gap.source);
        exact_source(service, &gap.source, "fsharp", &written);
    }
    let relations = document
        .relations
        .iter()
        .filter(|relation| relation.subject == RelationEndpoint::Occurrence(read.id))
        .collect::<Vec<_>>();
    assert_eq!(relations.len(), 1);
    assert_eq!(relations[0].predicate, RelationPredicate::RefersTo);
    assert_eq!(relations[0].subject, RelationEndpoint::Occurrence(read.id));
    assert_eq!(relations[0].object, RelationEndpoint::Entity(parameter.id));
    assert_eq!(relations[0].evidence.source.as_ref(), Some(&read.source));
    assert!(!document.relations.iter().any(|relation| {
        matches!(
            relation.predicate,
            RelationPredicate::Calls | RelationPredicate::DispatchCandidate
        ) && relation
            .evidence
            .source
            .as_ref()
            .is_some_and(|reference| reference.span().file() == file)
    }));
    let relationships = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([function.id]),
            vec![RelationFamily::References],
            Some(RelationDirection::Outbound),
            0,
            8,
            0,
            &deadline(),
        )
        .unwrap();
    assert_eq!(relationships.data.generation, receipt.generation);
    assert!(!relationships.data.truncated);
    let targets = relationships
        .data
        .groups
        .iter()
        .flat_map(|group| &group.items)
        .collect::<Vec<_>>();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].symbol, parameter.id);
    assert_eq!(targets[0].source_refs, vec![read.source.clone()]);
    (file, BTreeSet::from([function.id, parameter.id]))
}

#[test]
fn fsharp_native_admission_resource_limits_preserve_unknown_sources_after_reopen() {
    let source = "let identity value = value\n";
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    fs::write(repository.path().join("Native.fs"), source).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(
        Some("version='1.3'\n[storage]\nretained_generations=2\n"),
        2,
    )
    .unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let ordinary_limits = service.analysis_limits.clone();
    let mut limited = AnalysisLimits::new(
        ordinary_limits.max_source_bytes(),
        1,
        ordinary_limits.max_syntax_depth(),
        ordinary_limits.max_embedded_ranges(),
        ordinary_limits.max_reported_memory_bytes(),
        ordinary_limits.syntax_stream().clone(),
        ordinary_limits.ir_stream().clone(),
        ordinary_limits.ir().clone(),
    )
    .unwrap();
    if let Some(project) = ordinary_limits.project() {
        limited = limited.with_project_limits(project);
    }
    service.analysis_limits = limited.clone();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let cold = index(&mut service, repository.path(), options);
    assert_eq!(cold.evidence().rebuilt_files, 0);
    let receipt = cold.receipt().clone();
    let file = assert_native_admission_resource_bounded(&service, &receipt, source.as_bytes());
    let bounded_route = retained_parser_route(&service, receipt.generation, file);
    let noop = index(&mut service, repository.path(), options);
    assert_eq!(noop.receipt(), &receipt);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    reopened.analysis_limits = limited;
    assert_eq!(
        assert_native_admission_resource_bounded(&reopened, &receipt, source.as_bytes()),
        file,
    );
    assert_eq!(
        retained_parser_route(&reopened, receipt.generation, file),
        bounded_route
    );
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt(), &receipt);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    reopened.analysis_limits = ordinary_limits;
    let admitted = index(
        &mut reopened,
        repository.path(),
        FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
    );
    assert_eq!(admitted.evidence().rebuilt_files, 1);
    assert_eq!(
        assert_native_function_generation(
            &reopened,
            admitted.receipt(),
            source,
            source.as_bytes(),
            "utf-8",
        )
        .0,
        file,
    );
    assert_ne!(
        retained_parser_route(&reopened, admitted.receipt().generation, file),
        bounded_route
    );
    assert_eq!(
        assert_native_admission_resource_bounded(&reopened, &receipt, source.as_bytes()),
        file,
    );
}

fn assert_native_admission_resource_bounded(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    bytes: &[u8],
) -> FileId {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(file.path, "Native.fs");
    assert_eq!(file.language, "unknown");
    assert_eq!(file.encoding, "utf-8");
    assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(bytes));
    let reference = file.evidence.source.as_ref().unwrap();
    assert_eq!(reference.repository(), receipt.repository);
    assert_eq!(reference.generation(), receipt.generation);
    assert_eq!(
        reference.span(),
        SourceSpan::new(file.id, 0, file.byte_length).unwrap()
    );
    assert_eq!(reference.content_hash(), file.content_hash);
    exact_source(service, reference, "unknown", bytes);
    assert!(document.entities.is_empty());
    assert!(document.occurrences.is_empty());
    assert!(document.relations.is_empty());
    assert!(!document.diagnostics.iter().any(|diagnostic| {
        matches!(
            diagnostic.code.as_str(),
            "unsupported-language" | "unsupported-encoding"
        )
    }));
    let diagnostics = document
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "language-admission-resource-bounded")
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 1);
    let message = "bounded native language admission exhausted its analysis resources and source remains available without a language determination";
    assert!(
        receipt.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "language-admission-resource-bounded"
                && diagnostic.message == message
        }),
        "bounded native admission must retain its precise public diagnostic: {:?}",
        receipt.diagnostics
    );
    assert_eq!(diagnostics[0].message, message);
    assert_eq!(diagnostics[0].source.as_ref(), Some(reference));
    assert_eq!(diagnostics[0].evidence.source.as_ref(), Some(reference));
    assert_eq!(diagnostics[0].coverage_effect, CoverageStatus::Unknown);
    assert_eq!(diagnostics[0].provenance, file.provenance);
    let producer = document
        .provenance
        .iter()
        .find(|proof| proof.id == diagnostics[0].provenance)
        .unwrap();
    assert_eq!(
        producer.binary_digest,
        content_hash(LANGUAGE_DISPOSITION_PROVIDER_SEED)
    );
    assert_eq!(
        producer.rule.as_deref(),
        Some("language-admission-resource-bounded")
    );
    assert_eq!(producer.input_sources, std::slice::from_ref(reference));
    assert_eq!(producer.evidence_sources, std::slice::from_ref(reference));
    assert!(!document.skipped_regions.is_empty());
    for gap in &document.skipped_regions {
        assert_eq!(gap.reason, SkippedRegionReason::ResourceLimit);
        assert_eq!(gap.source, *reference);
        assert_eq!(gap.evidence.source.as_ref(), Some(reference));
    }
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(coverage.language, "unknown");
    assert_eq!(coverage.tier, AnalysisTier::TierD);
    assert_eq!(coverage.status, CoverageStatus::Unknown);
    assert_eq!(
        coverage.reason,
        Some(FirstSliceSourceCoverageReason::ResourceBounded)
    );
    file.id
}

#[test]
fn fsharp_declaration_routes_preserve_type_heads_after_reopen() {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let alias = "namespace Example.Types\ntype Alias<'T> = 'T list\n";
    let primary = "module Example.Classes\ntype Holder [<ConstructorMarker>] (seed: int, offset: int) =\n    member this.Value = seed + offset\n";
    let updated_primary = primary.replace(
        "this.Value = seed + offset",
        "this.Value = seed + offset + 1",
    );
    fs::write(repository.path().join("Alias.fs"), alias).unwrap();
    fs::write(repository.path().join("Holder.fs"), primary).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(
        Some("version='1.3'\n[storage]\nretained_generations=4\n"),
        4,
    )
    .unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let initial = index(&mut service, repository.path(), options);
    assert_eq!(initial.evidence().rebuilt_files, 2);
    let alias_identity = assert_default_fsharp_declaration(
        &service,
        initial.receipt(),
        "Alias.fs",
        alias,
        "Alias",
        EntityKind::TypeAlias,
    );
    let primary_identity = assert_default_fsharp_declaration(
        &service,
        initial.receipt(),
        "Holder.fs",
        primary,
        "Holder",
        EntityKind::Class,
    );
    assert_default_fsharp_declaration(
        &service,
        initial.receipt(),
        "Holder.fs",
        primary,
        "Value",
        EntityKind::Property,
    );
    let original = initial.receipt().clone();
    let noop = index(&mut service, repository.path(), options);
    assert_eq!(noop.receipt().generation, original.generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    fs::write(repository.path().join("Holder.fs"), &updated_primary).unwrap();
    let mutation = index(&mut service, repository.path(), options);
    assert_eq!(mutation.evidence().changed_files, 1);
    assert_eq!(mutation.evidence().rebuilt_files, 1);
    assert_eq!(mutation.evidence().reused_files, 1);
    assert_eq!(
        assert_default_fsharp_declaration(
            &service,
            mutation.receipt(),
            "Alias.fs",
            alias,
            "Alias",
            EntityKind::TypeAlias,
        ),
        alias_identity
    );
    assert_eq!(
        assert_default_fsharp_declaration(
            &service,
            mutation.receipt(),
            "Holder.fs",
            &updated_primary,
            "Holder",
            EntityKind::Class,
        ),
        primary_identity
    );
    let changed = mutation.receipt().clone();
    let clean = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
    );
    assert!(changed.logical_snapshot.is_some());
    assert_eq!(clean.receipt().logical_snapshot, changed.logical_snapshot);
    assert_eq!(clean.evidence().rebuilt_files, 2);
    assert_eq!(
        assert_default_fsharp_declaration(
            &service,
            clean.receipt(),
            "Holder.fs",
            &updated_primary,
            "Holder",
            EntityKind::Class,
        ),
        primary_identity
    );
    let last = clean.receipt().clone();
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_eq!(
        assert_default_fsharp_declaration(
            &reopened,
            &original,
            "Alias.fs",
            alias,
            "Alias",
            EntityKind::TypeAlias,
        ),
        alias_identity
    );
    assert_eq!(
        assert_default_fsharp_declaration(
            &reopened,
            &original,
            "Holder.fs",
            primary,
            "Holder",
            EntityKind::Class,
        ),
        primary_identity
    );
    assert_eq!(
        assert_default_fsharp_declaration(
            &reopened,
            &last,
            "Alias.fs",
            alias,
            "Alias",
            EntityKind::TypeAlias,
        ),
        alias_identity
    );
    assert_eq!(
        assert_default_fsharp_declaration(
            &reopened,
            &last,
            "Holder.fs",
            &updated_primary,
            "Holder",
            EntityKind::Class,
        ),
        primary_identity
    );
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt().generation, last.generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
}

#[test]
fn fsharp_owned_type_members_preserve_queries_and_identity_after_reopen() {
    for (source, name, kind, interface) in [
        (
            "type Container =\n    new () = {}\n    static member Value = 1\n",
            "Value",
            EntityKind::Property,
            false,
        ),
        (
            "type Container =\n    static member Value = 1\n",
            "Value",
            EntityKind::Property,
            false,
        ),
        (
            "type Container =\n    member this.Value = 1\n",
            "Value",
            EntityKind::Property,
            false,
        ),
        (
            "type Contract =\n    abstract Read: unit -> int\nlet following = 1\n",
            "following",
            EntityKind::Variable,
            true,
        ),
    ] {
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let path = repository.path().join("Owned.fs");
        let peer = "let peer = 1\n";
        fs::write(&path, source).unwrap();
        fs::write(repository.path().join("Peer.fs"), peer).unwrap();
        let changed_source = source.replace("= 1\n", "= 2\n");
        assert_eq!(source.matches("= 1\n").count(), 1);
        let policy = FirstSliceStoragePolicy::resolve_user_config(
            Some("version='1.3'\n[storage]\nretained_generations=4\n"),
            4,
        )
        .unwrap();
        let mut service = FirstSliceService::new_durable_with_policy(
            policy.clone(),
            paths.state_dir(),
            &deadline(),
        )
        .unwrap();
        let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
        let cold = index(&mut service, repository.path(), options);
        let initial = cold.receipt().clone();
        let root = RepositoryRoot::open(initial.repository, repository.path()).unwrap();
        let manifest = rootlight_discovery::discover(
            &root,
            &service.config,
            &DiscoveryPolicy::build(Vec::new(), false).unwrap(),
            DiscoveryLimits::from_config(&service.config),
            &deadline(),
        )
        .unwrap();
        let input = manifest
            .inputs
            .iter()
            .find(|input| input.path == "Owned.fs")
            .unwrap();
        assert!(input.language_signals.is_empty());
        assert!(input.project_language_witnesses.is_empty());
        assert_eq!(input.content_hash, content_hash(source.as_bytes()));
        assert_eq!(cold.evidence().rebuilt_files, 1);
        let check_generation = |service: &FirstSliceService,
                                receipt: &FirstSliceIndexReceipt,
                                source: &str| {
            let identity =
                assert_default_fsharp_declaration(service, receipt, "Owned.fs", source, name, kind);
            assert_project_member(
                service,
                receipt,
                "Peer.fs",
                peer.as_bytes(),
                "unknown",
                None,
            );
            let snapshot = service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap();
            let document = snapshot.document();
            let gaps = document
                .skipped_regions
                .iter()
                .filter(|gap| gap.source.span().file() == identity.0)
                .collect::<Vec<_>>();
            assert!(!gaps.iter().any(|gap| matches!(
                gap.reason,
                SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
            )));
            assert!(
                !gaps
                    .iter()
                    .any(|gap| gap.domain == rootlight_ir::FactDomain::Entities
                        && matches!(
                            gap.detail.as_str(),
                            "fsharp-type-representation-ambiguous"
                                | "stable-scope-identity-unavailable"
                        ))
            );
            let (owner_name, owner_kind, member_name, member_kind) = if interface {
                (
                    "Contract",
                    EntityKind::Interface,
                    "Read",
                    EntityKind::Method,
                )
            } else {
                (
                    "Container",
                    EntityKind::Class,
                    "Value",
                    EntityKind::Property,
                )
            };
            let owner = assert_default_fsharp_declaration(
                service, receipt, "Owned.fs", source, owner_name, owner_kind,
            );
            let member = assert_default_fsharp_declaration(
                service,
                receipt,
                "Owned.fs",
                source,
                member_name,
                member_kind,
            );
            let declaration = document
                .entities
                .iter()
                .find(|entity| entity.id == member.1)
                .unwrap();
            assert_eq!(declaration.container, Some(ContainerRef::Entity(owner.1)));
            let containment = document
                .relations
                .iter()
                .filter(|relation| {
                    relation.predicate == RelationPredicate::Contains
                        && relation.subject == RelationEndpoint::Entity(owner.1)
                        && relation.object == RelationEndpoint::Entity(member.1)
                })
                .collect::<Vec<_>>();
            assert_eq!(containment.len(), 1);
            let reference = containment[0].evidence.source.as_ref().unwrap();
            assert_eq!(reference.repository(), receipt.repository);
            assert_eq!(reference.generation(), receipt.generation);
            assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
            exact_source(
                service,
                reference,
                "fsharp",
                &source.as_bytes()[usize::try_from(reference.span().start_byte()).unwrap()
                    ..usize::try_from(reference.span().end_byte()).unwrap()],
            );
            let located = service
                .code_locate_with_filters_and_budget(
                    receipt.generation,
                    name.to_owned(),
                    LocateMode::Text,
                    vec!["fsharp".to_owned()],
                    vec!["Owned.fs".to_owned()],
                    8,
                    0,
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            assert_eq!(located.data.generation, receipt.generation);
            assert!(!located.data.truncated);
            let hit = located
                .data
                .hits
                .iter()
                .find(|hit| hit.file == identity.0 && hit.symbol == Some(identity.1))
                .unwrap();
            let reference = hit.source.as_ref().unwrap();
            assert_eq!(reference.repository(), receipt.repository);
            assert_eq!(reference.generation(), receipt.generation);
            assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
            exact_source(
                service,
                reference,
                "fsharp",
                &source.as_bytes()[usize::try_from(reference.span().start_byte()).unwrap()
                    ..usize::try_from(reference.span().end_byte()).unwrap()],
            );
            (identity, owner.1, member.1)
        };
        let identity = check_generation(&service, &initial, source);
        let noop = index(&mut service, repository.path(), options);
        assert_eq!(noop.receipt().generation, initial.generation);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
        fs::write(&path, &changed_source).unwrap();
        let mutation = index(&mut service, repository.path(), options);
        assert_eq!(mutation.evidence().changed_files, 1);
        assert_eq!(mutation.evidence().rebuilt_files, 1);
        let changed = mutation.receipt().clone();
        assert_ne!(changed.generation, initial.generation);
        assert_eq!(
            check_generation(&service, &changed, &changed_source),
            identity
        );
        let clean = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
        );
        assert!(changed.logical_snapshot.is_some());
        assert_eq!(clean.receipt().logical_snapshot, changed.logical_snapshot);
        assert_eq!(clean.evidence().rebuilt_files, 1);
        let last = clean.receipt().clone();
        assert_eq!(check_generation(&service, &last, &changed_source), identity);
        drop(service);
        let mut reopened =
            FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline())
                .unwrap();
        assert_eq!(check_generation(&reopened, &initial, source), identity);
        assert_eq!(
            check_generation(&reopened, &last, &changed_source),
            identity
        );
        let noop = index(&mut reopened, repository.path(), options);
        assert_eq!(noop.receipt().generation, last.generation);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
    }
}

#[test]
fn fsharp_declaration_routes_preserve_return_annotations_at_sample_boundary() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let prefix = "module Example\nlet prior = 1\n";
    let header = "let build\n    (value: int)\n    : int =\n";
    let body = "        value\n";
    let padding = 8_192 - prefix.len() - 3 - header.len() - "        va".len();
    let source = format!("{prefix}//{}\n{header}{body}", " ".repeat(padding));
    let body_start = source.rfind(body).unwrap();
    assert_eq!(body_start + "        va".len(), 8_192);
    assert_eq!(source.get(8_190..8_195), Some("value"));
    assert!(source.len() > 8_192);
    fs::write(repository.path().join("Boundary.fs"), &source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let initial = index(&mut service, repository.path(), options);
    assert_eq!(initial.evidence().rebuilt_files, 1);
    let function = assert_default_fsharp_declaration(
        &service,
        initial.receipt(),
        "Boundary.fs",
        &source,
        "build",
        EntityKind::Function,
    );
    assert_default_fsharp_declaration(
        &service,
        initial.receipt(),
        "Boundary.fs",
        &source,
        "prior",
        EntityKind::Variable,
    );
    let parameter = assert_default_fsharp_declaration(
        &service,
        initial.receipt(),
        "Boundary.fs",
        &source,
        "value",
        EntityKind::Parameter,
    );
    let snapshot = service
        .loaded_generation_snapshot(initial.receipt().generation)
        .unwrap();
    let document = snapshot.document();
    let read_span = SourceSpan::new(function.0, 8_190, 8_195).unwrap();
    let read = document
        .occurrences
        .iter()
        .find(|site| {
            site.file == function.0
                && site.role == OccurrenceRole::Reference
                && site.source.span() == read_span
        })
        .unwrap();
    assert_eq!(read.enclosing, Some(function.1));
    assert_eq!(
        read.target,
        OccurrenceTarget::Resolved {
            symbol: parameter.1
        }
    );
    exact_source(&service, &read.source, "fsharp", b"value");
    assert!(document.relations.iter().any(|relation| {
        relation.subject == RelationEndpoint::Occurrence(read.id)
            && relation.predicate == RelationPredicate::RefersTo
            && relation.object == RelationEndpoint::Entity(parameter.1)
            && relation.evidence.source.as_ref() == Some(&read.source)
    }));
    assert_eq!(
        service
            .source_file_coverage_until(initial.receipt().generation, function.0, &deadline(),)
            .unwrap()
            .status,
        CoverageStatus::Complete
    );
    let noop = index(&mut service, repository.path(), options);
    assert_eq!(noop.receipt().generation, initial.receipt().generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
}

#[test]
fn fsharp_declaration_routes_preserve_utf16_return_annotations_at_sample_boundary() {
    for encoding in ["utf-16le", "utf-16be"] {
        let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let prefix = "module Example\nlet prior = 1\n";
        let header = "let build\n    (value: int)\n    : int =\n";
        let body = "        value\n";
        // The BOM and two-byte ASCII units exhaust the original-byte budget
        // before this decoded UTF-8 prefix reaches its own byte ceiling.
        let padding = 4_095 - prefix.len() - 3 - header.len() - "        v".len();
        let source = format!("{prefix}//{}\n{header}{body}", " ".repeat(padding));
        assert_eq!(source.rfind(body).unwrap() + "        v".len(), 4_095);
        let mut bytes = if encoding == "utf-16le" {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        bytes.extend(typed_formal_leaf_bytes(&source, encoding));
        let written_body = typed_formal_leaf_bytes("value", encoding);
        assert_eq!(bytes.get(8_190..8_200), Some(written_body.as_slice()));
        assert!(bytes.len() > 8_192);
        fs::write(repository.path().join("Boundary.fs"), &bytes).unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
        let initial = index(&mut service, repository.path(), options);
        assert_eq!(initial.evidence().rebuilt_files, 1, "{encoding}");
        let snapshot = service
            .loaded_generation_snapshot(initial.receipt().generation)
            .unwrap();
        let document = snapshot.document();
        assert_eq!(document.files.len(), 1);
        let file = &document.files[0];
        assert_eq!(file.language, "fsharp", "{encoding}");
        assert_eq!(file.encoding, encoding);
        assert_eq!(file.content_hash, content_hash(&bytes));
        assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
        let check_source = |reference: &SourceRef| {
            assert_eq!(reference.repository(), initial.receipt().repository);
            assert_eq!(reference.generation(), initial.receipt().generation);
            assert_eq!(reference.span().file(), file.id);
            assert_eq!(reference.content_hash(), file.content_hash);
            let written = bytes
                .get(
                    usize::try_from(reference.span().start_byte()).unwrap()
                        ..usize::try_from(reference.span().end_byte()).unwrap(),
                )
                .unwrap();
            exact_source(&service, reference, "fsharp", written);
        };
        let full = file.evidence.source.as_ref().unwrap();
        assert_eq!(
            full.span(),
            SourceSpan::new(file.id, 0, file.byte_length).unwrap()
        );
        check_source(full);
        let mut identities = BTreeMap::new();
        for (name, kind) in [
            ("build", EntityKind::Function),
            ("value", EntityKind::Parameter),
            ("prior", EntityKind::Variable),
        ] {
            let entities = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.language == "fsharp"
                        && entity.canonical_name == name
                        && entity.kind == kind
                        && entity
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|reference| reference.span().file() == file.id)
                })
                .collect::<Vec<_>>();
            assert_eq!(entities.len(), 1, "{encoding}: authored {name}");
            let entity = entities[0];
            identities.insert(name, entity.id);
            check_source(entity.evidence.source.as_ref().unwrap());
            let definitions = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.file == file.id
                        && site.role == OccurrenceRole::Definition
                        && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                })
                .collect::<Vec<_>>();
            assert_eq!(definitions.len(), 1, "{encoding}: definition {name}");
            check_source(&definitions[0].source);
            exact_source(
                &service,
                &definitions[0].source,
                "fsharp",
                &typed_formal_leaf_bytes(name, encoding),
            );
            let located = service
                .code_locate_with_filters_and_budget(
                    initial.receipt().generation,
                    name.to_owned(),
                    LocateMode::Exact,
                    vec!["fsharp".to_owned()],
                    vec!["Boundary.fs".to_owned()],
                    8,
                    0,
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            let hit = located
                .data
                .hits
                .iter()
                .find(|hit| hit.file == file.id && hit.symbol == Some(entity.id))
                .unwrap();
            check_source(hit.source.as_ref().unwrap());
        }
        let parameter = document
            .entities
            .iter()
            .find(|entity| entity.id == identities["value"])
            .unwrap();
        assert_eq!(
            parameter.container,
            Some(ContainerRef::Entity(identities["build"]))
        );
        let read_span = SourceSpan::new(file.id, 8_190, 8_200).unwrap();
        let reads = document
            .occurrences
            .iter()
            .filter(|site| {
                site.file == file.id
                    && site.role == OccurrenceRole::Reference
                    && site.source.span() == read_span
            })
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 1);
        let read = reads[0];
        assert_eq!(read.enclosing, Some(identities["build"]));
        assert_eq!(
            read.target,
            OccurrenceTarget::Resolved {
                symbol: identities["value"]
            }
        );
        check_source(&read.source);
        let edges = document
            .relations
            .iter()
            .filter(|relation| relation.subject == RelationEndpoint::Occurrence(read.id))
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].predicate, RelationPredicate::RefersTo);
        assert_eq!(
            edges[0].object,
            RelationEndpoint::Entity(identities["value"])
        );
        assert_eq!(edges[0].evidence.source.as_ref(), Some(&read.source));
        let relationships = service
            .symbol_relationships(
                initial.receipt().generation,
                BTreeSet::from([identities["build"]]),
                vec![RelationFamily::References],
                Some(RelationDirection::Outbound),
                0,
                8,
                0,
                &deadline(),
            )
            .unwrap();
        assert!(!relationships.data.truncated);
        let targets = relationships
            .data
            .groups
            .iter()
            .flat_map(|group| &group.items)
            .collect::<Vec<_>>();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].symbol, identities["value"]);
        assert_eq!(targets[0].source_refs, vec![read.source.clone()]);
        assert!(document.skipped_regions.is_empty());
        assert!(
            !document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls)
        );
        let coverage = service
            .source_file_coverage_until(initial.receipt().generation, file.id, &deadline())
            .unwrap();
        assert_eq!(coverage.language, "fsharp");
        assert_eq!(coverage.tier, AnalysisTier::TierD);
        assert_eq!(coverage.status, CoverageStatus::Complete);
        assert_eq!(coverage.reason, None);
        let noop = index(&mut service, repository.path(), options);
        assert_eq!(noop.receipt().generation, initial.receipt().generation);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
    }
}

fn assert_default_fsharp_declaration(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    source: &str,
    name: &str,
    kind: EntityKind,
) -> (FileId, SymbolId) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == path)
        .unwrap();
    assert_eq!(file.language, "fsharp", "{path}");
    assert_eq!(file.encoding, "utf-8");
    assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(source.as_bytes()));
    let check_source = |reference: &SourceRef| {
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), file.content_hash);
        let written = source
            .as_bytes()
            .get(
                usize::try_from(reference.span().start_byte()).unwrap()
                    ..usize::try_from(reference.span().end_byte()).unwrap(),
            )
            .unwrap();
        exact_source(service, reference, "fsharp", written);
    };
    let full = file.evidence.source.as_ref().unwrap();
    assert_eq!(
        full.span(),
        SourceSpan::new(file.id, 0, file.byte_length).unwrap()
    );
    check_source(full);
    let entities = document
        .entities
        .iter()
        .filter(|entity| {
            entity.language == "fsharp"
                && entity.canonical_name == name
                && entity.kind == kind
                && entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|reference| reference.span().file() == file.id)
        })
        .collect::<Vec<_>>();
    assert_eq!(entities.len(), 1, "{path}: exact authored {name}");
    let entity = entities[0];
    check_source(entity.evidence.source.as_ref().unwrap());
    let definitions = document
        .occurrences
        .iter()
        .filter(|site| {
            site.file == file.id
                && site.role == OccurrenceRole::Definition
                && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1, "{path}: exact definition {name}");
    check_source(&definitions[0].source);
    exact_source(service, &definitions[0].source, "fsharp", name.as_bytes());
    let located = service
        .code_locate_with_filters_and_budget(
            receipt.generation,
            name.to_owned(),
            LocateMode::Exact,
            vec!["fsharp".to_owned()],
            vec![path.to_owned()],
            8,
            0,
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(located.data.generation, receipt.generation);
    let hit = located
        .data
        .hits
        .iter()
        .find(|hit| hit.file == file.id && hit.symbol == Some(entity.id))
        .unwrap();
    assert_eq!(hit.path, path);
    check_source(hit.source.as_ref().unwrap());
    let gaps = document
        .skipped_regions
        .iter()
        .filter(|gap| gap.source.span().file() == file.id)
        .collect::<Vec<_>>();
    for gap in &gaps {
        assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
        assert_eq!(gap.repository, receipt.repository);
        assert_eq!(gap.generation, receipt.generation);
        assert_eq!(gap.evidence.source.as_ref(), Some(&gap.source));
        check_source(&gap.source);
    }
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(coverage.language, "fsharp");
    assert_eq!(coverage.tier, AnalysisTier::TierD);
    assert_eq!(coverage.reason, None);
    assert_eq!(
        coverage.status,
        if gaps.is_empty() {
            CoverageStatus::Complete
        } else {
            CoverageStatus::Bounded
        }
    );
    assert!(!document.relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Calls
            && relation
                .evidence
                .source
                .as_ref()
                .is_some_and(|reference| reference.span().file() == file.id)
    }));
    let calls = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([entity.id]),
            vec![RelationFamily::Calls],
            Some(RelationDirection::Outbound),
            0,
            8,
            0,
            &deadline(),
        )
        .unwrap();
    assert!(!calls.data.truncated);
    assert_eq!(calls.data.returned_edges, 0);
    (file.id, entity.id)
}

#[test]
fn fsharp_content_routes_preserve_decoded_source_and_definitions() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let mut cases = Vec::new();
    for (newline_name, newline) in [("Lf", "\n"), ("CrLf", "\r\n")] {
        for (shape_name, prefix) in [
            (
                "ReturnAnnotation",
                "module Example\nlet prior = 1\nlet build\n    (value: int[])\n    (items: Example.Item option)\n    (pairs: (int * string) list)\n    : int =\n        value.Length\n",
            ),
            (
                "Open",
                "open Example.Support\nmodule Example =\n    let build value = value\n",
            ),
            (
                "Attribute",
                "[<AutoOpen>]\nmodule Example\nlet build value = value\n",
            ),
            (
                "Warning",
                "#nowarn \"9\"\nmodule Example\nlet build value = value\n",
            ),
            (
                "Combined",
                "#nowarn \"9\"\nopen Example.Support\n[<AutoOpen>]\nmodule Example =\n    let build value = value\n",
            ),
        ] {
            let prefix = prefix.replace('\n', newline);
            for (case_name, source) in [
                ("Complete", prefix.clone()),
                ("Sampled", format!("{prefix}//{}", " ".repeat(8192))),
            ] {
                for encoding in ["utf-8", "utf-16le", "utf-16be"] {
                    let bytes = if encoding == "utf-8" {
                        source.as_bytes().to_vec()
                    } else {
                        std::iter::once(0xfeff_u16)
                            .chain(source.encode_utf16())
                            .flat_map(|unit| {
                                if encoding == "utf-16le" {
                                    unit.to_le_bytes()
                                } else {
                                    unit.to_be_bytes()
                                }
                            })
                            .collect()
                    };
                    let path = format!("{shape_name}{newline_name}{case_name}{encoding}.fs");
                    fs::write(repository.path().join(&path), &bytes).unwrap();
                    let complete = shape_name == "Warning";
                    cases.push((
                        path,
                        bytes,
                        encoding,
                        complete,
                        shape_name == "ReturnAnnotation",
                        matches!(shape_name, "Attribute" | "Combined"),
                    ));
                }
            }
        }
    }
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(receipt.binary_inputs, 0);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), cases.len());
    for (path, bytes, encoding, complete, unsupported_body, unsupported_attribute) in cases {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, "fsharp", "{path}");
        assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
        assert_eq!(file.content_hash, content_hash(&bytes));
        let full = file.evidence.source.as_ref().unwrap();
        assert_eq!(full.repository(), receipt.repository);
        assert_eq!(full.generation(), receipt.generation);
        assert_eq!(full.span().file(), file.id);
        assert_eq!(full.span().start_byte(), 0);
        assert_eq!(full.span().end_byte(), file.byte_length);
        assert_eq!(full.content_hash(), file.content_hash);
        exact_source(&service, full, "fsharp", &bytes);
        let entity = document
            .entities
            .iter()
            .find(|entity| {
                entity.language == "fsharp"
                    && entity.canonical_name == "build"
                    && entity.kind == EntityKind::Function
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|reference| reference.span().file() == file.id)
            })
            .unwrap_or_else(|| panic!("missing authored function in {path}"));
        let definition = document
            .occurrences
            .iter()
            .find(|site| {
                site.file == file.id
                    && site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .unwrap();
        assert_eq!(definition.source.generation(), receipt.generation);
        assert_eq!(definition.source.repository(), receipt.repository);
        assert_eq!(definition.source.span().file(), file.id);
        assert_eq!(definition.source.content_hash(), file.content_hash);
        let identifier: Vec<u8> = match encoding {
            "utf-16le" => "build".encode_utf16().flat_map(u16::to_le_bytes).collect(),
            "utf-16be" => "build".encode_utf16().flat_map(u16::to_be_bytes).collect(),
            _ => b"build".to_vec(),
        };
        exact_source(&service, &definition.source, "fsharp", &identifier);
        let located = service
            .code_locate_with_filters_and_budget(
                receipt.generation,
                "build".to_owned(),
                LocateMode::Exact,
                Vec::new(),
                vec![path.clone()],
                8,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(located.data.generation, receipt.generation);
        let hit = located
            .data
            .hits
            .iter()
            .find(|hit| hit.symbol == Some(entity.id) && hit.file == file.id)
            .unwrap_or_else(|| panic!("missing exact function locate in {path}"));
        assert_eq!(hit.path, path);
        assert_eq!(hit.language, "fsharp");
        let reference = hit.source.as_ref().unwrap();
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), file.content_hash);
        exact_source(
            &service,
            reference,
            "fsharp",
            &bytes[usize::try_from(reference.span().start_byte()).unwrap()
                ..usize::try_from(reference.span().end_byte()).unwrap()],
        );
        let coverage = service
            .source_file_coverage_until(receipt.generation, file.id, &deadline())
            .unwrap();
        assert_eq!(coverage.language, "fsharp");
        assert_eq!(
            coverage.status,
            if complete {
                CoverageStatus::Complete
            } else {
                CoverageStatus::Bounded
            },
            "{path}"
        );
        assert_eq!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().file() == file.id),
            !complete,
            "{path}",
        );
        let body_gaps = document
            .skipped_regions
            .iter()
            .filter(|gap| {
                gap.source.span().file() == file.id
                    && gap.detail == "fsharp-body-binding-and-call-analysis-unavailable"
            })
            .collect::<Vec<_>>();
        assert_eq!(body_gaps.len(), usize::from(unsupported_body), "{path}");
        if let Some(gap) = body_gaps.first() {
            let expected = typed_formal_leaf_bytes("value.Length", encoding);
            let start = bytes
                .windows(expected.len())
                .position(|window| window == expected)
                .unwrap();
            assert_eq!(gap.domain, rootlight_ir::FactDomain::Relations);
            assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
            assert_eq!(gap.repository, receipt.repository);
            assert_eq!(gap.generation, receipt.generation);
            assert_eq!(gap.source.repository(), receipt.repository);
            assert_eq!(gap.source.generation(), receipt.generation);
            assert_eq!(gap.source.content_hash(), file.content_hash);
            assert_eq!(
                gap.source.span().start_byte(),
                u64::try_from(start).unwrap()
            );
            assert_eq!(
                gap.source.span().end_byte(),
                u64::try_from(start + expected.len()).unwrap(),
            );
            assert_eq!(gap.evidence.source.as_ref(), Some(&gap.source));
            assert_eq!(gap.provenance, entity.provenance);
            exact_source(&service, &gap.source, "fsharp", &expected);
        }
        let attribute_gaps = document
            .skipped_regions
            .iter()
            .filter(|gap| {
                gap.source.span().file() == file.id
                    && gap.detail == "fsharp-attribute-binding-analysis-unavailable"
            })
            .collect::<Vec<_>>();
        assert_eq!(
            attribute_gaps.len(),
            2 * usize::from(unsupported_attribute),
            "{path}",
        );
        let domains = attribute_gaps
            .iter()
            .map(|gap| gap.domain)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            domains,
            if unsupported_attribute {
                BTreeSet::from([
                    rootlight_ir::FactDomain::Occurrences,
                    rootlight_ir::FactDomain::Relations,
                ])
            } else {
                BTreeSet::new()
            },
            "{path}",
        );
        for gap in attribute_gaps {
            let expected = typed_formal_leaf_bytes("AutoOpen", encoding);
            let start = bytes
                .windows(expected.len())
                .position(|window| window == expected)
                .unwrap();
            assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
            assert_eq!(gap.repository, receipt.repository);
            assert_eq!(gap.generation, receipt.generation);
            assert_eq!(gap.source.repository(), receipt.repository);
            assert_eq!(gap.source.generation(), receipt.generation);
            assert_eq!(gap.source.content_hash(), file.content_hash);
            assert_eq!(
                gap.source.span().start_byte(),
                u64::try_from(start).unwrap()
            );
            assert_eq!(
                gap.source.span().end_byte(),
                u64::try_from(start + expected.len()).unwrap(),
            );
            assert_eq!(gap.evidence.source.as_ref(), Some(&gap.source));
            assert_eq!(gap.provenance, entity.provenance);
            exact_source(&service, &gap.source, "fsharp", &expected);
        }
    }
}

#[test]
fn fsharp_content_respects_exact_configuration_and_unsupported_overrides() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let implementation = "module Example.Core\nlet value = 1\n";
    let signature = "module Example.Core\nval value: int\n";
    let forth = ": square dup * ;\n";
    for (path, source) in [
        ("Default.fs", implementation),
        ("Blocked.fs", implementation),
        ("Contract.fs", signature),
        ("Explicit.fs", forth),
    ] {
        fs::write(repository.path().join(path), source).unwrap();
    }
    let policy = FirstSliceStoragePolicy::resolve_user_config(Some("version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nfs='fsharp'\n[extensions.source-languages.data.paths]\n'Blocked.fs'='unsupported-language'\n'Contract.fs'='fsharp-signature'\n"), 2).unwrap();
    let mut service = FirstSliceService::new_with_storage_policy(
        2,
        MAX_RETAINED_SOURCE_BYTES,
        None,
        None,
        policy,
    )
    .unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    for (path, language, source) in [
        ("Default.fs", "fsharp", implementation),
        ("Blocked.fs", "unsupported-language", implementation),
        ("Contract.fs", "fsharp-signature", signature),
        ("Explicit.fs", "fsharp", forth),
    ] {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, language, "{path}");
        let provenance = document
            .provenance
            .iter()
            .find(|record| record.id == file.provenance)
            .unwrap();
        assert_eq!(provenance.language, language);
        assert_eq!(provenance.tier, AnalysisTier::TierD);
        assert_eq!(
            provenance.producer_kind,
            if path == "Blocked.fs" {
                ProducerKind::Rule
            } else {
                ProducerKind::Parser
            }
        );
        exact_source(
            &service,
            file.evidence.source.as_ref().unwrap(),
            language,
            source.as_bytes(),
        );
    }
}

#[test]
fn fsharp_content_admission_rejects_conflicting_file_evidence_but_not_project_hints() {
    let service = FirstSliceService::new(2).unwrap();
    for path in [".fs", "dir/.fs", "Library.fs.bak"] {
        assert!(!is_shared_fsharp_source(path), "{path}");
    }
    for path in ["Library.fs", "dir/LIBRARY.FS"] {
        assert!(is_shared_fsharp_source(path), "{path}");
    }
    let signal = |language: &str, evidence| rootlight_discovery::LanguageSignal {
        language: language.to_owned(),
        evidence,
    };
    let mut input = ManifestInput {
        file: FileId::from_bytes([0; 20]),
        path: "Library.fs".to_owned(),
        content_hash: content_hash(b""),
        bytes: 0,
        encoding: "utf-8".to_owned(),
        class: InputClass::Source,
        language_signals: vec![signal("fsharp", LanguageEvidence::Content)],
        project_language_witnesses: Vec::new(),
        decisive_rule: None,
    };
    assert_eq!(
        supported_source_language(&input, &service.analyzers),
        Some("fsharp")
    );
    assert_eq!(detected_source_language(&input), Some("fsharp"));
    for evidence in [
        LanguageEvidence::Extension,
        LanguageEvidence::Filename,
        LanguageEvidence::Shebang,
        LanguageEvidence::Content,
    ] {
        input.language_signals.push(signal("rust", evidence));
        assert_eq!(supported_source_language(&input, &service.analyzers), None);
        assert_eq!(detected_source_language(&input), None);
        input.language_signals.pop();
    }
    for evidence in [LanguageEvidence::Manifest, LanguageEvidence::Context] {
        input.language_signals.push(signal("rust", evidence));
        assert_eq!(
            supported_source_language(&input, &service.analyzers),
            Some("fsharp")
        );
        assert_eq!(detected_source_language(&input), Some("fsharp"));
        input.language_signals.remove(0);
        assert_eq!(supported_source_language(&input, &service.analyzers), None);
        assert_eq!(detected_source_language(&input), None);
        input.language_signals = vec![signal("fsharp", LanguageEvidence::Content)];
    }
    input.language_signals.push(signal(
        "unsupported-language",
        LanguageEvidence::Configuration,
    ));
    assert_eq!(supported_source_language(&input, &service.analyzers), None);
    assert_eq!(
        detected_source_language(&input),
        Some("unsupported-language")
    );
    input.language_signals.pop();
    input.encoding = "utf-16-le".to_owned();
    assert_eq!(supported_source_language(&input, &service.analyzers), None);
}

#[test]
fn fsharp_content_truncation_distinguishes_prior_evidence_from_late_declarations() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bound = rootlight_discovery::MAX_CLASSIFICATION_BYTES;
    let mut incomplete_utf8 = "module Example\nlet value = 1\n// ".to_owned();
    incomplete_utf8.extend(std::iter::repeat_n(' ', bound - 1 - incomplete_utf8.len()));
    incomplete_utf8.push('λ');
    let mut late_declaration = "module Example\n".to_owned();
    late_declaration.extend(std::iter::repeat_n(' ', bound - late_declaration.len()));
    late_declaration.push_str("let value = 1\n");
    for (name, source) in [("Cut.fs", &incomplete_utf8), ("Late.fs", &late_declaration)] {
        fs::write(repository.path().join(name), source).unwrap();
    }
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    for (path, language, source) in [
        ("Cut.fs", "fsharp", incomplete_utf8),
        ("Late.fs", "unknown", late_declaration),
    ] {
        let file = snapshot
            .document()
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, language);
        assert_eq!(file.content_hash, content_hash(source.as_bytes()));
        let coverage = service
            .source_file_coverage_until(receipt.generation, file.id, &deadline())
            .unwrap();
        assert_eq!(coverage.tier, AnalysisTier::TierD);
        let entities = snapshot
            .document()
            .entities
            .iter()
            .filter(|entity| {
                entity.evidence.source.as_ref().is_some_and(|reference| {
                    reference.span().file() == file.id
                        && reference.generation() == receipt.generation
                })
            })
            .collect::<Vec<_>>();
        if language == "unknown" {
            assert!(entities.is_empty());
            assert_eq!(coverage.status, CoverageStatus::Unknown);
            assert_eq!(
                coverage.reason,
                Some(FirstSliceSourceCoverageReason::UnsupportedLanguage)
            );
        } else {
            assert!(entities.iter().any(|entity| {
                entity.display_name == "value"
                    && entity.kind == EntityKind::Variable
                    && entity.language == "fsharp"
            }));
            assert_ne!(
                coverage.reason,
                Some(FirstSliceSourceCoverageReason::UnsupportedLanguage)
            );
            assert!(!snapshot.document().skipped_regions.iter().any(|gap| {
                gap.source.span().file() == file.id && gap.reason == SkippedRegionReason::ParseError
            }));
        }
        exact_source(
            &service,
            file.evidence.source.as_ref().unwrap(),
            language,
            source.as_bytes(),
        );
    }
}

fn literal_project_input() -> ManifestInput {
    let source = b"let answer = 1\n";
    ManifestInput {
        file: FileId::from_bytes([1; 20]),
        path: "Member.fs".to_owned(),
        content_hash: content_hash(source),
        bytes: u64::try_from(source.len()).unwrap(),
        encoding: "utf-8".to_owned(),
        class: InputClass::Source,
        language_signals: vec![rootlight_discovery::LanguageSignal {
            language: "fsharp".to_owned(),
            evidence: LanguageEvidence::Project,
        }],
        project_language_witnesses: vec![rootlight_discovery::ProjectLanguageWitness {
            language: "fsharp".to_owned(),
            project_file: FileId::from_bytes([2; 20]),
            project_path: "Example.fsproj".to_owned(),
            project_content_hash: content_hash(
                b"<Project><ItemGroup><Compile Include=\"Member.fs\" /></ItemGroup></Project>\n",
            ),
        }],
        decisive_rule: None,
    }
}

#[test]
fn fsharp_project_selection_requires_source_bound_unanimous_witnesses() {
    let service = FirstSliceService::new(2).unwrap();
    let input = literal_project_input();
    assert_eq!(fsharp_content_language(&input), None);
    assert_eq!(detected_source_language(&input), Some("fsharp"));
    assert_eq!(
        supported_source_language(&input, &service.analyzers),
        Some("fsharp")
    );
    let signal = |language: &str, evidence| rootlight_discovery::LanguageSignal {
        language: language.to_owned(),
        evidence,
    };
    let mut invalid = input.clone();
    invalid.project_language_witnesses.clear();
    assert_eq!(detected_source_language(&invalid), None);
    invalid = input.clone();
    invalid.language_signals.clear();
    assert_eq!(detected_source_language(&invalid), None);
    for evidence in [LanguageEvidence::Manifest, LanguageEvidence::Context] {
        invalid.language_signals = vec![signal("fsharp", evidence)];
        assert_eq!(
            supported_source_language(&invalid, &service.analyzers),
            None
        );
    }
    for evidence in [
        LanguageEvidence::Project,
        LanguageEvidence::Content,
        LanguageEvidence::Extension,
        LanguageEvidence::Filename,
        LanguageEvidence::Shebang,
    ] {
        invalid = input.clone();
        invalid.language_signals.push(signal("rust", evidence));
        assert_eq!(detected_source_language(&invalid), None, "{evidence:?}");
        assert_eq!(
            supported_source_language(&invalid, &service.analyzers),
            None
        );
    }
    for path in [
        "../Example.fsproj",
        "/Example.fsproj",
        "dir/./Example.fsproj",
        "Example.fsproj.bak",
        ".fsproj",
    ] {
        invalid = input.clone();
        invalid.project_language_witnesses[0].project_path = path.to_owned();
        assert_eq!(detected_source_language(&invalid), None, "{path}");
    }
    invalid = input.clone();
    invalid.project_language_witnesses[0].project_file = input.file;
    assert_eq!(detected_source_language(&invalid), None);
    invalid = input.clone();
    invalid.project_language_witnesses[0].language = "rust".to_owned();
    assert_eq!(detected_source_language(&invalid), None);
    invalid
        .language_signals
        .push(signal("fsharp", LanguageEvidence::Content));
    assert_eq!(detected_source_language(&invalid), Some("fsharp"));
    invalid
        .language_signals
        .push(signal("rust", LanguageEvidence::Configuration));
    assert_eq!(
        supported_source_language(&invalid, &service.analyzers),
        Some("rust")
    );
    invalid = input;
    invalid.encoding = "utf-16-le".to_owned();
    assert_eq!(detected_source_language(&invalid), Some("fsharp"));
    assert_eq!(
        supported_source_language(&invalid, &service.analyzers),
        None
    );
}

#[test]
fn fsharp_project_routes_bind_member_witnesses_without_global_project_dependencies() {
    let input = literal_project_input();
    let original = file_parser_route_hash(&input, Some("fsharp"))
        .unwrap()
        .unwrap();
    for component in 0..4 {
        let mut changed = input.clone();
        let witness = &mut changed.project_language_witnesses[0];
        match component {
            0 => witness.language = "rust".to_owned(),
            1 => witness.project_file = FileId::from_bytes([3; 20]),
            2 => witness.project_path = "Renamed.fsproj".to_owned(),
            _ => witness.project_content_hash = content_hash(b"changed project document"),
        }
        assert_ne!(
            original,
            file_parser_route_hash(&changed, Some("fsharp"))
                .unwrap()
                .unwrap()
        );
    }
    let mut unknown = input.clone();
    unknown.language_signals.clear();
    unknown.project_language_witnesses.clear();
    let unproven = file_parser_route_hash(&unknown, None).unwrap().unwrap();
    assert_ne!(unproven, original);
    unknown.project_language_witnesses = input.project_language_witnesses.clone();
    assert_ne!(
        unproven,
        file_parser_route_hash(&unknown, None).unwrap().unwrap()
    );
    let mut independent = input.clone();
    for evidence in [LanguageEvidence::Content, LanguageEvidence::Configuration] {
        independent
            .language_signals
            .push(rootlight_discovery::LanguageSignal {
                language: "fsharp".to_owned(),
                evidence,
            });
        let before = file_parser_route_hash(&independent, Some("fsharp")).unwrap();
        independent.project_language_witnesses[0].project_content_hash =
            content_hash(b"unconsumed edit");
        assert_eq!(
            before,
            file_parser_route_hash(&independent, Some("fsharp")).unwrap()
        );
        independent = input.clone();
    }
    let mut objective_c = input;
    objective_c.path = "Example.m".to_owned();
    assert_eq!(
        file_parser_route_hash(&objective_c, Some("matlab")).unwrap(),
        Some(hash_static_components(&[b"rootlight.file-parser-route/1", b"matlab"]).unwrap())
    );
    assert_eq!(file_parser_route_hash(&objective_c, None).unwrap(), None);
}

fn assert_project_member(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    source: &[u8],
    language: &str,
    definition: Option<&str>,
) -> FileId {
    assert_project_member_coverage(
        service,
        receipt,
        path,
        source,
        language,
        definition,
        CoverageStatus::Complete,
    )
}

fn assert_project_member_coverage(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    source: &[u8],
    language: &str,
    definition: Option<&str>,
    expected: CoverageStatus,
) -> FileId {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .chain(
            snapshot
                .source_files()
                .entries()
                .iter()
                .map(|entry| entry.file()),
        )
        .find(|file| file.path == path)
        .unwrap();
    assert_eq!(file.language, language, "{path}");
    assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(source));
    let reference = file.evidence.source.as_ref().unwrap();
    assert_eq!(reference.generation(), receipt.generation);
    assert_eq!(reference.span().file(), file.id);
    assert_eq!(reference.span().start_byte(), 0);
    assert_eq!(reference.span().end_byte(), file.byte_length);
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(rootlight_source::SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].reference, *reference);
    assert_eq!(read.data.chunks[0].content_hash, file.content_hash);
    assert_eq!(read.data.chunks[0].bytes, source);
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(coverage.language, language);
    assert_eq!(coverage.tier, AnalysisTier::TierD);
    if let Some(name) = definition {
        assert_eq!(coverage.status, expected);
        if expected == CoverageStatus::Bounded {
            assert_signature_mask_gap_preserves_structure(document, file);
        }
        assert_eq!(coverage.reason, None);
        let entity = document
            .entities
            .iter()
            .find(|entity| {
                entity.canonical_name == name
                    && entity.language == language
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|reference| reference.span().file() == file.id)
            })
            .unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.file == file.id
                    && site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .unwrap();
        assert_eq!(site.source.generation(), receipt.generation);
        let written_name = match file.encoding.as_str() {
            "utf-8" => name.as_bytes().to_vec(),
            "utf-16le" => name.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            "utf-16be" => name.encode_utf16().flat_map(u16::to_be_bytes).collect(),
            encoding => panic!("unsupported fixture source encoding: {encoding}"),
        };
        exact_source(service, &site.source, language, &written_name);
        let located = service
            .code_locate_with_filters_and_budget(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                Vec::new(),
                vec![path.to_owned()],
                8,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert!(
            located
                .data
                .hits
                .iter()
                .any(|hit| hit.file == file.id && hit.symbol == Some(entity.id))
        );
    } else {
        assert_eq!(coverage.status, CoverageStatus::Unknown);
        let reason = if document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == SOURCE_FILE_FALLBACK_DIAGNOSTIC_CODE)
        {
            FirstSliceSourceCoverageReason::ResourceBounded
        } else if file.encoding != "utf-8" && service.analyzers.contains_key(language) {
            FirstSliceSourceCoverageReason::UnsupportedEncoding
        } else {
            FirstSliceSourceCoverageReason::UnsupportedLanguage
        };
        assert_eq!(coverage.reason, Some(reason));
        assert!(!document.entities.iter().any(|entity| {
            entity
                .evidence
                .source
                .as_ref()
                .is_some_and(|reference| reference.span().file() == file.id)
        }));
    }
    file.id
}

fn retained_parser_route(
    service: &FirstSliceService,
    generation: GenerationId,
    file: FileId,
) -> ContentHash {
    service
        .incremental_inputs
        .get(&generation)
        .unwrap()
        .value(InputKey::FileParserRoute(file))
        .unwrap()
}

#[test]
fn fsharp_literal_project_routes_track_add_remove_change_and_durable_reopen() {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let member = b"let answer = 1\n";
    let peer = b"let peer = 2\n";
    let strong = b"module Strong\nlet stable = 3\n";
    for (path, source) in [
        ("Member.fs", member.as_slice()),
        ("Peer.fs", peer.as_slice()),
        ("Strong.fs", strong.as_slice()),
    ] {
        fs::write(repository.path().join(path), source).unwrap();
    }
    let project_path = repository.path().join("Example.fsproj");
    let project = "<Project><ItemGroup><Compile Include=\"Member.fs\" /><Compile Include=\"Strong.fs\" /></ItemGroup></Project>\n";
    let peer_path = repository.path().join("Peer.fsproj");
    let peer_project =
        "<Project><ItemGroup><Compile Include=\"Peer.fs\" /></ItemGroup></Project>\n";
    let policy = FirstSliceStoragePolicy::resolve_user_config(
        Some("version='1.3'\n[storage]\nretained_generations=4\n"),
        4,
    )
    .unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let initial = index(&mut service, repository.path(), options);
    let member_id = assert_project_member(
        &service,
        initial.receipt(),
        "Member.fs",
        member,
        "unknown",
        None,
    );
    let unknown_route = retained_parser_route(&service, initial.receipt().generation, member_id);
    fs::write(&project_path, project).unwrap();
    let added = index(&mut service, repository.path(), options);
    assert_eq!(
        assert_project_member(
            &service,
            added.receipt(),
            "Member.fs",
            member,
            "fsharp",
            Some("answer")
        ),
        member_id
    );
    assert_eq!(added.evidence().rebuilt_files, 1);
    let added_route = retained_parser_route(&service, added.receipt().generation, member_id);
    assert_ne!(unknown_route, added_route);
    fs::write(&peer_path, peer_project).unwrap();
    let paired = index(&mut service, repository.path(), options);
    let peer_id = assert_project_member(
        &service,
        paired.receipt(),
        "Peer.fs",
        peer,
        "fsharp",
        Some("peer"),
    );
    let strong_id = assert_project_member(
        &service,
        paired.receipt(),
        "Strong.fs",
        strong,
        "fsharp",
        Some("stable"),
    );
    let strong_route = retained_parser_route(&service, paired.receipt().generation, strong_id);
    let peer_route = retained_parser_route(&service, paired.receipt().generation, peer_id);
    assert_eq!(
        retained_parser_route(&service, paired.receipt().generation, member_id),
        added_route
    );
    let noop = index(&mut service, repository.path(), options);
    assert_eq!(noop.receipt().generation, paired.receipt().generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    fs::write(
        &project_path,
        project.replace("</Project>", "<!-- authored documentation --></Project>"),
    )
    .unwrap();
    let changed = index(&mut service, repository.path(), options);
    assert_project_member(
        &service,
        changed.receipt(),
        "Member.fs",
        member,
        "fsharp",
        Some("answer"),
    );
    let changed_route = retained_parser_route(&service, changed.receipt().generation, member_id);
    assert_ne!(changed_route, added_route);
    assert_eq!(
        retained_parser_route(&service, changed.receipt().generation, peer_id),
        peer_route
    );
    assert_eq!(
        retained_parser_route(&service, changed.receipt().generation, strong_id),
        strong_route
    );
    assert_eq!(changed.evidence().rebuilt_files, 1);
    assert_eq!(changed.evidence().reused_files, 2);
    fs::remove_file(&project_path).unwrap();
    let removed = index(&mut service, repository.path(), options);
    assert_project_member(
        &service,
        removed.receipt(),
        "Member.fs",
        member,
        "unknown",
        None,
    );
    assert_eq!(
        retained_parser_route(&service, removed.receipt().generation, member_id),
        unknown_route
    );
    assert_eq!(removed.evidence().rebuilt_files, 0);
    fs::write(&project_path, project).unwrap();
    let restored = index(&mut service, repository.path(), options);
    assert_project_member(
        &service,
        restored.receipt(),
        "Member.fs",
        member,
        "fsharp",
        Some("answer"),
    );
    assert_eq!(
        retained_parser_route(&service, restored.receipt().generation, member_id),
        added_route
    );
    assert_eq!(restored.evidence().rebuilt_files, 1);
    fs::write(
        &peer_path,
        peer_project.replace("</Project>", "<!-- peer-only documentation --></Project>"),
    )
    .unwrap();
    let unrelated = index(&mut service, repository.path(), options);
    assert_project_member(
        &service,
        unrelated.receipt(),
        "Member.fs",
        member,
        "fsharp",
        Some("answer"),
    );
    assert_eq!(
        retained_parser_route(&service, unrelated.receipt().generation, member_id),
        added_route
    );
    assert_ne!(
        retained_parser_route(&service, unrelated.receipt().generation, peer_id),
        peer_route
    );
    assert_eq!(unrelated.evidence().rebuilt_files, 1);
    assert_eq!(unrelated.evidence().reused_files, 2);
    let last = unrelated.receipt().clone();
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_project_member(
        &reopened,
        &last,
        "Member.fs",
        member,
        "fsharp",
        Some("answer"),
    );
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt().generation, last.generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
}

#[test]
fn fsharp_conditioned_project_routes_preserve_literal_source_identity() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = b"let answer = 1\n";
    fs::write(repository.path().join("Library.fs"), source).unwrap();
    let project_path = repository.path().join("Example.fsproj");
    let mut service = FirstSliceService::new(4).unwrap();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let initial = index(&mut service, repository.path(), options);
    let file = assert_project_member(
        &service,
        initial.receipt(),
        "Library.fs",
        source,
        "unknown",
        None,
    );
    let initial_route = retained_parser_route(&service, initial.receipt().generation, file);
    let mut previous_route = initial_route;
    for project in [
        "<Project Condition='false'><ItemGroup><Compile Include='Library.fs'/></ItemGroup></Project>",
        "<Project><ItemGroup Condition='false'><Compile Include='Library.fs'/></ItemGroup></Project>",
        "<Project><ItemGroup><Compile Condition='false' Include='Library.fs'/></ItemGroup></Project>",
    ] {
        fs::write(&project_path, project).unwrap();
        let indexed = index(&mut service, repository.path(), options);
        assert_eq!(
            assert_project_member(
                &service,
                indexed.receipt(),
                "Library.fs",
                source,
                "fsharp",
                Some("answer")
            ),
            file
        );
        let route = retained_parser_route(&service, indexed.receipt().generation, file);
        assert_ne!(route, previous_route);
        assert_eq!(indexed.evidence().rebuilt_files, 1);
        let noop = index(&mut service, repository.path(), options);
        assert_eq!(noop.receipt().generation, indexed.receipt().generation);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
        previous_route = route;
    }
    fs::remove_file(project_path).unwrap();
    let removed = index(&mut service, repository.path(), options);
    assert_eq!(
        assert_project_member(
            &service,
            removed.receipt(),
            "Library.fs",
            source,
            "unknown",
            None
        ),
        file
    );
    assert_eq!(
        retained_parser_route(&service, removed.receipt().generation, file),
        initial_route
    );
}

#[test]
fn fsharp_project_declarations_do_not_expand_admission_or_accept_unevaluated_expressions() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = b"let answer = 1\n";
    let cases = [
        (
            "malformed",
            "Example.fsproj",
            "<Project><ItemGroup><Compile Include=\"Library.fs\" /></ItemGroup></Project><broken",
        ),
        (
            "conditional-dynamic",
            "Example.fsproj",
            "<Project><ItemGroup Condition=\"false\"><Compile Include=\"$(Source)\" /></ItemGroup></Project>",
        ),
        (
            "dynamic",
            "Example.fsproj",
            "<Project><ItemGroup><Compile Include=\"$(Source)\" /></ItemGroup></Project>",
        ),
        (
            "suffix",
            "Example.fsproj.bak",
            "<Project><ItemGroup><Compile Include=\"Library.fs\" /></ItemGroup></Project>",
        ),
        (
            "missing",
            "Example.fsproj",
            "<Project><ItemGroup><Compile Include=\"Missing.fs\" /></ItemGroup></Project>",
        ),
        (
            "wrong-member",
            "Example.fsproj",
            "<Project><ItemGroup><Compile Include=\"Library.fs.bak\" /></ItemGroup></Project>",
        ),
    ];
    for (directory, project_name, project) in cases {
        let root = repository.path().join(directory);
        fs::create_dir(&root).unwrap();
        fs::write(root.join("Library.fs"), source).unwrap();
        fs::write(root.join(project_name), project).unwrap();
    }
    fs::create_dir(repository.path().join("target")).unwrap();
    fs::write(repository.path().join("target/Hidden.fs"), source).unwrap();
    fs::write(
        repository.path().join("IgnoredMember.fsproj"),
        "<Project><ItemGroup><Compile Include=\"target/Hidden.fs\" /></ItemGroup></Project>",
    )
    .unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    for (directory, _, _) in cases {
        assert_project_member(
            &service,
            &receipt,
            &format!("{directory}/Library.fs"),
            source,
            "unknown",
            None,
        );
    }
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    assert!(
        !snapshot
            .document()
            .files
            .iter()
            .chain(
                snapshot
                    .source_files()
                    .entries()
                    .iter()
                    .map(|entry| entry.file())
            )
            .any(|file| file.path == "target/Hidden.fs")
    );
}

#[test]
fn fsharp_project_routes_preserve_configuration_and_encoded_source_identity() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let plain = b"let answer = 1\n";
    let signature = b"val declared: int\n";
    let encoded = [
        vec![0xff, 0xfe],
        "let encoded = 2\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect(),
    ]
    .concat();
    fs::write(repository.path().join("Blocked.fs"), plain).unwrap();
    fs::write(repository.path().join("Signature.fs"), signature).unwrap();
    fs::write(repository.path().join("Encoded.fs"), &encoded).unwrap();
    fs::write(repository.path().join("Example.fsproj"), "<Project><ItemGroup><Compile Include=\"Blocked.fs\" /><Compile Include=\"Signature.fs\" /><Compile Include=\"Encoded.fs\" /></ItemGroup></Project>").unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(Some("version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'Blocked.fs'='unsupported-language'\n'Signature.fs'='fsharp-signature'\n"), 2).unwrap();
    let mut service = FirstSliceService::new_with_storage_policy(
        2,
        MAX_RETAINED_SOURCE_BYTES,
        None,
        None,
        policy,
    )
    .unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_project_member(
        &service,
        &receipt,
        "Blocked.fs",
        plain,
        "unsupported-language",
        None,
    );
    assert_project_member_coverage(
        &service,
        &receipt,
        "Signature.fs",
        signature,
        "fsharp-signature",
        Some("declared"),
        CoverageStatus::Bounded,
    );
    let file_id = assert_project_member(
        &service,
        &receipt,
        "Encoded.fs",
        &encoded,
        "fsharp",
        Some("encoded"),
    );
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let file = snapshot
        .document()
        .files
        .iter()
        .find(|file| file.id == file_id)
        .unwrap();
    assert_eq!(file.encoding, "utf-16le");
    let definition = snapshot
        .document()
        .occurrences
        .iter()
        .find(|site| {
            site.file == file_id
                && site.role == OccurrenceRole::Definition
                && site.syntactic_text_hash == content_hash(b"encoded")
        })
        .unwrap();
    assert!(matches!(
        definition.target,
        OccurrenceTarget::Resolved { .. }
    ));
    assert_eq!(
        definition.source.span(),
        SourceSpan::new(file_id, 10, 24).unwrap()
    );
    assert_eq!(definition.source.content_hash(), content_hash(&encoded));
    assert_eq!(definition.source.generation(), receipt.generation);
    let coverage = service
        .source_file_coverage_until(receipt.generation, file_id, &deadline())
        .unwrap();
    assert_eq!(coverage.status, CoverageStatus::Complete);
    assert_eq!(coverage.reason, None);
}

#[test]
fn fsharp_project_route_survives_zero_supported_transitions() {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let source = b"let standalone = 1\n";
    fs::write(repository.path().join("Member.fs"), source).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(
        Some("version='1.3'\n[storage]\nretained_generations=4\n"),
        4,
    )
    .unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let initial = index(&mut service, repository.path(), options);
    assert_eq!(initial.evidence().rebuilt_files, 0);
    let file = assert_project_member(
        &service,
        initial.receipt(),
        "Member.fs",
        source,
        "unknown",
        None,
    );
    let unknown_route = retained_parser_route(&service, initial.receipt().generation, file);
    let project_path = repository.path().join("Example.fsproj");
    fs::write(
        &project_path,
        "<Project><ItemGroup><Compile Include=\"Member.fs\" /></ItemGroup></Project>\n",
    )
    .unwrap();
    let added = index(&mut service, repository.path(), options);
    assert_eq!(added.evidence().rebuilt_files, 1);
    assert_eq!(
        assert_project_member(
            &service,
            added.receipt(),
            "Member.fs",
            source,
            "fsharp",
            Some("standalone")
        ),
        file
    );
    assert_ne!(
        retained_parser_route(&service, added.receipt().generation, file),
        unknown_route
    );
    let noop = index(&mut service, repository.path(), options);
    assert_eq!(noop.receipt().generation, added.receipt().generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    fs::remove_file(&project_path).unwrap();
    let removed = index(&mut service, repository.path(), options);
    assert_eq!(removed.evidence().rebuilt_files, 0);
    assert_eq!(
        assert_project_member(
            &service,
            removed.receipt(),
            "Member.fs",
            source,
            "unknown",
            None
        ),
        file
    );
    assert_eq!(
        retained_parser_route(&service, removed.receipt().generation, file),
        unknown_route
    );
    let last = removed.receipt().clone();
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_project_member(&reopened, &last, "Member.fs", source, "unknown", None);
    assert_eq!(
        retained_parser_route(&reopened, last.generation, file),
        unknown_route
    );
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt().generation, last.generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
}

#[test]
fn fsharp_project_routes_survive_resource_fallback() {
    fn assert_fallback_member(
        service: &FirstSliceService,
        receipt: &FirstSliceIndexReceipt,
        source: &[u8],
        language: &str,
    ) -> FileId {
        assert!(
            receipt
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == SOURCE_FILE_FALLBACK_DIAGNOSTIC_CODE)
        );
        assert!(receipt.discovery_complete);
        assert_eq!(receipt.entities, 0);
        assert_eq!(receipt.extracted_files, 0);
        let file = assert_project_member(service, receipt, "Member.fs", source, language, None);
        for query in ["Member.fs", "standalone"] {
            let located = service
                .code_locate(
                    receipt.generation,
                    query.to_owned(),
                    LocateMode::Exact,
                    8,
                    0,
                    &deadline(),
                )
                .unwrap();
            let hit = located
                .data
                .hits
                .iter()
                .find(|hit| hit.file == file)
                .unwrap();
            assert_eq!(hit.kind, "file");
            assert_eq!(hit.symbol, None);
            let reference = hit.source.as_ref().unwrap();
            assert_eq!(reference.generation(), receipt.generation);
            assert_eq!(reference.span().file(), file);
            assert_eq!(reference.content_hash(), content_hash(source));
        }
        file
    }

    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let source = b"let standalone = 1\n";
    fs::write(repository.path().join("Member.fs"), source).unwrap();
    fs::write(repository.path().join("Peer.fs"), b"let peer = 2\n").unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(
        Some("version='1.3'\n[storage]\nretained_generations=4\n"),
        4,
    )
    .unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let current = &service.analysis_limits;
    let mut ir = current.ir().clone();
    // Two admitted files force the real resource fallback, not merely an empty
    // parser selection. Every other analysis ceiling remains unchanged.
    ir.max_files = 1;
    let mut limits = AnalysisLimits::new(
        current.max_source_bytes(),
        current.max_syntax_nodes(),
        current.max_syntax_depth(),
        current.max_embedded_ranges(),
        current.max_reported_memory_bytes(),
        current.syntax_stream().clone(),
        current.ir_stream().clone(),
        ir,
    )
    .unwrap();
    if let Some(project) = current.project() {
        limits = limits.with_project_limits(project);
    }
    service.analysis_limits = limits.clone();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let initial = index(&mut service, repository.path(), options);
    let file = assert_fallback_member(&service, initial.receipt(), source, "unknown");
    let unknown_route = retained_parser_route(&service, initial.receipt().generation, file);
    let project_path = repository.path().join("Example.fsproj");
    let project = "<Project><ItemGroup><Compile Include=\"Member.fs\" /></ItemGroup></Project>\n";
    fs::write(&project_path, project).unwrap();
    let added = index(&mut service, repository.path(), options);
    assert_eq!(
        assert_fallback_member(&service, added.receipt(), source, "fsharp"),
        file
    );
    let project_route = retained_parser_route(&service, added.receipt().generation, file);
    assert_ne!(project_route, unknown_route);
    let noop = index(&mut service, repository.path(), options);
    assert_eq!(noop.receipt().generation, added.receipt().generation);
    assert_fallback_member(&service, noop.receipt(), source, "fsharp");
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    fs::write(
        &project_path,
        format!("{project}<!-- retained provenance -->\n"),
    )
    .unwrap();
    let changed = index(&mut service, repository.path(), options);
    assert_eq!(
        assert_fallback_member(&service, changed.receipt(), source, "fsharp"),
        file
    );
    assert_ne!(
        retained_parser_route(&service, changed.receipt().generation, file),
        project_route
    );
    fs::remove_file(&project_path).unwrap();
    let removed = index(&mut service, repository.path(), options);
    assert_eq!(
        assert_fallback_member(&service, removed.receipt(), source, "unknown"),
        file
    );
    assert_eq!(
        retained_parser_route(&service, removed.receipt().generation, file),
        unknown_route
    );
    let last = removed.receipt().clone();
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    reopened.analysis_limits = limits;
    assert_eq!(
        assert_fallback_member(&reopened, &last, source, "unknown"),
        file
    );
    assert_eq!(
        retained_parser_route(&reopened, last.generation, file),
        unknown_route
    );
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt().generation, last.generation);
    assert_fallback_member(&reopened, noop.receipt(), source, "unknown");
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
}

#[test]
fn fsharp_typed_single_formals_survive_durable_reopen_and_noop() {
    let source = "module Formals\r\n// ☃ 🧭\r\nlet first (value: int) = value + absent\r\nlet second (value: int) = value + 1\r\n";
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let mut bytes = if encoding == "utf-8" {
            Vec::new()
        } else {
            typed_formal_leaf_bytes("\u{feff}", encoding)
        };
        bytes.extend(typed_formal_leaf_bytes(source, encoding));
        fs::write(repository.path().join("Formals.fs"), &bytes).unwrap();
        let mut service =
            FirstSliceService::new_durable_with_policy(policy(), paths.state_dir(), &deadline())
                .unwrap();
        let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
        let initial = index(&mut service, repository.path(), options);
        let receipt = initial.receipt().clone();
        assert_eq!(receipt.indexed_files, 1);
        assert!(receipt.discovery_complete);
        assert_eq!(initial.evidence().rebuilt_files, 1);
        let symbols = assert_typed_formal_generation(&service, &receipt, source, &bytes, encoding);
        let original = serde_json::to_value(
            service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap()
                .document(),
        )
        .unwrap();
        let noop = index(&mut service, repository.path(), options);
        assert_eq!(noop.receipt(), &receipt);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
        assert_eq!(
            assert_typed_formal_generation(&service, &receipt, source, &bytes, encoding),
            symbols
        );
        drop(service);

        let mut reopened =
            FirstSliceService::new_durable_with_policy(policy(), paths.state_dir(), &deadline())
                .unwrap();
        assert_eq!(
            assert_typed_formal_generation(&reopened, &receipt, source, &bytes, encoding),
            symbols
        );
        assert_eq!(
            serde_json::to_value(
                reopened
                    .loaded_generation_snapshot(receipt.generation)
                    .unwrap()
                    .document(),
            )
            .unwrap(),
            original
        );
        let noop = index(&mut reopened, repository.path(), options);
        assert_eq!(noop.receipt(), &receipt);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
        assert_eq!(
            assert_typed_formal_generation(&reopened, &receipt, source, &bytes, encoding),
            symbols
        );
    }
}

fn typed_formal_leaf_bytes(text: &str, encoding: &str) -> Vec<u8> {
    match encoding {
        "utf-8" => text.as_bytes().to_vec(),
        "utf-16le" => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        "utf-16be" => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        _ => panic!("unsupported fixture encoding"),
    }
}

fn assert_typed_formal_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    bytes: &[u8],
    encoding: &str,
) -> BTreeSet<SymbolId> {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(file.path, "Formals.fs");
    assert_eq!(file.language, "fsharp");
    assert_eq!(file.encoding, encoding);
    assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(bytes));
    let span = |start: usize, written: &str| {
        assert_eq!(
            source.get(start..start.checked_add(written.len()).unwrap()),
            Some(written)
        );
        let prefix_bytes = typed_formal_leaf_bytes(source.get(..start).unwrap(), encoding).len();
        let start = prefix_bytes
            .checked_add(if encoding == "utf-8" { 0 } else { 2 })
            .unwrap();
        let end = start
            .checked_add(typed_formal_leaf_bytes(written, encoding).len())
            .unwrap();
        SourceSpan::new(
            file.id,
            u64::try_from(start).unwrap(),
            u64::try_from(end).unwrap(),
        )
        .unwrap()
    };
    let check_source = |reference: &SourceRef| {
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), file.content_hash);
        let raw = bytes
            .get(
                usize::try_from(reference.span().start_byte()).unwrap()
                    ..usize::try_from(reference.span().end_byte()).unwrap(),
            )
            .unwrap();
        exact_source(service, reference, "fsharp", raw);
    };
    let full = file.evidence.source.as_ref().unwrap();
    assert_eq!(
        full.span(),
        SourceSpan::new(file.id, 0, file.byte_length).unwrap()
    );
    check_source(full);
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        2
    );
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .count(),
        3
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::RefersTo)
            .count(),
        2
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    assert!(!document.skipped_regions.iter().any(|gap| {
        matches!(
            gap.reason,
            SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
        )
    }));
    let mut symbols = BTreeSet::new();
    for name in ["first", "second"] {
        let functions = document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Function && entity.canonical_name == name)
            .collect::<Vec<_>>();
        assert_eq!(functions.len(), 1);
        let function = functions[0];
        assert!(symbols.insert(function.id));
        let head = format!("{name} (value: int)");
        let head_start = source.find(&head).unwrap();
        let declaration = source
            .get(head_start..)
            .unwrap()
            .split("\r\n")
            .next()
            .unwrap();
        let function_source = function.evidence.source.as_ref().unwrap();
        assert_eq!(function_source.span(), span(head_start, declaration));
        check_source(function_source);
        let parameters = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == "value"
                    && entity.container == Some(ContainerRef::Entity(function.id))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            parameters.len(),
            1,
            "{encoding}: own typed parameter of {name}"
        );
        let parameter = parameters[0];
        assert!(symbols.insert(parameter.id));
        let parameter_source = parameter.evidence.source.as_ref().unwrap();
        assert_eq!(
            parameter_source.span(),
            span(head_start + name.len() + 2, "value")
        );
        check_source(parameter_source);
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == (OccurrenceTarget::Resolved {
                            symbol: parameter.id,
                        })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        assert_eq!(&definitions[0].source, parameter_source);
        check_source(&definitions[0].source);
        let read_span = span(head_start + head.len() + 3, "value");
        let reads = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference && site.source.span() == read_span
            })
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 1);
        let read = reads[0];
        assert_eq!(read.enclosing, Some(function.id));
        assert_eq!(
            read.target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            }
        );
        check_source(&read.source);
        let relations = document
            .relations
            .iter()
            .filter(|relation| relation.subject == RelationEndpoint::Occurrence(read.id))
            .collect::<Vec<_>>();
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].predicate, RelationPredicate::RefersTo);
        assert_eq!(relations[0].object, RelationEndpoint::Entity(parameter.id));
        assert_eq!(relations[0].evidence.source.as_ref(), Some(&read.source));
        assert!(!document.skipped_regions.iter().any(|gap| {
            gap.detail == "fsharp-formal-binding-target-unavailable" && gap.source == read.source
        }));
    }
    let missing_span = span(source.find("absent").unwrap(), "absent");
    let missing = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Reference && site.source.span() == missing_span)
        .collect::<Vec<_>>();
    assert_eq!(missing.len(), 1);
    assert!(matches!(
        missing[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    check_source(&missing[0].source);
    assert!(
        document
            .relations
            .iter()
            .all(|relation| { relation.subject != RelationEndpoint::Occurrence(missing[0].id) })
    );
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.domain == rootlight_ir::FactDomain::Relations
            && gap.reason == SkippedRegionReason::UnsupportedConstruct
            && gap.detail == "fsharp-formal-binding-target-unavailable"
            && gap.source == missing[0].source
    }));
    symbols
}

#[test]
fn fsharp_formal_proofs_preserve_callable_signatures_after_reopen() {
    let source = "module Headers\r\n// ☃ 🧭\r\nlet choose plain (typed: int) = plain + typed\r\n";
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let mut bytes = if encoding == "utf-8" {
            Vec::new()
        } else {
            typed_formal_leaf_bytes("\u{feff}", encoding)
        };
        bytes.extend(typed_formal_leaf_bytes(source, encoding));
        fs::write(repository.path().join("Headers.fs"), &bytes).unwrap();
        let mut service =
            FirstSliceService::new_durable_with_policy(policy(), paths.state_dir(), &deadline())
                .unwrap();
        let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
        let initial = index(&mut service, repository.path(), options);
        let receipt = initial.receipt().clone();
        assert_eq!(receipt.indexed_files, 1);
        assert!(receipt.discovery_complete);
        let symbols =
            assert_formal_signature_generation(&service, &receipt, source, &bytes, encoding);
        let document = serde_json::to_value(
            service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap()
                .document(),
        )
        .unwrap();
        let noop = index(&mut service, repository.path(), options);
        assert_eq!(noop.receipt(), &receipt);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
        assert_eq!(
            assert_formal_signature_generation(&service, &receipt, source, &bytes, encoding),
            symbols
        );
        drop(service);
        let mut reopened =
            FirstSliceService::new_durable_with_policy(policy(), paths.state_dir(), &deadline())
                .unwrap();
        assert_eq!(
            assert_formal_signature_generation(&reopened, &receipt, source, &bytes, encoding),
            symbols
        );
        assert_eq!(
            serde_json::to_value(
                reopened
                    .loaded_generation_snapshot(receipt.generation)
                    .unwrap()
                    .document()
            )
            .unwrap(),
            document,
        );
        let noop = index(&mut reopened, repository.path(), options);
        assert_eq!(noop.receipt(), &receipt);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
        assert_eq!(
            assert_formal_signature_generation(&reopened, &receipt, source, &bytes, encoding),
            symbols
        );
    }
}

fn assert_formal_signature_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    bytes: &[u8],
    encoding: &str,
) -> BTreeSet<SymbolId> {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(file.path, "Headers.fs");
    assert_eq!(file.language, "fsharp");
    assert_eq!(file.encoding, encoding);
    assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(bytes));
    let span = |start: usize, written: &str| {
        assert_eq!(source.get(start..start + written.len()), Some(written));
        let start = typed_formal_leaf_bytes(source.get(..start).unwrap(), encoding)
            .len()
            .checked_add(if encoding == "utf-8" { 0 } else { 2 })
            .unwrap();
        let end = start
            .checked_add(typed_formal_leaf_bytes(written, encoding).len())
            .unwrap();
        SourceSpan::new(
            file.id,
            u64::try_from(start).unwrap(),
            u64::try_from(end).unwrap(),
        )
        .unwrap()
    };
    let check_source = |reference: &SourceRef| {
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), file.content_hash);
        let raw = bytes
            .get(
                usize::try_from(reference.span().start_byte()).unwrap()
                    ..usize::try_from(reference.span().end_byte()).unwrap(),
            )
            .unwrap();
        exact_source(service, reference, "fsharp", raw);
    };
    let full = file.evidence.source.as_ref().unwrap();
    assert_eq!(
        full.span(),
        SourceSpan::new(file.id, 0, file.byte_length).unwrap()
    );
    check_source(full);
    let functions = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "choose")
        .collect::<Vec<_>>();
    assert_eq!(functions.len(), 1);
    let function = functions[0];
    let header = "choose plain (typed: int) ";
    let header_start = source.find(header).unwrap();
    let function_source = function.evidence.source.as_ref().unwrap();
    assert_eq!(
        function_source.span(),
        span(header_start, "choose plain (typed: int) = plain + typed")
    );
    check_source(function_source);
    let definitions = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.target
                    == (OccurrenceTarget::Resolved {
                        symbol: function.id,
                    })
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].source.span(), span(header_start, "choose"));
    check_source(&definitions[0].source);
    let explained = service
        .symbol_explain(receipt.generation, function.id, &deadline())
        .unwrap();
    let signature = explained
        .data
        .signature
        .as_ref()
        .expect("callable header remains source-backed");
    assert_eq!(signature.text(), header);
    assert!(!signature.is_truncated());
    let signatures = document
        .extensions
        .iter()
        .filter_map(|extension| {
            if extension.namespace != rootlight_ir::LEXICAL_EXTENSION_NAMESPACE {
                return None;
            }
            let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
            (lexical.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                && lexical.subject() == rootlight_ir::FactRef::Entity(function.id))
            .then_some((extension, lexical))
        })
        .collect::<Vec<_>>();
    assert_eq!(signatures.len(), 1);
    let (envelope, lexical) = &signatures[0];
    assert_eq!(lexical, signature);
    assert_eq!(
        lexical.complete_text_hash(),
        content_hash(header.as_bytes())
    );
    assert_eq!(
        lexical.source_encoding(),
        match encoding {
            "utf-8" => None,
            "utf-16le" => Some(rootlight_ir::SourceTextEncoding::Utf16Le),
            "utf-16be" => Some(rootlight_ir::SourceTextEncoding::Utf16Be),
            _ => panic!("unsupported fixture encoding"),
        }
    );
    let reference = envelope.evidence.source.as_ref().unwrap();
    assert_eq!(reference.span(), span(header_start, header));
    check_source(reference);
    assert_eq!(
        rootlight_ir::validate_lexical_evidence_envelope(
            envelope,
            rootlight_ir::FactRef::Entity(function.id),
            reference,
            function.provenance,
        )
        .unwrap(),
        *lexical,
    );
    lexical
        .verify_source_bytes(
            bytes
                .get(
                    usize::try_from(reference.span().start_byte()).unwrap()
                        ..usize::try_from(reference.span().end_byte()).unwrap(),
                )
                .unwrap(),
        )
        .unwrap();
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.domain == rootlight_ir::FactDomain::Extensions
            && gap.detail == "signature-capture-unavailable"
            && gap.source == *function_source
    }));
    let parameters = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Parameter)
        .collect::<Vec<_>>();
    assert_eq!(parameters.len(), 2);
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .count(),
        2
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::RefersTo)
            .count(),
        2
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    let mut symbols = BTreeSet::from([function.id]);
    for name in ["plain", "typed"] {
        let selected = parameters
            .iter()
            .copied()
            .filter(|entity| entity.canonical_name == name)
            .collect::<Vec<_>>();
        assert_eq!(selected.len(), 1);
        let parameter = selected[0];
        assert!(symbols.insert(parameter.id));
        assert_eq!(parameter.container, Some(ContainerRef::Entity(function.id)));
        let parameter_source = parameter.evidence.source.as_ref().unwrap();
        assert_eq!(
            parameter_source.span(),
            span(header_start + header.find(name).unwrap(), name)
        );
        check_source(parameter_source);
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == (OccurrenceTarget::Resolved {
                            symbol: parameter.id,
                        })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        assert_eq!(&definitions[0].source, parameter_source);
        check_source(&definitions[0].source);
        let reads = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span() == span(source.rfind(name).unwrap(), name)
            })
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 1);
        let read = reads[0];
        assert_eq!(read.enclosing, Some(function.id));
        assert_eq!(
            read.target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            }
        );
        check_source(&read.source);
        let edges = document
            .relations
            .iter()
            .filter(|relation| relation.subject == RelationEndpoint::Occurrence(read.id))
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].predicate, RelationPredicate::RefersTo);
        assert_eq!(edges[0].object, RelationEndpoint::Entity(parameter.id));
        assert_eq!(edges[0].evidence.source.as_ref(), Some(&read.source));
        assert!(!document.skipped_regions.iter().any(|gap| {
            gap.detail == "fsharp-formal-binding-target-unavailable" && gap.source == read.source
        }));
        let written = if name == "typed" {
            "(typed: int)"
        } else {
            name
        };
        let explained = service
            .symbol_explain(receipt.generation, parameter.id, &deadline())
            .unwrap();
        let signature = explained
            .data
            .signature
            .as_ref()
            .expect("authored parameter header remains source-backed");
        assert_eq!(signature.text(), written);
        assert!(!signature.is_truncated());
        assert_eq!(
            signature.complete_text_hash(),
            content_hash(written.as_bytes())
        );
        let signatures = document
            .extensions
            .iter()
            .filter_map(|envelope| {
                if envelope.namespace != rootlight_ir::LEXICAL_EXTENSION_NAMESPACE {
                    return None;
                }
                let lexical = rootlight_ir::decode_lexical_evidence_envelope(envelope).unwrap();
                (lexical.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                    && lexical.subject() == rootlight_ir::FactRef::Entity(parameter.id))
                .then_some((envelope, lexical))
            })
            .collect::<Vec<_>>();
        assert_eq!(signatures.len(), 1);
        let (envelope, lexical) = &signatures[0];
        assert_eq!(lexical, signature);
        let reference = envelope.evidence.source.as_ref().unwrap();
        assert_eq!(
            reference.span(),
            span(header_start + header.find(written).unwrap(), written)
        );
        check_source(reference);
        assert_eq!(
            rootlight_ir::validate_lexical_evidence_envelope(
                envelope,
                rootlight_ir::FactRef::Entity(parameter.id),
                reference,
                parameter.provenance,
            )
            .unwrap(),
            *lexical,
        );
        lexical
            .verify_source_bytes(
                bytes
                    .get(
                        usize::try_from(reference.span().start_byte()).unwrap()
                            ..usize::try_from(reference.span().end_byte()).unwrap(),
                    )
                    .unwrap(),
            )
            .unwrap();
        assert!(!document.skipped_regions.iter().any(|gap| {
            gap.domain == rootlight_ir::FactDomain::Extensions
                && gap.reason == SkippedRegionReason::UnsupportedConstruct
                && gap.detail == "signature-capture-unavailable"
                && gap.source == *parameter_source
        }));
        for paths in [Vec::new(), vec![file.path.clone()]] {
            let located = service
                .code_locate_with_entity_filters_and_budget(
                    receipt.generation,
                    written.to_owned(),
                    LocateMode::Text,
                    vec!["fsharp".to_owned()],
                    paths,
                    Some(vec!["parameter".to_owned()]),
                    10,
                    0,
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            assert_eq!(located.data.hits.len(), 1, "{encoding}: {written}");
            let hit = &located.data.hits[0];
            assert_eq!(hit.symbol, Some(parameter.id));
            assert_eq!(hit.file, file.id);
            assert_eq!(hit.path, file.path);
            assert_eq!(hit.kind, "parameter");
            assert_eq!(hit.language, "fsharp");
            assert_eq!(hit.source.as_ref(), Some(parameter_source));
            assert_eq!(hit.signature.as_ref(), Some(lexical));
            assert_eq!(hit.signature.as_ref().unwrap().text(), written);
            assert!(!located.data.truncated);
            assert!(located.data.limiting_resources.is_empty());
        }
    }
    assert_eq!(symbols.len(), 3);
    symbols
}

#[test]
fn fsharp_ordinary_module_bindings_survive_recovery_without_stale_calls() {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let source = "module Example\nlet seed = 10\nlet alias = seed\nlet add (value: int) = value + seed\nlet run (value: int) = add value\nlet shadow (seed: int) = seed + 1\n";
    assert_eq!(source.matches("= add value").count(), 1);
    let changed_source = source.replace("= add value", "= missing value");
    fs::write(repository.path().join("Bindings.fs"), source).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(
        Some("version='1.3'\n[storage]\nretained_generations=4\n"),
        4,
    )
    .unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let cold = index(&mut service, repository.path(), options);
    assert_eq!(cold.evidence().rebuilt_files, 1);
    let initial = cold.receipt().clone();
    let (symbols, old_source) =
        assert_ordinary_binding_generation(&service, &initial, source, true);
    let noop = index(&mut service, repository.path(), options);
    assert_eq!(noop.receipt().generation, initial.generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    assert_eq!(
        assert_ordinary_binding_generation(&service, &initial, source, true).0,
        symbols
    );
    fs::write(repository.path().join("Bindings.fs"), &changed_source).unwrap();
    let mutation = index(&mut service, repository.path(), options);
    assert_eq!(mutation.evidence().changed_files, 1);
    assert_eq!(mutation.evidence().rebuilt_files, 1);
    let changed = mutation.receipt().clone();
    assert_ne!(changed.generation, initial.generation);
    assert_eq!(changed.parent, Some(initial.generation));
    let (changed_symbols, updated_source) =
        assert_ordinary_binding_generation(&service, &changed, &changed_source, false);
    assert_eq!(changed_symbols, symbols);
    assert_eq!(updated_source.span().file(), old_source.span().file());
    assert_ne!(updated_source.content_hash(), old_source.content_hash());
    exact_source(&service, &old_source, "fsharp", source.as_bytes());
    assert!(
        service
            .source_read_with_options_and_budget(
                changed.generation,
                vec![old_source],
                SourceReadOptions::new(),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .is_err()
    );
    let clean = index(
        &mut service,
        repository.path(),
        FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
    );
    assert_eq!(clean.evidence().rebuilt_files, 1);
    assert!(changed.logical_snapshot.is_some());
    assert_eq!(clean.receipt().logical_snapshot, changed.logical_snapshot);
    let last = clean.receipt().clone();
    assert_eq!(
        assert_ordinary_binding_generation(&service, &last, &changed_source, false).0,
        symbols
    );
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_eq!(
        assert_ordinary_binding_generation(&reopened, &initial, source, true).0,
        symbols
    );
    assert_eq!(
        assert_ordinary_binding_generation(&reopened, &changed, &changed_source, false).0,
        symbols
    );
    assert_eq!(
        assert_ordinary_binding_generation(&reopened, &last, &changed_source, false).0,
        symbols
    );
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt().generation, last.generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
}

#[test]
fn fsharp_tuple_formals_preserve_queries_and_call_slots_after_reopen() {
    let source = "module Tuples\n// ☃ 🧭\nlet seed = 10\nlet select (left, right) = left + right\nlet carry (first, second) extra = first + second + extra\nlet shadow (seed, spare) = seed + spare\nlet invoke input = select (input, input)\nlet run input = carry (input, input) input\nlet partial input = carry (input, input)\n";
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let source = if encoding == "utf-8" {
            source.to_owned()
        } else {
            source.replace('\n', "\r\n")
        };
        assert_eq!(source.matches("= left + right").count(), 1);
        let changed_source = source.replace("= left + right", "= (left) + right");
        let encode = |text: &str| {
            let mut bytes = if encoding == "utf-8" {
                Vec::new()
            } else {
                typed_formal_leaf_bytes("\u{feff}", encoding)
            };
            bytes.extend(typed_formal_leaf_bytes(text, encoding));
            bytes
        };
        let bytes = encode(&source);
        let changed_bytes = encode(&changed_source);
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let path = repository.path().join("Tuples.fs");
        fs::write(&path, &bytes).unwrap();
        let mut service =
            FirstSliceService::new_durable_with_policy(policy(), paths.state_dir(), &deadline())
                .unwrap();
        let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
        let cold = index(&mut service, repository.path(), options);
        assert_eq!(cold.evidence().rebuilt_files, 1);
        let initial = cold.receipt().clone();
        let (symbols, old_source) =
            assert_tuple_formal_generation(&service, &initial, &source, &bytes, encoding);
        let noop = index(&mut service, repository.path(), options);
        assert_eq!(noop.receipt(), &initial);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
        fs::write(&path, &changed_bytes).unwrap();
        let mutation = index(&mut service, repository.path(), options);
        assert_eq!(mutation.evidence().changed_files, 1);
        assert_eq!(mutation.evidence().rebuilt_files, 1);
        let changed = mutation.receipt().clone();
        assert_ne!(changed.generation, initial.generation);
        assert_eq!(changed.parent, Some(initial.generation));
        let (changed_symbols, updated_source) = assert_tuple_formal_generation(
            &service,
            &changed,
            &changed_source,
            &changed_bytes,
            encoding,
        );
        assert_eq!(changed_symbols, symbols);
        assert_eq!(updated_source.span().file(), old_source.span().file());
        assert_ne!(updated_source.content_hash(), old_source.content_hash());
        exact_source(&service, &old_source, "fsharp", &bytes);
        assert!(
            service
                .source_read_with_options_and_budget(
                    changed.generation,
                    vec![old_source],
                    SourceReadOptions::new(),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .is_err()
        );
        let clean = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
        );
        assert_eq!(clean.evidence().rebuilt_files, 1);
        assert!(changed.logical_snapshot.is_some());
        assert_eq!(clean.receipt().logical_snapshot, changed.logical_snapshot);
        let last = clean.receipt().clone();
        drop(service);
        let mut reopened =
            FirstSliceService::new_durable_with_policy(policy(), paths.state_dir(), &deadline())
                .unwrap();
        for (receipt, text, raw) in [
            (&initial, source.as_str(), bytes.as_slice()),
            (&changed, changed_source.as_str(), changed_bytes.as_slice()),
            (&last, changed_source.as_str(), changed_bytes.as_slice()),
        ] {
            assert_eq!(
                assert_tuple_formal_generation(&reopened, receipt, text, raw, encoding).0,
                symbols
            );
        }
        let noop = index(&mut reopened, repository.path(), options);
        assert_eq!(noop.receipt(), &last);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
    }
}

fn assert_tuple_formal_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    bytes: &[u8],
    encoding: &str,
) -> (BTreeMap<String, SymbolId>, SourceRef) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let lexical_envelopes = logical_lexical_envelopes(document);
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(
        (
            file.path.as_str(),
            file.language.as_str(),
            file.encoding.as_str()
        ),
        ("Tuples.fs", "fsharp", encoding)
    );
    assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(bytes));
    let reference = |start: usize, written: &str| {
        assert_eq!(source.get(start..start + written.len()), Some(written));
        let physical = typed_formal_leaf_bytes(source.get(..start).unwrap(), encoding).len()
            + usize::from(encoding != "utf-8") * 2;
        SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                file.id,
                u64::try_from(physical).unwrap(),
                u64::try_from(physical + typed_formal_leaf_bytes(written, encoding).len()).unwrap(),
            )
            .unwrap(),
            file.content_hash,
            None,
        )
    };
    let check_source = |reference: &SourceRef| {
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), file.content_hash);
        exact_source(
            service,
            reference,
            "fsharp",
            bytes
                .get(
                    usize::try_from(reference.span().start_byte()).unwrap()
                        ..usize::try_from(reference.span().end_byte()).unwrap(),
                )
                .unwrap(),
        );
    };
    let full = file.evidence.source.as_ref().unwrap();
    assert_eq!(
        full.span(),
        SourceSpan::new(file.id, 0, file.byte_length).unwrap()
    );
    check_source(full);
    let declaration = |name: &str| {
        let start = source.find(&format!("let {name} ")).unwrap() + "let ".len();
        (start, source.get(start..).unwrap().lines().next().unwrap())
    };
    let mut symbols = BTreeMap::new();
    for name in [
        "seed", "select", "carry", "shadow", "invoke", "run", "partial",
    ] {
        let kind = if name == "seed" {
            EntityKind::Variable
        } else {
            EntityKind::Function
        };
        let matches = document
            .entities
            .iter()
            .filter(|entity| entity.kind == kind && entity.canonical_name == name)
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "{encoding}: {name}");
        symbols.insert(name.to_owned(), matches[0].id);
        let (start, written) = declaration(name);
        assert_eq!(
            matches[0].evidence.source.as_ref(),
            Some(&reference(start, written))
        );
    }
    for (owner, names) in [
        ("select", &["left", "right"][..]),
        ("carry", &["first", "second", "extra"][..]),
        ("shadow", &["seed", "spare"][..]),
        ("invoke", &["input"][..]),
        ("run", &["input"][..]),
        ("partial", &["input"][..]),
    ] {
        for name in names {
            let parameters = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.kind == EntityKind::Parameter
                        && entity.canonical_name == *name
                        && entity.container == Some(ContainerRef::Entity(symbols[owner]))
                })
                .collect::<Vec<_>>();
            assert_eq!(parameters.len(), 1, "{encoding}: {owner}.{name}");
            symbols.insert(format!("{owner}.{name}"), parameters[0].id);
            let (start, written) = declaration(owner);
            let header = written.split_once('=').unwrap().0;
            assert_eq!(
                parameters[0].evidence.source.as_ref(),
                Some(&reference(start + header.find(name).unwrap(), name))
            );
        }
    }
    assert_eq!(symbols.len(), 17);
    assert_eq!(document.entities.len(), symbols.len() + 2);
    let file_modules = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Module && entity.canonical_name == "Tuples.fs")
        .collect::<Vec<_>>();
    assert_eq!(file_modules.len(), 1);
    assert_eq!(file_modules[0].evidence.source.as_ref(), Some(full));
    check_source(file_modules[0].evidence.source.as_ref().unwrap());
    let modules = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Module && entity.canonical_name == "Tuples")
        .collect::<Vec<_>>();
    assert_eq!(modules.len(), 1);
    check_source(modules[0].evidence.source.as_ref().unwrap());
    for (key, symbol) in &symbols {
        let explained = service
            .symbol_explain(receipt.generation, *symbol, &deadline())
            .unwrap();
        assert_eq!(explained.data.entity.id, *symbol);
        let entity_source = explained.data.entity.evidence.source.as_ref().unwrap();
        check_source(entity_source);
        let name = key.rsplit('.').next().unwrap();
        let (start, written) = declaration(key.split_once('.').map_or(name, |(owner, _)| owner));
        let header = written.split_once('=').unwrap().0;
        let expected_definition = reference(start + header.find(name).unwrap(), name);
        let definitions = explained
            .data
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: *symbol })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1, "{key}");
        assert_eq!(definitions[0].source, expected_definition);
        check_source(&definitions[0].source);
        if key != "seed" {
            let signature = explained.data.signature.as_ref().unwrap();
            let expected_header = if key.contains('.') { name } else { header };
            assert_eq!(signature.text(), expected_header, "{encoding}: {key}");
            assert!(!signature.is_truncated());
            assert_eq!(
                signature.complete_text_hash(),
                content_hash(expected_header.as_bytes())
            );
            let envelopes = lexical_envelopes
                .iter()
                .filter_map(|envelope| {
                    let lexical = rootlight_ir::decode_lexical_evidence_envelope(envelope).unwrap();
                    (lexical.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                        && lexical.subject() == rootlight_ir::FactRef::Entity(*symbol))
                    .then_some((envelope, lexical))
                })
                .collect::<Vec<_>>();
            assert_eq!(envelopes.len(), 1, "{key}");
            let (envelope, lexical) = &envelopes[0];
            assert_eq!(lexical, signature);
            let header_source = envelope.evidence.source.as_ref().unwrap();
            assert_eq!(
                header_source,
                &reference(
                    start + header.find(expected_header).unwrap(),
                    expected_header
                )
            );
            check_source(header_source);
            assert_eq!(
                rootlight_ir::validate_lexical_evidence_envelope(
                    envelope,
                    rootlight_ir::FactRef::Entity(*symbol),
                    header_source,
                    explained.data.entity.provenance,
                )
                .unwrap(),
                *lexical
            );
        }
        for mode in [LocateMode::Exact, LocateMode::Text] {
            let located = service
                .code_locate_with_entity_filters_and_budget(
                    receipt.generation,
                    name.to_owned(),
                    mode,
                    vec!["fsharp".to_owned()],
                    vec![file.path.clone()],
                    Some(vec![
                        if key.contains('.') {
                            "parameter"
                        } else if key == "seed" {
                            "variable"
                        } else {
                            "function"
                        }
                        .to_owned(),
                    ]),
                    32,
                    0,
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            assert!(!located.data.truncated);
            let hit = located
                .data
                .hits
                .iter()
                .find(|hit| hit.symbol == Some(*symbol))
                .unwrap();
            assert_eq!(hit.source.as_ref(), Some(entity_source));
            assert_eq!(hit.signature, explained.data.signature);
            check_source(hit.source.as_ref().unwrap());
        }
    }
    let mut reads = Vec::new();
    for (owner, written, target) in [
        ("select", "left", "select.left"),
        ("select", "right", "select.right"),
        ("carry", "first", "carry.first"),
        ("carry", "second", "carry.second"),
        ("carry", "extra", "carry.extra"),
        ("shadow", "seed", "shadow.seed"),
        ("shadow", "spare", "shadow.spare"),
        ("invoke", "select", "select"),
        ("invoke", "input", "invoke.input"),
        ("run", "carry", "carry"),
        ("run", "input", "run.input"),
        ("partial", "carry", "carry"),
        ("partial", "input", "partial.input"),
    ] {
        let (start, declaration) = declaration(owner);
        let body_start = declaration.find(" = ").unwrap() + " = ".len();
        let body = declaration.get(body_start..).unwrap();
        for (offset, _) in body.match_indices(written) {
            let expected = reference(start + body_start + offset, written);
            let sites = document
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Reference && site.source == expected)
                .collect::<Vec<_>>();
            assert_eq!(sites.len(), 1, "{owner}: {written}");
            let site = sites[0];
            assert_eq!(site.enclosing, Some(symbols[owner]));
            assert_eq!(
                site.target,
                OccurrenceTarget::Resolved {
                    symbol: symbols[target]
                }
            );
            check_source(&site.source);
            let edges = document
                .relations
                .iter()
                .filter(|edge| {
                    edge.predicate == RelationPredicate::RefersTo
                        && edge.subject == RelationEndpoint::Occurrence(site.id)
                })
                .collect::<Vec<_>>();
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].object, RelationEndpoint::Entity(symbols[target]));
            assert_eq!(edges[0].evidence.source.as_ref(), Some(&expected));
            reads.push((owner, symbols[target], expected));
        }
    }
    assert_eq!(reads.len(), 17);
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .count(),
        reads.len()
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo)
            .count(),
        reads.len()
    );
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::CallSite)
            .count(),
        3
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .count(),
        2
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::DispatchCandidate)
    );
    for (owner, target, saturated) in [
        ("invoke", "select", true),
        ("run", "carry", true),
        ("partial", "carry", false),
    ] {
        let (start, written) = declaration(owner);
        let body_start = written.find(" = ").unwrap() + " = ".len();
        let expected = reference(start + body_start, written.get(body_start..).unwrap());
        let calls = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Calls
                    && edge.evidence.source.as_ref() == Some(&expected)
            })
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), usize::from(saturated));
        if let Some(call) = calls.first() {
            assert_eq!(call.subject, RelationEndpoint::Entity(symbols[owner]));
            assert_eq!(call.object, RelationEndpoint::Entity(symbols[target]));
        } else {
            assert_eq!(
                document
                    .skipped_regions
                    .iter()
                    .filter(|gap| {
                        gap.domain == IrFactDomain::Relations
                            && gap.reason == SkippedRegionReason::UnsupportedConstruct
                            && gap.source == expected
                            && gap.detail == "fsharp-direct-call-target-unavailable"
                    })
                    .count(),
                1
            );
        }
        let result = service
            .symbol_relationships(
                receipt.generation,
                BTreeSet::from([symbols[owner]]),
                vec![RelationFamily::Calls],
                Some(RelationDirection::Outbound),
                0,
                16,
                0,
                &deadline(),
            )
            .unwrap();
        assert!(!result.data.truncated);
        assert_eq!(result.data.returned_edges, u32::from(saturated));
        let items = result
            .data
            .groups
            .iter()
            .flat_map(|group| &group.items)
            .collect::<Vec<_>>();
        assert_eq!(items.len(), usize::from(saturated));
        if let Some(item) = items.first() {
            assert_eq!(item.symbol, symbols[target]);
            assert_eq!(item.source_refs, vec![expected]);
        }
    }
    let result = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([symbols["shadow"]]),
            vec![RelationFamily::References],
            Some(RelationDirection::Outbound),
            0,
            16,
            0,
            &deadline(),
        )
        .unwrap();
    assert!(!result.data.truncated);
    let mut actual = result
        .data
        .groups
        .iter()
        .flat_map(|group| &group.items)
        .flat_map(|item| {
            item.source_refs
                .iter()
                .map(|source| (item.symbol, source.clone()))
        })
        .collect::<Vec<_>>();
    let mut expected = reads
        .iter()
        .filter(|(owner, _, _)| *owner == "shadow")
        .map(|(_, target, source)| (*target, source.clone()))
        .collect::<Vec<_>>();
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected);
    assert!(!actual.iter().any(|(target, _)| *target == symbols["seed"]));
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.domain == IrFactDomain::Entities
            || matches!(
                gap.reason,
                SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
            )
    }));
    for gap in &document.skipped_regions {
        check_source(&gap.source);
    }
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(
        (coverage.language.as_str(), coverage.tier, coverage.reason),
        ("fsharp", AnalysisTier::TierD, None)
    );
    assert_eq!(coverage.status, CoverageStatus::Bounded);
    (symbols, full.clone())
}

#[test]
fn fsharp_unit_formals_survive_header_mutation_and_durable_reopen() {
    let source = "module Units\r\n// ☃ 🧭\r\nlet seed = 10\r\nlet read () = seed\r\nlet carry (value: int) () = value + seed\r\nlet invoke () = read ()\r\nlet run (value: int) = carry value ()\r\n";
    assert_eq!(source.matches("let read () =").count(), 1);
    let changed_source = source.replace("let read () =", "let read (extra: int) () =");
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let encode = |text: &str| {
            let mut bytes = if encoding == "utf-8" {
                Vec::new()
            } else {
                typed_formal_leaf_bytes("\u{feff}", encoding)
            };
            bytes.extend(typed_formal_leaf_bytes(text, encoding));
            bytes
        };
        let bytes = encode(source);
        let changed_bytes = encode(&changed_source);
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        fs::write(repository.path().join("Units.fs"), &bytes).unwrap();
        let policy = FirstSliceStoragePolicy::resolve_user_config(
            Some("version='1.3'\n[storage]\nretained_generations=4\n"),
            4,
        )
        .unwrap();
        let mut service = FirstSliceService::new_durable_with_policy(
            policy.clone(),
            paths.state_dir(),
            &deadline(),
        )
        .unwrap();
        let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
        let cold = index(&mut service, repository.path(), options);
        assert_eq!(cold.evidence().rebuilt_files, 1);
        let initial = cold.receipt().clone();
        assert_eq!(initial.indexed_files, 1);
        assert!(initial.discovery_complete);
        let (symbols, old_source, definition) =
            assert_unit_formal_generation(&service, &initial, source, &bytes, encoding, false);
        let noop = index(&mut service, repository.path(), options);
        assert_eq!(noop.receipt(), &initial);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
        fs::write(repository.path().join("Units.fs"), &changed_bytes).unwrap();
        let mutation = index(&mut service, repository.path(), options);
        assert_eq!(mutation.evidence().changed_files, 1);
        assert_eq!(mutation.evidence().rebuilt_files, 1);
        let changed = mutation.receipt().clone();
        assert_ne!(changed.generation, initial.generation);
        assert_eq!(changed.parent, Some(initial.generation));
        let (changed_symbols, updated_source, updated_definition) = assert_unit_formal_generation(
            &service,
            &changed,
            &changed_source,
            &changed_bytes,
            encoding,
            true,
        );
        for key in ["seed", "carry", "invoke", "run", "carry.value", "run.value"] {
            assert_eq!(changed_symbols[key], symbols[key], "{encoding}: {key}");
        }
        assert_eq!(updated_source.span().file(), old_source.span().file());
        assert_eq!(updated_definition.span(), definition.span());
        assert_ne!(updated_source.content_hash(), old_source.content_hash());
        exact_source(&service, &old_source, "fsharp", &bytes);
        assert!(
            service
                .source_read_with_options_and_budget(
                    changed.generation,
                    vec![definition],
                    SourceReadOptions::new(),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .is_err()
        );
        let clean = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
        );
        assert_eq!(clean.evidence().rebuilt_files, 1);
        assert!(changed.logical_snapshot.is_some());
        assert_eq!(clean.receipt().logical_snapshot, changed.logical_snapshot);
        let last = clean.receipt().clone();
        assert_eq!(
            assert_unit_formal_generation(
                &service,
                &last,
                &changed_source,
                &changed_bytes,
                encoding,
                true,
            )
            .0,
            changed_symbols
        );
        drop(service);
        let mut reopened =
            FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline())
                .unwrap();
        assert_eq!(
            assert_unit_formal_generation(&reopened, &initial, source, &bytes, encoding, false,).0,
            symbols
        );
        for receipt in [&changed, &last] {
            assert_eq!(
                assert_unit_formal_generation(
                    &reopened,
                    receipt,
                    &changed_source,
                    &changed_bytes,
                    encoding,
                    true,
                )
                .0,
                changed_symbols
            );
        }
        let noop = index(&mut reopened, repository.path(), options);
        assert_eq!(noop.receipt(), &last);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
    }
}

#[test]
fn fsharp_recursive_groups_survive_header_mutation_and_durable_reopen() {
    let source = "module Recursion\r\n// ☃ 🧭\r\nlet seed = 1\r\nlet rec repeat (value: int) = repeat value\r\nlet rec first () = second seed ()\r\nand second (value: int) () = first () + value\r\nlet rec shadow (first: int) = first\r\nlet after () = first ()\r\n";
    assert_eq!(source.matches("let rec first () =").count(), 1);
    let changed_source = source.replace("let rec first () =", "let rec first (extra: int) () =");
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let encode = |text: &str| {
            let mut bytes = if encoding == "utf-8" {
                Vec::new()
            } else {
                typed_formal_leaf_bytes("\u{feff}", encoding)
            };
            bytes.extend(typed_formal_leaf_bytes(text, encoding));
            bytes
        };
        let bytes = encode(source);
        let changed_bytes = encode(&changed_source);
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        fs::write(repository.path().join("Recursion.fs"), &bytes).unwrap();
        let policy = FirstSliceStoragePolicy::resolve_user_config(
            Some("version='1.3'\n[storage]\nretained_generations=4\n"),
            4,
        )
        .unwrap();
        let mut service = FirstSliceService::new_durable_with_policy(
            policy.clone(),
            paths.state_dir(),
            &deadline(),
        )
        .unwrap();
        let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
        let cold = index(&mut service, repository.path(), options);
        assert_eq!(cold.evidence().rebuilt_files, 1);
        let initial = cold.receipt().clone();
        assert_eq!(initial.indexed_files, 1);
        assert!(initial.discovery_complete);
        let (symbols, old_source, definition) =
            assert_recursive_group_generation(&service, &initial, source, &bytes, encoding, false);
        let noop = index(&mut service, repository.path(), options);
        assert_eq!(noop.receipt(), &initial);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
        fs::write(repository.path().join("Recursion.fs"), &changed_bytes).unwrap();
        let mutation = index(&mut service, repository.path(), options);
        assert_eq!(mutation.evidence().changed_files, 1);
        assert_eq!(mutation.evidence().rebuilt_files, 1);
        let changed = mutation.receipt().clone();
        assert_ne!(changed.generation, initial.generation);
        assert_eq!(changed.parent, Some(initial.generation));
        let (changed_symbols, updated_source, updated_definition) =
            assert_recursive_group_generation(
                &service,
                &changed,
                &changed_source,
                &changed_bytes,
                encoding,
                true,
            );
        for key in [
            "seed",
            "repeat",
            "second",
            "shadow",
            "after",
            "repeat.value",
            "second.value",
            "shadow.first",
        ] {
            assert_eq!(changed_symbols[key], symbols[key], "{encoding}: {key}");
        }
        assert_eq!(updated_source.span().file(), old_source.span().file());
        assert_eq!(updated_definition.span(), definition.span());
        assert_ne!(updated_source.content_hash(), old_source.content_hash());
        exact_source(&service, &old_source, "fsharp", &bytes);
        assert!(
            service
                .source_read_with_options_and_budget(
                    changed.generation,
                    vec![definition],
                    SourceReadOptions::new(),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .is_err()
        );
        let clean = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
        );
        assert_eq!(clean.evidence().rebuilt_files, 1);
        assert!(changed.logical_snapshot.is_some());
        assert_eq!(clean.receipt().logical_snapshot, changed.logical_snapshot);
        let last = clean.receipt().clone();
        assert_eq!(
            assert_recursive_group_generation(
                &service,
                &last,
                &changed_source,
                &changed_bytes,
                encoding,
                true,
            )
            .0,
            changed_symbols
        );
        drop(service);
        let mut reopened =
            FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline())
                .unwrap();
        assert_eq!(assert_recursive_group_generation(
            &reopened, &initial, source, &bytes, encoding, false,
        ).0, symbols);
        for receipt in [&changed, &last] {
            assert_eq!(
                assert_recursive_group_generation(
                    &reopened,
                    receipt,
                    &changed_source,
                    &changed_bytes,
                    encoding,
                    true,
                )
                .0,
                changed_symbols
            );
        }
        let noop = index(&mut reopened, repository.path(), options);
        assert_eq!(noop.receipt(), &last);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LocalBindingPhase {
    Initial,
    OuterCapture,
    MixedReadArity,
}

#[test]
fn fsharp_atomic_lambda_formals_survive_durable_reopen_and_noop() {
    let source = "module LambdaShapes\r\n// ☃ 🧭\r\nlet seed = 3\r\nlet outer input =\r\n    let callback = fun (value: int) () (other) -> value + other + input + seed\r\n    let shadow = fun (input: int) -> input\r\n    let wait = fun () -> input + seed\r\n    callback input () input + shadow input + wait ()\r\n";
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let mut bytes = if encoding == "utf-8" {
            Vec::new()
        } else {
            typed_formal_leaf_bytes("\u{feff}", encoding)
        };
        bytes.extend(typed_formal_leaf_bytes(source, encoding));
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        fs::write(repository.path().join("LambdaShapes.fs"), &bytes).unwrap();
        let mut service =
            FirstSliceService::new_durable_with_policy(policy(), paths.state_dir(), &deadline())
                .unwrap();
        let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
        let cold = index(&mut service, repository.path(), options);
        assert_eq!(cold.evidence().rebuilt_files, 1);
        let receipt = cold.receipt().clone();
        assert_eq!(receipt.indexed_files, 1);
        assert!(receipt.discovery_complete);
        let symbols = assert_atomic_lambda_generation(&service, &receipt, source, &bytes, encoding);
        let original = serde_json::to_value(
            service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap()
                .document(),
        )
        .unwrap();
        let noop = index(&mut service, repository.path(), options);
        assert_eq!(noop.receipt(), &receipt);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
        assert_eq!(
            assert_atomic_lambda_generation(&service, &receipt, source, &bytes, encoding),
            symbols
        );
        drop(service);
        let mut reopened =
            FirstSliceService::new_durable_with_policy(policy(), paths.state_dir(), &deadline())
                .unwrap();
        assert_eq!(
            assert_atomic_lambda_generation(&reopened, &receipt, source, &bytes, encoding),
            symbols
        );
        assert_eq!(
            serde_json::to_value(
                reopened
                    .loaded_generation_snapshot(receipt.generation)
                    .unwrap()
                    .document()
            )
            .unwrap(),
            original
        );
        let noop = index(&mut reopened, repository.path(), options);
        assert_eq!(noop.receipt(), &receipt);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
    }
}

fn assert_atomic_lambda_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    bytes: &[u8],
    encoding: &str,
) -> BTreeMap<String, SymbolId> {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let lexical_envelopes = logical_lexical_envelopes(document);
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(
        (
            file.path.as_str(),
            file.language.as_str(),
            file.encoding.as_str()
        ),
        ("LambdaShapes.fs", "fsharp", encoding)
    );
    assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(bytes));
    exact_source(
        service,
        file.evidence.source.as_ref().unwrap(),
        "fsharp",
        bytes,
    );
    let reference = |start: usize, written: &str| {
        assert_eq!(source.get(start..start + written.len()), Some(written));
        let physical = typed_formal_leaf_bytes(source.get(..start).unwrap(), encoding).len()
            + usize::from(encoding != "utf-8") * 2;
        SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                file.id,
                u64::try_from(physical).unwrap(),
                u64::try_from(physical + typed_formal_leaf_bytes(written, encoding).len()).unwrap(),
            )
            .unwrap(),
            file.content_hash,
            None,
        )
    };
    let check_source = |reference: &SourceRef| {
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), file.content_hash);
        exact_source(
            service,
            reference,
            "fsharp",
            bytes
                .get(
                    usize::try_from(reference.span().start_byte()).unwrap()
                        ..usize::try_from(reference.span().end_byte()).unwrap(),
                )
                .unwrap(),
        );
    };
    let mut symbols = BTreeMap::new();
    for (name, kind) in [
        ("seed", EntityKind::Variable),
        ("outer", EntityKind::Function),
        ("callback", EntityKind::Variable),
        ("shadow", EntityKind::Variable),
        ("wait", EntityKind::Variable),
    ] {
        let matches = document
            .entities
            .iter()
            .filter(|entity| entity.kind == kind && entity.canonical_name == name)
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "{name}");
        symbols.insert(name.to_owned(), matches[0].id);
        check_source(matches[0].evidence.source.as_ref().unwrap());
    }
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Function)
            .count(),
        1
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        4
    );
    for (key, owner, written, name, pattern) in [
        ("outer.input", "outer", "outer input =", "input", "input"),
        (
            "callback.value",
            "callback",
            "callback = fun (value: int) () (other) ->",
            "value",
            "(value: int)",
        ),
        (
            "callback.other",
            "callback",
            "callback = fun (value: int) () (other) ->",
            "other",
            "(other)",
        ),
        (
            "shadow.input",
            "shadow",
            "shadow = fun (input: int) ->",
            "input",
            "(input: int)",
        ),
    ] {
        let start = source.find(written).unwrap() + written.find(name).unwrap();
        let leaf = reference(start, name);
        let matches = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == name
                    && entity.container == Some(ContainerRef::Entity(symbols[owner]))
            })
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "{key}");
        let parameter = matches[0];
        symbols.insert(key.to_owned(), parameter.id);
        assert_eq!(parameter.evidence.source.as_ref(), Some(&leaf));
        check_source(&leaf);
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == (OccurrenceTarget::Resolved {
                            symbol: parameter.id,
                        })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].source, leaf);
        let explained = service
            .symbol_explain(receipt.generation, parameter.id, &deadline())
            .unwrap();
        assert_eq!(explained.data.entity.evidence.source.as_ref(), Some(&leaf));
        let signature = explained.data.signature.as_ref().unwrap();
        assert_eq!(signature.text(), pattern);
        assert!(!signature.is_truncated());
        signature
            .verify_source_bytes(&typed_formal_leaf_bytes(pattern, encoding))
            .unwrap();
        let header = reference(
            source.find(written).unwrap() + written.find(pattern).unwrap(),
            pattern,
        );
        check_source(&header);
        let retained = lexical_envelopes
            .iter()
            .filter(|extension| {
                let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
                lexical.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                    && lexical.subject() == rootlight_ir::FactRef::Entity(parameter.id)
            })
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 1, "{key}");
        assert_eq!(retained[0].evidence.source.as_ref(), Some(&header));
        assert_eq!(
            rootlight_ir::validate_lexical_evidence_envelope(
                retained[0],
                rootlight_ir::FactRef::Entity(parameter.id),
                &header,
                parameter.provenance
            )
            .unwrap(),
            *signature
        );
        let located = service
            .code_locate_with_filters_and_budget(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                vec!["fsharp".to_owned()],
                vec!["LambdaShapes.fs".to_owned()],
                16,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        let hits = located
            .data
            .hits
            .iter()
            .filter(|hit| hit.symbol == Some(parameter.id))
            .collect::<Vec<_>>();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source.as_ref(), Some(&leaf));
        assert_eq!(hits[0].signature, explained.data.signature);
        assert!(!located.data.truncated);
    }
    assert_ne!(symbols["outer.input"], symbols["shadow.input"]);
    let mut expected = BTreeMap::<&str, Vec<(SymbolId, SourceRef)>>::new();
    for (owner, target, written, name) in [
        (
            "callback",
            "callback.value",
            "value + other + input + seed",
            "value",
        ),
        (
            "callback",
            "callback.other",
            "value + other + input + seed",
            "other",
        ),
        (
            "callback",
            "outer.input",
            "value + other + input + seed",
            "input",
        ),
        ("callback", "seed", "value + other + input + seed", "seed"),
        (
            "shadow",
            "shadow.input",
            "shadow = fun (input: int) -> input",
            "input",
        ),
        (
            "wait",
            "outer.input",
            "wait = fun () -> input + seed",
            "input",
        ),
        ("wait", "seed", "wait = fun () -> input + seed", "seed"),
    ] {
        let read_source = reference(
            source.find(written).unwrap() + written.rfind(name).unwrap(),
            name,
        );
        let reads = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference && site.source == read_source)
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 1, "{owner}: {name}");
        let read = reads[0];
        assert_eq!(read.enclosing, Some(symbols[owner]));
        assert_eq!(
            read.target,
            OccurrenceTarget::Resolved {
                symbol: symbols[target]
            }
        );
        check_source(&read_source);
        let edges = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(read.id)
            })
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].object, RelationEndpoint::Entity(symbols[target]));
        assert_eq!(edges[0].evidence.source.as_ref(), Some(&read_source));
        expected
            .entry(owner)
            .or_default()
            .push((symbols[target], read_source));
    }
    for (owner, mut wanted) in expected {
        let relationships = service
            .symbol_relationships(
                receipt.generation,
                BTreeSet::from([symbols[owner]]),
                vec![RelationFamily::References],
                Some(RelationDirection::Outbound),
                0,
                16,
                0,
                &deadline(),
            )
            .unwrap();
        assert_eq!(relationships.data.generation, receipt.generation);
        assert!(!relationships.data.truncated);
        assert_eq!(
            relationships.data.returned_edges,
            u32::try_from(wanted.len()).unwrap()
        );
        let mut actual = relationships
            .data
            .groups
            .iter()
            .flat_map(|group| {
                group.items.iter().map(|item| {
                    assert_eq!(item.source_refs.len(), 1);
                    check_source(&item.source_refs[0]);
                    (item.symbol, item.source_refs[0].clone())
                })
            })
            .collect::<Vec<_>>();
        actual.sort();
        wanted.sort();
        assert_eq!(actual, wanted);
    }
    assert!(!document.relations.iter().any(|edge| matches!(
        edge.predicate,
        RelationPredicate::Calls | RelationPredicate::DispatchCandidate
    )));
    assert!(!document.skipped_regions.iter().any(|gap| matches!(
        gap.reason,
        SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
    )));
    symbols
}

#[test]
fn fsharp_lambda_bindings_preserve_capture_and_shadowing_after_reopen() {
    let source = "module Lambdas\r\n// ☃ 🧭\r\nlet seed = 3\r\nlet echo value = value\r\nlet build input =\r\n    let capture = fun value -> value + input + seed\r\n    let shadow = fun input -> input\r\n    let nested = fun outer -> fun inner -> outer + inner + input\r\n    let sibling = fun inner -> inner\r\n    let blocked = fun echo -> echo input\r\n    capture input + shadow input + nested input input + sibling input + blocked input + echo input\r\nlet outside () = inner\r\n";
    let body_source = source.replace("value + input + seed", "value + input + input");
    let header_source = body_source.replace(
        "shadow = fun input -> input",
        "shadow = fun renamed -> renamed",
    );
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let encode = |text: &str| {
            let mut bytes = if encoding == "utf-8" {
                Vec::new()
            } else {
                typed_formal_leaf_bytes("\u{feff}", encoding)
            };
            bytes.extend(typed_formal_leaf_bytes(text, encoding));
            bytes
        };
        let initial_bytes = encode(source);
        let body_bytes = encode(&body_source);
        let header_bytes = encode(&header_source);
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        fs::write(repository.path().join("Lambdas.fs"), &initial_bytes).unwrap();
        let policy = FirstSliceStoragePolicy::resolve_user_config(
            Some("version='1.3'\n[storage]\nretained_generations=4\n"),
            4,
        )
        .unwrap();
        let mut service = FirstSliceService::new_durable_with_policy(
            policy.clone(),
            paths.state_dir(),
            &deadline(),
        )
        .unwrap();
        let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
        let cold = index(&mut service, repository.path(), options);
        assert_eq!(cold.evidence().rebuilt_files, 1);
        let initial = cold.receipt().clone();
        assert_eq!(initial.indexed_files, 1);
        assert!(initial.discovery_complete);
        let (initial_symbols, original_source) = assert_lambda_binding_generation(
            &service,
            &initial,
            source,
            &initial_bytes,
            encoding,
            LambdaBindingPhase::Initial,
        );
        let noop = index(&mut service, repository.path(), options);
        assert_eq!(noop.receipt(), &initial);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
        let stages = [
            (
                source,
                initial_bytes.as_slice(),
                LambdaBindingPhase::Initial,
            ),
            (
                body_source.as_str(),
                body_bytes.as_slice(),
                LambdaBindingPhase::OuterCapture,
            ),
            (
                header_source.as_str(),
                header_bytes.as_slice(),
                LambdaBindingPhase::RenamedShadow,
            ),
        ];
        let mut receipts = vec![initial.clone()];
        let mut symbol_sets = vec![initial_symbols.clone()];
        for (text, raw, phase) in stages.iter().skip(1).copied() {
            fs::write(repository.path().join("Lambdas.fs"), raw).unwrap();
            let mutation = index(&mut service, repository.path(), options);
            assert_eq!(mutation.evidence().changed_files, 1);
            assert_eq!(mutation.evidence().rebuilt_files, 1);
            let changed = mutation.receipt().clone();
            assert_ne!(changed.generation, receipts.last().unwrap().generation);
            assert_eq!(changed.parent, Some(receipts.last().unwrap().generation));
            let (symbols, updated_source) =
                assert_lambda_binding_generation(&service, &changed, text, raw, encoding, phase);
            for (key, symbol) in &initial_symbols {
                if phase == LambdaBindingPhase::RenamedShadow && key == "shadow.parameter" {
                    assert_ne!(symbols[key], *symbol);
                } else {
                    assert_eq!(symbols[key], *symbol, "{encoding}: {key}");
                }
            }
            assert_eq!(updated_source.span().file(), original_source.span().file());
            assert_ne!(
                updated_source.content_hash(),
                original_source.content_hash()
            );
            exact_source(&service, &original_source, "fsharp", &initial_bytes);
            assert!(
                service
                    .source_read_with_options_and_budget(
                        changed.generation,
                        vec![original_source.clone()],
                        SourceReadOptions::new(),
                        FirstSliceBudget::default(),
                        &deadline()
                    )
                    .is_err()
            );
            receipts.push(changed);
            symbol_sets.push(symbols);
        }
        let clean = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
        );
        assert_eq!(clean.evidence().rebuilt_files, 1);
        assert!(receipts.last().unwrap().logical_snapshot.is_some());
        assert_eq!(
            clean.receipt().logical_snapshot,
            receipts.last().unwrap().logical_snapshot
        );
        let final_receipt = clean.receipt().clone();
        assert_eq!(
            assert_lambda_binding_generation(
                &service,
                &final_receipt,
                &header_source,
                &header_bytes,
                encoding,
                LambdaBindingPhase::RenamedShadow
            )
            .0,
            *symbol_sets.last().unwrap()
        );
        drop(service);
        let mut reopened =
            FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline())
                .unwrap();
        for ((receipt, expected), (text, raw, phase)) in
            receipts.iter().zip(&symbol_sets).zip(stages)
        {
            assert_eq!(
                assert_lambda_binding_generation(&reopened, receipt, text, raw, encoding, phase).0,
                *expected
            );
        }
        assert_eq!(
            assert_lambda_binding_generation(
                &reopened,
                &final_receipt,
                &header_source,
                &header_bytes,
                encoding,
                LambdaBindingPhase::RenamedShadow
            )
            .0,
            *symbol_sets.last().unwrap()
        );
        let noop = index(&mut reopened, repository.path(), options);
        assert_eq!(noop.receipt(), &final_receipt);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LambdaBindingPhase {
    Initial,
    OuterCapture,
    RenamedShadow,
}

fn assert_lambda_binding_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    bytes: &[u8],
    encoding: &str,
    phase: LambdaBindingPhase,
) -> (BTreeMap<String, SymbolId>, SourceRef) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(
        (
            file.path.as_str(),
            file.language.as_str(),
            file.encoding.as_str()
        ),
        ("Lambdas.fs", "fsharp", encoding)
    );
    assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(bytes));
    let reference = |start: usize, written: &str| {
        assert_eq!(source.get(start..start + written.len()), Some(written));
        let start = typed_formal_leaf_bytes(source.get(..start).unwrap(), encoding).len()
            + usize::from(encoding != "utf-8") * 2;
        SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                file.id,
                u64::try_from(start).unwrap(),
                u64::try_from(start + typed_formal_leaf_bytes(written, encoding).len()).unwrap(),
            )
            .unwrap(),
            file.content_hash,
            None,
        )
    };
    let check_source = |reference: &SourceRef| {
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), file.content_hash);
        let raw = bytes
            .get(
                usize::try_from(reference.span().start_byte()).unwrap()
                    ..usize::try_from(reference.span().end_byte()).unwrap(),
            )
            .unwrap();
        exact_source(service, reference, "fsharp", raw);
    };
    let full = file.evidence.source.as_ref().unwrap();
    assert_eq!(
        full.span(),
        SourceSpan::new(file.id, 0, file.byte_length).unwrap()
    );
    check_source(full);
    let module = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Module && entity.canonical_name == "Lambdas")
        .collect::<Vec<_>>();
    assert_eq!(module.len(), 1);
    let mut symbols = BTreeMap::from([("module".to_owned(), module[0].id)]);
    let mut definitions = BTreeMap::new();
    let mut declarations = BTreeMap::new();
    for (key, name, kind, owner) in [
        ("seed", "seed", EntityKind::Variable, "module"),
        ("echo", "echo", EntityKind::Function, "module"),
        ("build", "build", EntityKind::Function, "module"),
        ("capture", "capture", EntityKind::Variable, "build"),
        ("shadow", "shadow", EntityKind::Variable, "build"),
        ("nested", "nested", EntityKind::Variable, "build"),
        ("sibling", "sibling", EntityKind::Variable, "build"),
        ("blocked", "blocked", EntityKind::Variable, "build"),
        ("outside", "outside", EntityKind::Function, "module"),
    ] {
        let matching = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == kind
                    && entity.canonical_name == name
                    && entity.container == Some(ContainerRef::Entity(symbols[owner]))
            })
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "{key}");
        symbols.insert(key.to_owned(), matching[0].id);
        let start = source.find(&format!("let {name}")).unwrap() + "let ".len();
        let end = if key == "build" {
            source.find("\r\nlet outside ").unwrap()
        } else {
            start + source.get(start..).unwrap().find("\r\n").unwrap()
        };
        let declaration = reference(start, source.get(start..end).unwrap());
        assert_eq!(matching[0].evidence.source.as_ref(), Some(&declaration));
        check_source(matching[0].evidence.source.as_ref().unwrap());
        declarations.insert(key.to_owned(), declaration);
        definitions.insert(key.to_owned(), reference(start, name));
    }
    let shadow_name = if phase == LambdaBindingPhase::RenamedShadow {
        "renamed"
    } else {
        "input"
    };
    let shadow_header = format!("shadow = fun {shadow_name} ->");
    let parameters = [
        ("echo.value", "echo", "echo value = value", "value"),
        ("build.input", "build", "build input =", "input"),
        (
            "capture.value",
            "capture",
            "capture = fun value ->",
            "value",
        ),
        (
            "shadow.parameter",
            "shadow",
            shadow_header.as_str(),
            shadow_name,
        ),
        ("nested.outer", "nested", "nested = fun outer ->", "outer"),
        (
            "nested.inner",
            "nested",
            "fun outer -> fun inner ->",
            "inner",
        ),
        (
            "sibling.inner",
            "sibling",
            "sibling = fun inner ->",
            "inner",
        ),
        ("blocked.echo", "blocked", "blocked = fun echo ->", "echo"),
    ];
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        parameters.len()
    );
    for (key, owner, written, name) in parameters {
        let expected = reference(
            source.find(written).unwrap() + written.find(name).unwrap(),
            name,
        );
        let matching = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == name
                    && entity.evidence.source.as_ref() == Some(&expected)
            })
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "{key}");
        assert_eq!(
            matching[0].container,
            Some(ContainerRef::Entity(symbols[owner]))
        );
        symbols.insert(key.to_owned(), matching[0].id);
        declarations.insert(key.to_owned(), expected.clone());
        definitions.insert(key.to_owned(), expected);
    }
    assert_ne!(symbols["echo.value"], symbols["capture.value"]);
    assert_ne!(symbols["build.input"], symbols["shadow.parameter"]);
    assert_ne!(symbols["nested.inner"], symbols["sibling.inner"]);
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Function)
            .count(),
        3
    );
    for (key, expected) in &definitions {
        let symbol = symbols[key];
        let explained = service
            .symbol_explain(receipt.generation, symbol, &deadline())
            .unwrap();
        assert_eq!(explained.data.generation, receipt.generation);
        assert_eq!(explained.data.entity.id, symbol);
        assert_eq!(
            explained.data.entity.evidence.source.as_ref(),
            Some(&declarations[key]),
        );
        check_source(explained.data.entity.evidence.source.as_ref().unwrap());
        let sites = explained
            .data
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol })
            })
            .collect::<Vec<_>>();
        assert_eq!(sites.len(), 1, "{key}");
        assert_eq!(&sites[0].source, expected);
        check_source(expected);
        let located = service
            .code_locate_with_filters_and_budget(
                receipt.generation,
                explained.data.entity.canonical_name.clone(),
                LocateMode::Exact,
                vec!["fsharp".to_owned()],
                vec!["Lambdas.fs".to_owned()],
                16,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        let hits = located
            .data
            .hits
            .iter()
            .filter(|hit| hit.symbol == Some(symbol))
            .collect::<Vec<_>>();
        assert_eq!(hits.len(), 1, "{key}");
        assert_eq!(hits[0].source.as_ref(), Some(&declarations[key]));
        check_source(hits[0].source.as_ref().unwrap());
        assert_eq!(hits[0].signature, explained.data.signature);
        for coverage in [&explained.data.coverage, &located.data.coverage] {
            assert!(coverage.iter().all(|record| record.repository == receipt.repository && record.generation == receipt.generation && !matches!(record.scope, rootlight_ir::CoverageScope::File(other) if other != file.id)));
            assert!(coverage.iter().any(|record| record.scope
                == rootlight_ir::CoverageScope::File(file.id)
                && record.domain == IrFactDomain::Relations
                && record.tier == AnalysisTier::TierD
                && record.status != CoverageStatus::Complete));
        }
    }
    let mut reads = Vec::<(&str, Option<SymbolId>, SourceRef)>::new();
    let mut add_read = |owner, target: Option<&str>, written: &str, name: &str, last: bool| {
        let start = source.find(written).unwrap()
            + if last {
                written.rfind(name).unwrap()
            } else {
                written.find(name).unwrap()
            };
        reads.push((
            owner,
            target.map(|key| symbols[key]),
            reference(start, name),
        ));
    };
    add_read(
        "echo",
        Some("echo.value"),
        "echo value = value",
        "value",
        true,
    );
    let capture = if phase == LambdaBindingPhase::Initial {
        "value + input + seed"
    } else {
        "value + input + input"
    };
    add_read("capture", Some("capture.value"), capture, "value", false);
    add_read("capture", Some("build.input"), capture, "input", false);
    if phase == LambdaBindingPhase::Initial {
        add_read("capture", Some("seed"), capture, "seed", false);
    } else {
        add_read("capture", Some("build.input"), capture, "input", true);
    }
    let shadow = format!("shadow = fun {shadow_name} -> {shadow_name}");
    add_read(
        "shadow",
        Some("shadow.parameter"),
        &shadow,
        shadow_name,
        true,
    );
    for (target, name) in [
        ("nested.outer", "outer"),
        ("nested.inner", "inner"),
        ("build.input", "input"),
    ] {
        add_read("nested", Some(target), "outer + inner + input", name, false);
    }
    add_read(
        "sibling",
        Some("sibling.inner"),
        "sibling = fun inner -> inner",
        "inner",
        true,
    );
    add_read(
        "blocked",
        Some("blocked.echo"),
        "blocked = fun echo -> echo input",
        "echo",
        true,
    );
    add_read(
        "blocked",
        Some("build.input"),
        "blocked = fun echo -> echo input",
        "input",
        false,
    );
    for name in ["capture", "shadow", "sibling", "blocked", "echo"] {
        let application = format!("{name} input");
        let written = if name == "echo" {
            "blocked input + echo input".to_owned()
        } else {
            application.clone()
        };
        add_read("build", Some(name), &written, name, true);
        add_read("build", Some("build.input"), &written, "input", true);
    }
    add_read(
        "build",
        Some("nested"),
        "nested input input",
        "nested",
        false,
    );
    add_read(
        "build",
        Some("build.input"),
        "nested input input",
        "input",
        false,
    );
    add_read(
        "build",
        Some("build.input"),
        "nested input input",
        "input",
        true,
    );
    add_read("outside", None, "outside () = inner", "inner", false);
    assert_eq!(reads.len(), 25);
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .count(),
        reads.len()
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo)
            .count(),
        24
    );
    for (owner, target, expected) in &reads {
        let matching = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference && site.source == *expected)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "{owner}: {expected:?}");
        let read = matching[0];
        assert_eq!(read.enclosing, Some(symbols[*owner]));
        check_source(expected);
        let edges = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(read.id)
            })
            .collect::<Vec<_>>();
        if let Some(target) = target {
            assert_eq!(read.target, OccurrenceTarget::Resolved { symbol: *target });
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].object, RelationEndpoint::Entity(*target));
            assert_eq!(edges[0].evidence.source.as_ref(), Some(expected));
        } else {
            assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
            assert!(edges.is_empty());
        }
    }
    let call_source = reference(source.rfind("echo input").unwrap(), "echo input");
    let calls = document
        .relations
        .iter()
        .filter(|edge| edge.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].subject, RelationEndpoint::Entity(symbols["build"]));
    assert_eq!(calls[0].object, RelationEndpoint::Entity(symbols["echo"]));
    assert_eq!(calls[0].evidence.source.as_ref(), Some(&call_source));
    check_source(&call_source);
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::DispatchCandidate)
    );
    let relationships =
        |seed: &str, family, direction, mut expected: Vec<(SymbolId, SourceRef)>| {
            let result = service
                .symbol_relationships(
                    receipt.generation,
                    BTreeSet::from([symbols[seed]]),
                    vec![family],
                    Some(direction),
                    0,
                    32,
                    0,
                    &deadline(),
                )
                .unwrap();
            assert_eq!(result.data.generation, receipt.generation);
            assert!(!result.data.truncated);
            assert!(!result.data.exact);
            assert_eq!(
                result.data.returned_edges,
                u32::try_from(expected.len()).unwrap()
            );
            assert_eq!(
                result.data.total_edges,
                u32::try_from(expected.len()).unwrap()
            );
            let mut actual = Vec::new();
            for group in &result.data.groups {
                assert_eq!(
                    (group.seed, group.family, group.direction),
                    (symbols[seed], family, direction)
                );
                for item in &group.items {
                    assert_eq!(item.source_refs.len(), 1);
                    check_source(&item.source_refs[0]);
                    actual.push((item.symbol, item.source_refs[0].clone()));
                }
            }
            actual.sort();
            expected.sort();
            assert_eq!(actual, expected);
        };
    for owner in [
        "echo", "build", "capture", "shadow", "nested", "sibling", "blocked", "outside",
    ] {
        relationships(
            owner,
            RelationFamily::References,
            RelationDirection::Outbound,
            reads
                .iter()
                .filter(|(consumer, target, _)| *consumer == owner && target.is_some())
                .map(|(_, target, source)| (target.unwrap(), source.clone()))
                .collect(),
        );
        relationships(
            owner,
            RelationFamily::Calls,
            RelationDirection::Outbound,
            if owner == "build" {
                vec![(symbols["echo"], call_source.clone())]
            } else {
                Vec::new()
            },
        );
    }
    for (key, symbol) in &symbols {
        relationships(
            key,
            RelationFamily::References,
            RelationDirection::Inbound,
            reads
                .iter()
                .filter(|(_, target, _)| *target == Some(*symbol))
                .map(|(owner, _, source)| (symbols[*owner], source.clone()))
                .collect(),
        );
    }
    relationships(
        "echo",
        RelationFamily::CalledBy,
        RelationDirection::Inbound,
        vec![(symbols["build"], call_source)],
    );
    for gap in &document.skipped_regions {
        assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
        assert_eq!(gap.repository, receipt.repository);
        assert_eq!(gap.generation, receipt.generation);
        assert_eq!(gap.evidence.source.as_ref(), Some(&gap.source));
        check_source(&gap.source);
    }
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(
        (coverage.language.as_str(), coverage.tier),
        ("fsharp", AnalysisTier::TierD)
    );
    assert_eq!(coverage.reason, None);
    assert_ne!(coverage.status, CoverageStatus::Complete);
    (symbols, full.clone())
}

#[test]
fn fsharp_local_bindings_preserve_capture_and_order_after_reopen() {
    let source = "module Locals\r\n// ☃ 🧭\r\nlet later = 99\r\nlet outer (input: int) =\r\n    let value = input\r\n    let add (value: int) = value + input\r\n    let read () = value\r\n    let before = later\r\n    let later = value + 1\r\n    let input = later + 1\r\n    let invoke (argument: int) = add argument + read ()\r\n    invoke input + before\r\nlet outside () = value\r\n";
    assert_eq!(source.matches("let read () = value").count(), 1);
    let body_source = source.replace("let read () = value", "let read () = input");
    assert_eq!(body_source.matches("let read () = input").count(), 1);
    let header_source =
        body_source.replace("let read () = input", "let read (extra: int) () = input");
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let encode = |text: &str| {
            let mut bytes = if encoding == "utf-8" {
                Vec::new()
            } else {
                typed_formal_leaf_bytes("\u{feff}", encoding)
            };
            bytes.extend(typed_formal_leaf_bytes(text, encoding));
            bytes
        };
        let bytes = encode(source);
        let body_bytes = encode(&body_source);
        let header_bytes = encode(&header_source);
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        fs::write(repository.path().join("Locals.fs"), &bytes).unwrap();
        let policy = FirstSliceStoragePolicy::resolve_user_config(
            Some("version='1.3'\n[storage]\nretained_generations=4\n"),
            4,
        )
        .unwrap();
        let mut service = FirstSliceService::new_durable_with_policy(
            policy.clone(),
            paths.state_dir(),
            &deadline(),
        )
        .unwrap();
        let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
        let cold = index(&mut service, repository.path(), options);
        assert_eq!(cold.evidence().rebuilt_files, 1);
        let initial = cold.receipt().clone();
        assert_eq!(initial.indexed_files, 1);
        assert!(initial.discovery_complete);
        let (symbols, original_source, definition) = assert_local_binding_generation(
            &service,
            &initial,
            source,
            &bytes,
            encoding,
            LocalBindingPhase::Initial,
        );
        let noop = index(&mut service, repository.path(), options);
        assert_eq!(noop.receipt(), &initial);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
        let stages = [
            (source, bytes.as_slice(), LocalBindingPhase::Initial),
            (
                body_source.as_str(),
                body_bytes.as_slice(),
                LocalBindingPhase::OuterCapture,
            ),
            (
                header_source.as_str(),
                header_bytes.as_slice(),
                LocalBindingPhase::MixedReadArity,
            ),
        ];
        let mut receipts = vec![initial.clone()];
        let mut symbol_sets = vec![symbols.clone()];
        for (text, raw, phase) in stages.iter().skip(1).copied() {
            fs::write(repository.path().join("Locals.fs"), raw).unwrap();
            let mutation = index(&mut service, repository.path(), options);
            assert_eq!(mutation.evidence().changed_files, 1);
            assert_eq!(mutation.evidence().rebuilt_files, 1);
            let changed = mutation.receipt().clone();
            let previous = receipts.last().unwrap();
            assert_ne!(changed.generation, previous.generation);
            assert_eq!(changed.parent, Some(previous.generation));
            let (changed_symbols, updated_source, updated_definition) =
                assert_local_binding_generation(&service, &changed, text, raw, encoding, phase);
            if phase == LocalBindingPhase::OuterCapture {
                assert_eq!(changed_symbols, symbols);
            } else {
                for (key, symbol) in &symbols {
                    if key != "outer.read" {
                        assert_eq!(changed_symbols[key], *symbol, "{encoding}: {key}");
                    }
                }
            }
            assert_eq!(updated_source.span().file(), original_source.span().file());
            assert_eq!(updated_definition.span(), definition.span());
            assert_ne!(
                updated_source.content_hash(),
                original_source.content_hash()
            );
            exact_source(&service, &original_source, "fsharp", &bytes);
            assert!(
                service
                    .source_read_with_options_and_budget(
                        changed.generation,
                        vec![definition.clone()],
                        SourceReadOptions::new(),
                        FirstSliceBudget::default(),
                        &deadline(),
                    )
                    .is_err()
            );
            receipts.push(changed);
            symbol_sets.push(changed_symbols);
        }
        let clean = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
        );
        assert_eq!(clean.evidence().rebuilt_files, 1);
        let header = receipts.last().unwrap();
        assert!(header.logical_snapshot.is_some());
        assert_eq!(clean.receipt().logical_snapshot, header.logical_snapshot);
        let last = clean.receipt().clone();
        assert_eq!(
            assert_local_binding_generation(
                &service,
                &last,
                &header_source,
                &header_bytes,
                encoding,
                LocalBindingPhase::MixedReadArity,
            )
            .0,
            *symbol_sets.last().unwrap()
        );
        drop(service);
        let mut reopened =
            FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline())
                .unwrap();
        for ((receipt, expected), (text, raw, phase)) in
            receipts.iter().zip(&symbol_sets).zip(stages)
        {
            assert_eq!(
                assert_local_binding_generation(&reopened, receipt, text, raw, encoding, phase,).0,
                *expected
            );
        }
        assert_eq!(
            assert_local_binding_generation(
                &reopened,
                &last,
                &header_source,
                &header_bytes,
                encoding,
                LocalBindingPhase::MixedReadArity,
            )
            .0,
            *symbol_sets.last().unwrap()
        );
        let noop = index(&mut reopened, repository.path(), options);
        assert_eq!(noop.receipt(), &last);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
    }
}

fn assert_local_binding_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    bytes: &[u8],
    encoding: &str,
    phase: LocalBindingPhase,
) -> (BTreeMap<String, SymbolId>, SourceRef, SourceRef) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let lexical_envelopes = logical_lexical_envelopes(document);
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(
        (
            file.path.as_str(),
            file.language.as_str(),
            file.encoding.as_str()
        ),
        ("Locals.fs", "fsharp", encoding)
    );
    assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(bytes));
    let reference = |start: usize, written: &str| {
        assert_eq!(source.get(start..start + written.len()), Some(written));
        let start = typed_formal_leaf_bytes(source.get(..start).unwrap(), encoding).len()
            + usize::from(encoding != "utf-8") * 2;
        let end = start + typed_formal_leaf_bytes(written, encoding).len();
        SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                file.id,
                u64::try_from(start).unwrap(),
                u64::try_from(end).unwrap(),
            )
            .unwrap(),
            file.content_hash,
            None,
        )
    };
    let check_source = |reference: &SourceRef| {
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), file.content_hash);
        let raw = bytes
            .get(
                usize::try_from(reference.span().start_byte()).unwrap()
                    ..usize::try_from(reference.span().end_byte()).unwrap(),
            )
            .unwrap();
        exact_source(service, reference, "fsharp", raw);
    };
    let full = file.evidence.source.as_ref().unwrap();
    assert_eq!(
        full.span(),
        SourceSpan::new(file.id, 0, file.byte_length).unwrap()
    );
    check_source(full);
    let module = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Module && entity.canonical_name == "Locals")
        .collect::<Vec<_>>();
    assert_eq!(module.len(), 1);
    let mut symbols = BTreeMap::from([("module".to_owned(), module[0].id)]);
    let outer_start = source.find("outer (input: int) =").unwrap();
    let outer_head = source
        .get(outer_start..source.find("\r\nlet outside ").unwrap())
        .unwrap();
    let read_head = match phase {
        LocalBindingPhase::Initial => "read () = value",
        LocalBindingPhase::OuterCapture => "read () = input",
        LocalBindingPhase::MixedReadArity => "read (extra: int) () = input",
    };
    let declarations = [
        (
            "module.later",
            "module",
            "later",
            EntityKind::Variable,
            "later = 99",
        ),
        ("outer", "module", "outer", EntityKind::Function, outer_head),
        (
            "outer.value",
            "outer",
            "value",
            EntityKind::Variable,
            "value = input",
        ),
        (
            "outer.add",
            "outer",
            "add",
            EntityKind::Function,
            "add (value: int) = value + input",
        ),
        (
            "outer.read",
            "outer",
            "read",
            EntityKind::Function,
            read_head,
        ),
        (
            "outer.before",
            "outer",
            "before",
            EntityKind::Variable,
            "before = later",
        ),
        (
            "outer.later",
            "outer",
            "later",
            EntityKind::Variable,
            "later = value + 1",
        ),
        (
            "outer.input.variable",
            "outer",
            "input",
            EntityKind::Variable,
            "input = later + 1",
        ),
        (
            "outer.invoke",
            "outer",
            "invoke",
            EntityKind::Function,
            "invoke (argument: int) = add argument + read ()",
        ),
        (
            "outside",
            "module",
            "outside",
            EntityKind::Function,
            "outside () = value",
        ),
    ];
    let head_start = |key: &str| {
        let (_, _, _, _, head) = declarations
            .iter()
            .find(|(name, _, _, _, _)| *name == key)
            .unwrap();
        source.find(&format!("let {head}")).unwrap() + "let ".len()
    };
    let body_start = |key: &str| {
        let start = head_start(key);
        start + source.get(start..).unwrap().find(" = ").unwrap() + " = ".len()
    };
    let mut definitions = BTreeMap::<String, SourceRef>::new();
    let mut headers = BTreeMap::<String, (String, SourceRef)>::new();
    for (key, owner, name, kind, head) in declarations {
        let matching = document
            .entities
            .iter()
            .filter(|entity| {
                entity.language == "fsharp"
                    && entity.canonical_name == name
                    && entity.kind == kind
                    && entity.container == Some(ContainerRef::Entity(symbols[owner]))
            })
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "{key}");
        let entity = matching[0];
        symbols.insert(key.to_owned(), entity.id);
        assert_eq!(
            entity.evidence.source.as_ref(),
            Some(&reference(head_start(key), head))
        );
        check_source(entity.evidence.source.as_ref().unwrap());
        let parent = document
            .entities
            .iter()
            .find(|entity| entity.id == symbols[owner])
            .unwrap();
        assert_eq!(
            entity.qualified_name,
            format!("{}::{name}", parent.qualified_name)
        );
        definitions.insert(key.to_owned(), reference(head_start(key), name));
        let header = head.get(..head.find('=').unwrap()).unwrap();
        headers.insert(
            key.to_owned(),
            (header.to_owned(), reference(head_start(key), header)),
        );
    }
    let mut formals = vec![
        ("outer.input.parameter", "outer", "input"),
        ("outer.add.value", "outer.add", "value"),
        ("outer.invoke.argument", "outer.invoke", "argument"),
    ];
    if phase == LocalBindingPhase::MixedReadArity {
        formals.push(("outer.read.extra", "outer.read", "extra"));
    }
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        formals.len()
    );
    for (key, owner, name) in formals {
        let matching = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == name
                    && entity.container == Some(ContainerRef::Entity(symbols[owner]))
            })
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "{key}");
        symbols.insert(key.to_owned(), matching[0].id);
        let pattern = format!("({name}: int)");
        let start = head_start(owner)
            + source
                .get(head_start(owner)..)
                .unwrap()
                .find(&pattern)
                .unwrap();
        definitions.insert(key.to_owned(), reference(start + 1, name));
        headers.insert(
            key.to_owned(),
            (pattern.clone(), reference(start, &pattern)),
        );
    }
    assert_ne!(
        symbols["outer.input.parameter"],
        symbols["outer.input.variable"]
    );
    assert_ne!(symbols["module.later"], symbols["outer.later"]);
    for (key, expected) in &definitions {
        let symbol = symbols[key];
        let explained = service
            .symbol_explain(receipt.generation, symbol, &deadline())
            .unwrap();
        assert_eq!(explained.data.generation, receipt.generation);
        assert_eq!(explained.data.entity.id, symbol);
        check_source(explained.data.entity.evidence.source.as_ref().unwrap());
        let sites = explained
            .data
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol })
            })
            .collect::<Vec<_>>();
        assert_eq!(sites.len(), 1, "{key}");
        assert_eq!(&sites[0].source, expected);
        check_source(expected);
        let located = service
            .code_locate_with_filters_and_budget(
                receipt.generation,
                explained.data.entity.canonical_name.clone(),
                LocateMode::Exact,
                vec!["fsharp".to_owned()],
                vec!["Locals.fs".to_owned()],
                16,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        let hit = located
            .data
            .hits
            .iter()
            .find(|hit| hit.symbol == Some(symbol))
            .unwrap();
        assert_eq!(hit.file, file.id);
        check_source(hit.source.as_ref().unwrap());
        assert_eq!(hit.signature, explained.data.signature);
        for coverage in [&explained.data.coverage, &located.data.coverage] {
            assert!(coverage.iter().all(|record| {
                record.repository == receipt.repository && record.generation == receipt.generation
                    && !matches!(record.scope, rootlight_ir::CoverageScope::File(other) if other != file.id)
            }));
            assert!(coverage.iter().any(|record| {
                record.scope == rootlight_ir::CoverageScope::File(file.id)
                    && record.domain == IrFactDomain::Relations
                    && record.tier == AnalysisTier::TierD
                    && record.status != CoverageStatus::Complete
            }));
        }
        let (header, header_source) = &headers[key];
        let signature = explained.data.signature.as_ref().unwrap();
        assert_eq!(signature.text(), header.as_str());
        assert!(!signature.is_truncated());
        assert_eq!(
            signature.complete_text_hash(),
            content_hash(header.as_bytes())
        );
        assert_eq!(
            signature.source_encoding(),
            match encoding {
                "utf-8" => None,
                "utf-16le" => Some(rootlight_ir::SourceTextEncoding::Utf16Le),
                "utf-16be" => Some(rootlight_ir::SourceTextEncoding::Utf16Be),
                _ => panic!("unsupported fixture encoding"),
            }
        );
        let retained = lexical_envelopes
            .iter()
            .filter(|extension| {
                let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
                lexical.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                    && lexical.subject() == rootlight_ir::FactRef::Entity(symbol)
            })
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 1, "{key}");
        assert_eq!(retained[0].evidence.source.as_ref(), Some(header_source));
        check_source(header_source);
        signature
            .verify_source_bytes(
                bytes
                    .get(
                        usize::try_from(header_source.span().start_byte()).unwrap()
                            ..usize::try_from(header_source.span().end_byte()).unwrap(),
                    )
                    .unwrap(),
            )
            .unwrap();
    }
    let final_start = source.find("invoke input + before").unwrap();
    let read_name = if phase == LocalBindingPhase::Initial {
        "value"
    } else {
        "input"
    };
    let read_target = if phase == LocalBindingPhase::Initial {
        "outer.value"
    } else {
        "outer.input.parameter"
    };
    let mut reads = Vec::new();
    for (owner, start, written, target) in [
        (
            "outer.value",
            body_start("outer.value"),
            "input",
            Some("outer.input.parameter"),
        ),
        (
            "outer.add",
            body_start("outer.add"),
            "value",
            Some("outer.add.value"),
        ),
        (
            "outer.add",
            body_start("outer.add") + "value + ".len(),
            "input",
            Some("outer.input.parameter"),
        ),
        (
            "outer.read",
            body_start("outer.read"),
            read_name,
            Some(read_target),
        ),
        (
            "outer.before",
            body_start("outer.before"),
            "later",
            Some("module.later"),
        ),
        (
            "outer.later",
            body_start("outer.later"),
            "value",
            Some("outer.value"),
        ),
        (
            "outer.input.variable",
            body_start("outer.input.variable"),
            "later",
            Some("outer.later"),
        ),
        (
            "outer.invoke",
            body_start("outer.invoke"),
            "add",
            Some("outer.add"),
        ),
        (
            "outer.invoke",
            body_start("outer.invoke") + "add ".len(),
            "argument",
            Some("outer.invoke.argument"),
        ),
        (
            "outer.invoke",
            body_start("outer.invoke") + "add argument + ".len(),
            "read",
            Some("outer.read"),
        ),
        ("outer", final_start, "invoke", Some("outer.invoke")),
        (
            "outer",
            final_start + "invoke ".len(),
            "input",
            Some("outer.input.variable"),
        ),
        (
            "outer",
            final_start + "invoke input + ".len(),
            "before",
            Some("outer.before"),
        ),
        ("outside", body_start("outside"), "value", None),
    ] {
        let expected = reference(start, written);
        let sites = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference && site.source.span() == expected.span()
            })
            .collect::<Vec<_>>();
        assert_eq!(sites.len(), 1, "{owner}: {written}");
        assert_eq!(sites[0].enclosing, Some(symbols[owner]));
        assert_eq!(sites[0].source, expected);
        check_source(&expected);
        let edges = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(sites[0].id)
            })
            .collect::<Vec<_>>();
        if let Some(target) = target {
            assert_eq!(
                sites[0].target,
                OccurrenceTarget::Resolved {
                    symbol: symbols[target]
                }
            );
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].object, RelationEndpoint::Entity(symbols[target]));
            assert_eq!(edges[0].evidence.source.as_ref(), Some(&expected));
            assert!(
                !document
                    .skipped_regions
                    .iter()
                    .any(|gap| { gap.domain == IrFactDomain::Relations && gap.source == expected })
            );
            reads.push((owner, symbols[target], expected));
        } else {
            assert!(matches!(
                sites[0].target,
                OccurrenceTarget::Unresolved { .. }
            ));
            assert!(edges.is_empty());
            assert_eq!(
                document
                    .skipped_regions
                    .iter()
                    .filter(|gap| {
                        gap.domain == IrFactDomain::Relations
                            && gap.source == expected
                            && gap.reason == SkippedRegionReason::UnsupportedConstruct
                            && gap.detail == "fsharp-formal-binding-target-unavailable"
                    })
                    .count(),
                1
            );
        }
    }
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .count(),
        14
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo)
            .count(),
        13
    );
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::CallSite)
            .count(),
        3
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .count(),
        if phase == LocalBindingPhase::MixedReadArity {
            2
        } else {
            3
        }
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::DispatchCandidate)
    );
    let mut calls = Vec::new();
    for (owner, target, start, application, saturated) in [
        (
            "outer.invoke",
            "outer.add",
            body_start("outer.invoke"),
            "add argument",
            true,
        ),
        (
            "outer.invoke",
            "outer.read",
            body_start("outer.invoke") + "add argument + ".len(),
            "read ()",
            phase != LocalBindingPhase::MixedReadArity,
        ),
        ("outer", "outer.invoke", final_start, "invoke input", true),
    ] {
        let expected = reference(start, application);
        let sites = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::CallSite && site.source.span() == expected.span()
            })
            .collect::<Vec<_>>();
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].enclosing, Some(symbols[owner]));
        assert_eq!(sites[0].source, expected);
        check_source(&expected);
        let edges = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Calls
                    && edge.evidence.source.as_ref() == Some(&expected)
            })
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), usize::from(saturated));
        if saturated {
            assert_eq!(edges[0].subject, RelationEndpoint::Entity(symbols[owner]));
            assert_eq!(edges[0].object, RelationEndpoint::Entity(symbols[target]));
            calls.push((owner, symbols[target], expected));
        } else {
            assert_eq!(
                document
                    .skipped_regions
                    .iter()
                    .filter(|gap| {
                        gap.domain == IrFactDomain::Relations
                            && gap.source == expected
                            && gap.reason == SkippedRegionReason::UnsupportedConstruct
                            && gap.detail == "fsharp-direct-call-target-unavailable"
                    })
                    .count(),
                1
            );
        }
    }
    let relationships = |seed: &str,
                         family: RelationFamily,
                         direction: RelationDirection,
                         mut expected: Vec<(SymbolId, SourceRef)>| {
        let result = service
            .symbol_relationships(
                receipt.generation,
                BTreeSet::from([symbols[seed]]),
                vec![family],
                Some(direction),
                0,
                16,
                0,
                &deadline(),
            )
            .unwrap();
        assert_eq!(result.data.generation, receipt.generation);
        assert!(!result.data.truncated);
        assert!(!result.data.exact);
        assert_eq!(
            result.data.returned_edges,
            u32::try_from(expected.len()).unwrap()
        );
        assert_eq!(
            result.data.total_edges,
            u32::try_from(expected.len()).unwrap()
        );
        let mut actual = Vec::new();
        for group in &result.data.groups {
            assert_eq!(
                (group.seed, group.family, group.direction),
                (symbols[seed], family, direction)
            );
            for item in &group.items {
                assert_eq!(item.source_refs.len(), 1);
                check_source(&item.source_refs[0]);
                actual.push((item.symbol, item.source_refs[0].clone()));
            }
        }
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected);
    };
    for (owner, _, _, _, _) in declarations {
        relationships(
            owner,
            RelationFamily::References,
            RelationDirection::Outbound,
            reads
                .iter()
                .filter(|(consumer, _, _)| *consumer == owner)
                .map(|(_, target, reference)| (*target, reference.clone()))
                .collect(),
        );
        relationships(
            owner,
            RelationFamily::Calls,
            RelationDirection::Outbound,
            calls
                .iter()
                .filter(|(consumer, _, _)| *consumer == owner)
                .map(|(_, target, reference)| (*target, reference.clone()))
                .collect(),
        );
        relationships(
            owner,
            RelationFamily::CalledBy,
            RelationDirection::Inbound,
            calls
                .iter()
                .filter(|(_, target, _)| *target == symbols[owner])
                .map(|(consumer, _, reference)| (symbols[*consumer], reference.clone()))
                .collect(),
        );
    }
    for (key, symbol) in &symbols {
        relationships(
            key,
            RelationFamily::References,
            RelationDirection::Inbound,
            reads
                .iter()
                .filter(|(_, target, _)| target == symbol)
                .map(|(consumer, _, reference)| (symbols[*consumer], reference.clone()))
                .collect(),
        );
    }
    for gap in &document.skipped_regions {
        assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
        assert_ne!(gap.domain, IrFactDomain::Entities);
        assert_eq!(gap.repository, receipt.repository);
        assert_eq!(gap.generation, receipt.generation);
        assert_eq!(gap.evidence.source.as_ref(), Some(&gap.source));
        check_source(&gap.source);
    }
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(
        (coverage.language.as_str(), coverage.tier),
        ("fsharp", AnalysisTier::TierD)
    );
    assert_eq!(coverage.reason, None);
    assert_ne!(coverage.status, CoverageStatus::Complete);
    (symbols, full.clone(), definitions["outer.read"].clone())
}

fn assert_recursive_group_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    bytes: &[u8],
    encoding: &str,
    mixed: bool,
) -> (BTreeMap<String, SymbolId>, SourceRef, SourceRef) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(
        (
            file.path.as_str(),
            file.language.as_str(),
            file.encoding.as_str()
        ),
        ("Recursion.fs", "fsharp", encoding)
    );
    assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(bytes));
    let reference = |start: usize, written: &str| {
        assert_eq!(source.get(start..start + written.len()), Some(written));
        let start = typed_formal_leaf_bytes(source.get(..start).unwrap(), encoding).len()
            + usize::from(encoding != "utf-8") * 2;
        let end = start + typed_formal_leaf_bytes(written, encoding).len();
        SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                file.id,
                u64::try_from(start).unwrap(),
                u64::try_from(end).unwrap(),
            )
            .unwrap(),
            file.content_hash,
            None,
        )
    };
    let check_source = |reference: &SourceRef| {
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), file.content_hash);
        let raw = bytes
            .get(
                usize::try_from(reference.span().start_byte()).unwrap()
                    ..usize::try_from(reference.span().end_byte()).unwrap(),
            )
            .unwrap();
        exact_source(service, reference, "fsharp", raw);
    };
    let full = file.evidence.source.as_ref().unwrap();
    assert_eq!(
        full.span(),
        SourceSpan::new(file.id, 0, file.byte_length).unwrap()
    );
    check_source(full);
    let declarations = [
        ("seed", "let ", "seed = 1", EntityKind::Variable),
        (
            "repeat",
            "let rec ",
            "repeat (value: int) = repeat value",
            EntityKind::Function,
        ),
        (
            "first",
            "let rec ",
            if mixed {
                "first (extra: int) () = second seed ()"
            } else {
                "first () = second seed ()"
            },
            EntityKind::Function,
        ),
        (
            "second",
            "and ",
            "second (value: int) () = first () + value",
            EntityKind::Function,
        ),
        (
            "shadow",
            "let rec ",
            "shadow (first: int) = first",
            EntityKind::Function,
        ),
        ("after", "let ", "after () = first ()", EntityKind::Function),
    ];
    let head_start = |owner: &str| {
        let (_, prefix, head, _) = declarations
            .iter()
            .find(|(name, _, _, _)| *name == owner)
            .unwrap();
        source.find(&format!("{prefix}{head}")).unwrap() + prefix.len()
    };
    let body_start = |owner: &str| {
        let start = head_start(owner);
        start + source.get(start..).unwrap().find(" = ").unwrap() + " = ".len()
    };
    let mut symbols = BTreeMap::new();
    for (name, _, head, kind) in declarations {
        let matching = document
            .entities
            .iter()
            .filter(|entity| {
                entity.language == "fsharp" && entity.canonical_name == name && entity.kind == kind
            })
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "{name}");
        symbols.insert(name.to_owned(), matching[0].id);
        if kind == EntityKind::Function {
            assert_eq!(
                matching[0].evidence.source.as_ref(),
                Some(&reference(head_start(name), head))
            );
            check_source(matching[0].evidence.source.as_ref().unwrap());
        }
    }
    let module = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Module && entity.canonical_name == "Recursion")
        .collect::<Vec<_>>();
    assert_eq!(module.len(), 1);
    for owner in ["repeat", "first", "second", "shadow", "after"] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbols[owner])
            .unwrap();
        assert_eq!(entity.container, Some(ContainerRef::Entity(module[0].id)));
    }
    let first = document
        .entities
        .iter()
        .find(|entity| entity.id == symbols["first"])
        .unwrap();
    let second = document
        .entities
        .iter()
        .find(|entity| entity.id == symbols["second"])
        .unwrap();
    assert!(
        first.evidence.source.as_ref().unwrap().span().end_byte()
            <= second.evidence.source.as_ref().unwrap().span().start_byte()
    );
    let mut formals = vec![
        ("repeat", "value"),
        ("second", "value"),
        ("shadow", "first"),
    ];
    if mixed {
        formals.push(("first", "extra"));
    }
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        formals.len()
    );
    for (owner, name) in formals {
        let matching = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == name
                    && entity.container == Some(ContainerRef::Entity(symbols[owner]))
            })
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "{owner}.{name}");
        symbols.insert(format!("{owner}.{name}"), matching[0].id);
    }
    let mut first_definition = None;
    for (key, symbol) in &symbols {
        let explained = service
            .symbol_explain(receipt.generation, *symbol, &deadline())
            .unwrap();
        assert_eq!(explained.data.generation, receipt.generation);
        assert_eq!(explained.data.entity.id, *symbol);
        check_source(explained.data.entity.evidence.source.as_ref().unwrap());
        let name = key.rsplit('.').next().unwrap();
        let start = if let Some((owner, _)) = key.split_once('.') {
            let head = head_start(owner);
            head + source
                .get(head..body_start(owner))
                .unwrap()
                .find(&format!("({name}:"))
                .unwrap()
                + 1
        } else {
            head_start(name)
        };
        let expected = reference(start, name);
        let definitions = explained
            .data
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: *symbol })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1, "{key}");
        assert_eq!(definitions[0].source, expected);
        check_source(&expected);
        if key == "first" {
            first_definition = Some(expected);
        }
        let located = service
            .code_locate_with_filters_and_budget(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                vec!["fsharp".to_owned()],
                vec!["Recursion.fs".to_owned()],
                16,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        let hit = located
            .data
            .hits
            .iter()
            .find(|hit| hit.symbol == Some(*symbol))
            .unwrap();
        assert_eq!(hit.file, file.id);
        check_source(hit.source.as_ref().unwrap());
        assert_eq!(hit.signature, explained.data.signature);
        if !key.contains('.') && key != "seed" {
            let header = source
                .get(head_start(key)..body_start(key) - "= ".len())
                .unwrap();
            let signature = explained.data.signature.as_ref().unwrap();
            assert_eq!(signature.text(), header);
            assert!(!signature.is_truncated());
            assert_eq!(
                signature.complete_text_hash(),
                content_hash(header.as_bytes())
            );
        }
    }
    let mut reads = Vec::new();
    for (owner, offset, written, target) in [
        ("repeat", 0, "repeat", "repeat"),
        ("repeat", "repeat ".len(), "value", "repeat.value"),
        ("first", 0, "second", "second"),
        ("first", "second ".len(), "seed", "seed"),
        ("second", 0, "first", "first"),
        ("second", "first () + ".len(), "value", "second.value"),
        ("shadow", 0, "first", "shadow.first"),
        ("after", 0, "first", "first"),
    ] {
        let expected = reference(body_start(owner) + offset, written);
        let sites = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference && site.source.span() == expected.span()
            })
            .collect::<Vec<_>>();
        assert_eq!(sites.len(), 1, "{owner}: {written}");
        assert_eq!(
            sites[0].target,
            OccurrenceTarget::Resolved {
                symbol: symbols[target]
            }
        );
        assert_eq!(sites[0].enclosing, Some(symbols[owner]));
        assert_eq!(sites[0].source, expected);
        check_source(&expected);
        let edges = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(sites[0].id)
            })
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].object, RelationEndpoint::Entity(symbols[target]));
        assert_eq!(edges[0].evidence.source.as_ref(), Some(&expected));
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| { gap.domain == IrFactDomain::Relations && gap.source == expected })
        );
        reads.push((owner, symbols[target], expected));
    }
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .count(),
        8
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo)
            .count(),
        8
    );
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::CallSite)
            .count(),
        4
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .count(),
        if mixed { 2 } else { 4 }
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::DispatchCandidate)
    );
    let mut calls = Vec::new();
    for (owner, target, application, saturated) in [
        ("repeat", "repeat", "repeat value", true),
        ("first", "second", "second seed ()", true),
        ("second", "first", "first ()", !mixed),
        ("after", "first", "first ()", !mixed),
    ] {
        let expected = reference(body_start(owner), application);
        let sites = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::CallSite && site.source.span() == expected.span()
            })
            .collect::<Vec<_>>();
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].enclosing, Some(symbols[owner]));
        assert_eq!(sites[0].source, expected);
        check_source(&expected);
        let edges = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Calls
                    && edge.evidence.source.as_ref() == Some(&expected)
            })
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), usize::from(saturated));
        if saturated {
            assert_eq!(edges[0].subject, RelationEndpoint::Entity(symbols[owner]));
            assert_eq!(edges[0].object, RelationEndpoint::Entity(symbols[target]));
            calls.push((owner, symbols[target], expected));
        } else {
            assert_eq!(
                document
                    .skipped_regions
                    .iter()
                    .filter(|gap| {
                        gap.domain == IrFactDomain::Relations
                            && gap.reason == SkippedRegionReason::UnsupportedConstruct
                            && gap.source == expected
                            && gap.detail == "fsharp-direct-call-target-unavailable"
                    })
                    .count(),
                1
            );
        }
    }
    let relationships = |seed: &str,
                         family: RelationFamily,
                         direction: RelationDirection,
                         mut expected: Vec<(SymbolId, SourceRef)>| {
        let result = service
            .symbol_relationships(
                receipt.generation,
                BTreeSet::from([symbols[seed]]),
                vec![family],
                Some(direction),
                0,
                16,
                0,
                &deadline(),
            )
            .unwrap();
        assert_eq!(result.data.generation, receipt.generation);
        assert!(!result.data.truncated);
        assert!(!result.data.exact);
        assert_eq!(
            result.data.returned_edges,
            u32::try_from(expected.len()).unwrap()
        );
        assert_eq!(
            result.data.total_edges,
            u32::try_from(expected.len()).unwrap()
        );
        let mut actual = Vec::new();
        for group in &result.data.groups {
            assert_eq!(
                (group.seed, group.family, group.direction),
                (symbols[seed], family, direction)
            );
            for item in &group.items {
                assert_eq!(item.source_refs.len(), 1);
                check_source(&item.source_refs[0]);
                actual.push((item.symbol, item.source_refs[0].clone()));
            }
        }
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected);
    };
    for owner in ["repeat", "first", "second", "shadow", "after"] {
        relationships(
            owner,
            RelationFamily::References,
            RelationDirection::Outbound,
            reads
                .iter()
                .filter(|(consumer, _, _)| *consumer == owner)
                .map(|(_, target, reference)| (*target, reference.clone()))
                .collect(),
        );
        relationships(
            owner,
            RelationFamily::Calls,
            RelationDirection::Outbound,
            calls
                .iter()
                .filter(|(consumer, _, _)| *consumer == owner)
                .map(|(_, target, reference)| (*target, reference.clone()))
                .collect(),
        );
        relationships(
            owner,
            RelationFamily::CalledBy,
            RelationDirection::Inbound,
            calls
                .iter()
                .filter(|(_, target, _)| *target == symbols[owner])
                .map(|(consumer, _, reference)| (symbols[*consumer], reference.clone()))
                .collect(),
        );
    }
    for (key, symbol) in &symbols {
        relationships(
            key,
            RelationFamily::References,
            RelationDirection::Inbound,
            reads
                .iter()
                .filter(|(_, target, _)| target == symbol)
                .map(|(consumer, _, reference)| (symbols[*consumer], reference.clone()))
                .collect(),
        );
    }
    for gap in &document.skipped_regions {
        assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
        assert_ne!(gap.domain, IrFactDomain::Entities);
        assert_eq!(gap.repository, receipt.repository);
        assert_eq!(gap.generation, receipt.generation);
        assert_eq!(gap.evidence.source.as_ref(), Some(&gap.source));
        check_source(&gap.source);
    }
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(
        (coverage.language.as_str(), coverage.tier),
        ("fsharp", AnalysisTier::TierD)
    );
    assert_eq!(coverage.reason, None);
    if mixed || !document.skipped_regions.is_empty() {
        assert_ne!(coverage.status, CoverageStatus::Complete);
    }
    (symbols, full.clone(), first_definition.unwrap())
}

fn assert_unit_formal_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    bytes: &[u8],
    encoding: &str,
    mixed: bool,
) -> (BTreeMap<String, SymbolId>, SourceRef, SourceRef) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(
        (
            file.path.as_str(),
            file.language.as_str(),
            file.encoding.as_str()
        ),
        ("Units.fs", "fsharp", encoding)
    );
    assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(bytes));
    let span = |start: usize, written: &str| {
        assert_eq!(source.get(start..start + written.len()), Some(written));
        let start = typed_formal_leaf_bytes(source.get(..start).unwrap(), encoding).len()
            + usize::from(encoding != "utf-8") * 2;
        let end = start + typed_formal_leaf_bytes(written, encoding).len();
        SourceSpan::new(
            file.id,
            u64::try_from(start).unwrap(),
            u64::try_from(end).unwrap(),
        )
        .unwrap()
    };
    let reference = |start: usize, written: &str| {
        SourceRef::new(
            receipt.repository,
            receipt.generation,
            span(start, written),
            file.content_hash,
            None,
        )
    };
    let check_source = |reference: &SourceRef| {
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), file.content_hash);
        let raw = bytes
            .get(
                usize::try_from(reference.span().start_byte()).unwrap()
                    ..usize::try_from(reference.span().end_byte()).unwrap(),
            )
            .unwrap();
        exact_source(service, reference, "fsharp", raw);
    };
    let full = file.evidence.source.as_ref().unwrap();
    assert_eq!(
        full.span(),
        SourceSpan::new(file.id, 0, file.byte_length).unwrap()
    );
    check_source(full);
    let mut symbols = BTreeMap::new();
    for (name, kind) in [
        ("seed", EntityKind::Variable),
        ("read", EntityKind::Function),
        ("carry", EntityKind::Function),
        ("invoke", EntityKind::Function),
        ("run", EntityKind::Function),
    ] {
        let matching = document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "{name}");
        symbols.insert(name.to_owned(), matching[0].id);
    }
    let mut formals = vec![("carry", "value"), ("run", "value")];
    if mixed {
        formals.push(("read", "extra"));
    }
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        formals.len()
    );
    for (owner, name) in formals {
        let matching = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == name
                    && entity.container == Some(ContainerRef::Entity(symbols[owner]))
            })
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "{owner}.{name}");
        symbols.insert(format!("{owner}.{name}"), matching[0].id);
    }
    let mut read_definition = None;
    for (key, symbol) in &symbols {
        let explained = service
            .symbol_explain(receipt.generation, *symbol, &deadline())
            .unwrap();
        assert_eq!(explained.data.generation, receipt.generation);
        assert_eq!(explained.data.entity.id, *symbol);
        check_source(explained.data.entity.evidence.source.as_ref().unwrap());
        let definitions = explained
            .data
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: *symbol })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1, "{key}");
        let name = key.rsplit('.').next().unwrap();
        let start = if let Some((owner, _)) = key.split_once('.') {
            source.find(&format!("let {owner} ")).unwrap()
                + source
                    .get(source.find(&format!("let {owner} ")).unwrap()..)
                    .unwrap()
                    .find(name)
                    .unwrap()
        } else {
            source.find(&format!("let {name} ")).unwrap() + "let ".len()
        };
        assert_eq!(definitions[0].source.span(), span(start, name));
        check_source(&definitions[0].source);
        if key == "read" {
            read_definition = Some(definitions[0].source.clone());
        }
        let located = service
            .code_locate_with_filters_and_budget(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                vec!["fsharp".to_owned()],
                vec!["Units.fs".to_owned()],
                16,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        let hit = located
            .data
            .hits
            .iter()
            .find(|hit| hit.symbol == Some(*symbol))
            .unwrap();
        check_source(hit.source.as_ref().unwrap());
        assert_eq!(hit.signature, explained.data.signature);
    }
    for (owner, header) in [
        (
            "read",
            if mixed {
                "read (extra: int) () "
            } else {
                "read () "
            },
        ),
        ("carry", "carry (value: int) () "),
        ("invoke", "invoke () "),
        ("run", "run (value: int) "),
    ] {
        let explained = service
            .symbol_explain(receipt.generation, symbols[owner], &deadline())
            .unwrap();
        let signature = explained.data.signature.as_ref().unwrap();
        assert_eq!(signature.text(), header);
        assert!(!signature.is_truncated());
        assert_eq!(
            signature.complete_text_hash(),
            content_hash(header.as_bytes())
        );
    }
    let body_start = |owner: &str| {
        let start = source.find(&format!("let {owner} ")).unwrap();
        start + source.get(start..).unwrap().find(" = ").unwrap() + " = ".len()
    };
    let mut reads = Vec::new();
    for (owner, offset, written, target) in [
        ("read", 0, "seed", "seed"),
        ("carry", 0, "value", "carry.value"),
        ("carry", "value + ".len(), "seed", "seed"),
        ("invoke", 0, "read", "read"),
        ("run", 0, "carry", "carry"),
        ("run", "carry ".len(), "value", "run.value"),
    ] {
        let expected = reference(body_start(owner) + offset, written);
        let sites = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference && site.source.span() == expected.span()
            })
            .collect::<Vec<_>>();
        assert_eq!(sites.len(), 1);
        assert_eq!(
            sites[0].target,
            OccurrenceTarget::Resolved {
                symbol: symbols[target]
            }
        );
        assert_eq!(sites[0].enclosing, Some(symbols[owner]));
        assert_eq!(sites[0].source, expected);
        check_source(&expected);
        let edges = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(sites[0].id)
            })
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].object, RelationEndpoint::Entity(symbols[target]));
        assert_eq!(edges[0].evidence.source.as_ref(), Some(&expected));
        reads.push((owner, symbols[target], expected));
    }
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .count(),
        6
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo)
            .count(),
        6
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::DispatchCandidate)
    );
    let relationships = |owner: &str,
                         family: RelationFamily,
                         direction: RelationDirection,
                         mut expected: Vec<(SymbolId, SourceRef)>| {
        let result = service
            .symbol_relationships(
                receipt.generation,
                BTreeSet::from([symbols[owner]]),
                vec![family],
                Some(direction),
                0,
                16,
                0,
                &deadline(),
            )
            .unwrap();
        assert_eq!(result.data.generation, receipt.generation);
        assert!(!result.data.truncated);
        assert!(!result.data.exact);
        assert_eq!(
            result.data.returned_edges,
            u32::try_from(expected.len()).unwrap()
        );
        assert_eq!(
            result.data.total_edges,
            u32::try_from(expected.len()).unwrap()
        );
        let mut actual = Vec::new();
        for group in &result.data.groups {
            assert_eq!(
                (group.seed, group.family, group.direction),
                (symbols[owner], family, direction)
            );
            for item in &group.items {
                assert_eq!(item.source_refs.len(), 1);
                check_source(&item.source_refs[0]);
                actual.push((item.symbol, item.source_refs[0].clone()));
            }
        }
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected);
    };
    for owner in ["read", "carry", "invoke", "run"] {
        relationships(
            owner,
            RelationFamily::References,
            RelationDirection::Outbound,
            reads
                .iter()
                .filter(|(consumer, _, _)| *consumer == owner)
                .map(|(_, target, reference)| (*target, reference.clone()))
                .collect(),
        );
    }
    relationships(
        "seed",
        RelationFamily::References,
        RelationDirection::Inbound,
        reads
            .iter()
            .filter(|(_, target, _)| *target == symbols["seed"])
            .map(|(owner, _, reference)| (symbols[*owner], reference.clone()))
            .collect(),
    );
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::CallSite)
            .count(),
        2
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .count(),
        if mixed { 1 } else { 2 }
    );
    for (owner, target, body, saturated) in [
        ("invoke", "read", "read ()", !mixed),
        ("run", "carry", "carry value ()", true),
    ] {
        let expected = reference(body_start(owner), body);
        let sites = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::CallSite && site.source.span() == expected.span()
            })
            .collect::<Vec<_>>();
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].enclosing, Some(symbols[owner]));
        assert_eq!(sites[0].source, expected);
        check_source(&expected);
        let calls = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Calls
                    && edge.evidence.source.as_ref() == Some(&expected)
            })
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), usize::from(saturated));
        if let Some(call) = calls.first() {
            assert_eq!(call.subject, RelationEndpoint::Entity(symbols[owner]));
            assert_eq!(call.object, RelationEndpoint::Entity(symbols[target]));
        } else {
            assert_eq!(
                document
                    .skipped_regions
                    .iter()
                    .filter(|gap| gap.domain == IrFactDomain::Relations
                        && gap.reason == SkippedRegionReason::UnsupportedConstruct
                        && gap.source == expected
                        && gap.detail == "fsharp-direct-call-target-unavailable")
                    .count(),
                1
            );
        }
        relationships(
            owner,
            RelationFamily::Calls,
            RelationDirection::Outbound,
            if saturated {
                vec![(symbols[target], expected.clone())]
            } else {
                Vec::new()
            },
        );
        relationships(
            target,
            RelationFamily::CalledBy,
            RelationDirection::Inbound,
            if saturated {
                vec![(symbols[owner], expected)]
            } else {
                Vec::new()
            },
        );
    }
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == IrFactDomain::Entities
                || matches!(
                    gap.reason,
                    SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
                ))
    );
    for gap in &document.skipped_regions {
        check_source(&gap.source);
    }
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(
        (coverage.language.as_str(), coverage.tier),
        ("fsharp", AnalysisTier::TierD)
    );
    if mixed {
        assert_ne!(coverage.status, CoverageStatus::Complete);
    }
    (symbols, full.clone(), read_definition.unwrap())
}

fn assert_ordinary_binding_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    resolved_call: bool,
) -> (BTreeMap<String, SymbolId>, SourceRef) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(file.path, "Bindings.fs");
    assert_eq!(file.language, "fsharp");
    assert_eq!(file.encoding, "utf-8");
    assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(source.as_bytes()));
    let check_source = |reference: &SourceRef| {
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), file.content_hash);
        let written = source
            .as_bytes()
            .get(
                usize::try_from(reference.span().start_byte()).unwrap()
                    ..usize::try_from(reference.span().end_byte()).unwrap(),
            )
            .unwrap();
        exact_source(service, reference, "fsharp", written);
    };
    let full = file.evidence.source.as_ref().unwrap();
    assert_eq!(
        full.span(),
        SourceSpan::new(file.id, 0, file.byte_length).unwrap()
    );
    check_source(full);
    let mut symbols = BTreeMap::new();
    for (name, kind) in [
        ("seed", EntityKind::Variable),
        ("alias", EntityKind::Variable),
        ("add", EntityKind::Function),
        ("run", EntityKind::Function),
        ("shadow", EntityKind::Function),
    ] {
        let entities = document
            .entities
            .iter()
            .filter(|entity| {
                entity.language == "fsharp"
                    && entity.canonical_name == name
                    && entity.kind == kind
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|reference| reference.span().file() == file.id)
            })
            .collect::<Vec<_>>();
        assert_eq!(entities.len(), 1, "authored {name}");
        symbols.insert(name.to_owned(), entities[0].id);
    }
    for (owner, name) in [("add", "value"), ("run", "value"), ("shadow", "seed")] {
        let parameters = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == name
                    && entity.container == Some(ContainerRef::Entity(symbols[owner]))
            })
            .collect::<Vec<_>>();
        assert_eq!(parameters.len(), 1, "own formal of {owner}");
        symbols.insert(format!("{owner}.{name}"), parameters[0].id);
    }
    for symbol in symbols.values() {
        let explained = service
            .symbol_explain(receipt.generation, *symbol, &deadline())
            .unwrap();
        assert_eq!(explained.data.generation, receipt.generation);
        assert_eq!(explained.data.entity.id, *symbol);
        check_source(explained.data.entity.evidence.source.as_ref().unwrap());
        let definitions = explained
            .data
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: *symbol })
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        check_source(&definitions[0].source);
        exact_source(
            service,
            &definitions[0].source,
            "fsharp",
            explained.data.entity.canonical_name.as_bytes(),
        );
        let located = service
            .code_locate_with_filters_and_budget(
                receipt.generation,
                explained.data.entity.canonical_name.clone(),
                LocateMode::Exact,
                vec!["fsharp".to_owned()],
                vec!["Bindings.fs".to_owned()],
                16,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        let hit = located
            .data
            .hits
            .iter()
            .find(|hit| hit.file == file.id && hit.symbol == Some(*symbol))
            .unwrap();
        check_source(hit.source.as_ref().unwrap());
    }
    let callee = if resolved_call { "add" } else { "missing" };
    let body_start = |owner: &str| {
        let start = source.find(&format!("let {owner} ")).unwrap();
        start + source.get(start..).unwrap().find(" = ").unwrap() + " = ".len()
    };
    let mut proved_reads = Vec::new();
    for (owner, offset, written, target_key) in [
        ("alias", 0, "seed", Some("seed")),
        ("add", 0, "value", Some("add.value")),
        ("add", "value + ".len(), "seed", Some("seed")),
        ("run", 0, callee, resolved_call.then_some("add")),
        ("run", callee.len() + 1, "value", Some("run.value")),
        ("shadow", 0, "seed", Some("shadow.seed")),
    ] {
        let start = body_start(owner) + offset;
        assert_eq!(source.get(start..start + written.len()), Some(written));
        let span = SourceSpan::new(
            file.id,
            u64::try_from(start).unwrap(),
            u64::try_from(start + written.len()).unwrap(),
        )
        .unwrap();
        let reads = document
            .occurrences
            .iter()
            .filter(|site| site.source.span() == span)
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 1, "exact body read {owner}: {written}");
        let read = reads[0];
        assert_eq!(read.enclosing, Some(symbols[owner]));
        assert_eq!(read.role, OccurrenceRole::Reference);
        check_source(&read.source);
        let references = document
            .relations
            .iter()
            .filter(|relation| {
                relation.subject == RelationEndpoint::Occurrence(read.id)
                    && relation.predicate == RelationPredicate::RefersTo
            })
            .collect::<Vec<_>>();
        if let Some(target_key) = target_key {
            let target = symbols[target_key];
            assert_eq!(read.target, OccurrenceTarget::Resolved { symbol: target });
            assert_eq!(references.len(), 1);
            assert_eq!(references[0].object, RelationEndpoint::Entity(target));
            assert_eq!(references[0].evidence.source.as_ref(), Some(&read.source));
            assert!(!document.skipped_regions.iter().any(|gap| {
                gap.source == read.source && gap.domain == rootlight_ir::FactDomain::Relations
            }));
            proved_reads.push((owner, target_key, read.source.clone()));
        } else {
            assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
            assert!(references.is_empty());
            assert!(document.skipped_regions.iter().any(|gap| {
                gap.source == read.source
                    && gap.domain == rootlight_ir::FactDomain::Relations
                    && gap.reason == SkippedRegionReason::UnsupportedConstruct
            }));
        }
    }
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .count(),
        6
    );
    let call_sites = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::CallSite)
        .collect::<Vec<_>>();
    assert_eq!(call_sites.len(), 1);
    let call_site = call_sites[0];
    let call_text = format!("{callee} value");
    let call_start = body_start("run");
    let call_span = SourceSpan::new(
        file.id,
        u64::try_from(call_start).unwrap(),
        u64::try_from(call_start + call_text.len()).unwrap(),
    )
    .unwrap();
    assert_eq!(call_site.source.span(), call_span);
    assert_eq!(call_site.enclosing, Some(symbols["run"]));
    let call_reference = &call_site.source;
    check_source(call_reference);
    exact_source(service, call_reference, "fsharp", call_text.as_bytes());
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::RefersTo)
            .count(),
        if resolved_call { 6 } else { 5 }
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::DispatchCandidate)
    );
    let calls = document
        .relations
        .iter()
        .filter(|relation| relation.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), usize::from(resolved_call));
    if let Some(call) = calls.first() {
        assert_eq!(call.object, RelationEndpoint::Entity(symbols["add"]));
        assert_eq!(call.evidence.source.as_ref(), Some(call_reference));
        let own_subject = match call.subject {
            RelationEndpoint::Entity(symbol) => symbol == symbols["run"],
            RelationEndpoint::Occurrence(id) => id == call_site.id,
            _ => false,
        };
        assert!(own_subject);
    }
    let assert_relationships =
        |seed: &str,
         family: RelationFamily,
         direction: Option<RelationDirection>,
         expected: Vec<(SymbolId, SourceRef)>| {
            let result = service
                .symbol_relationships(
                    receipt.generation,
                    BTreeSet::from([symbols[seed]]),
                    vec![family],
                    direction,
                    0,
                    16,
                    0,
                    &deadline(),
                )
                .unwrap();
            assert_eq!(result.data.generation, receipt.generation);
            assert!(!result.data.truncated);
            assert!(!result.data.exact);
            assert_eq!(
                result.data.returned_edges,
                u32::try_from(expected.len()).unwrap()
            );
            assert_eq!(
                result.data.total_edges,
                u32::try_from(expected.len()).unwrap()
            );
            let mut actual = Vec::new();
            for group in &result.data.groups {
                assert_eq!(group.seed, symbols[seed]);
                assert_eq!(group.family, family);
                assert_eq!(
                    group.direction,
                    direction.unwrap_or_else(|| family.natural_direction())
                );
                for item in &group.items {
                    assert_eq!(item.source_refs.len(), 1);
                    check_source(&item.source_refs[0]);
                    actual.push((item.symbol, item.source_refs[0].clone()));
                }
            }
            actual.sort();
            let mut expected = expected;
            expected.sort();
            assert_eq!(actual, expected);
        };
    for owner in ["alias", "add", "run", "shadow"] {
        let expected = proved_reads
            .iter()
            .filter(|(consumer, _, _)| *consumer == owner)
            .map(|(_, target, reference)| (symbols[*target], reference.clone()))
            .collect();
        assert_relationships(
            owner,
            RelationFamily::References,
            Some(RelationDirection::Outbound),
            expected,
        );
    }
    let inbound_seed = proved_reads
        .iter()
        .filter(|(_, target, _)| *target == "seed")
        .map(|(owner, _, reference)| (symbols[*owner], reference.clone()))
        .collect();
    assert_relationships(
        "seed",
        RelationFamily::References,
        Some(RelationDirection::Inbound),
        inbound_seed,
    );
    let outbound_call = if resolved_call {
        vec![(symbols["add"], call_reference.clone())]
    } else {
        Vec::new()
    };
    assert_relationships(
        "run",
        RelationFamily::Calls,
        Some(RelationDirection::Outbound),
        outbound_call,
    );
    let incoming_call = if resolved_call {
        vec![(symbols["run"], call_reference.clone())]
    } else {
        Vec::new()
    };
    assert_relationships("add", RelationFamily::CalledBy, None, incoming_call);
    for gap in &document.skipped_regions {
        assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
        assert_eq!(gap.repository, receipt.repository);
        assert_eq!(gap.generation, receipt.generation);
        assert_eq!(gap.evidence.source.as_ref(), Some(&gap.source));
        check_source(&gap.source);
    }
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(coverage.language, "fsharp");
    assert_eq!(coverage.tier, AnalysisTier::TierD);
    assert_eq!(coverage.reason, None);
    if document.skipped_regions.is_empty() {
        assert_eq!(coverage.status, CoverageStatus::Complete);
    } else {
        assert_ne!(coverage.status, CoverageStatus::Complete);
    }
    (symbols, full.clone())
}
