//! Haskell module membership uses the explicit header and exact source identities.
//! The original corpus witness keeps datatype, constructor and field ownership
//! separate; annotations and equal names in another file cannot cross owners.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ContainerRef, EntityKind, EntityRecord, ExtensionSupport,
    IrLimits, NormalizedIrDocument, OccurrenceRole, OccurrenceTarget, ProducerIdentity,
    RelationEndpoint, RelationPredicate, SourceRef, SourceSpan, derive_relation_record_id,
    validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::{TempDir, tempdir};

const ORIGINAL: &str = include_str!("fixtures/haskell_module_ownership/Types.hs");
const ORIGINAL_PATH: &str = "src/library/PostgREST/Auth/Types.hs";

struct Fixture {
    _directory: TempDir,
    root: RepositoryRoot,
}

impl Fixture {
    fn new(sources: &[(&str, &str)]) -> Self {
        let directory = tempdir().unwrap();
        for (path, text) in sources {
            let destination = directory.path().join(path);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::write(destination, text.as_bytes()).unwrap();
        }
        let repository = derive_repository(b"haskell-explicit-module-ownership-contract").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        Self {
            _directory: directory,
            root,
        }
    }

    fn analyze(&self, path: &str) -> AnalysisOutput {
        self.analyze_options(path, IrLimits::default(), &Cancellation::new())
            .unwrap()
    }

    fn analyze_options(
        &self,
        path: &str,
        ir: IrLimits,
        cancellation: &Cancellation,
    ) -> Result<AnalysisOutput, rootlight_adapter_sdk::AdapterError> {
        let snapshot = self
            .root
            .snapshot(&RelativePath::parse(Path::new(path)).unwrap(), 16_384)
            .unwrap();
        let source = SourceRef::new(
            self.root.repository(),
            GenerationId::from_bytes([21; 20]),
            SourceSpan::new(
                snapshot.file(),
                0,
                u64::try_from(snapshot.content().len()).unwrap(),
            )
            .unwrap(),
            snapshot.content_hash(),
            None,
        );
        let provider = Arc::new(
            TreeSitterProvider::new(
                RuntimeConfig::new(
                    16_384,
                    16_384,
                    128,
                    32,
                    64,
                    1,
                    16 * 1024 * 1024,
                    ParserSettings::new(512).unwrap(),
                )
                .unwrap(),
            )
            .unwrap(),
        );
        let analyzer = TreeSitterAnalyzer::new(
            provider,
            ProducerIdentity::new(
                "haskell-module-contract",
                "1.0",
                content_hash(b"haskell-module-configuration"),
            )
            .unwrap(),
            LanguageId::new("haskell").unwrap(),
            "haskell-written-module",
            content_hash(b"haskell-module-contract-binary"),
        )
        .unwrap();
        let stream = StreamLimits::new(
            128,
            4096,
            16 * 1024 * 1024,
            128,
            128 * 1024,
            1024 * 1024,
            BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap(),
        )
        .unwrap();
        let limits = AnalysisLimits::new(
            16_384,
            16_384,
            128,
            32,
            16 * 1024 * 1024,
            stream.clone(),
            stream,
            ir,
        )
        .unwrap();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
            LanguageId::new("haskell").unwrap(),
            EncodingId::utf8(),
            Vec::new(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"haskell-module-build-context")),
            &limits,
        )
        .unwrap()
        .with_generated_status(false);
        let output = execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            cancellation,
        )?;
        validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
        Ok(output)
    }
}

fn definition(document: &NormalizedIrDocument, start: u64, end: u64) -> &EntityRecord {
    let occurrences: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(occurrences.len(), 1, "exact definition at {start}..{end}");
    let OccurrenceTarget::Resolved { symbol } = &occurrences[0].target else {
        panic!("definition has no exact entity");
    };
    document
        .entities
        .iter()
        .find(|entity| entity.id == *symbol)
        .unwrap()
}

fn named_definition<'a>(
    document: &'a NormalizedIrDocument,
    text: &str,
    needle: &str,
    ordinal: usize,
) -> &'a EntityRecord {
    let begin = text.match_indices(needle).nth(ordinal).unwrap().0;
    definition(
        document,
        u64::try_from(begin).unwrap(),
        u64::try_from(begin + needle.len()).unwrap(),
    )
}

