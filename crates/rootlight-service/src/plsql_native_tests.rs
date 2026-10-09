//! Public-service PL/SQL declarations, source navigation and durable parser routes.
//! Authored identities remain useful without claiming database catalogs, package
//! specification/body linkage, overload binding or MCP transport qualification.

use super::*;
use rootlight_ir::{
    ContainerRef, LexicalEvidenceKind, OccurrenceTarget, decode_lexical_evidence_envelope,
};
use rootlight_query::{RepositoryDataTrust, SourceChunkEncoding};
use std::fs;
use tempfile::TempDir;

const SPECIFICATION: &str = concat!(
    "create or replace package MixedKit as\n",
    "  procedure Adjust(AdjustArg in number);\n",
    "  function Compute(ComputeArg in number) return number;\n",
    "end MixedKit;\n",
    "/\n",
);

const BODY: &str = concat!(
    "create or replace package body MixedKit as\n",
    "  type RecordValue is record (FieldValue number);\n",
    "  StoredValue number;\n",
    "  \"QuotedValue\" number;\n",
    "  procedure Adjust(AdjustArg in number) as\n",
    "    LocalValue number;\n",
    "  begin\n",
    "    LocalValue := AdjustArg;\n",
    "    LocalValue := Compute(AdjustArg);\n",
    "  end Adjust;\n",
    "  function Compute(ComputeArg in number) return number as\n",
    "  begin\n",
    "    return ComputeArg;\n",
    "  end Compute;\n",
    "end MixedKit;\n",
    "/\n",
);

fn deadline() -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(60))
            .expect("fixture deadline is representable"),
    )
}

fn directory() -> TempDir {
    let current = std::env::current_dir().expect("test working directory exists");
    tempfile::tempdir_in(current).expect("fixture avoids aliased system temp roots")
}

fn inspect(service: &FirstSliceService, receipt: &FirstSliceIndexReceipt) -> NormalizedIrDocument {
    service
        .inspect_generation(
            receipt.repository,
            Some(receipt.generation),
            &deadline(),
            |snapshot| snapshot.document().clone(),
        )
        .expect("published generation is publicly inspectable")
}

fn publish(service: &mut FirstSliceService, repository: &Path) -> FirstSliceIndexCommit {
    let cancellation = deadline();
    let prepared = service
        .prepare_repository_with_options(
            repository,
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
            &cancellation,
        )
        .expect("synthetic repository prepares");
    service
        .publish_prepared_with_metrics(prepared, &cancellation)
        .expect("synthetic repository publishes")
}

fn file<'a>(document: &'a NormalizedIrDocument, path: &str) -> &'a FileRecord {
    document
        .files
        .iter()
        .find(|file| file.path == path)
        .expect("physical source file is retained")
}

fn entity<'a>(
    document: &'a NormalizedIrDocument,
    path: &str,
    name: &str,
    kind: EntityKind,
) -> &'a EntityRecord {
    let file = file(document, path);
    let entities: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == kind
                && entity.canonical_name == name
                && entity.language == "plsql"
                && entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == file.id)
        })
        .collect();
    assert_eq!(entities.len(), 1, "one authored {kind:?} {name} in {path}");
    assert_eq!(entities[0].display_name, name);
    assert_eq!(entities[0].tier, AnalysisTier::TierD);
    entities[0]
}

fn selected<'a>(source: &'a str, reference: &SourceRef) -> &'a [u8] {
    let span = reference.span();
    source
        .as_bytes()
        .get(usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap())
        .expect("reference is bounded by original physical source")
}

