//! Shared .v suffixes require source evidence at both discovery and admission.
//! Exact retrieval remains available when a file has no admitted parser.

use super::*;
use rootlight_discovery::LanguageSignal;
use std::fs;

#[test]
fn coq_standalone_record_and_instance_commands_keep_source_evidence() {
    for (source, expected) in [
        (
            "Structure RouteRecord : Type := { route_field : nat }.\n",
            Some((
                "RouteRecord",
                EntityKind::Struct,
                OccurrenceRole::Definition,
            )),
        ),
        (
            "Declare Instance route_instance : nat.\n",
            Some((
                "route_instance",
                EntityKind::Constant,
                OccurrenceRole::Declaration,
            )),
        ),
        ("Existing Class Registry.Owner.\n", None),
    ] {
        let fixture = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        fs::write(fixture.path().join("standalone.v"), source).unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let cancellation = Cancellation::new();
        let receipt = service
            .index_repository(fixture.path(), &cancellation)
            .unwrap();
        let document = service
            .inspect_generation(
                receipt.repository,
                Some(receipt.generation),
                &cancellation,
                |snapshot| snapshot.document().clone(),
            )
            .unwrap();
        assert_eq!(document.files.len(), 1);
        let file = &document.files[0];
        assert_eq!(file.language, "coq", "{source}");
        assert_eq!(file.content_hash, content_hash(source.as_bytes()));
        assert_eq!(
            service
                .source_file_coverage_until(receipt.generation, file.id, &cancellation)
                .unwrap()
                .tier,
            AnalysisTier::TierD
        );
        if let Some((name, kind, role)) = expected {
            let entities: Vec<_> = document
                .entities
                .iter()
                .filter(|entity| entity.canonical_name == name && entity.kind == kind)
                .collect();
            assert_eq!(entities.len(), 1, "{source}");
            let sites: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == role
                        && site.target
                            == rootlight_ir::OccurrenceTarget::Resolved {
                                symbol: entities[0].id,
                            }
                })
                .collect();
            assert_eq!(sites.len(), 1);
            let start = u64::try_from(source.find(name).unwrap()).unwrap();
            assert_eq!(sites[0].source.span().start_byte(), start);
            assert_eq!(
                sites[0].source.span().end_byte(),
                start + u64::try_from(name.len()).unwrap()
            );
            let located = service
                .code_locate(
                    receipt.generation,
                    name.to_owned(),
                    LocateMode::Exact,
                    10,
                    0,
                    &cancellation,
                )
                .unwrap();
            assert!(
                located
                    .data
                    .hits
                    .iter()
                    .any(|hit| hit.symbol == Some(entities[0].id))
            );
            assert!(!located.data.truncated);
        } else {
            assert!(!document.entities.iter().any(|entity| matches!(
                entity.kind,
                EntityKind::Class
                    | EntityKind::Struct
                    | EntityKind::Constructor
                    | EntityKind::Field
                    | EntityKind::Constant
            )));
        }
        let reference = SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
            file.content_hash,
            None,
        );
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![reference],
                SourceReadOptions::new()
                    .with_encoding(SourceEncoding::Bytes)
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, source.as_bytes());
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls)
        );
    }
}

