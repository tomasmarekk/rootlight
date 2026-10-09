//! Public-service Fortran declarations and physical-source navigation.
//! Authored identity and headers remain useful without claiming unresolved
//! procedure binding, array-call equivalence, or expanded Fypp variants.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    time::{Duration, Instant},
};

use rootlight_cancel::Cancellation;
use rootlight_ids::{SymbolId, content_hash};
use rootlight_ir::{
    AnalysisTier, ContainerRef, CoverageScope, CoverageStatus, EntityKind, EntityRecord,
    FactDomain, FactRef, FileRecord, LEXICAL_EXTENSION_NAMESPACE, LexicalEvidenceKind,
    NormalizedIrDocument, OccurrenceRole, OccurrenceTarget, RelationEndpoint, RelationPredicate,
    SourceRef, decode_lexical_evidence_envelope,
};
use rootlight_query::{LocateMode, RepositoryDataTrust, SourceChunkEncoding};
use rootlight_runtime::RuntimePaths;
use tempfile::TempDir;

use rootlight_service::{
    FirstSliceBudget, FirstSliceIndexReceipt, FirstSliceService, RelationDirection, RelationFamily,
    SourceEncoding, SourceReadOptions,
};

const MODULE: &str = concat!(
    "MoDuLe MiXeDKit\n",
    "  implicit none\n",
    "  TyPe :: DataBox\n",
    "    integer :: Stored\n",
    "  EnD TyPe DataBox\n",
    "contains\n",
    "  InTeGeR FuNcTiOn SumValue(InPuT)\n",
    "    integer, intent(in) :: InPuT\n",
    "    SumValue = InPuT + 1\n",
    "  EnD FuNcTiOn SumValue\n",
    "  SuBrOuTiNe StoreValue(InPuT)\n",
    "    integer, intent(in) :: InPuT\n",
    "    integer :: items(2), selected\n",
    "    selected = items(1)\n",
    "    CaLl dIsPaTcH(InPuT)\n",
    "  EnD SuBrOuTiNe StoreValue\n",
    "  subroutine Dispatch(value)\n",
    "    integer :: value\n",
    "    value = value + 1\n",
    "  end subroutine Dispatch\n",
    "EnD MoDuLe MiXeDKit\n",
);

const PROGRAM: &str = concat!(
    "PrOgRaM Runner\n",
    "  use MiXeDKit\n",
    "  implicit none\n",
    "  integer :: result\n",
    "  result = SumValue(2)\n",
    "  call StoreValue(result)\n",
    "EnD PrOgRaM Runner\n",
);

