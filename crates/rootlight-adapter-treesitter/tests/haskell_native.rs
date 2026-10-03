//! Real Haskell native captures must preserve lexical binding ownership.
//! Equation grouping and written annotations remain structural evidence.
//! Complete simple headers and admitted native paths prove formal reads.
//! Application dispatch, inferred types and other use-site names remain unavailable.

use std::{
    collections::BTreeSet,
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, ParseRequest, StreamLimits,
    execute_analysis, execute_parse,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, SymbolId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ContainerRef, CoverageStatus, EntityKind, EntityRecord,
    ExtensionSupport, FactRef, IrLimits, LEXICAL_EXTENSION_NAMESPACE, NormalizedIrDocument,
    OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint, RelationPredicate,
    SourceRef, SourceSpan, decode_lexical_evidence_envelope, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

const SOURCE: &str = concat!(
    "module Sample where\n",
    "pick :: Int -> Int\n",
    "pick 0 = 1\n",
    "pick value = value\n",
    "pick value = 2\n",
    "class Label a where\n",
    "  label :: a -> Int\n",
    "  label value = 3\n",
    "  label value = 4\n",
    "outer value =\n",
    "  let\n",
    "    pick :: Int -> Int\n",
    "    pick 0 = 5\n",
    "    pick item = item\n",
    "  in pick value\n",
    "shadow xs ys = [x | x <- xs, x <- ys]\n",
);

struct Fixture {
    _directory: TempDir,
    text: String,
    snapshot: SourceSnapshot,
    source: SourceRef,
}

impl Fixture {
    fn new(text: String) -> Self {
        let directory = tempdir_in(std::env::current_dir().unwrap()).unwrap();
        fs::write(directory.path().join("Sample.hs"), &text).unwrap();
        let repository = derive_repository(b"haskell-native-lexical-contract").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(&RelativePath::parse(Path::new("Sample.hs")).unwrap(), 4096)
            .unwrap();
        let source = SourceRef::new(
            repository,
            GenerationId::from_bytes([11; 20]),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(text.len()).unwrap()).unwrap(),
            snapshot.content_hash(),
            None,
        );
        Self {
            _directory: directory,
            text,
            snapshot,
            source,
        }
    }

    fn span(&self, written: &str, index: usize) -> SourceSpan {
        let start = self.text.match_indices(written).nth(index).unwrap().0;
        SourceSpan::new(
            self.snapshot.file(),
            u64::try_from(start).unwrap(),
            u64::try_from(start + written.len()).unwrap(),
        )
        .unwrap()
    }

    fn head(&self, written: &str, index: usize, name: &str) -> SourceSpan {
        assert!(written.starts_with(name));
        let start = self.span(written, index).start_byte();
        SourceSpan::new(
            self.snapshot.file(),
            start,
            start + u64::try_from(name.len()).unwrap(),
        )
        .unwrap()
    }

    fn analyze(&self) -> AnalysisOutput {
        let provider = Arc::new(
            TreeSitterProvider::new(
                RuntimeConfig::new(
                    4096,
                    4096,
                    128,
                    32,
                    64,
                    1,
                    4 * 1024 * 1024,
                    ParserSettings::new(512).unwrap(),
                )
                .unwrap(),
            )
            .unwrap(),
        );
        let language = LanguageId::new("haskell").unwrap();
        let stream = StreamLimits::new(
            128,
            4096,
            16 * 1024 * 1024,
            128,
            128 * 1024,
            4 * 1024 * 1024,
            BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024).unwrap(),
        )
        .unwrap();
        let limits = AnalysisLimits::new(
            4096,
            4096,
            128,
            32,
            16 * 1024 * 1024,
            stream.clone(),
            stream,
            IrLimits::default(),
        )
        .unwrap();
        let parse = ParseRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            language.clone(),
            EncodingId::utf8(),
            Vec::new(),
            &limits,
        )
        .unwrap();
        let syntax = execute_parse(
            provider.as_ref(),
            &parse,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        assert_eq!(
            syntax.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert!(
            syntax.diagnostics().is_empty(),
            "{:?}",
            syntax.diagnostics()
        );
        let analyzer = TreeSitterAnalyzer::new(
            provider,
            ProducerIdentity::new(
                "haskell-native-contract",
                "1.0",
                content_hash(b"haskell-native-configuration"),
            )
            .unwrap(),
            language.clone(),
            "tree-sitter-haskell-0.23.1",
            content_hash(b"haskell-native-binary"),
        )
        .unwrap();
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            language,
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"haskell-native-build-context")),
            &limits,
        )
        .unwrap()
        .with_generated_status(false);
        let output = execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
        for occurrence in &output.document().occurrences {
            assert_eq!(
                occurrence.id,
                rootlight_ir::derive_occurrence_record_id(occurrence).unwrap(),
                "occurrence identity must include its final evidence"
            );
        }
        output
    }
}

fn defined_entity(document: &NormalizedIrDocument, span: SourceSpan) -> &EntityRecord {
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Definition && site.source.span() == span)
        .collect();
    assert_eq!(definitions.len(), 1, "definition at {span:?}");
    let OccurrenceTarget::Resolved { symbol } = &definitions[0].target else {
        panic!("written definition must have a structural owner");
    };
    document
        .entities
        .iter()
        .find(|entity| entity.id == *symbol)
        .unwrap()
}