fn has_edge(document: &NormalizedIrDocument, owner: &EntityRecord, member: &EntityRecord) -> bool {
    document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Contains
            && edge.subject == RelationEndpoint::Entity(owner.id)
            && edge.object == RelationEndpoint::Entity(member.id)
    })
}

fn assert_edge(document: &NormalizedIrDocument, owner: &EntityRecord, member: &EntityRecord) {
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(owner.id)
                && edge.object == RelationEndpoint::Entity(member.id)
        })
        .collect();
    assert_eq!(edges.len(), 1);
    let edge = edges[0];
    assert_eq!(member.container, Some(ContainerRef::Entity(owner.id)));
    assert_eq!(edge.id, derive_relation_record_id(edge).unwrap());
    assert_eq!(edge.evidence.source, member.evidence.source);
}

#[test]
fn original_explicit_module_owns_datatype_without_collapsing_constructor_or_record_fields() {
    assert_eq!(ORIGINAL.len(), 403);
    assert_eq!(&ORIGINAL.as_bytes()[7..27], b"PostgREST.Auth.Types");
    assert_eq!(&ORIGINAL.as_bytes()[33..43], b"AuthResult");
    let fixture = Fixture::new(&[(ORIGINAL_PATH, ORIGINAL)]);
    let output = fixture.analyze(ORIGINAL_PATH);
    let document = output.document();
    let module = definition(document, 7, 27);
    let datatype = definition(document, 300, 310);
    let constructor = definition(document, 313, 323);
    let claims = definition(document, 329, 339);
    let role = definition(document, 369, 377);
    assert_eq!(module.kind, EntityKind::Module);
    assert_eq!(datatype.kind, EntityKind::Enum);
    assert_eq!(constructor.kind, EntityKind::Constructor);
    assert_eq!(claims.kind, EntityKind::Field);
    assert_eq!(role.kind, EntityKind::Field);
    let header = module.evidence.source.as_ref().unwrap();
    assert_eq!(
        (header.span().start_byte(), header.span().end_byte()),
        (0, 59)
    );
    assert_eq!(header.content_hash(), content_hash(ORIGINAL.as_bytes()));
    assert_edge(document, module, datatype);
    assert_edge(document, datatype, constructor);
    assert_edge(document, constructor, claims);
    assert_edge(document, constructor, role);
    assert_ne!(datatype.id, constructor.id);
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 33
                && site.source.span().end_byte() == 43)
    );
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "haskell-module-owner-unavailable")
    );
}

#[test]
fn identical_module_and_member_names_in_another_file_keep_distinct_owners() {
    let other_path = "other/PostgREST/Auth/Types.hs";
    let fixture = Fixture::new(&[(ORIGINAL_PATH, ORIGINAL), (other_path, ORIGINAL)]);
    let first = fixture.analyze(ORIGINAL_PATH);
    let second = fixture.analyze(other_path);
    let first_module = definition(first.document(), 7, 27);
    let second_module = definition(second.document(), 7, 27);
    let first_datatype = definition(first.document(), 300, 310);
    let second_datatype = definition(second.document(), 300, 310);
    assert_ne!(first_module.id, second_module.id);
    assert_ne!(first_datatype.id, second_datatype.id);
    assert_ne!(
        first_module.evidence.source.as_ref().unwrap().span().file(),
        second_module
            .evidence
            .source
            .as_ref()
            .unwrap()
            .span()
            .file()
    );
    assert_edge(first.document(), first_module, first_datatype);
    assert_edge(second.document(), second_module, second_datatype);
    assert!(!has_edge(first.document(), first_module, second_datatype));
    assert!(!has_edge(second.document(), second_module, first_datatype));
}