fn assert_source(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    file: &FileRecord,
    reference: &SourceRef,
    original: &str,
    expected: &[u8],
) {
    assert_eq!(reference.repository(), receipt.repository);
    assert_eq!(reference.generation(), receipt.generation);
    assert_eq!(reference.span().file(), file.id);
    assert_eq!(reference.content_hash(), file.content_hash);
    assert_eq!(reference.content_hash(), content_hash(original.as_bytes()));
    assert_eq!(selected(original, reference), expected);
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .expect("generation-bound original bytes are publicly readable");
    assert_eq!(read.data.generation, receipt.generation);
    assert_eq!(read.data.chunks.len(), 1);
    let chunk = &read.data.chunks[0];
    assert_eq!(&chunk.reference, reference);
    assert_eq!(chunk.content_hash, file.content_hash);
    assert_eq!(chunk.path, file.path);
    assert_eq!(chunk.language, file.language);
    assert_eq!(chunk.start_byte, reference.span().start_byte());
    assert_eq!(chunk.end_byte, reference.span().end_byte());
    assert_eq!(chunk.encoding, SourceChunkEncoding::Bytes);
    assert_eq!(chunk.bytes, expected);
    assert_eq!(chunk.trust, RepositoryDataTrust::UntrustedRepositoryData);
    assert!(chunk.extracted_from.is_none());
}

fn assert_file(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    document: &NormalizedIrDocument,
    path: &str,
    language: &str,
    source: &str,
) -> SourceRef {
    let file = file(document, path);
    assert_eq!(file.language, language);
    assert_eq!(file.encoding, "utf-8");
    assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(source.as_bytes()));
    assert!(!file.generated);
    let full = file.evidence.source.as_ref().unwrap();
    assert_eq!(full.span().start_byte(), 0);
    assert_eq!(full.span().end_byte(), file.byte_length);
    assert_source(service, receipt, file, full, source, source.as_bytes());
    full.clone()
}

fn assert_declaration(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    document: &NormalizedIrDocument,
    path: &str,
    source: &str,
    name: &str,
    kind: EntityKind,
) -> SymbolId {
    let file = file(document, path);
    let entity = entity(document, path, name, kind);
    let explained = service
        .symbol_explain(receipt.generation, entity.id, &deadline())
        .expect("authored PL/SQL declaration is explainable");
    assert_eq!(explained.data.generation, receipt.generation);
    assert_eq!(explained.data.entity, *entity);
    assert_eq!(
        explained.data.trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
    let definitions: Vec<_> = explained
        .data
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
        })
        .collect();
    assert_eq!(definitions.len(), 1, "one exact definition for {name}");
    let definition = definitions[0];
    assert!(definition.syntax_kind.starts_with("plsql."));
    assert!(definition.syntax_kind.ends_with(".definition"));
    assert_eq!(
        definition.syntactic_text_hash,
        content_hash(name.as_bytes())
    );
    assert_source(
        service,
        receipt,
        file,
        &definition.source,
        source,
        name.as_bytes(),
    );

    let located = service
        .code_locate(
            receipt.generation,
            name.to_owned(),
            LocateMode::Exact,
            16,
            0,
            &deadline(),
        )
        .expect("source-authored PL/SQL declaration is publicly locatable");
    assert_eq!(located.data.generation, receipt.generation);
    assert!(!located.data.truncated);
    let hits: Vec<_> = located
        .data
        .hits
        .iter()
        .filter(|hit| hit.symbol == Some(entity.id) && hit.file == file.id)
        .collect();
    assert_eq!(hits.len(), 1, "one public symbol hit for {name}");
    let hit = hits[0];
    assert_eq!(hit.identifier, name);
    assert_eq!(hit.path, path);
    assert_eq!(hit.language, "plsql");
    assert_eq!(hit.trust, RepositoryDataTrust::UntrustedRepositoryData);
    let reference = hit.source.as_ref().unwrap();
    assert_source(
        service,
        receipt,
        file,
        reference,
        source,
        selected(source, reference),
    );
    entity.id
}