fn assert_exact_clause_reads(
    fixture: &Fixture,
    document: &NormalizedIrDocument,
    clause: &str,
    reads: &[(&str, &str)],
) {
    let mut expected_reads = BTreeSet::new();
    let mut expected_relations = BTreeSet::new();
    for &(name, anchor) in reads {
        let anchor = anchor.replace(
            '\n',
            if clause.contains("\r\n") {
                "\r\n"
            } else {
                "\n"
            },
        );
        assert!(anchor.starts_with(name));
        let (header, _) = clause.split_once(" = ").unwrap();
        let formal_offset = header.find(&format!(" {name}")).unwrap() + 1;
        let start = fixture.span(clause, 0).start_byte();
        let formal_span = SourceSpan::new(
            fixture.snapshot.file(),
            start + u64::try_from(formal_offset).unwrap(),
            start + u64::try_from(formal_offset + name.len()).unwrap(),
        )
        .unwrap();
        let parameter = defined_entity(document, formal_span);
        assert_eq!(parameter.kind, EntityKind::Parameter, "{clause}");
        let function_name = header.split_whitespace().next().unwrap();
        let function = defined_entity(document, fixture.head(clause, 0, function_name));
        assert_eq!(parameter.container, Some(ContainerRef::Entity(function.id)));
        let read_span = fixture.head(&anchor, 0, name);
        let occurrences = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference && site.source.span() == read_span
            })
            .collect::<Vec<_>>();
        assert_eq!(occurrences.len(), 1, "{clause}: {anchor}");
        let read = occurrences[0];
        assert!(expected_reads.insert(read.id));
        assert_eq!(
            read.target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            }
        );
        assert_eq!(read.source.repository(), fixture.source.repository());
        assert_eq!(read.source.generation(), fixture.source.generation());
        assert_eq!(read.source.content_hash(), fixture.source.content_hash());
        assert_eq!(read.syntactic_text_hash, content_hash(name.as_bytes()));
        assert_eq!(
            read.id,
            rootlight_ir::derive_occurrence_record_id(read).unwrap()
        );
        let provenance = document
            .provenance
            .iter()
            .find(|record| record.id == read.provenance)
            .unwrap();
        assert_eq!(provenance.repository, fixture.source.repository());
        assert_eq!(provenance.generation, fixture.source.generation());
        assert!(provenance.input_sources.iter().any(|source| {
            source.repository() == read.source.repository()
                && source.generation() == read.source.generation()
                && source.content_hash() == read.source.content_hash()
                && source.span().file() == read_span.file()
                && source.span().start_byte() <= read_span.start_byte()
                && read_span.end_byte() <= source.span().end_byte()
        }));
        let relations = document
            .relations
            .iter()
            .filter(|relation| relation.subject == RelationEndpoint::Occurrence(read.id))
            .collect::<Vec<_>>();
        assert_eq!(relations.len(), 1, "{clause}: {anchor}");
        let relation = relations[0];
        assert!(expected_relations.insert(relation.id));
        assert_eq!(relation.predicate, RelationPredicate::RefersTo);
        assert_eq!(relation.object, RelationEndpoint::Entity(parameter.id));
        assert_eq!(relation.evidence_kind, rootlight_ir::EvidenceKind::Derived);
        assert_eq!(relation.provenance, read.provenance);
        assert_eq!(relation.evidence.source.as_ref(), Some(&read.source));
        assert_eq!(
            relation.id,
            rootlight_ir::derive_relation_record_id(relation).unwrap()
        );
        assert!(
            !document.skipped_regions.iter().any(|gap| {
                gap.detail == "haskell-binding-target-unavailable" && gap.source.span() == read_span
            }),
            "{clause}: {anchor}"
        );
    }
    let uses = document
        .occurrences
        .iter()
        .filter(|site| {
            matches!(
                site.role,
                OccurrenceRole::Reference
                    | OccurrenceRole::CallSite
                    | OccurrenceRole::TypeUse
                    | OccurrenceRole::ImportUse
            )
        })
        .collect::<Vec<_>>();
    assert!(!uses.is_empty(), "{clause}");
    for site in uses {
        if expected_reads.contains(&site.id) {
            continue;
        }
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{clause}: {site:?}"
        );
        if site.role != OccurrenceRole::CallSite {
            assert!(
                document.skipped_regions.iter().any(|gap| {
                    gap.detail == "haskell-binding-target-unavailable"
                        && gap.source.span() == site.source.span()
                }),
                "{clause}: {site:?}"
            );
        }
    }
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|relation| { relation.predicate == RelationPredicate::RefersTo })
            .map(|relation| relation.id)
            .collect::<BTreeSet<_>>(),
        expected_relations
    );
    assert!(
        document.relations.iter().all(|relation| {
            matches!(
                relation.predicate,
                RelationPredicate::Contains
                    | RelationPredicate::Declares
                    | RelationPredicate::DefinesAt
            ) || expected_relations.contains(&relation.id)
        }),
        "{clause}"
    );
}

fn assert_equations(
    fixture: &Fixture,
    document: &NormalizedIrDocument,
    entity: &EntityRecord,
    equations: &[&str],
) {
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
        })
        .collect();
    assert_eq!(
        definitions.len(),
        equations.len(),
        "{}",
        entity.canonical_name
    );
    for equation in equations {
        let declaration = fixture.span(equation, 0);
        let name = fixture.head(equation, 0, &entity.canonical_name);
        let definition = definitions
            .iter()
            .find(|site| site.source.span() == name)
            .unwrap();
        assert_eq!(
            definition.syntactic_text_hash,
            content_hash(entity.canonical_name.as_bytes())
        );
        assert_eq!(
            definition.evidence.source.as_ref().unwrap().span(),
            declaration
        );
        assert!(document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.object == RelationEndpoint::Entity(entity.id)
                && relation.evidence.source.as_ref().unwrap().span() == declaration
        }));
    }
}

fn assert_annotation(
    fixture: &Fixture,
    document: &NormalizedIrDocument,
    written: &str,
    index: usize,
    entity: &EntityRecord,
) {
    let name = fixture.head(written, index, &entity.canonical_name);
    let declarations: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Declaration && site.source.span() == name)
        .collect();
    assert_eq!(declarations.len(), 1);
    let annotation = declarations[0];
    assert_eq!(
        annotation.target,
        OccurrenceTarget::Resolved { symbol: entity.id }
    );
    assert_eq!(
        annotation.evidence.source.as_ref().unwrap().span(),
        fixture.span(written, index)
    );
    let envelopes: Vec<_> = document
        .extensions
        .iter()
        .filter(|extension| {
            extension.namespace == LEXICAL_EXTENSION_NAMESPACE
                && decode_lexical_evidence_envelope(extension)
                    .unwrap()
                    .subject()
                    == FactRef::Fact(annotation.id)
        })
        .collect();
    assert_eq!(envelopes.len(), 1);
    let evidence = decode_lexical_evidence_envelope(envelopes[0]).unwrap();
    assert_eq!(evidence.text(), written);
    assert_eq!(
        evidence.complete_text_hash(),
        content_hash(written.as_bytes())
    );
}

fn assert_clause_formals(
    fixture: &Fixture,
    document: &NormalizedIrDocument,
    owner: SymbolId,
    clauses: &[(&str, &str)],
) {
    let mut identities = BTreeSet::new();
    for (clause, name) in clauses {
        let start = fixture.span(clause, 0).start_byte()
            + u64::try_from(clause.find(name).unwrap()).unwrap();
        let span = SourceSpan::new(
            fixture.snapshot.file(),
            start,
            start + u64::try_from(name.len()).unwrap(),
        )
        .unwrap();
        let parameter = defined_entity(document, span);
        assert_eq!(parameter.kind, EntityKind::Parameter);
        assert_eq!(parameter.canonical_name, *name);
        assert_eq!(parameter.container, Some(ContainerRef::Entity(owner)));
        assert!(
            identities.insert(parameter.id),
            "formals must belong to separate clauses: {clause}, {parameter:?}"
        );
    }
}