#[test]
fn module_annotations_associate_with_top_level_equations_in_lf_and_crlf_sources() {
    for newline in ["\n", "\r\n"] {
        let text = concat!(
            "module Example.Deep where\n",
            "data Value = Value { field :: Int }\n",
            "evaluate :: Value -> Int\n",
            "evaluate value = local\n",
            "  where local = field value\n"
        )
        .replace('\n', newline);
        let fixture = Fixture::new(&[("Example.hs", &text)]);
        let output = fixture.analyze("Example.hs");
        let document = output.document();
        let module = named_definition(document, &text, "Example.Deep", 0);
        let datatype = named_definition(document, &text, "Value", 0);
        let constructor = named_definition(document, &text, "Value", 1);
        let field = named_definition(document, &text, "field", 0);
        let function = named_definition(document, &text, "evaluate", 1);
        let local = named_definition(document, &text, "local", 1);
        let annotation_start = u64::try_from(text.find("evaluate").unwrap()).unwrap();
        let annotations: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Declaration
                    && site.source.span().start_byte() == annotation_start
            })
            .collect();
        assert_eq!(annotations.len(), 1);
        assert_eq!(
            annotations[0].target,
            OccurrenceTarget::Resolved {
                symbol: function.id
            }
        );
        assert_eq!(annotations[0].enclosing, Some(module.id));
        assert_eq!(
            module.evidence.source.as_ref().unwrap().span().end_byte(),
            u64::try_from("module Example.Deep where".len()).unwrap()
        );
        assert_edge(document, module, datatype);
        assert_edge(document, module, function);
        assert_edge(document, datatype, constructor);
        assert_edge(document, constructor, field);
        assert_ne!(local.container, Some(ContainerRef::Entity(module.id)));
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "haskell-annotation-owner-unavailable"
                    && gap.source.span().start_byte() == annotation_start)
        );
    }
}

#[test]
fn absent_explicit_header_retains_implicit_file_ownership() {
    let text = "data Value = Value { field :: Int }\nidentity value = value\n";
    let fixture = Fixture::new(&[("Implicit.hs", text)]);
    let output = fixture.analyze("Implicit.hs");
    let document = output.document();
    let datatype = named_definition(document, text, "Value", 0);
    let constructor = named_definition(document, text, "Value", 1);
    assert_edge(document, datatype, constructor);
    let Some(ContainerRef::Entity(owner)) = datatype.container else {
        panic!("implicit file module is missing");
    };
    let module = document
        .entities
        .iter()
        .find(|entity| entity.id == owner)
        .unwrap();
    assert_eq!(module.kind, EntityKind::Module);
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.target == OccurrenceTarget::Resolved { symbol: module.id })
    );
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "haskell-module-owner-unavailable")
    );
}

#[test]
fn syntax_recovery_retains_definitions_without_certifying_explicit_module_membership() {
    let text = "module Example.Deep where\ndata Value = Value\nbroken = (\n";
    let fixture = Fixture::new(&[("Recovery.hs", text)]);
    let output = fixture.analyze("Recovery.hs");
    let document = output.document();
    let module = named_definition(document, text, "Example.Deep", 0);
    let datatype = named_definition(document, text, "Value", 0);
    let constructor = named_definition(document, text, "Value", 1);
    assert!(!has_edge(document, module, datatype));
    assert_edge(document, datatype, constructor);
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| { gap.reason == rootlight_ir::SkippedRegionReason::ParseError })
    );
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.detail == "haskell-module-owner-unavailable"
            && gap.source.span() == datatype.evidence.source.as_ref().unwrap().span()
    }));
}

fn assert_projected_field(
    document: &NormalizedIrDocument,
    datatype: &EntityRecord,
    constructor: &EntityRecord,
    field: &EntityRecord,
) {
    assert_edge(document, datatype, constructor);
    assert_edge(document, constructor, field);
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(datatype.id)
                && edge.object == RelationEndpoint::Entity(field.id)
        })
        .collect();
    assert_eq!(edges.len(), 1, "written datatype-to-field membership");
    let edge = edges[0];
    assert_eq!(edge.id, derive_relation_record_id(edge).unwrap());
    assert_eq!(edge.evidence.source, field.evidence.source);
    assert_eq!(edge.provenance, field.provenance);
    assert_eq!(edge.evidence_kind, rootlight_ir::EvidenceKind::Derived);
    let mut proof = vec![
        rootlight_ir::FactRef::Entity(datatype.id),
        rootlight_ir::FactRef::Entity(constructor.id),
        rootlight_ir::FactRef::Entity(field.id),
    ];
    proof.sort_unstable();
    assert_eq!(edge.evidence.derivation, proof);
    assert_eq!(field.container, Some(ContainerRef::Entity(constructor.id)));
}