#[test]
fn coq_structure_keeps_attribution_literal_matches_and_exact_encoded_reads() {
    let source = concat!(
        "(* λ😀 *)\nDefinition written_value : nat := 1.\nLemma statement : True. Proof. exact I. Qed.\n",
        "Definition chosen := 0.\nDefinition consumer (chosen : nat) : nat := chosen.\n",
        include_str!(
            "../../rootlight-adapter-treesitter/tests/fixtures/coq-instances/subst_instance_list.v"
        ),
        "\nModule Type Contract.\nDeclare Instance promised : nat.\nEnd Contract.\n",
        include_str!(
            "../../rootlight-adapter-treesitter/tests/fixtures/coq-records/binder_annot.v"
        ),
        "\n",
        include_str!("../../rootlight-adapter-treesitter/tests/fixtures/coq-records/debug_opt.v"),
        "\n",
        include_str!("../../rootlight-adapter-treesitter/tests/fixtures/coq-records/listable.v"),
        "\n",
        include_str!(
            "../../rootlight-adapter-treesitter/tests/fixtures/coq-projections/singleton.v"
        ),
        "\n",
        include_str!(
            "../../rootlight-adapter-treesitter/tests/fixtures/coq-projections/neighbor.v"
        ),
        "\n",
        include_str!(
            "../../../tests/native-grammars/tests/fixtures/coq-tactic-notations/MRUtils.v"
        ),
        "\n",
    );
    for encoding in ["utf-8", "utf-8-bom", "utf-16le", "utf-16be"] {
        let fixture = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let mut bytes = match encoding {
            "utf-8" => Vec::new(),
            "utf-8-bom" => vec![0xef, 0xbb, 0xbf],
            "utf-16le" => vec![0xff, 0xfe],
            "utf-16be" => vec![0xfe, 0xff],
            _ => unreachable!(),
        };
        if encoding.starts_with("utf-8") {
            bytes.extend(source.as_bytes());
        } else {
            for unit in source.encode_utf16() {
                bytes.extend(if encoding == "utf-16le" {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
        }
        let path = fixture.path().join("proof.v");
        fs::write(&path, &bytes).unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let cancellation = Cancellation::new();
        let receipt = service
            .index_repository(fixture.path(), &cancellation)
            .unwrap();
        let document = service
            .inspect_generation(
                receipt.repository,
                Some(receipt.generation),
                &cancellation,
                |snapshot| snapshot.document().clone(),
            )
            .unwrap();
        assert_eq!(document.files.len(), 1);
        let file = &document.files[0];
        assert_eq!(file.path, "proof.v");
        assert_eq!(file.language, "coq", "{encoding}");
        assert_eq!(
            file.encoding,
            if encoding == "utf-8-bom" {
                "utf-8"
            } else {
                encoding
            }
        );
        assert_eq!(file.content_hash, content_hash(&bytes));
        assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
        let physical_span_at = |start: usize, spelling: &str| {
            let (start, length) = if encoding.starts_with("utf-8") {
                (
                    start + if encoding == "utf-8-bom" { 3 } else { 0 },
                    spelling.len(),
                )
            } else {
                (
                    2 + source[..start].encode_utf16().count() * 2,
                    spelling.encode_utf16().count() * 2,
                )
            };
            (
                u64::try_from(start).unwrap(),
                u64::try_from(start + length).unwrap(),
            )
        };
        let physical_span =
            |spelling: &str| physical_span_at(source.find(spelling).unwrap(), spelling);
        for (name, kind, role, syntax, marker) in [
            (
                "written_value",
                OccurrenceRole::Definition,
                "coq.name.definition",
            ),
            (
                "statement",
                OccurrenceRole::Definition,
                "coq.name.definition",
            ),
            (
                "subst_instance_list",
                OccurrenceRole::Definition,
                "coq.name.definition",
            ),
            ("chosen", OccurrenceRole::Definition, "coq.name.definition"),
            (
                "consumer",
                OccurrenceRole::Definition,
                "coq.name.definition",
            ),
            (
                "promised",
                OccurrenceRole::Declaration,
                "coq.declared_instance_name.definition",
            ),
            (
                "iff_forall",
                OccurrenceRole::Definition,
                "coq.name.definition",
            ),
            ("iff_ex", OccurrenceRole::Definition, "coq.name.definition"),
            (
                "if_true_false",
                OccurrenceRole::Definition,
                "coq.name.definition",
            ),
            (
                "iff_is_true_eq_bool",
                OccurrenceRole::Definition,
                "coq.name.definition",
            ),
            (
                "uip_bool",
                OccurrenceRole::Definition,
                "coq.name.definition",
            ),
        ]
        .into_iter()
        .map(|(name, role, syntax)| (name, EntityKind::Constant, role, syntax, name))
        .chain([
            (
                "binder_annot",
                EntityKind::Struct,
                OccurrenceRole::Definition,
                "coq.name.definition",
                "Record binder_annot",
            ),
            (
                "mkBindAnn",
                EntityKind::Constructor,
                OccurrenceRole::Definition,
                "coq.name.definition",
                ":= mkBindAnn",
            ),
            (
                "binder_name",
                EntityKind::Field,
                OccurrenceRole::Declaration,
                "coq.record_field_name.definition",
                "{ binder_name",
            ),
            (
                "binder_relevance",
                EntityKind::Field,
                OccurrenceRole::Declaration,
                "coq.record_field_name.definition",
                "; binder_relevance",
            ),
            (
                "debug_opt",
                EntityKind::Class,
                OccurrenceRole::Definition,
                "coq.name.definition",
                "Class debug_opt",
            ),
            (
                "debug",
                EntityKind::Field,
                OccurrenceRole::Declaration,
                "coq.class_field_name.definition",
                ":= debug",
            ),
            (
                "Listable",
                EntityKind::Class,
                OccurrenceRole::Definition,
                "coq.name.definition",
                "Class Listable",
            ),
            (
                "list_all",
                EntityKind::Field,
                OccurrenceRole::Declaration,
                "coq.record_field_name.definition",
                "{ list_all",
            ),
            (
                "find_index",
                EntityKind::Field,
                OccurrenceRole::Declaration,
                "coq.record_field_name.definition",
                "; find_index",
            ),
            (
                "cardinality",
                EntityKind::Field,
                OccurrenceRole::Declaration,
                "coq.record_field_name.definition",
                "; cardinality",
            ),
            (
                "t",
                EntityKind::Class,
                OccurrenceRole::Definition,
                "coq.name.definition",
                "Class t",
            ),
            (
                "preserves_obs",
                EntityKind::Field,
                OccurrenceRole::Declaration,
                "coq.class_field_name.definition",
                ":= preserves_obs",
            ),
            (
                "extends_eprogram",
                EntityKind::Constant,
                OccurrenceRole::Definition,
                "coq.name.definition",
                "Definition extends_eprogram",
            ),
        ]) {
            let entities: Vec<_> = document
                .entities
                .iter()
                .filter(|entity| entity.canonical_name == name && entity.kind == kind)
                .collect();
            assert_eq!(entities.len(), 1, "{encoding}: {name}");
            let entity = entities[0];
            assert_eq!(
                document
                    .provenance
                    .iter()
                    .find(|record| record.id == entity.provenance)
                    .unwrap()
                    .tier,
                AnalysisTier::TierD,
            );
            let sites: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.file == file.id
                        && site.role == role
                        && site.target
                            == rootlight_ir::OccurrenceTarget::Resolved { symbol: entity.id }
                })
                .collect();
            assert_eq!(sites.len(), 1, "{encoding}: {name}");
            let site = sites[0];
            let marker_start = source.find(marker).unwrap();
            let (start, end) = physical_span_at(marker_start + marker.find(name).unwrap(), name);
            assert_eq!(site.syntax_kind, syntax);
            assert_eq!(site.source.span().start_byte(), start);
            assert_eq!(site.source.span().end_byte(), end);
            assert_eq!(site.source.repository(), receipt.repository);
            assert_eq!(site.source.generation(), receipt.generation);
            assert_eq!(site.source.content_hash(), file.content_hash);
            assert_eq!(site.syntactic_text_hash, content_hash(name.as_bytes()));
            assert!(!document.occurrences.iter().any(|site| {
                matches!(
                    site.role,
                    OccurrenceRole::Definition | OccurrenceRole::Declaration
                ) && site.role != role
                    && site.target == rootlight_ir::OccurrenceTarget::Resolved { symbol: entity.id }
            }));
        }
        for (child, owner) in [
            ("mkBindAnn", "binder_annot"),
            ("binder_name", "binder_annot"),
            ("binder_relevance", "binder_annot"),
            ("debug", "debug_opt"),
            ("list_all", "Listable"),
            ("find_index", "Listable"),
            ("cardinality", "Listable"),
            ("preserves_obs", "t"),
        ] {
            let entity = |name| {
                document
                    .entities
                    .iter()
                    .find(|entity| entity.canonical_name == name)
                    .unwrap()
            };
            let child = entity(child);
            let owner = entity(owner);
            assert_eq!(
                child.container,
                Some(rootlight_ir::ContainerRef::Entity(owner.id))
            );
            assert!(
                document
                    .relations
                    .iter()
                    .any(|edge| edge.predicate == RelationPredicate::Contains
                        && edge.subject == RelationEndpoint::Entity(owner.id)
                        && edge.object == RelationEndpoint::Entity(child.id))
            );
        }
        assert_eq!(
            document
                .entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Constructor)
                .count(),
            1
        );
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name.starts_with("Build_"))
        );
        let (start, end_with_dot) = physical_span("chosen.");
        let end = end_with_dot
            .checked_sub(if encoding.starts_with("utf-8") { 1 } else { 2 })
            .unwrap();
        let shadowed: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.syntax_kind == "coq.written_reference.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect();
        assert_eq!(shadowed.len(), 1, "{encoding}");
        assert_eq!(
            shadowed[0].target,
            rootlight_ir::OccurrenceTarget::Unresolved {
                text_hash: content_hash(b"chosen"),
            },
            "{encoding}: an unmodeled binder must not become a global-name candidate",
        );
        let projections: Vec<_> = source.match_indices(".(").collect();
        assert_eq!(projections.len(), 4);
        for (offset, _) in projections {
            let start = offset + 2;
            let name = source[start..].split(')').next().unwrap();
            assert!(matches!(name, "pre" | "transform"));
            let (start, end) = physical_span_at(start, name);
            let sites: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.syntax_kind == "coq.written_reference.reference"
                        && site.source.span().start_byte() == start
                        && site.source.span().end_byte() == end
                })
                .collect();
            assert_eq!(sites.len(), 1, "{encoding}: {name}");
            assert_eq!(sites[0].role, OccurrenceRole::Reference);
            assert_eq!(
                sites[0].target,
                rootlight_ir::OccurrenceTarget::Unresolved {
                    text_hash: content_hash(name.as_bytes()),
                },
            );
            assert_eq!(sites[0].source.content_hash(), file.content_hash);
        }
        for name in [
            "t",
            "extends_eprogram",
            "iff_forall",
            "iff_ex",
            "if_true_false",
            "iff_is_true_eq_bool",
            "uip_bool",
        ] {
            let entity = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name)
                .unwrap();
            let located = service
                .code_locate(
                    receipt.generation,
                    name.to_owned(),
                    LocateMode::Exact,
                    10,
                    0,
                    &cancellation,
                )
                .unwrap();
            let hits: Vec<_> = located
                .data
                .hits
                .iter()
                .filter(|hit| hit.symbol == Some(entity.id))
                .collect();
            assert_eq!(hits.len(), 1, "{encoding}: {name}");
            assert!(!located.data.truncated);
            let reference = hits[0].source.clone().unwrap();
            assert_eq!(entity.evidence.source.as_ref(), Some(&reference));
            assert_eq!(reference.repository(), receipt.repository);
            assert_eq!(reference.generation(), receipt.generation);
            assert_eq!(reference.span().file(), file.id);
            assert_eq!(reference.content_hash(), file.content_hash);
            let read = service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    vec![reference.clone()],
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0)
                        .with_encoding(SourceEncoding::Bytes),
                    FirstSliceBudget::default(),
                    &cancellation,
                )
                .unwrap();
            let start = usize::try_from(reference.span().start_byte()).unwrap();
            let end = usize::try_from(reference.span().end_byte()).unwrap();
            assert_eq!(read.data.chunks.len(), 1);
            assert_eq!(read.data.chunks[0].reference, reference);
            assert_eq!(read.data.chunks[0].bytes, bytes[start..end]);
        }
        assert!(!document.entities.iter().any(|entity| matches!(
            entity.canonical_name.as_str(),
            "destruct" | "toProp" | "forward"
        )));
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls)
        );
        let reference = SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
            file.content_hash,
            None,
        );
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "unsupported-language")
        );
        for (domain, detail) in [
            (
                IrFactDomain::Entities,
                "coq-local-term-binder-projection-and-generated-declaration-coverage-incomplete",
            ),
            (
                IrFactDomain::Relations,
                "coq-elaboration-typechecking-proofchecking-and-term-binding-unavailable",
            ),
            (
                IrFactDomain::Occurrences,
                "coq-term-pattern-tactic-and-notation-reference-surface-incomplete",
            ),
            (
                IrFactDomain::Relations,
                "coq-proof-state-and-tactic-semantics-unavailable",
            ),
            (
                IrFactDomain::Relations,
                "coq-name-resolution-and-implicit-argument-elaboration-unavailable",
            ),
            (
                IrFactDomain::Relations,
                "coq-record-class-elaboration-projection-generation-and-field-binding-unavailable",
            ),
        ] {
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail == detail
                        && gap.domain == domain
                        && gap.source.repository() == receipt.repository
                        && gap.source.generation() == receipt.generation
                        && gap.source.span().file() == file.id
                        && gap.source.content_hash() == file.content_hash
                        && gap.source.span().start_byte() < gap.source.span().end_byte()
                        && gap.source.span().end_byte() <= file.byte_length),
                "{encoding}: {detail}"
            );
        }
        assert!(document.occurrences.iter().any(|site| site.file == file.id
            && site.syntax_kind == "coq.written_reference.reference"
            && matches!(
                site.target,
                rootlight_ir::OccurrenceTarget::Unresolved { .. }
            )));
        let coverage = service
            .source_file_coverage_until(receipt.generation, file.id, &cancellation)
            .unwrap();
        assert_eq!(coverage.tier, AnalysisTier::TierD);
        assert_ne!(coverage.status, CoverageStatus::Complete);
        let no_op = service
            .index_repository(fixture.path(), &cancellation)
            .unwrap();
        assert_eq!(no_op.generation, receipt.generation);
        fs::write(&path, b"different live source").unwrap();
        let found = service
            .find_literal(
                receipt.generation,
                file.id,
                "written_value",
                None,
                rootlight_source::SourceFindBudget::new(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(found.matches.len(), 1);
        assert!(found.continuation.is_none());
        assert_eq!(found.stop, rootlight_source::SourceFindStop::EndOfSource);
        let start = source.find("written_value").unwrap();
        let (start, length) = if encoding.starts_with("utf-8") {
            (
                start + if encoding == "utf-8-bom" { 3 } else { 0 },
                "written_value".len(),
            )
        } else {
            (
                2 + source[..start].encode_utf16().count() * 2,
                "written_value".len() * 2,
            )
        };
        let matched = &found.matches[0];
        assert_eq!(matched.repository(), receipt.repository);
        assert_eq!(matched.generation(), receipt.generation);
        assert_eq!(matched.content_hash(), file.content_hash);
        assert_eq!(matched.span().start_byte(), u64::try_from(start).unwrap());
        assert_eq!(
            matched.span().end_byte(),
            u64::try_from(start + length).unwrap()
        );
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![reference.clone(), matched.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 2);
        assert_eq!(read.data.chunks[0].reference, reference);
        assert_eq!(read.data.chunks[0].bytes, bytes);
        assert_eq!(read.data.chunks[1].reference, *matched);
        assert_eq!(read.data.chunks[1].bytes, bytes[start..start + length]);
    }
}