fn assert_unproven_uses(fixture: &Fixture, document: &NormalizedIrDocument) {
    let uses: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            matches!(
                site.role,
                OccurrenceRole::Reference
                    | OccurrenceRole::CallSite
                    | OccurrenceRole::TypeUse
                    | OccurrenceRole::ImportUse
            )
        })
        .collect();
    assert!(!uses.is_empty());
    let exact_read_span = fixture.span("value", 1);
    let exact_parameter = defined_entity(document, fixture.span("value", 0));
    let mut exact_reads = 0;
    for site in uses {
        if site.source.span() == exact_read_span {
            exact_reads += 1;
            assert_eq!(site.role, OccurrenceRole::Reference);
            assert_eq!(
                site.target,
                OccurrenceTarget::Resolved {
                    symbol: exact_parameter.id
                }
            );
            assert_eq!(site.syntactic_text_hash, content_hash(b"value"));
            assert_eq!(site.source.generation(), fixture.source.generation());
            assert_eq!(site.source.content_hash(), fixture.source.content_hash());
            continue;
        }
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{site:?}"
        );
        let span = site.source.span();
        let text = fixture
            .text
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap();
        let spelling = if site.role == OccurrenceRole::CallSite {
            // Call provenance covers the application; its spelling is the
            // native terminal callee, not the argument-bearing source range.
            assert_eq!(text, "pick value");
            "pick"
        } else {
            text
        };
        assert_eq!(site.syntactic_text_hash, content_hash(spelling.as_bytes()));
        assert_eq!(site.source.generation(), fixture.source.generation());
        assert_eq!(site.source.content_hash(), fixture.source.content_hash());
    }
    assert_eq!(exact_reads, 1);
    let exact_read = document
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Reference && site.source.span() == exact_read_span
        })
        .unwrap();
    let exact_relations = document
        .relations
        .iter()
        .filter(|relation| relation.subject == RelationEndpoint::Occurrence(exact_read.id))
        .collect::<Vec<_>>();
    assert_eq!(exact_relations.len(), 1);
    assert_eq!(exact_relations[0].predicate, RelationPredicate::RefersTo);
    assert_eq!(
        exact_relations[0].object,
        RelationEndpoint::Entity(exact_parameter.id)
    );
    assert_eq!(
        exact_relations[0].evidence.source.as_ref(),
        Some(&exact_read.source)
    );
    assert!(document.relations.iter().all(|relation| matches!(
        relation.predicate,
        RelationPredicate::Contains | RelationPredicate::Declares | RelationPredicate::DefinesAt
    ) || relation.id == exact_relations[0].id));
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "haskell-binding-target-unavailable"
                && gap.source.span() == exact_read_span)
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| { gap.detail == "haskell-binding-target-unavailable" })
    );
}

#[test]
fn native_equations_annotations_and_generators_preserve_lexical_owners() {
    for newline in ["\n", "\r\n"] {
        let fixture = Fixture::new(SOURCE.replace('\n', newline));
        let output = fixture.analyze();
        let document = output.document();
        let top = defined_entity(document, fixture.head("pick 0 = 1", 0, "pick"));
        let outer = defined_entity(document, fixture.head("outer value", 0, "outer"));
        let local = defined_entity(document, fixture.head("pick 0 = 5", 0, "pick"));
        let class = defined_entity(document, fixture.span("Label", 0));
        let method = defined_entity(document, fixture.head("label value = 3", 0, "label"));
        assert_eq!(top.kind, EntityKind::Function);
        assert_eq!(local.kind, EntityKind::Function);
        assert_eq!(class.kind, EntityKind::Trait);
        assert_eq!(method.kind, EntityKind::Method);
        assert_ne!(top.id, local.id);
        assert_eq!(top.container, outer.container);
        assert_eq!(local.container, Some(ContainerRef::Entity(outer.id)));
        assert_eq!(method.container, Some(ContainerRef::Entity(class.id)));
        assert_eq!(
            document
                .entities
                .iter()
                .filter(|entity| {
                    entity.canonical_name == "pick" && entity.kind == EntityKind::Function
                })
                .count(),
            2
        );
        assert_equations(
            &fixture,
            document,
            top,
            &["pick 0 = 1", "pick value = value", "pick value = 2"],
        );
        assert_equations(
            &fixture,
            document,
            local,
            &["pick 0 = 5", "pick item = item"],
        );
        assert_equations(
            &fixture,
            document,
            method,
            &["label value = 3", "label value = 4"],
        );
        assert_annotation(&fixture, document, "pick :: Int -> Int", 0, top);
        assert_annotation(&fixture, document, "pick :: Int -> Int", 1, local);
        assert_annotation(&fixture, document, "label :: a -> Int", 0, method);
        assert_clause_formals(
            &fixture,
            document,
            top.id,
            &[("pick value = value", "value"), ("pick value = 2", "value")],
        );
        assert_clause_formals(
            &fixture,
            document,
            method.id,
            &[("label value = 3", "value"), ("label value = 4", "value")],
        );
        let shadow = defined_entity(document, fixture.head("shadow xs ys", 0, "shadow"));
        let first = defined_entity(document, fixture.head("x <- xs", 0, "x"));
        let second = defined_entity(document, fixture.head("x <- ys", 0, "x"));
        assert_ne!(first.id, second.id);
        for variable in [first, second] {
            assert_eq!(variable.kind, EntityKind::Variable);
            assert_eq!(variable.canonical_name, "x");
            assert_eq!(variable.container, Some(ContainerRef::Entity(shadow.id)));
        }
        assert_eq!(
            document
                .entities
                .iter()
                .filter(|entity| {
                    entity.kind == EntityKind::Variable && entity.canonical_name == "x"
                })
                .count(),
            2
        );
        assert_unproven_uses(&fixture, document);
    }
}

#[test]
fn unguarded_equation_reads_use_only_their_own_written_simple_formals() {
    for newline in ["\n", "\r\n"] {
        let fixture = Fixture::new(
            concat!(
                "module Exact where\n",
                "identity x = x\n",
                "choose x y = x\n",
                "choose x y = y\n",
                "forward x = unknown x\n",
                "group x = (x)\n",
                "literals x = unknown 1 1.5 'a' \"text\" x\n",
                "free x = missing\n",
            )
            .replace('\n', newline),
        );
        let output = fixture.analyze();
        let document = output.document();
        let mut choose_parameters = BTreeSet::new();
        for (clause, name) in [
            ("identity x = x", "x"),
            ("choose x y = x", "x"),
            ("choose x y = y", "y"),
            ("forward x = unknown x", "x"),
            ("group x = (x)", "x"),
            ("literals x = unknown 1 1.5 'a' \"text\" x", "x"),
        ] {
            let start = fixture.span(clause, 0).start_byte();
            let span = |offset| {
                SourceSpan::new(
                    fixture.snapshot.file(),
                    start + u64::try_from(offset).unwrap(),
                    start + u64::try_from(offset + name.len()).unwrap(),
                )
                .unwrap()
            };
            let parameter = defined_entity(document, span(clause.find(name).unwrap()));
            assert_eq!(parameter.kind, EntityKind::Parameter);
            if clause.starts_with("choose") {
                assert!(choose_parameters.insert(parameter.id));
            }
            let read_span = span(clause.rfind(name).unwrap());
            let reads = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Reference && site.source.span() == read_span
                })
                .collect::<Vec<_>>();
            assert_eq!(reads.len(), 1, "{clause}");
            let read = reads[0];
            assert_eq!(
                read.target,
                OccurrenceTarget::Resolved {
                    symbol: parameter.id
                }
            );
            assert_eq!(
                read.id,
                rootlight_ir::derive_occurrence_record_id(read).unwrap()
            );
            assert_eq!(read.syntactic_text_hash, content_hash(name.as_bytes()));
            assert_eq!(read.source.generation(), fixture.source.generation());
            assert_eq!(read.source.content_hash(), fixture.source.content_hash());
            let relations = document
                .relations
                .iter()
                .filter(|relation| relation.subject == RelationEndpoint::Occurrence(read.id))
                .collect::<Vec<_>>();
            assert_eq!(relations.len(), 1);
            assert_eq!(relations[0].predicate, RelationPredicate::RefersTo);
            assert_eq!(relations[0].object, RelationEndpoint::Entity(parameter.id));
            assert_eq!(relations[0].evidence.source.as_ref(), Some(&read.source));
            assert_eq!(
                relations[0].id,
                rootlight_ir::derive_relation_record_id(relations[0]).unwrap()
            );
            assert!(
                !document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail == "haskell-binding-target-unavailable"
                        && gap.source.span() == read_span)
            );
        }
        assert_eq!(choose_parameters.len(), 2);
        let mut pair_ids = BTreeSet::new();
        let mut choose_owner = None;
        for clause in ["choose x y = x", "choose x y = y"] {
            let function = defined_entity(document, fixture.head(clause, 0, "choose"));
            assert_eq!(*choose_owner.get_or_insert(function.id), function.id);
            for name in ["x", "y"] {
                let start = fixture.span(clause, 0).start_byte()
                    + u64::try_from(clause.find(name).unwrap()).unwrap();
                let parameter = defined_entity(
                    document,
                    SourceSpan::new(
                        fixture.snapshot.file(),
                        start,
                        start + u64::try_from(name.len()).unwrap(),
                    )
                    .unwrap(),
                );
                assert_eq!(parameter.container, Some(ContainerRef::Entity(function.id)));
                assert!(
                    pair_ids.insert(parameter.id),
                    "same-spelled formals must remain clause-owned"
                );
            }
        }
        assert_eq!(pair_ids.len(), 4);
        for site in document.occurrences.iter().filter(|site| {
            site.role == OccurrenceRole::CallSite
                || site.source.span() == fixture.span("missing", 0)
        }) {
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        }
        assert!(
            document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::CallSite)
        );
        assert!(
            document
                .relations
                .iter()
                .all(|relation| relation.predicate != RelationPredicate::Calls)
        );
    }
}

