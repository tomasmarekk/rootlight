//! Written Kotlin declaration names must survive native capture and IR lowering.
//! The original fixture separates source-name evidence from calls and strings.

use std::{collections::HashMap, fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId, GenerationBoundSnapshot,
    IncludedRange, LanguageId, MemoryAdmissionPolicy, ParseOutput, ParseProvider, StreamLimits,
    SyntaxFact, SyntaxFactKind, execute_analysis, execute_parse, testkit::MockParseProvider,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, SymbolId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ContainerRef, EntityKind, ExtensionSupport, IrLimits,
    NormalizedIrDocument, OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint,
    RelationPredicate, SourceRef, SourceSpan, derive_relation_record_id, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};

const ORIGINAL: &[u8] =
    include_bytes!("fixtures/kotlin_original_declarations/SlideInRightAnimation.kt");

fn native_definition(facts: &[SyntaxFact], start: u64, end: u64, declaration: &str) {
    let names: Vec<_> = facts
        .iter()
        .filter(|fact| {
            fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "kotlin.identifier.definition"
                && fact.span().start_byte() == start
                && fact.span().end_byte() == end
        })
        .collect();
    assert_eq!(
        names.len(),
        1,
        "exact native declaration name {start}..{end}"
    );
    let by_id: HashMap<_, _> = facts.iter().map(|fact| (fact.local_id(), fact)).collect();
    let mut next = names[0].parent();
    let mut owner = None;
    for _ in 0..facts.len() {
        let Some(parent) = next.and_then(|parent| by_id.get(&parent).copied()) else {
            break;
        };
        if parent.kind() == SyntaxFactKind::Declaration {
            owner = Some(parent);
            break;
        }
        next = parent.parent();
    }
    let owner = owner.expect("a named native leaf needs its written declaration owner");
    assert_eq!(owner.syntax_kind().as_str(), declaration);
    assert!(owner.span().start_byte() <= start && owner.span().end_byte() >= end);
}

fn definition(
    document: &NormalizedIrDocument,
    source_bytes: &[u8],
    start: u64,
    end: u64,
    name: &str,
    kind: EntityKind,
) -> SymbolId {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "exact lowered definition {name}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("definition must resolve to its own source entity")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.canonical_name, name);
    assert_eq!(sites[0].source.content_hash(), content_hash(source_bytes));
    assert_eq!(sites[0].syntactic_text_hash, content_hash(name.as_bytes()));
    symbol
}

fn analyze(source_bytes: &[u8]) -> (ParseOutput, NormalizedIrDocument) {
    analyze_input(source_bytes, EncodingId::utf8(), &[], None)
}

fn analyze_input(
    source_bytes: &[u8],
    encoding: EncodingId,
    selected: &[(u64, u64)],
    replay: Option<Vec<SyntaxFact>>,
) -> (ParseOutput, NormalizedIrDocument) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("Source.kt"), source_bytes).unwrap();
    let repository = derive_repository(b"kotlin-original-declaration-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("Source.kt")).unwrap(),
            1024 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([9; 20]),
        SourceSpan::new(
            snapshot.file(),
            0,
            u64::try_from(source_bytes.len()).unwrap(),
        )
        .unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = Arc::new(
        TreeSitterProvider::new(
            RuntimeConfig::new(
                1024 * 1024,
                16_384,
                128,
                16,
                64,
                1,
                16 * 1024 * 1024,
                ParserSettings::new(256).unwrap(),
            )
            .unwrap(),
        )
        .unwrap(),
    );
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
        1024 * 1024,
        16_384,
        128,
        16,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("kotlin").unwrap(),
        encoding,
        selected
            .iter()
            .map(|&(start, end)| {
                IncludedRange::new(
                    SourceSpan::new(snapshot.file(), start, end).unwrap(),
                    LanguageId::new("kotlin").unwrap(),
                )
            })
            .collect(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"kotlin-original-contract-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let cancellation = Cancellation::new();
    let parsed = execute_parse(
        provider.as_ref(),
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &cancellation,
    )
    .unwrap();
    // Only bounded fact identities and coordinates are printed, not source bodies.
    // This distinguishes a missing native name from failed lowerer association.
    for fact in parsed.facts() {
        eprintln!(
            "native fact local={} parent={:?} depth={} kind={:?} span={:?} syntax={}",
            fact.local_id(),
            fact.parent(),
            fact.depth(),
            fact.kind(),
            fact.span(),
            fact.syntax_kind().as_str()
        );
    }
    let parser: Arc<dyn ParseProvider> = match replay {
        Some(facts) => Arc::new(MockParseProvider::new(
            provider.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        )),
        None => provider,
    };
    let analyzer = TreeSitterAnalyzer::new(
        parser,
        ProducerIdentity::new(
            "kotlin-original-contract",
            "1.0",
            content_hash(b"configuration"),
        )
        .unwrap(),
        LanguageId::new("kotlin").unwrap(),
        "kotlin-original-source-contract",
        content_hash(b"kotlin-original-contract-binary"),
    )
    .unwrap();
    let output = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &cancellation,
    )
    .unwrap();
    validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    eprintln!(
        "lowered entity count={} definition count={} gaps={:?}",
        output.document().entities.len(),
        output
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Definition)
            .count(),
        output.document().skipped_regions
    );
    let (document, _, _) = output.into_parts();
    (parsed, document)
}