#[test]
fn original_record_fields_add_datatype_membership_without_reparenting() {
    assert_eq!(ORIGINAL.len(), 403);
    let fixture = Fixture::new(&[(ORIGINAL_PATH, ORIGINAL)]);
    let output = fixture.analyze(ORIGINAL_PATH);
    let document = output.document();
    let module = definition(document, 7, 27);
    let datatype = definition(document, 300, 310);
    let constructor = definition(document, 313, 323);
    let claims = definition(document, 329, 339);
    let role = definition(document, 369, 377);
    assert_eq!(datatype.kind, EntityKind::Enum);
    assert_edge(document, module, datatype);
    assert_projected_field(document, datatype, constructor, claims);
    assert_projected_field(document, datatype, constructor, role);
    for member in [datatype, constructor, claims, role] {
        assert_eq!(
            member.evidence.source.as_ref().unwrap().content_hash(),
            content_hash(ORIGINAL.as_bytes())
        );
    }
    assert_eq!(
        module.evidence.source.as_ref().unwrap().span().end_byte(),
        59
    );
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 33
                && site.source.span().end_byte() == 43)
    );
}

#[test]
fn repeated_record_names_follow_each_written_constructor_and_datatype_in_lf_and_crlf() {
    for newline in ["\n", "\r\n"] {
        let text = concat!(
            "{-# LANGUAGE DuplicateRecordFields #-}\nmodule Records where\n",
            "data First = LeftRecord { shared :: Int, leftOnly :: Int }\n",
            "           | RightRecord { shared :: Int, rightOnly :: Int }\n",
            "data Second = OtherRecord { shared :: Int }\n"
        )
        .replace('\n', newline);
        let fixture = Fixture::new(&[("Records.hs", &text), ("other/Records.hs", &text)]);
        let first = fixture.analyze("Records.hs");
        let second = fixture.analyze("other/Records.hs");
        for output in [&first, &second] {
            let document = output.document();
            let datatype = named_definition(document, &text, "First", 0);
            let sibling = named_definition(document, &text, "Second", 0);
            for (ctor_name, ordinal) in [("LeftRecord", 0), ("RightRecord", 1)] {
                let constructor = named_definition(document, &text, ctor_name, 0);
                let field = named_definition(document, &text, "shared", ordinal);
                assert_projected_field(document, datatype, constructor, field);
                assert!(!has_edge(document, sibling, field));
            }
            let constructor = named_definition(document, &text, "OtherRecord", 0);
            let field = named_definition(document, &text, "shared", 2);
            assert_projected_field(document, sibling, constructor, field);
            assert!(!has_edge(document, datatype, field));
        }
        assert_ne!(
            named_definition(first.document(), &text, "shared", 0).id,
            named_definition(second.document(), &text, "shared", 0).id
        );
        assert!(!has_edge(
            first.document(),
            named_definition(first.document(), &text, "First", 0),
            named_definition(second.document(), &text, "shared", 0)
        ));
    }
}

#[test]
fn newtype_gadt_and_recovered_frames_do_not_borrow_ordinary_record_membership() {
    for (text, datatype_name, field_name) in [
        (
            "newtype Wrapped = Wrapped { wrapped :: Int }\n",
            "Wrapped",
            "wrapped",
        ),
        (
            "{-# LANGUAGE GADTs #-}\ndata General where G :: { gadtField :: Int } -> General\n",
            "General",
            "gadtField",
        ),
        (
            "data Item = Item { field :: Int }\nbroken = (\n",
            "Item",
            "field",
        ),
    ] {
        let fixture = Fixture::new(&[("Barrier.hs", text)]);
        let output = fixture.analyze("Barrier.hs");
        let document = output.document();
        let datatype = named_definition(document, text, datatype_name, 0);
        let field = named_definition(document, text, field_name, 0);
        assert!(!has_edge(document, datatype, field));
        assert!(matches!(field.container, Some(ContainerRef::Entity(_))));
        if text.contains("broken") {
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.reason == rootlight_ir::SkippedRegionReason::ParseError)
            );
        }
    }
}

#[test]
fn record_membership_observes_actual_relation_quota_and_cooperative_cancellation() {
    let fixture = Fixture::new(&[(ORIGINAL_PATH, ORIGINAL)]);
    let output = fixture.analyze(ORIGINAL_PATH);
    let mut ir = IrLimits::default();
    ir.max_relations = output.document().relations.len().checked_sub(1).unwrap();
    assert!(
        fixture
            .analyze_options(ORIGINAL_PATH, ir, &Cancellation::new())
            .is_err()
    );
    let cancellation = Cancellation::new();
    cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    assert!(
        fixture
            .analyze_options(ORIGINAL_PATH, IrLimits::default(), &cancellation)
            .is_err()
    );
}