fn assert_header(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    document: &NormalizedIrDocument,
    path: &str,
    source: &str,
    symbol: SymbolId,
    expected: &str,
) {
    let signatures: Vec<_> = document
        .extensions
        .iter()
        .filter(|envelope| {
            envelope.namespace == LEXICAL_EXTENSION_NAMESPACE
                && envelope.evidence.derivation == [FactRef::Entity(symbol)]
        })
        .filter_map(|envelope| {
            let lexical = decode_lexical_evidence_envelope(envelope).unwrap();
            (lexical.kind() == LexicalEvidenceKind::Signature).then_some((envelope, lexical))
        })
        .collect();
    assert_eq!(signatures.len(), 1, "one source-authored header");
    let (envelope, signature) = &signatures[0];
    let reference = envelope.evidence.source.as_ref().unwrap();
    assert_source(
        service,
        receipt,
        file(document, path),
        reference,
        source,
        expected.as_bytes(),
    );
    assert_eq!(signature.text(), expected);
    assert_eq!(signature.subject(), FactRef::Entity(symbol));
    assert_eq!(
        signature.complete_text_hash(),
        content_hash(expected.as_bytes())
    );
    assert!(!signature.is_truncated());
    signature.verify_source_bytes(expected.as_bytes()).unwrap();
    let explained = service
        .symbol_explain(receipt.generation, symbol, &deadline())
        .unwrap();
    assert_eq!(explained.data.signature.as_ref(), Some(signature));
}

fn sql_policy(language: &str) -> FirstSliceStoragePolicy {
    let configuration = format!(
        "version='1.3'\n[storage]\nretained_generations=8\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nsql='{language}'\n"
    );
    FirstSliceStoragePolicy::resolve_user_config(Some(&configuration), 8).unwrap()
}

#[test]
fn package_declarations_public_locate_and_source_preserve_case_quotes_and_headers() {
    for newline in ["\n", "\r\n"] {
        let repository = directory();
        let specification = SPECIFICATION.replace('\n', newline);
        let body = BODY.replace('\n', newline);
        fs::write(repository.path().join("mixed.pks"), &specification).unwrap();
        fs::write(repository.path().join("mixed.pkb"), &body).unwrap();
        let mut service = FirstSliceService::new(4).unwrap();
        let receipt = service
            .index_repository(repository.path(), &deadline())
            .unwrap();
        let document = inspect(&service, &receipt);
        assert_eq!(receipt.indexed_files, 2);
        assert!(receipt.discovery_complete);
        assert!(!document.skipped_regions.iter().any(|gap| {
            matches!(
                gap.reason,
                SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
            )
        }));
        for (path, source) in [("mixed.pks", &specification), ("mixed.pkb", &body)] {
            assert_file(&service, &receipt, &document, path, "plsql", source);
            let module = assert_declaration(
                &service,
                &receipt,
                &document,
                path,
                source,
                "MixedKit",
                EntityKind::Module,
            );
            let header = if path == "mixed.pks" {
                "create or replace package MixedKit as"
            } else {
                "create or replace package body MixedKit as"
            };
            assert_header(&service, &receipt, &document, path, source, module, header);
            for (name, header) in [
                (
                    "Adjust",
                    if path == "mixed.pks" {
                        "procedure Adjust(AdjustArg in number);"
                    } else {
                        "procedure Adjust(AdjustArg in number) as"
                    },
                ),
                (
                    "Compute",
                    if path == "mixed.pks" {
                        "function Compute(ComputeArg in number) return number;"
                    } else {
                        "function Compute(ComputeArg in number) return number as"
                    },
                ),
            ] {
                let function = assert_declaration(
                    &service,
                    &receipt,
                    &document,
                    path,
                    source,
                    name,
                    EntityKind::Function,
                );
                assert_eq!(
                    entity(&document, path, name, EntityKind::Function).container,
                    Some(ContainerRef::Entity(module))
                );
                assert_header(
                    &service, &receipt, &document, path, source, function, header,
                );
            }
        }
        for (name, kind) in [
            ("RecordValue", EntityKind::Struct),
            ("FieldValue", EntityKind::Field),
            ("StoredValue", EntityKind::Variable),
            ("\"QuotedValue\"", EntityKind::Variable),
            ("LocalValue", EntityKind::Variable),
            ("AdjustArg", EntityKind::Parameter),
            ("ComputeArg", EntityKind::Parameter),
        ] {
            assert_declaration(
                &service,
                &receipt,
                &document,
                "mixed.pkb",
                &body,
                name,
                kind,
            );
        }
        let record = entity(&document, "mixed.pkb", "RecordValue", EntityKind::Struct);
        assert_header(
            &service,
            &receipt,
            &document,
            "mixed.pkb",
            &body,
            record.id,
            "type RecordValue is record (",
        );
        assert_eq!(
            entity(&document, "mixed.pkb", "FieldValue", EntityKind::Field).container,
            Some(ContainerRef::Entity(record.id))
        );
        let procedure = entity(&document, "mixed.pkb", "Adjust", EntityKind::Function);
        for (name, kind) in [
            ("AdjustArg", EntityKind::Parameter),
            ("LocalValue", EntityKind::Variable),
        ] {
            assert_eq!(
                entity(&document, "mixed.pkb", name, kind).container,
                Some(ContainerRef::Entity(procedure.id))
            );
        }
    }
}