#[test]
fn kotlin_package_keeps_the_full_qualified_definition_and_exact_source() {
    let raw = b"// package fake.comment\r\npackage com.facebook.react.model\r\nclass Model\r\nval text = \"package fake.string\"\r\n";
    let (parsed, document) = analyze(raw);
    assert!(parsed.diagnostics().is_empty());
    let package = definition(
        &document,
        raw,
        33,
        57,
        "com.facebook.react.model",
        EntityKind::Module,
    );
    let site = document
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Definition
                && site.target == OccurrenceTarget::Resolved { symbol: package }
        })
        .unwrap();
    assert_eq!(site.source.span().file(), document.files[0].id);
    assert_eq!(site.source.generation(), document.generation);
    assert_eq!(site.source.content_hash(), content_hash(raw));
    assert!(!document.entities.iter().any(|entity| matches!(
        entity.canonical_name.as_str(),
        "com" | "facebook" | "react" | "model" | "fake.comment" | "fake.string"
    )));
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.detail == "declaration-name-unavailable"
            && gap.source.span().start_byte() == 25
            && gap.source.span().end_byte() == 57
    }));
}

#[test]
fn kotlin_package_does_not_define_names_from_malformed_headers_or_nested_contexts() {
    for raw in [
        b"package\n".as_slice(),
        b"package com..facebook\nclass Model\n".as_slice(),
        b"package com.\nclass Model\n".as_slice(),
        b"package .com\nclass Model\n".as_slice(),
        b"class Model { package fake.nested }\n".as_slice(),
    ] {
        let (parsed, document) = analyze(raw);
        assert!(
            !parsed.diagnostics().is_empty()
                || document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail == "declaration-name-unavailable"),
            "malformed header must retain recovery or a naming refusal: {raw:?}"
        );
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Module),
            "malformed package input {raw:?}"
        );
    }
}

#[test]
fn kotlin_package_keeps_a_complete_header_before_unrelated_recovery() {
    let raw = b"package com.facebook.react.model\nfun broken(\n";
    let (parsed, document) = analyze(raw);
    assert!(!parsed.diagnostics().is_empty());
    definition(
        &document,
        raw,
        8,
        32,
        "com.facebook.react.model",
        EntityKind::Module,
    );
}

#[test]
fn original_kotlin_class_and_override_method_keep_names_and_containment() {
    assert_eq!(ORIGINAL.len(), 835);
    assert_eq!(&ORIGINAL[390..411], b"SlideInRightAnimation");
    assert_eq!(&ORIGINAL[580..588], b"animator");
    assert_eq!(&ORIGINAL[667..679], b"translationX");
    let (parsed, document) = analyze(ORIGINAL);
    let repository = derive_repository(b"kotlin-original-declaration-contract").id();
    native_definition(parsed.facts(), 390, 411, "kotlin.class.declaration");
    native_definition(parsed.facts(), 580, 588, "kotlin.function.declaration");
    native_definition(parsed.facts(), 589, 593, "kotlin.parameter.declaration");
    let document = &document;
    let class = definition(
        document,
        ORIGINAL,
        390,
        411,
        "SlideInRightAnimation",
        EntityKind::Class,
    );
    let method = definition(document, ORIGINAL, 580, 588, "animator", EntityKind::Method);
    let parameter = definition(document, ORIGINAL, 589, 593, "view", EntityKind::Parameter);
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(method)
                    && edge.object == RelationEndpoint::Entity(parameter)
            })
            .count(),
        1
    );
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(class)
                && edge.object == RelationEndpoint::Entity(method)
        })
        .collect();
    assert_eq!(edges.len(), 1, "written class contains its override method");
    assert_eq!(edges[0].id, derive_relation_record_id(edges[0]).unwrap());
    let witness = edges[0].evidence.source.as_ref().unwrap();
    assert_eq!(witness.repository(), repository);
    assert_eq!(witness.generation(), GenerationId::from_bytes([9; 20]));
    assert_eq!(witness.content_hash(), content_hash(ORIGINAL));
    assert!(witness.span().start_byte() <= 580 && witness.span().end_byte() >= 588);
    assert!(!document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && site.source.span().start_byte() == 667
            && site.source.span().end_byte() == 679
    }));
}