#[test]
fn unsupported_equation_subtrees_remain_unresolved_without_blocking_independent_reads() {
    let cases: &[(&str, &[(&str, &str)])] = &[
        ("blocked x = x where\n  x = 1\n", &[]),
        ("blocked x | True = x\n", &[]),
        ("blocked (Just x) = x\n", &[]),
        ("blocked (extract -> x) = x\n", &[]),
        ("blocked x = (\\x -> x)\n", &[]),
        ("blocked x = let x = 1 in x\n", &[]),
        ("blocked x = case x of { Just y -> y }\n", &[]),
        ("blocked x = do { y <- pure x; pure y }\n", &[]),
        ("blocked xs = [x | x <- xs]\n", &[]),
        ("blocked x = Module.x\n", &[]),
        ("blocked x = Record { field = x }\n", &[]),
        ("blocked x = (x Module.+)\n", &[]),
        ("blocked x = (x :: Int)\n", &[]),
        ("blocked x = [| x |]\n", &[]),
        ("blocked x = Just x\n", &[("x", "x\n")]),
        ("blocked x = Module.combine x\n", &[("x", "x\n")]),
        ("blocked x = unknown (\\y -> y) x\n", &[("x", "x\n")]),
    ];
    for newline in ["\n", "\r\n"] {
        for &(body, reads) in cases {
            let body = body.replace('\n', newline);
            let fixture = Fixture::new(format!("module Negative where{newline}{body}"));
            let output = fixture.analyze();
            assert_exact_clause_reads(&fixture, output.document(), &body, reads);
        }
    }
}

#[test]
fn conditional_field_reads_resolve_only_their_exact_clause_formals() {
    for newline in ["\n", "\r\n"] {
        let clauses: [(&str, &[&str]); 7] = [
            (
                "choose flag left right = if flag then left else right",
                &["flag", "left", "right"],
            ),
            (
                "choose flag left right = if flag then right else left",
                &["flag", "left", "right"],
            ),
            (
                "nested flag other left right = if flag then (if other then left else right) else right",
                &["flag", "other", "left", "right"],
            ),
            (
                "forward flag value = if flag then unknown value else value",
                &["flag", "value"],
            ),
            (
                "free flag value = if missing then value else 0",
                &["flag", "value"],
            ),
            (
                "numeric flag value = if flag then value else 0",
                &["flag", "value"],
            ),
            (
                "grouped flag value = (if (flag) then (value) else (value))",
                &["flag", "value"],
            ),
        ];
        let text = format!(
            "module Conditional where\n{}\n",
            clauses
                .iter()
                .map(|(clause, _)| *clause)
                .collect::<Vec<_>>()
                .join("\n")
        )
        .replace('\n', newline);
        let fixture = Fixture::new(text);
        let output = fixture.analyze();
        let document = output.document();
        let mut resolved_reads = BTreeSet::new();
        let mut choose_parameters = BTreeSet::new();
        let mut choose_owner = None;
        for (clause, names) in clauses {
            let start = fixture.span(clause, 0).start_byte();
            let (head, rhs) = clause.split_once('=').unwrap();
            let rhs_offset = head.len() + 1;
            let source_span = |offset: usize, length: usize| {
                SourceSpan::new(
                    fixture.snapshot.file(),
                    start + u64::try_from(offset).unwrap(),
                    start + u64::try_from(offset + length).unwrap(),
                )
                .unwrap()
            };
            if clause.starts_with("choose ") {
                let function = defined_entity(document, fixture.head(clause, 0, "choose"));
                assert_eq!(function.kind, EntityKind::Function);
                assert_eq!(*choose_owner.get_or_insert(function.id), function.id);
            }
            for &name in names {
                let parameter =
                    defined_entity(document, source_span(head.find(name).unwrap(), name.len()));
                assert_eq!(parameter.kind, EntityKind::Parameter);
                if clause.starts_with("choose ") {
                    assert_eq!(
                        parameter.container,
                        Some(ContainerRef::Entity(choose_owner.unwrap()))
                    );
                    assert!(choose_parameters.insert(parameter.id));
                }
                for (offset, _) in rhs.match_indices(name) {
                    let read_span = source_span(rhs_offset + offset, name.len());
                    let reads = document
                        .occurrences
                        .iter()
                        .filter(|site| {
                            site.role == OccurrenceRole::Reference
                                && site.source.span() == read_span
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(reads.len(), 1, "{clause}: {name}");
                    let read = reads[0];
                    assert_eq!(
                        read.target,
                        OccurrenceTarget::Resolved {
                            symbol: parameter.id
                        },
                        "{clause}: {name}"
                    );
                    assert!(resolved_reads.insert(read.id));
                    assert_eq!(
                        read.id,
                        rootlight_ir::derive_occurrence_record_id(read).unwrap()
                    );
                    assert_eq!(read.syntactic_text_hash, content_hash(name.as_bytes()));
                    assert_eq!(read.source.generation(), fixture.source.generation());
                    assert_eq!(read.source.content_hash(), fixture.source.content_hash());
                    let relations = document
                        .relations
                        .iter()
                        .filter(|relation| {
                            relation.subject == RelationEndpoint::Occurrence(read.id)
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(relations.len(), 1, "{clause}: {name}");
                    assert_eq!(relations[0].predicate, RelationPredicate::RefersTo);
                    assert_eq!(relations[0].object, RelationEndpoint::Entity(parameter.id));
                    assert_eq!(
                        relations[0].evidence_kind,
                        rootlight_ir::EvidenceKind::Derived
                    );
                    assert_eq!(relations[0].evidence.source.as_ref(), Some(&read.source));
                    assert_eq!(
                        relations[0].id,
                        rootlight_ir::derive_relation_record_id(relations[0]).unwrap()
                    );
                    assert!(
                        !document.skipped_regions.iter().any(|gap| {
                            gap.detail == "haskell-binding-target-unavailable"
                                && gap.source.span() == read_span
                        }),
                        "{clause}: {name}"
                    );
                }
            }
        }
        assert_eq!(choose_parameters.len(), 6);
        assert_eq!(resolved_reads.len(), 20);
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Reference
                        && matches!(site.target, OccurrenceTarget::Resolved { .. })
                })
                .count(),
            resolved_reads.len()
        );
        let missing_span = fixture.span("missing", 0);
        let missing = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference && site.source.span() == missing_span
            })
            .collect::<Vec<_>>();
        assert_eq!(missing.len(), 1);
        assert!(matches!(
            missing[0].target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.detail == "haskell-binding-target-unavailable" && gap.source.span() == missing_span
        }));
        let unknown = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::CallSite)
            .collect::<Vec<_>>();
        assert!(!unknown.is_empty());
        assert!(
            unknown
                .iter()
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
        );
        assert!(
            unknown
                .iter()
                .any(|site| site.source.span() == fixture.span("unknown value", 0))
        );
        assert!(
            document
                .relations
                .iter()
                .all(|relation| relation.predicate != RelationPredicate::Calls)
        );
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|relation| relation.predicate == RelationPredicate::RefersTo)
                .count(),
            resolved_reads.len()
        );
    }
}