#[test]
fn nested_conditional_cache_headers_are_publicly_source_exact_without_selection() {
    let dependencies = concat!(
        "$if $$TrackDependencies $then\n",
        "relies_on (DataA, DataB)\n",
        "$else\n",
        "$end",
    );
    let properties = format!("$if $$CacheEnabled $then\nresult_cache\n{dependencies}\n$else\n$end");
    let header = format!("function ReadValue(ReadArg in number) return number\n{properties}\nas");
    let source = format!(
        "create or replace package body CacheKit as\n{header}\nbegin return ReadArg; end ReadValue;\nend CacheKit;\n/\n"
    );
    let repository = directory();
    let path = "cache.pkb";
    fs::write(repository.path().join(path), &source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let document = inspect(&service, &receipt);
    assert_eq!(receipt.indexed_files, 1);
    assert!(receipt.discovery_complete);
    assert!(!document.skipped_regions.iter().any(|gap| {
        matches!(
            gap.reason,
            SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
        )
    }));
    assert_file(&service, &receipt, &document, path, "plsql", &source);
    let package = assert_declaration(
        &service,
        &receipt,
        &document,
        path,
        &source,
        "CacheKit",
        EntityKind::Module,
    );
    let function = assert_declaration(
        &service,
        &receipt,
        &document,
        path,
        &source,
        "ReadValue",
        EntityKind::Function,
    );
    assert_eq!(
        entity(&document, path, "ReadValue", EntityKind::Function).container,
        Some(ContainerRef::Entity(package))
    );
    assert_declaration(
        &service,
        &receipt,
        &document,
        path,
        &source,
        "ReadArg",
        EntityKind::Parameter,
    );
    assert_eq!(
        entity(&document, path, "ReadArg", EntityKind::Parameter).container,
        Some(ContainerRef::Entity(function))
    );
    assert_header(
        &service, &receipt, &document, path, &source, function, &header,
    );
    let file = file(&document, path);
    for written in [properties.as_str(), dependencies] {
        for domain in [
            IrFactDomain::Entities,
            IrFactDomain::Occurrences,
            IrFactDomain::Relations,
        ] {
            let gaps: Vec<_> = document
                .skipped_regions
                .iter()
                .filter(|gap| {
                    gap.domain == domain
                        && gap.detail == "plsql-conditional-selection-unavailable"
                        && selected(&source, &gap.source) == written.as_bytes()
                })
                .collect();
            assert_eq!(gaps.len(), 1, "one physical {domain:?} conditional gap");
            assert_eq!(gaps[0].reason, SkippedRegionReason::UnsupportedConstruct);
            assert_source(
                &service,
                &receipt,
                file,
                &gaps[0].source,
                &source,
                written.as_bytes(),
            );
        }
    }
    assert!(document.occurrences.iter().all(|site| {
        site.role != OccurrenceRole::CallSite
            && (site.role != OccurrenceRole::Reference
                || matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    }));
    assert!(!document.relations.iter().any(|relation| {
        matches!(
            relation.predicate,
            RelationPredicate::Calls | RelationPredicate::RefersTo
        )
    }));
    let relationships = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([function]),
            vec![RelationFamily::Calls, RelationFamily::References],
            Some(RelationDirection::Outbound),
            0,
            16,
            0,
            &deadline(),
        )
        .unwrap();
    assert_eq!(relationships.data.returned_edges, 0);
    assert!(!relationships.data.truncated);
    let summary = service
        .coverage_gap_summary_for_paths_until(
            receipt.repository,
            receipt.generation,
            &["plsql".to_owned()],
            &[path.to_owned()],
            &deadline(),
        )
        .unwrap();
    assert!(summary.incomplete);
    assert_eq!(summary.skipped_inputs, 1);
    assert_eq!(summary.omitted_gap_groups, 0);
    assert!(summary.gaps.iter().any(|gap| {
        gap.reason == FirstSliceCoverageGapReason::Unsupported
            && gap.language.as_deref() == Some("plsql")
            && gap.files == 1
    }));
}

#[test]
fn dedicated_plsql_suffixes_do_not_reassign_default_sql_sources() {
    let repository = directory();
    for path in [
        "one.pks",
        "two.pkb",
        "three.pls",
        "four.plb",
        "ordinary.sql",
    ] {
        fs::write(repository.path().join(path), BODY).unwrap();
    }
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let document = inspect(&service, &receipt);
    assert_eq!(receipt.discovered_inputs, 5);
    for path in ["one.pks", "two.pkb", "three.pls", "four.plb"] {
        assert_file(&service, &receipt, &document, path, "plsql", BODY);
        assert_declaration(
            &service,
            &receipt,
            &document,
            path,
            BODY,
            "Compute",
            EntityKind::Function,
        );
    }
    // A SQL route is not qualified as a PL/SQL parser merely because source is retained.
    assert_file(&service, &receipt, &document, "ordinary.sql", "sql", BODY);
    let sql_file = file(&document, "ordinary.sql");
    assert!(!document.entities.iter().any(|entity| {
        entity.language == "plsql"
            && entity
                .evidence
                .source
                .as_ref()
                .is_some_and(|source| source.span().file() == sql_file.id)
    }));
}

#[test]
fn noop_reopen_and_source_edit_keep_retained_generations_and_replace_active_identity() {
    let repository = directory();
    let storage = directory();
    fs::write(repository.path().join("mixed.pkb"), BODY).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let mut service = FirstSliceService::new_durable(8, paths.state_dir(), &deadline()).unwrap();
    let first = publish(&mut service, repository.path());
    let receipt = first.receipt().clone();
    let original = inspect(&service, &receipt);
    let symbol = assert_declaration(
        &service,
        &receipt,
        &original,
        "mixed.pkb",
        BODY,
        "Compute",
        EntityKind::Function,
    );
    let full = assert_file(&service, &receipt, &original, "mixed.pkb", "plsql", BODY);
    let noop = publish(&mut service, repository.path());
    assert_eq!(noop.receipt(), &receipt);
    assert_eq!(
        noop.evidence().strategy,
        FirstSliceIndexOperationStrategy::RetainedGeneration
    );
    assert_eq!(noop.evidence().changed_inputs, 0);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    drop(service);

    let mut reopened = FirstSliceService::new_durable(8, paths.state_dir(), &deadline()).unwrap();
    assert_eq!(inspect(&reopened, &receipt), original);
    assert_eq!(
        assert_declaration(
            &reopened,
            &receipt,
            &original,
            "mixed.pkb",
            BODY,
            "Compute",
            EntityKind::Function,
        ),
        symbol
    );
    let noop = publish(&mut reopened, repository.path());
    assert_eq!(noop.receipt(), &receipt);
    assert_eq!(
        noop.evidence().strategy,
        FirstSliceIndexOperationStrategy::RetainedGeneration
    );
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    let edited = BODY.replace("Compute", "Recompute");
    fs::write(repository.path().join("mixed.pkb"), &edited).unwrap();
    let changed = publish(&mut reopened, repository.path());
    assert_ne!(changed.receipt().generation, receipt.generation);
    assert_eq!(changed.evidence().changed_files, 1);
    assert_eq!(changed.evidence().rebuilt_files, 1);
    let current = inspect(&reopened, changed.receipt());
    let replacement = assert_declaration(
        &reopened,
        changed.receipt(),
        &current,
        "mixed.pkb",
        &edited,
        "Recompute",
        EntityKind::Function,
    );
    assert_ne!(replacement, symbol);
    assert!(!current.entities.iter().any(|entity| entity.id == symbol));
    assert_file(
        &reopened,
        changed.receipt(),
        &current,
        "mixed.pkb",
        "plsql",
        &edited,
    );
    assert_source(
        &reopened,
        &receipt,
        file(&original, "mixed.pkb"),
        &full,
        BODY,
        BODY.as_bytes(),
    );
    assert_eq!(
        assert_declaration(
            &reopened,
            &receipt,
            &original,
            "mixed.pkb",
            BODY,
            "Compute",
            EntityKind::Function,
        ),
        symbol
    );
    assert!(
        reopened
            .source_read(changed.receipt().generation, vec![full], &deadline())
            .is_err()
    );
}

#[test]
fn explicit_sql_route_edits_rebuild_unchanged_bytes_without_stale_plsql_identities() {
    let repository = directory();
    let storage = directory();
    fs::write(repository.path().join("mixed.sql"), BODY).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let mut previous = None;
    let mut first_plsql: Option<(
        FirstSliceIndexReceipt,
        NormalizedIrDocument,
        SourceRef,
        SymbolId,
    )> = None;
    for language in ["plsql", "sql", "plsql"] {
        let policy = sql_policy(language);
        let configuration_hash = policy.config.hash();
        let mut service =
            FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline())
                .unwrap();
        let committed = publish(&mut service, repository.path());
        let receipt = committed.receipt();
        assert_eq!(
            service
                .generation_metadata(receipt.generation)
                .unwrap()
                .configuration_hash(),
            configuration_hash
        );
        if let Some(previous) = previous {
            assert_ne!(receipt.generation, previous);
            assert!(committed.evidence().changed_inputs > 0);
            assert_eq!(committed.evidence().changed_files, 0);
            assert_ne!(
                committed.evidence().strategy,
                FirstSliceIndexOperationStrategy::RetainedGeneration
            );
        }
        let document = inspect(&service, receipt);
        let full = assert_file(&service, receipt, &document, "mixed.sql", language, BODY);
        if language == "plsql" {
            let symbol = assert_declaration(
                &service,
                receipt,
                &document,
                "mixed.sql",
                BODY,
                "Compute",
                EntityKind::Function,
            );
            if let Some((initial, _, _, initial_symbol)) = &first_plsql {
                assert_eq!(symbol, *initial_symbol);
                assert_eq!(receipt.logical_snapshot, initial.logical_snapshot);
            } else {
                first_plsql = Some((receipt.clone(), document.clone(), full, symbol));
            }
        } else {
            assert!(
                document
                    .entities
                    .iter()
                    .all(|entity| entity.language != "plsql")
            );
            assert!(
                document
                    .occurrences
                    .iter()
                    .all(|site| !site.syntax_kind.starts_with("plsql."))
            );
            let (_, _, _, symbol) = first_plsql.as_ref().unwrap();
            assert!(document.entities.iter().all(|entity| entity.id != *symbol));
        }
        if let Some((initial, initial_document, reference, _)) = &first_plsql {
            assert_source(
                &service,
                initial,
                file(initial_document, "mixed.sql"),
                reference,
                BODY,
                BODY.as_bytes(),
            );
            if initial.generation != receipt.generation {
                assert!(
                    service
                        .source_read(receipt.generation, vec![reference.clone()], &deadline())
                        .is_err()
                );
            }
        }
        let expected = receipt.clone();
        let noop = publish(&mut service, repository.path());
        assert_eq!(noop.receipt(), &expected);
        assert_eq!(
            noop.evidence().strategy,
            FirstSliceIndexOperationStrategy::RetainedGeneration
        );
        assert_eq!(noop.evidence().changed_inputs, 0);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        previous = Some(expected.generation);
        assert_eq!(
            fs::read(repository.path().join("mixed.sql")).unwrap(),
            BODY.as_bytes()
        );
    }
}