const TEMPLATE: &str = concat!(
    "module TemplateKit\n",
    "contains\n",
    "  subroutine LiteralStep(Value)\n",
    "    integer :: Value\n",
    "    Value = Value + 1\n",
    "  end subroutine LiteralStep\n",
    "#:if enabled\n",
    "  subroutine Generated_${suffix}$(Value)\n",
    "    integer :: Value\n",
    "    Value = Value + 2\n",
    "  end subroutine Generated_${suffix}$\n",
    "#:endif\n",
    "end module TemplateKit\n",
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

fn write_sources(root: &Path, sources: &[(&str, &str)]) {
    for (path, source) in sources {
        fs::write(root.join(path), source).expect("synthetic Fortran source writes");
    }
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

struct IndexedFixture {
    service: FirstSliceService,
    receipt: FirstSliceIndexReceipt,
    document: NormalizedIrDocument,
    sources: BTreeMap<String, String>,
    _directory: TempDir,
}

impl IndexedFixture {
    fn new(sources: &[(&str, &str)]) -> Self {
        let directory = directory();
        write_sources(directory.path(), sources);
        let mut service = FirstSliceService::new(2).expect("public service initializes");
        let receipt = service
            .index_repository(directory.path(), &deadline())
            .expect("physical Fortran fixture publishes");
        assert_eq!(receipt.indexed_files, u64::try_from(sources.len()).unwrap());
        let document = inspect(&service, &receipt);
        let fixture = Self {
            service,
            receipt,
            document,
            sources: sources
                .iter()
                .map(|(path, source)| ((*path).to_owned(), (*source).to_owned()))
                .collect(),
            _directory: directory,
        };
        for (path, source) in sources {
            let file = fixture.file(path);
            assert_eq!(file.language, "fortran");
            assert_eq!(file.encoding, "utf-8");
            assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
            assert!(!file.generated);
            let full = file
                .evidence
                .source
                .as_ref()
                .expect("full physical source exists");
            assert_eq!(full.span().start_byte(), 0);
            assert_eq!(full.span().end_byte(), u64::try_from(source.len()).unwrap());
            fixture.assert_source(full, path, source.as_bytes());
        }
        fixture
    }

    fn file(&self, path: &str) -> &FileRecord {
        self.document
            .files
            .iter()
            .find(|file| file.path == path)
            .expect("source file is retained")
    }

    fn entity(&self, path: &str, name: &str, kind: EntityKind) -> &EntityRecord {
        let file = self.file(path);
        let entities: Vec<_> = self
            .document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == kind
                    && entity.display_name == name
                    && entity.language == "fortran"
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file.id)
            })
            .collect();
        assert_eq!(entities.len(), 1, "one source-authored {kind:?} {name}");
        assert_eq!(entities[0].canonical_name, name);
        assert_eq!(entities[0].tier, AnalysisTier::TierD);
        entities[0]
    }

    fn selected<'a>(&'a self, reference: &SourceRef, path: &str) -> &'a [u8] {
        assert_eq!(reference.span().file(), self.file(path).id);
        self.sources[path]
            .as_bytes()
            .get(
                usize::try_from(reference.span().start_byte()).unwrap()
                    ..usize::try_from(reference.span().end_byte()).unwrap(),
            )
            .expect("reference is bounded by the original physical source")
    }

    fn assert_source(&self, reference: &SourceRef, path: &str, expected: &[u8]) {
        assert_eq!(reference.repository(), self.receipt.repository);
        assert_eq!(reference.generation(), self.receipt.generation);
        assert_eq!(
            reference.content_hash(),
            content_hash(self.sources[path].as_bytes())
        );
        assert_eq!(self.selected(reference, path), expected);
        assert_source_read(&self.service, reference, path, expected);
    }

    fn assert_declaration(
        &self,
        path: &str,
        name: &str,
        kind: EntityKind,
        header: &str,
    ) -> SymbolId {
        let entity = self.entity(path, name, kind);
        let explained = self
            .service
            .symbol_explain(self.receipt.generation, entity.id, &deadline())
            .expect("authored entity is explainable");
        assert_eq!(explained.data.generation, self.receipt.generation);
        assert_eq!(explained.data.entity, *entity);
        assert_eq!(
            explained.data.trust,
            RepositoryDataTrust::UntrustedRepositoryData
        );
        let source = entity
            .evidence
            .source
            .as_ref()
            .expect("declaration has direct source");
        self.assert_source(source, path, self.selected(source, path));

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
        assert_eq!(definition.syntax_kind, "fortran.identifier.definition");
        let header_start = self.sources[path]
            .find(header)
            .expect("header is source-authored");
        let name_start = header_start + header.find(name).expect("header contains its own name");
        assert_eq!(
            definition.source.span().start_byte(),
            u64::try_from(name_start).unwrap()
        );
        assert_eq!(
            definition.source.span().end_byte(),
            u64::try_from(name_start + name.len()).unwrap()
        );
        self.assert_source(&definition.source, path, name.as_bytes());
        let signatures: Vec<_> = self
            .document
            .extensions
            .iter()
            .filter(|envelope| {
                envelope.namespace == LEXICAL_EXTENSION_NAMESPACE
                    && envelope.evidence.derivation == [FactRef::Entity(entity.id)]
            })
            .filter_map(|envelope| {
                let lexical =
                    decode_lexical_evidence_envelope(envelope).expect("lexical evidence is valid");
                (lexical.kind() == LexicalEvidenceKind::Signature).then_some((envelope, lexical))
            })
            .collect();
        assert_eq!(signatures.len(), 1, "one authored header for {name}");
        let (envelope, signature) = &signatures[0];
        let header_source = envelope
            .evidence
            .source
            .as_ref()
            .expect("header has exact source");
        let header_bytes = self.selected(header_source, path);
        assert_eq!(
            header_source.span().start_byte(),
            u64::try_from(header_start).unwrap()
        );
        assert_eq!(
            std::str::from_utf8(header_bytes).unwrap().trim_end(),
            header
        );
        assert_eq!(signature.text().as_bytes(), header_bytes);
        assert_eq!(signature.subject(), FactRef::Entity(entity.id));
        assert_eq!(signature.complete_text_hash(), content_hash(header_bytes));
        assert!(!signature.is_truncated());
        signature
            .verify_source_bytes(header_bytes)
            .expect("signature matches original header bytes");
        self.assert_source(header_source, path, header_bytes);
        assert_eq!(explained.data.signature.as_ref(), Some(signature));

        let located = self
            .service
            .code_locate(
                self.receipt.generation,
                name.to_ascii_lowercase(),
                LocateMode::Exact,
                16,
                0,
                &deadline(),
            )
            .expect("case-insensitive locate finds the source-authentic declaration");
        assert_eq!(located.data.generation, self.receipt.generation);
        assert!(!located.data.truncated);
        let hits: Vec<_> = located
            .data
            .hits
            .iter()
            .filter(|hit| hit.symbol == Some(entity.id))
            .collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].identifier, name);
        assert_eq!(hits[0].path, path);
        assert_eq!(hits[0].language, "fortran");
        assert_eq!(hits[0].trust, RepositoryDataTrust::UntrustedRepositoryData);
        assert_eq!(hits[0].signature.as_ref(), Some(signature));
        let located_source = hits[0]
            .source
            .as_ref()
            .expect("located declaration has source");
        self.assert_source(located_source, path, self.selected(located_source, path));
        entity.id
    }
}