#[test]
fn conditional_read_paths_leave_unsupported_branches_unresolved() {
    let cases: &[(&str, &[(&str, &str)])] = &[
        (
            "blocked flag x = if flag then let x = 1 in x else x\n",
            &[("flag", "flag then"), ("x", "x\n")],
        ),
        (
            "blocked flag x = if flag then x else (\\x -> x)\n",
            &[("flag", "flag then"), ("x", "x else")],
        ),
        (
            "blocked flag x = if (let flag = x in flag) then x else x\n",
            &[("x", "x else"), ("x", "x\n")],
        ),
        (
            "blocked flag x = if flag then case x of { Just y -> y } else x\n",
            &[("flag", "flag then"), ("x", "x\n")],
        ),
        (
            "blocked flag x = if flag then x else do { y <- pure x; pure y }\n",
            &[("flag", "flag then"), ("x", "x else")],
        ),
        (
            "blocked flag x = if flag then [y | y <- x] else x\n",
            &[("flag", "flag then"), ("x", "x\n")],
        ),
        (
            "blocked flag x = if flag then (x :: Int) else x\n",
            &[("flag", "flag then"), ("x", "x\n")],
        ),
        (
            "blocked flag x = if Module.flag then x else x\n",
            &[("x", "x else"), ("x", "x\n")],
        ),
        (
            "blocked flag x = if flag then Just x else x\n",
            &[("flag", "flag then"), ("x", "x else"), ("x", "x\n")],
        ),
        (
            "blocked flag x = if flag then (x Module.+) else x\n",
            &[("flag", "flag then"), ("x", "x\n")],
        ),
        (
            "blocked flag x = if flag then x else x where\n  x = 1\n",
            &[],
        ),
        ("blocked flag x | flag = if flag then x else x\n", &[]),
        ("blocked flag (Just x) = if flag then x else x\n", &[]),
        ("blocked flag x = if | flag -> x\n", &[]),
    ];
    for newline in ["\n", "\r\n"] {
        for &(body, reads) in cases {
            let body = body.replace('\n', newline);
            let fixture = Fixture::new(format!("module NegativeConditional where{newline}{body}"));
            let output = fixture.analyze();
            assert_exact_clause_reads(&fixture, output.document(), &body, reads);
        }
    }
}