#[test]
fn ordinary_function_formals_keep_exact_names_and_function_owners() {
    let source = concat!(
        "internal fun zipFiles(destination: File, contents: List<File>) {\r\n",
        "  ZipOutputStream(BufferedOutputStream(FileOutputStream(destination.absolutePath))).use { out ->\r\n",
        "  }\r\n",
        "}\r\n",
        "fun neighboring(destination: Other, note: String = \"destination\", option: String = defaultOption(destination)) {\n",
        "    consume(destination, note, option)\n",
        "    val lambda = { destination: String -> destination }\n",
        "}\n",
        "class Holder(val constructorOnly: String)\n",
    ).as_bytes();
    let (parsed, document) = analyze(source);
    let start_of = |needle: &[u8]| {
        u64::try_from(
            source
                .windows(needle.len())
                .position(|window| window == needle)
                .unwrap(),
        )
        .unwrap()
    };
    let zip_start = start_of(b"zipFiles");
    let zip = definition(
        &document,
        source,
        zip_start,
        zip_start + 8,
        "zipFiles",
        EntityKind::Function,
    );
    let neighboring_start = start_of(b"neighboring");
    let neighboring = definition(
        &document,
        source,
        neighboring_start,
        neighboring_start + 11,
        "neighboring",
        EntityKind::Function,
    );
    let expected = [
        ("destination", b"destination: File".as_slice(), zip),
        ("contents", b"contents: List".as_slice(), zip),
        ("note", b"note: String".as_slice(), neighboring),
        ("destination", b"destination: Other".as_slice(), neighboring),
        ("option", b"option: String".as_slice(), neighboring),
    ];
    for (name, witness, owner) in expected {
        let start = start_of(witness);
        let end = start + u64::try_from(name.len()).unwrap();
        native_definition(parsed.facts(), start, end, "kotlin.parameter.declaration");
        let parameter = definition(&document, source, start, end, name, EntityKind::Parameter);
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == parameter)
            .unwrap();
        assert_eq!(entity.container, Some(ContainerRef::Entity(owner)));
        let definitions: Vec<_> = document.occurrences.iter().filter(|site| {
            site.role == OccurrenceRole::Definition
                && matches!(site.target, OccurrenceTarget::Resolved { symbol } if symbol == parameter)
        }).collect();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].enclosing, Some(owner));
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| {
                    edge.predicate == RelationPredicate::Contains
                        && edge.subject == RelationEndpoint::Entity(owner)
                        && edge.object == RelationEndpoint::Entity(parameter)
                })
                .count(),
            1
        );
        assert!(parsed.facts().iter().any(|fact| {
            fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "kotlin.parameters.signature"
                && fact.span().start_byte() <= start
                && fact.span().end_byte() >= end
        }));
    }
    let parameter = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Parameter
                && entity.canonical_name == "destination"
                && entity.container == Some(ContainerRef::Entity(zip))
        })
        .unwrap();
    let start = start_of(b"destination.absolutePath");
    let references: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == start + 11
        })
        .collect();
    assert_eq!(references.len(), 1);
    let reference = references[0];
    assert_eq!(reference.enclosing, Some(zip));
    assert_eq!(
        reference.target,
        OccurrenceTarget::Resolved {
            symbol: parameter.id
        }
    );
    assert_eq!(reference.source.content_hash(), content_hash(source));
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(reference.id)
                    && edge.object == RelationEndpoint::Entity(parameter.id)
                    && edge.evidence.source.as_ref() == Some(&reference.source)
            })
            .count(),
        1
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        expected.len()
    );
    assert!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .all(|entity| ![
                "File",
                "List",
                "String",
                "Other",
                "defaultOption",
                "constructorOnly"
            ]
            .contains(&entity.canonical_name.as_str()))
    );
}