fn assert_source_read(
    service: &FirstSliceService,
    reference: &SourceRef,
    path: &str,
    expected: &[u8],
) {
    let result = service
        .source_read_with_options_and_budget(
            reference.generation(),
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .expect("generation-bound original bytes are readable");
    assert_eq!(result.data.generation, reference.generation());
    assert_eq!(result.data.chunks.len(), 1);
    let chunk = &result.data.chunks[0];
    assert_eq!(&chunk.reference, reference);
    assert_eq!(chunk.content_hash, reference.content_hash());
    assert_eq!(chunk.start_byte, reference.span().start_byte());
    assert_eq!(chunk.end_byte, reference.span().end_byte());
    assert_eq!(chunk.path, path);
    assert_eq!(chunk.language, "fortran");
    assert_eq!(chunk.encoding, SourceChunkEncoding::Bytes);
    assert_eq!(chunk.bytes, expected);
    assert_eq!(chunk.trust, RepositoryDataTrust::UntrustedRepositoryData);
    assert!(chunk.extracted_from.is_none());
}

#[test]
fn declarations_headers_and_public_queries_preserve_mixed_case_source_identity() {
    for newline in ["\n", "\r\n"] {
        let module = MODULE.replace('\n', newline);
        let program = PROGRAM.replace('\n', newline);
        let fixture = IndexedFixture::new(&[("Mixed.F90", &module), ("runner.f95", &program)]);
        assert!(
            !fixture
                .document
                .diagnostics
                .iter()
                .any(|diagnostic| matches!(
                    diagnostic.code.as_str(),
                    "unsupported-language" | "unsupported-encoding"
                ))
        );
        let owner = fixture.assert_declaration(
            "Mixed.F90",
            "MiXeDKit",
            EntityKind::Module,
            "MoDuLe MiXeDKit",
        );
        for (name, kind, header) in [
            (
                "SumValue",
                EntityKind::Function,
                "InTeGeR FuNcTiOn SumValue(InPuT)",
            ),
            (
                "StoreValue",
                EntityKind::Function,
                "SuBrOuTiNe StoreValue(InPuT)",
            ),
            ("DataBox", EntityKind::Struct, "TyPe :: DataBox"),
        ] {
            let symbol = fixture.assert_declaration("Mixed.F90", name, kind, header);
            assert_eq!(
                fixture.entity("Mixed.F90", name, kind).container,
                Some(ContainerRef::Entity(owner))
            );
            let explained = fixture
                .service
                .symbol_explain(fixture.receipt.generation, owner, &deadline())
                .unwrap();
            assert!(explained.data.relations.iter().any(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(owner)
                    && relation.object == RelationEndpoint::Entity(symbol)
            }));
        }
        fixture.assert_declaration("runner.f95", "Runner", EntityKind::Module, "PrOgRaM Runner");
    }
}