#[test]
fn infix_operand_reads_resolve_only_their_exact_clause_formals() {
    for newline in ["\n", "\r\n"] {
        let clauses: [(&str, &[&str]); 11] = [
            ("add value = value + 1", &["value"]),
            (
                "mixed left right value = (left + right) * value",
                &["left", "right", "value"],
            ),
            ("forward value = unknown $ other value", &["value"]),
            ("choose left right = left + right", &["left", "right"]),
            ("choose left right = right + left", &["left", "right"]),
            ("tick left right = left `unknown` right", &["left", "right"]),
            (
                "dispatch operation left right = left `operation` right",
                &["left", "right"],
            ),
            (
                "nested flag left right = if flag then left + right else right",
                &["flag", "left", "right"],
            ),
            ("free value = value + missing", &["value"]),
            ("group value = (value + 1)", &["value"]),
            ("callee operation value = operation value + 1", &["value"]),
        ];
        let text = format!(
            "module Infix where\n{}\n",
            clauses
                .iter()
                .map(|(clause, _)| *clause)
                .collect::<Vec<_>>()
                .join("\n")
        )
        .replace('\n', newline);
        let fixture = Fixture::new(text);
        let output = fixture.analyze();
        let document = output.document();
        let mut resolved_reads = BTreeSet::new();
        let mut choose_parameters = BTreeSet::new();
        let mut choose_owner = None;
        for (clause, names) in clauses {
            let start = fixture.span(clause, 0).start_byte();
            let (head, rhs) = clause.split_once('=').unwrap();
            let rhs_offset = head.len() + 1;
            let source_span = |offset: usize, length: usize| {
                SourceSpan::new(
                    fixture.snapshot.file(),
                    start + u64::try_from(offset).unwrap(),
                    start + u64::try_from(offset + length).unwrap(),
                )
                .unwrap()
            };
            if clause.starts_with("choose ") {
                let function = defined_entity(document, fixture.head(clause, 0, "choose"));
                assert_eq!(function.kind, EntityKind::Function);
                assert_eq!(*choose_owner.get_or_insert(function.id), function.id);
            }
            for &name in names {
                let parameter =
                    defined_entity(document, source_span(head.find(name).unwrap(), name.len()));
                assert_eq!(parameter.kind, EntityKind::Parameter);
                if clause.starts_with("choose ") {
                    assert_eq!(
                        parameter.container,
                        Some(ContainerRef::Entity(choose_owner.unwrap()))
                    );
                    assert!(choose_parameters.insert(parameter.id));
                }
                for (offset, _) in rhs.match_indices(name) {
                    let read_span = source_span(rhs_offset + offset, name.len());
                    let reads = document
                        .occurrences
                        .iter()
                        .filter(|site| {
                            site.role == OccurrenceRole::Reference
                                && site.source.span() == read_span
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(reads.len(), 1, "{clause}: {name}");
                    let read = reads[0];
                    assert_eq!(
                        read.target,
                        OccurrenceTarget::Resolved {
                            symbol: parameter.id
                        },
                        "{clause}: {name}"
                    );
                    assert!(resolved_reads.insert(read.id));
                    assert_eq!(
                        read.id,
                        rootlight_ir::derive_occurrence_record_id(read).unwrap()
                    );
                    assert_eq!(read.syntactic_text_hash, content_hash(name.as_bytes()));
                    assert_eq!(read.source.generation(), fixture.source.generation());
                    assert_eq!(read.source.content_hash(), fixture.source.content_hash());
                    let relations = document
                        .relations
                        .iter()
                        .filter(|relation| {
                            relation.subject == RelationEndpoint::Occurrence(read.id)
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(relations.len(), 1, "{clause}: {name}");
                    assert_eq!(relations[0].predicate, RelationPredicate::RefersTo);
                    assert_eq!(relations[0].object, RelationEndpoint::Entity(parameter.id));
                    assert_eq!(
                        relations[0].evidence_kind,
                        rootlight_ir::EvidenceKind::Derived
                    );
                    assert_eq!(relations[0].evidence.source.as_ref(), Some(&read.source));
                    assert_eq!(
                        relations[0].id,
                        rootlight_ir::derive_relation_record_id(relations[0]).unwrap()
                    );
                    assert!(
                        !document.skipped_regions.iter().any(|gap| {
                            gap.detail == "haskell-binding-target-unavailable"
                                && gap.source.span() == read_span
                        }),
                        "{clause}: {name}"
                    );
                }
            }
        }
        assert_eq!(choose_parameters.len(), 4);
        assert_eq!(resolved_reads.len(), 20);
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Reference
                        && matches!(site.target, OccurrenceTarget::Resolved { .. })
                })
                .count(),
            resolved_reads.len()
        );
        for (clause, name) in [
            (
                "dispatch operation left right = left `operation` right",
                "operation",
            ),
            ("callee operation value = operation value + 1", "operation"),
        ] {
            let start = fixture.span(clause, 0).start_byte();
            let offset = clause.rfind(name).unwrap();
            let span = SourceSpan::new(
                fixture.snapshot.file(),
                start + u64::try_from(offset).unwrap(),
                start + u64::try_from(offset + name.len()).unwrap(),
            )
            .unwrap();
            let sites = document
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Reference && site.source.span() == span)
                .collect::<Vec<_>>();
            assert_eq!(sites.len(), 1, "{clause}");
            assert!(matches!(
                sites[0].target,
                OccurrenceTarget::Unresolved { .. }
            ));
            let parameter_span = SourceSpan::new(
                fixture.snapshot.file(),
                start + u64::try_from(clause.find(name).unwrap()).unwrap(),
                start + u64::try_from(clause.find(name).unwrap() + name.len()).unwrap(),
            )
            .unwrap();
            assert_eq!(
                defined_entity(document, parameter_span).kind,
                EntityKind::Parameter
            );
            assert!(
                document.relations.iter().all(|relation| {
                    relation.subject != RelationEndpoint::Occurrence(sites[0].id)
                })
            );
            assert!(document.skipped_regions.iter().any(|gap| {
                gap.detail == "haskell-binding-target-unavailable" && gap.source.span() == span
            }));
        }
        let dispatch_span = fixture.span("left `operation` right", 0);
        let dispatch = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::CallSite && site.source.span() == dispatch_span
            })
            .collect::<Vec<_>>();
        assert_eq!(dispatch.len(), 1);
        assert!(matches!(
            dispatch[0].target,
            OccurrenceTarget::Unresolved { .. }
        ));
        let missing_span = fixture.span("missing", 0);
        let missing = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference && site.source.span() == missing_span
            })
            .collect::<Vec<_>>();
        assert_eq!(missing.len(), 1);
        assert!(matches!(
            missing[0].target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.detail == "haskell-binding-target-unavailable" && gap.source.span() == missing_span
        }));
        let unknown = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::CallSite)
            .collect::<Vec<_>>();
        assert!(!unknown.is_empty());
        assert!(
            unknown
                .iter()
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
        );
        assert!(
            unknown
                .iter()
                .any(|site| site.source.span() == fixture.span("other value", 0))
        );
        assert!(
            document
                .relations
                .iter()
                .all(|relation| relation.predicate != RelationPredicate::Calls)
        );
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|relation| relation.predicate == RelationPredicate::RefersTo)
                .count(),
            resolved_reads.len()
        );
    }
}

#[test]
fn infix_read_paths_keep_unsupported_operands_and_dispatch_unresolved() {
    let cases: &[(&str, &[(&str, &str)])] = &[
        ("blocked x = (x Module.+)\n", &[]),
        ("blocked x = (Module.+ x)\n", &[]),
        ("blocked x = x Module.+ 1\n", &[]),
        ("blocked x = x `Module.combine` x\n", &[]),
        ("blocked x = x : x\n", &[]),
        ("blocked x = x `Just` x\n", &[]),
        ("blocked x = x + (let x = 1 in x)\n", &[("x", "x +")]),
        ("blocked x = x + (\\x -> x)\n", &[("x", "x +")]),
        (
            "blocked x = x + (case x of { Just y -> y })\n",
            &[("x", "x +")],
        ),
        (
            "blocked x = x + (do { y <- pure x; pure y })\n",
            &[("x", "x +")],
        ),
        ("blocked x = x + [y | y <- x]\n", &[("x", "x +")]),
        ("blocked x = x + (x :: Int)\n", &[("x", "x +")]),
        ("blocked x = x + 1 where\n  x = 1\n", &[]),
        ("blocked x | x = x + 1\n", &[]),
        ("blocked (Just x) = x + 1\n", &[]),
        ("blocked x = x + Module.value\n", &[("x", "x +")]),
    ];
    for newline in ["\n", "\r\n"] {
        for &(body, reads) in cases {
            let body = body.replace('\n', newline);
            let fixture = Fixture::new(format!("module NegativeInfix where{newline}{body}"));
            let output = fixture.analyze();
            assert_exact_clause_reads(&fixture, output.document(), &body, reads);
        }
    }
}