fn reference_at(
    document: &NormalizedIrDocument,
    start: u64,
    length: u64,
) -> &rootlight_ir::OccurrenceRecord {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == start + length
        })
        .collect();
    assert_eq!(sites.len(), 1, "one exact receiver Reference at {start}");
    sites[0]
}

fn certified_receiver(
    document: &NormalizedIrDocument,
    raw: &[u8],
    parameter_start: u64,
    usage_start: u64,
    owner_name: &str,
) -> SymbolId {
    let parameter = definition(
        document,
        raw,
        parameter_start,
        parameter_start + 4,
        "slot",
        EntityKind::Parameter,
    );
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == parameter)
        .unwrap();
    let Some(ContainerRef::Entity(owner)) = entity.container else {
        panic!("function-owned formal")
    };
    let function = document
        .entities
        .iter()
        .find(|entity| entity.id == owner)
        .unwrap();
    assert_eq!(function.kind, EntityKind::Function);
    assert_eq!(function.canonical_name, owner_name);
    assert_eq!(
        function.container,
        Some(ContainerRef::File(document.files[0].id))
    );
    let site = reference_at(document, usage_start, 4);
    assert_eq!(site.syntax_kind, "kotlin.identifier.reference");
    assert_eq!(site.enclosing, Some(owner));
    assert_eq!(
        site.target,
        OccurrenceTarget::Resolved { symbol: parameter }
    );
    assert_eq!(site.source.content_hash(), content_hash(raw));
    assert_eq!(site.syntactic_text_hash, content_hash(b"slot"));
    assert_eq!(site.provenance, entity.provenance);
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.object == RelationEndpoint::Entity(parameter)
                    && edge.evidence.source.as_ref() == Some(&site.source)
            })
            .count(),
        1
    );
    parameter
}

fn unbound_receiver(document: &NormalizedIrDocument, at: u64) {
    let site = reference_at(document, at, 4);
    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::RefersTo
            && edge.subject == RelationEndpoint::Occurrence(site.id)
    }));
}

#[test]
fn formal_receiver_bindings_keep_exact_owners_and_later_shadow_boundaries() {
    let text = concat!(
        "fun first(slot: File) { consume(slot.path).use { slot -> consume(slot.path) }\n",
        "  val slot = other\n}\n",
        "fun second(slot: Other) { consume(slot.path) }\n",
    );
    let (_, document) = analyze(text.as_bytes());
    let uses: Vec<_> = text
        .match_indices("slot.path")
        .map(|(at, _)| at as u64)
        .collect();
    assert_eq!(uses.len(), 3);
    let first = certified_receiver(
        &document,
        text.as_bytes(),
        text.find("slot: File").unwrap() as u64,
        uses[0],
        "first",
    );
    let second = certified_receiver(
        &document,
        text.as_bytes(),
        text.find("slot: Other").unwrap() as u64,
        uses[2],
        "second",
    );
    assert_ne!(first, second);
    unbound_receiver(&document, uses[1]);
    unbound_receiver(&document, uses[0] + 5);
}

#[test]
fn formal_receivers_preserve_crlf_bom_and_utf16_physical_evidence() {
    let text = "// \u{03bb}\u{1f600}\r\nfun owned(slot: File) {\r\n  consume(slot.path)\r\n}\r\n";
    let parameter = text.find("slot: File").unwrap();
    let usage = text.find("slot.path").unwrap();
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            raw.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let physical = |at| u64::try_from(2 + 2 * text[..at].encode_utf16().count()).unwrap();
        let (_, document) = analyze_input(
            &raw,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
            &[],
            None,
        );
        // The helper's ASCII width is four bytes; UTF-16 has eight physical bytes.
        let symbol = definition(
            &document,
            &raw,
            physical(parameter),
            physical(parameter + 4),
            "slot",
            EntityKind::Parameter,
        );
        let site = reference_at(&document, physical(usage), 8);
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol });
        assert_eq!(site.source.content_hash(), content_hash(&raw));
        assert_eq!(site.syntactic_text_hash, content_hash(b"slot"));
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| {
                    edge.predicate == RelationPredicate::RefersTo
                        && edge.subject == RelationEndpoint::Occurrence(site.id)
                        && edge.object == RelationEndpoint::Entity(symbol)
                        && edge.evidence.source.as_ref() == Some(&site.source)
                })
                .count(),
            1
        );
    }
    let raw = format!("\u{feff}{text}");
    let (_, document) = analyze(raw.as_bytes());
    certified_receiver(
        &document,
        raw.as_bytes(),
        parameter as u64 + 3,
        usage as u64 + 3,
        "owned",
    );
}