#[test]
fn array_application_and_explicit_call_remain_unresolved_without_false_call_edges() {
    let fixture = IndexedFixture::new(&[("mixed.f90", MODULE)]);
    let file = fixture.file("mixed.f90");
    let owner = fixture.entity("mixed.f90", "StoreValue", EntityKind::Function);
    let application_start = u64::try_from(MODULE.find("items(1)").unwrap()).unwrap();
    let applications: Vec<_> = fixture
        .document
        .occurrences
        .iter()
        .filter(|site| {
            site.file == file.id
                && site.source.span().start_byte() == application_start
                && site.source.span().end_byte() == application_start + 5
                && site.syntax_kind == "fortran.application.reference"
        })
        .collect();
    assert_eq!(applications.len(), 1);
    assert_eq!(applications[0].role, OccurrenceRole::Reference);
    assert!(matches!(
        applications[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    fixture.assert_source(&applications[0].source, "mixed.f90", b"items");
    assert!(!fixture.document.occurrences.iter().any(|site| {
        site.file == file.id
            && site.role == OccurrenceRole::CallSite
            && site.source.span().start_byte() >= application_start
            && site.source.span().end_byte() <= application_start + 8
    }));
    assert!(fixture.document.skipped_regions.iter().any(|gap| {
        gap.detail == "fortran-call-or-index-target-unavailable"
            && gap.source == applications[0].source
    }));

    let calls: Vec<_> = fixture
        .document
        .occurrences
        .iter()
        .filter(|site| {
            site.file == file.id && site.syntax_kind == "fortran.explicit_subroutine.call"
        })
        .collect();
    assert_eq!(calls.len(), 1);
    let call = calls[0];
    assert_eq!(call.role, OccurrenceRole::CallSite);
    assert_eq!(call.enclosing, Some(owner.id));
    assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
    let call_bytes = fixture.selected(&call.source, "mixed.f90");
    assert_eq!(
        std::str::from_utf8(call_bytes).unwrap().trim_end(),
        "CaLl dIsPaTcH(InPuT)"
    );
    fixture.assert_source(&call.source, "mixed.f90", call_bytes);
    assert!(fixture.document.skipped_regions.iter().any(|gap| {
        gap.detail == "fortran-call-target-unavailable" && gap.source == call.source
    }));
    assert!(!fixture.document.relations.iter().any(|relation| matches!(
        relation.predicate,
        RelationPredicate::Calls | RelationPredicate::RefersTo
    )));
    let relationships = fixture
        .service
        .symbol_relationships(
            fixture.receipt.generation,
            BTreeSet::from([owner.id]),
            vec![RelationFamily::Calls, RelationFamily::References],
            Some(RelationDirection::Outbound),
            0,
            16,
            0,
            &deadline(),
        )
        .expect("unsupported bindings remain queryable without invented edges");
    assert_eq!(relationships.data.returned_edges, 0);
    assert!(!relationships.data.truncated);
    assert_eq!(
        relationships.data.trust,
        RepositoryDataTrust::UntrustedRepositoryData
    );
    let summary = fixture
        .service
        .coverage_gap_summary_until(
            fixture.receipt.repository,
            fixture.receipt.generation,
            &["fortran".to_owned()],
            &deadline(),
        )
        .unwrap();
    assert!(summary.incomplete);
    assert!(
        summary
            .gaps
            .iter()
            .any(|gap| gap.language.as_deref() == Some("fortran"))
    );
}

#[test]
fn physical_fypp_retains_literal_declarations_and_explicit_template_gaps() {
    let fixture = IndexedFixture::new(&[("templates.fypp", TEMPLATE)]);
    let file = fixture.file("templates.fypp");
    fixture.assert_declaration(
        "templates.fypp",
        "LiteralStep",
        EntityKind::Function,
        "subroutine LiteralStep(Value)",
    );
    assert!(
        !fixture
            .document
            .entities
            .iter()
            .any(|entity| entity.display_name.starts_with("Generated"))
    );
    assert!(fixture.document.source_mappings.is_empty());
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        let gaps: Vec<_> = fixture
            .document
            .skipped_regions
            .iter()
            .filter(|gap| {
                gap.detail == "fortran-template-expansion-unavailable"
                    && gap.domain == domain
                    && gap.source.span().file() == file.id
            })
            .collect();
        assert_eq!(gaps.len(), 1, "one physical template gap in {domain:?}");
        fixture.assert_source(&gaps[0].source, "templates.fypp", TEMPLATE.as_bytes());
        let coverage = fixture
            .document
            .coverage_records
            .iter()
            .find(|record| record.scope == CoverageScope::File(file.id) && record.domain == domain)
            .expect("template partiality is file/domain scoped");
        assert_ne!(coverage.status, CoverageStatus::Complete);
        assert!(coverage.skipped > 0);
        assert_eq!(
            coverage.discovered,
            coverage.indexed.checked_add(coverage.skipped).unwrap()
        );
    }
    let located = fixture
        .service
        .code_locate(
            fixture.receipt.generation,
            "Generated_".to_owned(),
            LocateMode::Exact,
            16,
            0,
            &deadline(),
        )
        .unwrap();
    assert!(located.data.hits.iter().all(|hit| hit.symbol.is_none()));
    let summary = fixture
        .service
        .coverage_gap_summary_for_paths_until(
            fixture.receipt.repository,
            fixture.receipt.generation,
            &["fortran".to_owned()],
            &["templates.fypp".to_owned()],
            &deadline(),
        )
        .unwrap();
    assert!(summary.incomplete);
    assert_eq!(summary.skipped_inputs, 1);
}

#[test]
fn noop_and_durable_reopen_preserve_fortran_generation_and_original_sources() {
    let repository = directory();
    let storage = directory();
    write_sources(
        repository.path(),
        &[("module.f90", MODULE), ("templates.fypp", TEMPLATE)],
    );
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let mut service = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(receipt.indexed_files, 2);
    let original = inspect(&service, &receipt);
    let noop = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(noop.repository, receipt.repository);
    assert_eq!(noop.generation, receipt.generation);
    assert_eq!(inspect(&service, &noop), original);
    drop(service);

    let mut reopened = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
    assert_eq!(inspect(&reopened, &receipt), original);
    let noop = reopened
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(noop.repository, receipt.repository);
    assert_eq!(noop.generation, receipt.generation);
    let status = reopened
        .repository_status(receipt.repository, Some(receipt.generation))
        .unwrap();
    assert_eq!(status.active_generation, receipt.generation);
    assert_eq!(status.resolved_generation, receipt.generation);
    let literal = original
        .entities
        .iter()
        .find(|entity| entity.display_name == "LiteralStep" && entity.kind == EntityKind::Function)
        .expect("literal template declaration remains retained");
    let located = reopened
        .code_locate(
            receipt.generation,
            "LiteralStep".to_owned(),
            LocateMode::Exact,
            16,
            0,
            &deadline(),
        )
        .expect("reopened generation supports public locate");
    assert_eq!(located.data.generation, receipt.generation);
    assert!(!located.data.truncated);
    assert!(
        located
            .data
            .hits
            .iter()
            .any(|hit| hit.symbol == Some(literal.id))
    );
    for (path, source) in [("module.f90", MODULE), ("templates.fypp", TEMPLATE)] {
        let file = original
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        let reference = file.evidence.source.as_ref().unwrap();
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
        assert_source_read(&reopened, reference, path, source.as_bytes());
    }
    drop(reopened);
}