#[test]
fn operator_section_reads_resolve_only_their_exact_clause_formals() {
    for newline in ["\n", "\r\n"] {
        let clauses: [(&str, &[&str]); 12] = [
            ("left value = (value +)", &["value"]),
            ("right value = (+ value)", &["value"]),
            ("applyLeft value = unknown (value *)", &["value"]),
            ("applyRight value = unknown ( # value)", &["value"]),
            ("nested value = unknown ((value +) $)", &["value"]),
            (
                "pair left right = unknown (left +) ($ right)",
                &["left", "right"],
            ),
            ("choose value = unknown (value +)", &["value"]),
            ("choose value = unknown ($ value)", &["value"]),
            (
                "tick operation left right = unknown (left `operation`) (`operation` right)",
                &["left", "right"],
            ),
            ("dispatch operation value = (operation +) value", &["value"]),
            ("free value = unknown (value +) ($ missing)", &["value"]),
            (
                "conditional flag value = if flag then (value +) else ($ value)",
                &["flag", "value"],
            ),
        ];
        let text = format!(
            "module Sections where\n{}\n",
            clauses
                .iter()
                .map(|(clause, _)| *clause)
                .collect::<Vec<_>>()
                .join("\n")
        )
        .replace('\n', newline);
        let fixture = Fixture::new(text);
        let output = fixture.analyze();
        let document = output.document();
        let mut resolved_reads = BTreeSet::new();
        let mut choose_parameters = BTreeSet::new();
        let mut choose_owner = None;
        for (clause, names) in clauses {
            let start = fixture.span(clause, 0).start_byte();
            let (head, rhs) = clause.split_once('=').unwrap();
            let rhs_offset = head.len() + 1;
            let source_span = |offset: usize, length: usize| {
                SourceSpan::new(
                    fixture.snapshot.file(),
                    start + u64::try_from(offset).unwrap(),
                    start + u64::try_from(offset + length).unwrap(),
                )
                .unwrap()
            };
            if clause.starts_with("choose ") {
                let function = defined_entity(document, fixture.head(clause, 0, "choose"));
                assert_eq!(function.kind, EntityKind::Function);
                assert_eq!(*choose_owner.get_or_insert(function.id), function.id);
            }
            for &name in names {
                let parameter =
                    defined_entity(document, source_span(head.find(name).unwrap(), name.len()));
                assert_eq!(parameter.kind, EntityKind::Parameter);
                if clause.starts_with("choose ") {
                    assert_eq!(
                        parameter.container,
                        Some(ContainerRef::Entity(choose_owner.unwrap()))
                    );
                    assert!(choose_parameters.insert(parameter.id));
                }
                for (offset, _) in rhs.match_indices(name) {
                    let read_span = source_span(rhs_offset + offset, name.len());
                    let reads = document
                        .occurrences
                        .iter()
                        .filter(|site| {
                            site.role == OccurrenceRole::Reference
                                && site.source.span() == read_span
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(reads.len(), 1, "{clause}: {name}");
                    let read = reads[0];
                    assert_eq!(
                        read.target,
                        OccurrenceTarget::Resolved {
                            symbol: parameter.id
                        },
                        "{clause}: {name}"
                    );
                    assert!(resolved_reads.insert(read.id));
                    assert_eq!(
                        read.id,
                        rootlight_ir::derive_occurrence_record_id(read).unwrap()
                    );
                    assert_eq!(read.syntactic_text_hash, content_hash(name.as_bytes()));
                    assert_eq!(read.source.generation(), fixture.source.generation());
                    assert_eq!(read.source.content_hash(), fixture.source.content_hash());
                    let relations = document
                        .relations
                        .iter()
                        .filter(|relation| {
                            relation.subject == RelationEndpoint::Occurrence(read.id)
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(relations.len(), 1, "{clause}: {name}");
                    assert_eq!(relations[0].predicate, RelationPredicate::RefersTo);
                    assert_eq!(relations[0].object, RelationEndpoint::Entity(parameter.id));
                    assert_eq!(
                        relations[0].evidence_kind,
                        rootlight_ir::EvidenceKind::Derived
                    );
                    assert_eq!(relations[0].evidence.source.as_ref(), Some(&read.source));
                    assert_eq!(
                        relations[0].id,
                        rootlight_ir::derive_relation_record_id(relations[0]).unwrap()
                    );
                    assert!(
                        !document.skipped_regions.iter().any(|gap| {
                            gap.detail == "haskell-binding-target-unavailable"
                                && gap.source.span() == read_span
                        }),
                        "{clause}: {name}"
                    );
                }
            }
        }
        assert_eq!(choose_parameters.len(), 2);
        assert_eq!(resolved_reads.len(), 16);
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Reference
                        && matches!(site.target, OccurrenceTarget::Resolved { .. })
                })
                .count(),
            resolved_reads.len()
        );
        for (clause, occurrences) in [
            (
                "tick operation left right = unknown (left `operation`) (`operation` right)",
                2,
            ),
            ("dispatch operation value = (operation +) value", 1),
        ] {
            let start = fixture.span(clause, 0).start_byte();
            let (head, rhs) = clause.split_once('=').unwrap();
            let formal_offset = head.find("operation").unwrap();
            assert_eq!(head.match_indices("operation").count(), 1);
            let formal_span = SourceSpan::new(
                fixture.snapshot.file(),
                start + u64::try_from(formal_offset).unwrap(),
                start + u64::try_from(formal_offset + "operation".len()).unwrap(),
            )
            .unwrap();
            let parameter = defined_entity(document, formal_span);
            assert_eq!(parameter.kind, EntityKind::Parameter);
            assert_eq!(rhs.match_indices("operation").count(), occurrences);
            for (offset, _) in rhs.match_indices("operation") {
                let span = SourceSpan::new(
                    fixture.snapshot.file(),
                    start + u64::try_from(head.len() + 1 + offset).unwrap(),
                    start + u64::try_from(head.len() + 1 + offset + "operation".len()).unwrap(),
                )
                .unwrap();
                let sites = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.role == OccurrenceRole::Reference && site.source.span() == span
                    })
                    .collect::<Vec<_>>();
                assert_eq!(sites.len(), 1, "{clause}");
                assert!(matches!(
                    sites[0].target,
                    OccurrenceTarget::Unresolved { .. }
                ));
                assert!(document.relations.iter().all(|relation| {
                    relation.subject != RelationEndpoint::Occurrence(sites[0].id)
                }));
                assert!(document.skipped_regions.iter().any(|gap| {
                    gap.detail == "haskell-binding-target-unavailable" && gap.source.span() == span
                }));
            }
        }
        let missing_span = fixture.span("missing", 0);
        let missing = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference && site.source.span() == missing_span
            })
            .collect::<Vec<_>>();
        assert_eq!(missing.len(), 1);
        assert!(matches!(
            missing[0].target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.detail == "haskell-binding-target-unavailable" && gap.source.span() == missing_span
        }));
        let unknown = document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::CallSite)
            .collect::<Vec<_>>();
        assert!(!unknown.is_empty());
        assert!(
            unknown
                .iter()
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
        );
        assert!(
            unknown
                .iter()
                .any(|site| site.source.span() == fixture.span("unknown (value *)", 0))
        );
        assert!(
            document
                .relations
                .iter()
                .all(|relation| relation.predicate != RelationPredicate::Calls)
        );
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|relation| relation.predicate == RelationPredicate::RefersTo)
                .count(),
            resolved_reads.len()
        );
    }
}

#[test]
fn operator_section_read_paths_keep_unsupported_operands_and_dispatch_unresolved() {
    let cases: &[(&str, &[(&str, &str)])] = &[
        ("blocked x = unknown (x Module.+)\n", &[]),
        ("blocked x = unknown (Module.+ x)\n", &[]),
        ("blocked x = unknown (x :)\n", &[]),
        ("blocked x = unknown (: x)\n", &[]),
        ("blocked x = unknown (x `Module.combine`)\n", &[]),
        ("blocked x = unknown (`Module.combine` x)\n", &[]),
        ("blocked x = unknown (x `Just`)\n", &[]),
        ("blocked x = unknown (`Just` x)\n", &[]),
        ("blocked x = unknown ((let x = 1 in x) +)\n", &[]),
        ("blocked x = unknown (+ (\\x -> x))\n", &[]),
        (
            "blocked x = unknown (x +) (let x = 1 in x)\n",
            &[("x", "x +")],
        ),
    ];
    for newline in ["\n", "\r\n"] {
        for &(body, reads) in cases {
            let body = body.replace('\n', newline);
            let fixture = Fixture::new(format!("module NegativeSections where{newline}{body}"));
            let output = fixture.analyze();
            assert_exact_clause_reads(&fixture, output.document(), &body, reads);
        }
    }
}