#[test]
fn coq_module_and_parser_gaps_keep_exact_service_source_authority() {
    for (source, domain, detail) in [
        (
            "Module Written.\nDefinition nested_value := 0.\nEnd Written.\n",
            IrFactDomain::Relations,
            "coq-module-section-stack-and-qualified-ownership-unavailable",
        ),
        (
            include_str!(
                "../../rootlight-adapter-treesitter/tests/fixtures/coq-recovery/TemplateMonadCommon.v"
            ),
            IrFactDomain::Entities,
            "coq-parser-recovery-structure-unavailable",
        ),
    ] {
        let fixture = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        fs::write(fixture.path().join("proof.v"), source).unwrap();
        fs::write(
            fixture.path().join("neighbor.v"),
            "Definition independent_neighbor := 0.\n",
        )
        .unwrap();
        let cancellation = Cancellation::new();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository(fixture.path(), &cancellation)
            .unwrap();
        let document = service
            .inspect_generation(
                receipt.repository,
                Some(receipt.generation),
                &cancellation,
                |snapshot| snapshot.document().clone(),
            )
            .unwrap();
        let file = document
            .files
            .iter()
            .find(|file| file.path == "proof.v")
            .unwrap();
        assert_eq!(file.language, "coq");
        assert_eq!(file.content_hash, content_hash(source.as_bytes()));
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == domain
                    && gap.detail == detail
                    && gap.source.repository() == receipt.repository
                    && gap.source.generation() == receipt.generation
                    && gap.source.span().file() == file.id
                    && gap.source.content_hash() == file.content_hash
                    && gap.source.span().start_byte() < gap.source.span().end_byte()
                    && gap.source.span().end_byte() <= file.byte_length),
            "{detail}"
        );
        assert!(
            !document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls)
        );
        let neighbor = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Constant
                    && entity.canonical_name == "independent_neighbor"
            })
            .unwrap();
        let neighbor_file = document
            .files
            .iter()
            .find(|file| file.path == "neighbor.v")
            .unwrap();
        assert_eq!(neighbor_file.language, "coq");
        assert!(document.occurrences.iter().any(|site| {
            site.file == neighbor_file.id
                && site.role == OccurrenceRole::Definition
                && site.target
                    == rootlight_ir::OccurrenceTarget::Resolved {
                        symbol: neighbor.id,
                    }
        }));
        if domain == IrFactDomain::Relations {
            let module = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Module && entity.canonical_name == "Written"
                })
                .unwrap();
            assert!(
                !document.relations.iter().any(|relation| relation.subject
                    == rootlight_ir::RelationEndpoint::Entity(module.id)),
                "a written flat header does not establish namespace ownership"
            );
        }
        let full = SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
            file.content_hash,
            None,
        );
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![full.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].reference, full);
        assert_eq!(read.data.chunks[0].bytes, source.as_bytes());
    }
}