#[test]
fn missing_duplicate_or_reparented_native_witnesses_cannot_bind_formals() {
    let text = "fun owned(slot: File) { consume(slot.path) }\n";
    let (parsed, document) = analyze(text.as_bytes());
    let usage = text.find("slot.path").unwrap() as u64;
    certified_receiver(
        &document,
        text.as_bytes(),
        text.find("slot: File").unwrap() as u64,
        usage,
        "owned",
    );
    for (label, start) in [
        ("kotlin.function.declaration", 0),
        (
            "kotlin.identifier.definition",
            text.find("owned").unwrap() as u64,
        ),
        (
            "kotlin.parameters.signature",
            text.find('(').unwrap() as u64,
        ),
        (
            "kotlin.parameter.declaration",
            text.find("slot: File").unwrap() as u64,
        ),
        (
            "kotlin.identifier.definition",
            text.find("slot: File").unwrap() as u64,
        ),
        ("kotlin.block.scope", text.find('{').unwrap() as u64),
    ] {
        let witnesses: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.syntax_kind().as_str() == label && fact.span().start_byte() == start
            })
            .collect();
        assert_eq!(witnesses.len(), 1, "{label}");
        let witness = witnesses[0];
        for variant in 0..3 {
            let mut facts = parsed.facts().to_vec();
            if variant == 0 {
                facts.retain(|fact| fact.local_id() != witness.local_id());
                for fact in &mut facts {
                    if fact.parent() == Some(witness.local_id()) {
                        *fact = SyntaxFact::new(
                            fact.local_id(),
                            witness.parent(),
                            fact.kind(),
                            fact.span(),
                            fact.depth(),
                            fact.syntax_kind().clone(),
                        );
                    }
                }
            } else if variant == 1 {
                let next = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
                facts.push(SyntaxFact::new(
                    next,
                    witness.parent(),
                    witness.kind(),
                    witness.span(),
                    witness.depth(),
                    witness.syntax_kind().clone(),
                ));
            } else {
                let index = facts
                    .iter()
                    .position(|fact| fact.local_id() == witness.local_id())
                    .unwrap();
                facts[index] = SyntaxFact::new(
                    witness.local_id(),
                    None,
                    witness.kind(),
                    witness.span(),
                    witness.depth(),
                    witness.syntax_kind().clone(),
                );
            }
            let (_, document) =
                analyze_input(text.as_bytes(), EncodingId::utf8(), &[], Some(facts));
            unbound_receiver(&document, usage);
        }
    }
    let changed = text.replacen("slot: File", "item: File", 1);
    assert_eq!(changed.len(), text.len());
    let (_, document) = analyze_input(
        changed.as_bytes(),
        EncodingId::utf8(),
        &[],
        Some(parsed.facts().to_vec()),
    );
    unbound_receiver(&document, usage);
}

#[test]
fn unsupported_receiver_scopes_and_selected_views_stay_unbound() {
    for text in [
        "fun owned(slot: File) { consume(other.slot) }",
        "fun owned(slot: File) { consume(0).use { slot -> consume(slot.path) } }",
        "fun owned(slot: File) { val slot = other; consume(slot.path) }",
        "fun owned(slot: File) { val local = consume(slot.path) }",
        "fun File.owned(slot: File) { consume(slot.path) }",
        "fun <T> owned(slot: File) { consume(slot.path) }",
        "fun owned(slot: File = fallback) { consume(slot.path) }",
        "class Holder { fun owned(slot: File) { consume(slot.path) } }",
    ] {
        let (_, document) = analyze(text.as_bytes());
        let usage = text.rfind("slot").unwrap() as u64;
        unbound_receiver(&document, usage);
    }
    let text = "// prefix\nfun owned(slot: File) { consume(slot.path) }\n";
    let usage = text.find("slot.path").unwrap() as u64;
    for range in [(0, text.len() as u64), (10, text.len() as u64)] {
        let (_, document) = analyze_input(text.as_bytes(), EncodingId::utf8(), &[range], None);
        assert!(!document.occurrences.iter().any(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == usage
                && matches!(site.target, OccurrenceTarget::Resolved { .. })
        }));
    }
}