#[test]
fn applications_and_parameter_uses_remain_unresolved_without_invented_call_edges() {
    let repository = directory();
    fs::write(repository.path().join("mixed.pkb"), BODY).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let document = inspect(&service, &receipt);
    let file = file(&document, "mixed.pkb");
    let owner = entity(&document, "mixed.pkb", "Adjust", EntityKind::Function);
    let start = BODY.find("Compute(AdjustArg)").unwrap();
    let applications: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.file == file.id
                && site.syntax_kind == "plsql.application.reference"
                && site.source.span().start_byte() == u64::try_from(start).unwrap()
        })
        .collect();
    assert_eq!(applications.len(), 1);
    let application = applications[0];
    assert_eq!(application.role, OccurrenceRole::Reference);
    assert_eq!(application.enclosing, Some(owner.id));
    assert!(matches!(
        application.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_source(
        &service,
        &receipt,
        file,
        &application.source,
        BODY,
        b"Compute",
    );
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.domain == IrFactDomain::Relations
            && gap.detail == "plsql-call-or-index-target-unavailable"
            && gap.source == application.source
    }));
    let use_start = BODY.find("LocalValue := AdjustArg;").unwrap() + "LocalValue := ".len();
    let uses: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.file == file.id
                && site.syntax_kind == "plsql.identifier.reference"
                && site.source.span().start_byte() == u64::try_from(use_start).unwrap()
        })
        .collect();
    assert_eq!(uses.len(), 1);
    assert_eq!(uses[0].role, OccurrenceRole::Reference);
    assert!(matches!(
        uses[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_source(
        &service,
        &receipt,
        file,
        &uses[0].source,
        BODY,
        b"AdjustArg",
    );
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.domain == IrFactDomain::Relations
            && gap.detail == "plsql-binding-target-unavailable"
            && gap.source == uses[0].source
    }));
    assert!(!document.relations.iter().any(|relation| {
        matches!(
            relation.predicate,
            RelationPredicate::Calls | RelationPredicate::RefersTo
        )
    }));
    let relationships = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([owner.id]),
            vec![RelationFamily::Calls, RelationFamily::References],
            Some(RelationDirection::Outbound),
            0,
            16,
            0,
            &deadline(),
        )
        .unwrap();
    assert_eq!(relationships.data.returned_edges, 0);
    assert!(!relationships.data.truncated);
    assert_eq!(
        relationships.data.trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
    let summary = service
        .coverage_gap_summary_for_paths_until(
            receipt.repository,
            receipt.generation,
            &["plsql".to_owned()],
            &["mixed.pkb".to_owned()],
            &deadline(),
        )
        .unwrap();
    assert!(summary.incomplete);
    assert!(
        summary
            .gaps
            .iter()
            .any(|gap| gap.language.as_deref() == Some("plsql"))
    );
}