#[test]
fn hdl_content_routes_publish_definitions_and_exact_sources_with_no_op_reuse() {
    let fixture = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let inputs = [
        (
            "one.v",
            "module One(input wire a, output wire b); assign b=a; endmodule\n",
            Some(("verilog", "One")),
        ),
        (
            "upper.V",
            "module Upper(input wire a); endmodule\n",
            Some(("verilog", "Upper")),
        ),
        (
            "two.sv",
            "module Two; endmodule\n",
            Some(("systemverilog", "Two")),
        ),
        (
            "code.v",
            "fn written_value() int { return 1 }\n",
            Some(("v", "written_value")),
        ),
        (
            "fixture.vv",
            "fn fixture_value() int { return 1 }\n",
            Some(("v", "fixture_value")),
        ),
        (
            "script.vsh",
            "#!/usr/bin/env -S v run\nfn script_value() int { return 1 }\n",
            Some(("v", "script_value")),
        ),
        (
            "bare.vsh",
            "fn bare_value() int { return 1 }\n",
            Some(("v", "bare_value")),
        ),
        (
            "shader.vsh",
            "// fn fake() {}\nstruct Material { float value; };\nfloat println(float x) { return x; }\nvoid shader_value() {}\n",
            Some(("glsl", "shader_value")),
        ),
        (
            "comment.v",
            "// module Hidden(input wire a); endmodule\n",
            None,
        ),
        (
            "quoted.v",
            "'module Hidden(input wire a); endmodule'\n",
            None,
        ),
        (
            "proof.v",
            "(* module Hidden(input wire a); endmodule *)\n",
            None,
        ),
    ];
    for (path, source, _) in inputs {
        fs::write(fixture.path().join(path), source).unwrap();
    }
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    let document = service
        .inspect_generation(
            receipt.repository,
            Some(receipt.generation),
            &cancellation,
            |snapshot| snapshot.document().clone(),
        )
        .unwrap();
    assert_eq!(document.files.len(), inputs.len());
    for (path, original, expected) in inputs {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        let provenance = document
            .provenance
            .iter()
            .find(|row| row.id == file.provenance)
            .unwrap();
        if let Some((language, name)) = expected {
            assert_eq!(file.language, language, "{path}");
            assert_eq!(provenance.language, language);
            assert_eq!(provenance.producer_kind, ProducerKind::Parser, "{path}");
            assert!(
                document
                    .entities
                    .iter()
                    .any(|entity| entity.canonical_name == name
                        && entity.language == language
                        && entity
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|source| source.span().file() == file.id)),
                "missing written definition in {path}"
            );
            assert!(
                document
                    .occurrences
                    .iter()
                    .any(|site| site.file == file.id && site.role == OccurrenceRole::Definition)
            );
        } else {
            assert_ne!(
                provenance.producer_kind,
                ProducerKind::Parser,
                "inert source was admitted: {path}"
            );
            assert!(
                !document
                    .occurrences
                    .iter()
                    .any(|site| site.file == file.id && site.role == OccurrenceRole::Definition)
            );
        }
        let reference = SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(file.id, 0, u64::try_from(original.len()).unwrap()).unwrap(),
            content_hash(original.as_bytes()),
            None,
        );
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![reference.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].reference, reference);
        assert_eq!(read.data.chunks[0].bytes, original.as_bytes());
    }
    let no_op = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    assert_eq!(no_op.generation, receipt.generation);
}