#[test]
fn independent_rhs_paths_resolve_exact_formals_beside_binder_and_dispatch_subtrees() {
    let cases: &[(&str, &[(&str, &str)])] = &[
        ("mixed x = combine x (let x = 1 in x)\n", &[("x", "x (let")]),
        ("mixed x = combine (let x = 1 in x) x\n", &[("x", "x\n")]),
        ("mixed x = combine x (\\x -> x)\n", &[("x", "x (\\")]),
        ("mixed x = combine (\\x -> x) x\n", &[("x", "x\n")]),
        (
            "mixed x = combine x (case input of { Just x -> x })\n",
            &[("x", "x (case")],
        ),
        (
            "mixed x = combine (case input of { Just x -> x }) x\n",
            &[("x", "x\n")],
        ),
        (
            "mixed x = combine x (do { x <- source; pure x })\n",
            &[("x", "x (do")],
        ),
        (
            "mixed x = combine (do { x <- source; pure x }) x\n",
            &[("x", "x\n")],
        ),
        ("mixed x = combine x Module.x\n", &[("x", "x Module")]),
        ("mixed x = combine Module.x x\n", &[("x", "x\n")]),
        (
            "mixed x = combine x (Just x)\n",
            &[("x", "x (Just"), ("x", "x)\n")],
        ),
        (
            "mixed x = combine (Just x) x\n",
            &[("x", "x) x"), ("x", "x\n")],
        ),
        ("mixed x = Module.combine x\n", &[("x", "x\n")]),
        ("mixed x = x + (let x = 1 in x)\n", &[("x", "x +")]),
        ("mixed x = (let x = 1 in x) + x\n", &[("x", "x\n")]),
        (
            "mixed x = combine (x +) (let x = 1 in x)\n",
            &[("x", "x +")],
        ),
        (
            "mixed x = combine (let x = 1 in x) ($ x)\n",
            &[("x", "x)\n")],
        ),
        (
            "dispatch operation x = operation x (let x = 1 in x)\n",
            &[("x", "x (let")],
        ),
        (
            "dispatch operation x = x `operation` (let x = 1 in x)\n",
            &[("x", "x `")],
        ),
        (
            "dispatch operation x = combine (x `operation`) (let x = 1 in x)\n",
            &[("x", "x `")],
        ),
        ("qualified x = x Module.+ (let x = 1 in x)\n", &[]),
        ("constructor x = x : (let x = 1 in x)\n", &[]),
        ("duplicate x x = combine x (let y = 1 in y)\n", &[]),
        ("guarded x | True = combine x (let y = 1 in y)\n", &[]),
        ("trailing x = combine x (\\y -> y) where\n  x = 1\n", &[]),
        ("patterned (Just x) = combine x (\\y -> y)\n", &[]),
    ];
    let witnesses: &[(&str, &str, &str)] = &[
        ("mixed x = combine x (let x = 1 in x)\n", "x", "x)\n"),
        ("mixed x = combine (let x = 1 in x) x\n", "x", "x) x"),
        ("mixed x = combine x (\\x -> x)\n", "x", "x)\n"),
        ("mixed x = combine (\\x -> x) x\n", "x", "x) x"),
        (
            "mixed x = combine x (case input of { Just x -> x })\n",
            "x",
            "x })",
        ),
        (
            "mixed x = combine (case input of { Just x -> x }) x\n",
            "x",
            "x })",
        ),
        (
            "mixed x = combine x (do { x <- source; pure x })\n",
            "x",
            "x })",
        ),
        (
            "mixed x = combine (do { x <- source; pure x }) x\n",
            "x",
            "x })",
        ),
        ("mixed x = combine x Module.x\n", "Module.x", "Module.x\n"),
        ("mixed x = combine x (Just x)\n", "Just", "Just x)"),
        (
            "mixed x = Module.combine x\n",
            "Module.combine",
            "Module.combine x",
        ),
        (
            "dispatch operation x = operation x (let x = 1 in x)\n",
            "operation",
            "operation x (let",
        ),
        (
            "dispatch operation x = x `operation` (let x = 1 in x)\n",
            "operation",
            "operation`",
        ),
        (
            "dispatch operation x = combine (x `operation`) (let x = 1 in x)\n",
            "operation",
            "operation`)",
        ),
    ];
    let mut checked_witnesses = 0;
    for newline in ["\n", "\r\n"] {
        for &(body, reads) in cases {
            let authored_body = body;
            let body = body.replace('\n', newline);
            let fixture = Fixture::new(format!("module Independent where{newline}{body}"));
            let output = fixture.analyze();
            let document = output.document();
            assert_exact_clause_reads(&fixture, document, &body, reads);
            for &(witness_body, name, anchor) in witnesses {
                if witness_body != authored_body {
                    continue;
                }
                checked_witnesses += 1;
                let span = fixture.head(&anchor.replace('\n', newline), 0, name);
                let sites = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.role == OccurrenceRole::Reference && site.source.span() == span
                    })
                    .collect::<Vec<_>>();
                assert_eq!(sites.len(), 1, "{body}: {anchor}");
                assert!(matches!(
                    sites[0].target,
                    OccurrenceTarget::Unresolved { .. }
                ));
                assert_eq!(sites[0].source.generation(), fixture.source.generation());
                assert_eq!(
                    sites[0].source.content_hash(),
                    fixture.source.content_hash()
                );
                assert!(
                    document.skipped_regions.iter().any(|gap| {
                        gap.detail == "haskell-binding-target-unavailable"
                            && gap.source.span() == span
                    }),
                    "{body}: {anchor}"
                );
            }
        }
    }
    assert_eq!(checked_witnesses, witnesses.len() * 2);
}

#[test]
fn body_only_admission_changes_preserve_function_and_clause_formal_identities() {
    for newline in ["\n", "\r\n"] {
        let mut original = None;
        for body in [
            "x",
            "unknown y x",
            "if x then y else x",
            "x + 1",
            "x `unknown` y",
            "(x +)",
            "($ y)",
            "unknown (x +) ($ y)",
            "let z = x in z",
            "unknown x (let x = 1 in x)",
            "unknown (let x = 1 in x) x",
            "unknown x (\\x -> x)",
            "unknown (\\x -> x) x",
            "unknown x (case input of { Just x -> x })",
            "unknown (do { x <- source; pure x }) x",
            "if x then y else (let y = 1 in y)",
            "x + (let x = 1 in x)",
            "unknown (x +) (let x = 1 in x)",
        ] {
            let fixture = Fixture::new(
                format!("module Stable where\npick x y = {body}\n").replace('\n', newline),
            );
            let output = fixture.analyze();
            let document = output.document();
            let function = defined_entity(document, fixture.span("pick", 0));
            assert_eq!(function.kind, EntityKind::Function);
            let mut identities =
                vec![(function.kind, function.canonical_name.clone(), function.id)];
            for name in ["x", "y"] {
                let parameter = defined_entity(document, fixture.span(name, 0));
                assert_eq!(parameter.kind, EntityKind::Parameter);
                assert_eq!(parameter.container, Some(ContainerRef::Entity(function.id)));
                identities.push((
                    parameter.kind,
                    parameter.canonical_name.clone(),
                    parameter.id,
                ));
            }
            assert_eq!(
                original.get_or_insert_with(|| identities.clone()),
                &identities
            );
        }
    }
}