#[test]
fn shared_v_routes_require_unanimous_content_or_explicit_configuration() {
    let service = FirstSliceService::new(2).unwrap();
    let mut input = ManifestInput {
        file: FileId::from_bytes([3; 20]),
        path: "written.V".to_owned(),
        content_hash: content_hash(b""),
        bytes: 0,
        encoding: "utf-8".to_owned(),
        class: InputClass::Source,
        language_signals: vec![LanguageSignal {
            language: "verilog".to_owned(),
            evidence: LanguageEvidence::Extension,
        }],
        project_language_witnesses: Vec::new(),
        decisive_rule: None,
    };
    assert_eq!(detected_source_language(&input), None);
    assert_eq!(source_language_route(&input, &service.analyzers), None);
    input.language_signals.push(LanguageSignal {
        language: "verilog".to_owned(),
        evidence: LanguageEvidence::Content,
    });
    assert_eq!(detected_source_language(&input), Some("verilog"));
    assert_eq!(
        source_language_route(&input, &service.analyzers),
        Some("verilog")
    );
    let mut absent = service.analyzers.clone();
    assert!(absent.remove("verilog").is_some());
    assert_eq!(source_language_route(&input, &absent), None);
    assert_eq!(detected_source_language(&input), Some("verilog"));
    assert_ne!(
        file_parser_route_hash(&input, Some("verilog")).unwrap(),
        file_parser_route_hash(&input, Some("v")).unwrap()
    );
    input.language_signals.push(LanguageSignal {
        language: "coq".to_owned(),
        evidence: LanguageEvidence::Content,
    });
    assert_eq!(detected_source_language(&input), None);
    assert_eq!(source_language_route(&input, &service.analyzers), None);
    input.language_signals.push(LanguageSignal {
        language: "systemverilog".to_owned(),
        evidence: LanguageEvidence::Configuration,
    });
    assert_eq!(detected_source_language(&input), Some("systemverilog"));
    assert_eq!(
        source_language_route(&input, &service.analyzers),
        Some("systemverilog")
    );
    input.language_signals.push(LanguageSignal {
        language: "verilog".to_owned(),
        evidence: LanguageEvidence::Configuration,
    });
    assert_eq!(detected_source_language(&input), None);
    assert_eq!(source_language_route(&input, &service.analyzers), None);
}
