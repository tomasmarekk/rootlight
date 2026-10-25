//! Adversarial consumer controls preserve the current source and lexical owner joins.
//! Real native output supplies the positive frame before each witness is changed.

use super::super::{
    Lowering, NativeSourceView, TreeSitterAnalyzer, exclude_ambiguous_entities, materialize_entity,
};
use super::*;
use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId, GenerationBoundSnapshot,
    LanguageId, MemoryAdmissionPolicy, StreamLimits, SyntaxKindLabel, execute_analysis,
    execute_parse,
};
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ExtensionSupport, IrLimits, ProducerIdentity,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use std::{fs, path::Path, sync::Arc};

const ORIGINAL: &[u8] =
    include_bytes!("../../../tests/fixtures/javascript_const_receiver/webpack.common.js");

#[test]
fn complete_original_native_raw_and_materialized_frame_has_one_receiver_binding() {
    with_raw_entities(ORIGINAL, |lowering, entities, document| {
        let facts = lowering.coordinates.facts();
        let plan = joined(
            lowering,
            facts,
            entities,
            lowering.full_source,
            ORIGINAL,
            2_000_000,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(plan.bindings.len(), 1);
        assert_eq!(facts.iter().filter(|fact| required(fact)).count(), 1);
        let reference = facts.iter().find(|fact| required(fact)).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == REFERENCE)
            .unwrap();
        assert!(matches!(site.target, OccurrenceTarget::Resolved { .. }));
        let mut unresolved = site.clone();
        unresolved.target = OccurrenceTarget::Unresolved {
            text_hash: site.syntactic_text_hash,
        };
        let target = plan
            .resolve(
                reference,
                reference.span(),
                &unresolved,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(
            target,
            match site.target {
                OccurrenceTarget::Resolved { symbol } => Some(symbol),
                _ => None,
            }
        );
    });
}

fn current_plan(
    lowering: &Lowering<'_, '_>,
    entities: &HashMap<u64, MaterializedEntity>,
    bytes: &[u8],
) -> Targets {
    Targets::new(
        lowering.coordinates.facts(),
        entities,
        lowering.full_source,
        lowering.source_view,
        bytes,
        lowering.request.source().path().as_str(),
        lowering.request.build_context(),
        2_000_000,
        &Cancellation::new(),
    )
    .unwrap()
}

fn reidentify(entity: &mut MaterializedEntity) {
    let claim = &mut entity.identity_claim;
    claim.symbol = claim.derived_symbol();
    entity.record.id = claim.symbol;
    entity.identity_guard = super::super::entity_identity_guard(
        claim.repository,
        &claim.language,
        rootlight_ir::entity_kind_identity_label(claim.kind),
        &claim.container_identity,
        &claim.declared_identity,
        &claim.signature_discriminator,
        &claim.build_context_discriminator,
        None,
    )
    .unwrap();
}

#[test]
fn coherent_claim_and_derived_id_mutations_cannot_change_the_proved_module_or_variable_identity() {
    with_raw_entities(ORIGINAL, |lowering, entities, _| {
        assert_eq!(current_plan(lowering, entities, ORIGINAL).bindings.len(), 1);
        for (&local, original) in entities.iter().filter(|(_, entity)| {
            matches!(
                entity.record.kind,
                EntityKind::Module | EntityKind::Variable
            )
        }) {
            for mutation in 0..8 {
                let mut changed = entities.clone();
                let entity = changed.get_mut(&local).unwrap();
                let old_id = entity.record.id;
                match mutation {
                    0 => {
                        entity.identity_claim.repository =
                            rootlight_ids::RepositoryId::from_bytes([8; 16])
                    }
                    1 => entity.identity_claim.language = "typescript".to_owned(),
                    2 => entity.identity_claim.kind = EntityKind::Function,
                    3 => {
                        entity.identity_claim.container =
                            Some(ContainerRef::Entity(SymbolId::from_bytes([8; 20])))
                    }
                    4 => entity.identity_claim.declared_identity.push_str("_other"),
                    5 => entity.identity_claim.container_identity.push(8),
                    6 => entity.identity_claim.signature_discriminator.push(8),
                    _ => entity.identity_claim.build_context_discriminator.push(8),
                }
                reidentify(entity);
                assert_eq!(entity.record.id, entity.identity_claim.derived_symbol());
                if original.record.kind == EntityKind::Module {
                    let new_id = entity.record.id;
                    for child in changed
                        .values_mut()
                        .filter(|child| child.direct_parent == ContainerRef::Entity(old_id))
                    {
                        child.direct_parent = ContainerRef::Entity(new_id);
                        child.record.container = Some(child.direct_parent);
                        child.identity_claim.container = Some(child.direct_parent);
                        child.identity_claim.container_identity =
                            variable_container(new_id).to_vec();
                        reidentify(child);
                    }
                }
                assert!(
                    current_plan(lowering, &changed, ORIGINAL)
                        .bindings
                        .is_empty(),
                    "kind={:?}, mutation={mutation}",
                    original.record.kind
                );
            }
        }
    });
}

#[test]
fn existing_written_declaration_identity_uses_same_name_owner_and_kind_ordinal_zero() {
    for text in [
        "const value=1; target={value:value.use()};",
        "const earlier=2; const value=1; const later=3; target={value:value.use()};",
        "const elsewhere=2; const renamed=load('pkg'); target.root={nested:{renamed:renamed.finish()}}; const later=3;",
        "const value=1; target.if={class:value.return()};",
        "const from=1; of={default:from.class()};",
        "const of=1; from={while:of.if()};",
    ] {
        with_raw_entities(text.as_bytes(), |lowering, entities, _| {
            let plan = current_plan(lowering, entities, text.as_bytes());
            assert_eq!(plan.bindings.len(), 1, "{text}");
            let binding = plan.bindings.values().next().unwrap();
            let variable = entities
                .values()
                .find(|entity| entity.record.id == binding.target)
                .unwrap();
            let expected = variable_container(binding.owner);
            assert_eq!(
                variable.identity_claim.container_identity.as_slice(),
                expected.as_slice()
            );
            assert_eq!(
                variable.identity_claim.declared_identity,
                variable.record.canonical_name
            );
            assert_eq!(
                variable
                    .identity_claim
                    .build_context_discriminator
                    .as_slice(),
                lowering.request.build_context().digest().as_bytes()
            );
        });
    }
}

#[test]
fn current_tsx_walk_refuses_real_owners_and_invalid_reserved_bindings_after_every_line_terminator()
{
    let mut owners = Vec::new();
    for separator in ["\r", "\r\n", "\u{2028}", "\u{2029}"] {
        owners.push(format!("const value=1;\n//x{separator}with(other) {{\ntarget={{key:value.use()}};\n//x{separator}}}\n"));
    }
    for separator in ["\n", "\r\n"] {
        owners.push(format!("#!x{separator}with(other) {{\nconst value=1;\ntarget={{key:value.use()}};\n//x{separator}}}\n"));
    }
    for text in owners {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .unwrap();
        let actual = parser.parse(text.as_bytes(), None).unwrap();
        assert!(
            !actual.root_node().has_error(),
            "the adversary must reach the actual owner walk: {text:?}"
        );
        let mut cursor = actual.root_node().walk();
        assert!(
            actual
                .root_node()
                .named_children(&mut cursor)
                .any(|node| node.kind() == "with_statement")
        );
        let mut work = Work {
            remaining: 2_000_000,
            exhausted: false,
        };
        assert!(
            Program::new(text.as_bytes(), &mut work, &Cancellation::new())
                .unwrap()
                .is_none(),
            "{text:?}"
        );
    }
    for reserved in [
        "if",
        "else",
        "for",
        "while",
        "return",
        "switch",
        "class",
        "function",
        "await",
        "break",
        "case",
        "catch",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "enum",
        "export",
        "extends",
        "false",
        "finally",
        "import",
        "in",
        "instanceof",
        "let",
        "new",
        "null",
        "super",
        "this",
        "throw",
        "true",
        "try",
        "typeof",
        "var",
        "void",
        "with",
        "yield",
        "implements",
        "interface",
        "package",
        "private",
        "protected",
        "public",
        "static",
    ] {
        let text = format!("const {reserved}=1; target={{key:{reserved}.use()}};");
        let mut work = Work {
            remaining: 2_000_000,
            exhausted: false,
        };
        assert!(
            Program::new(text.as_bytes(), &mut work, &Cancellation::new())
                .unwrap()
                .is_none(),
            "{text}"
        );
    }
}

#[test]
fn coherent_current_hash_and_raw_markers_cannot_hide_a_with_owner_after_comment_or_hashbang() {
    let cases = [
        ("\r", false),
        ("\r\n", false),
        ("\u{2028}", false),
        ("\u{2029}", false),
        ("\n", true),
        ("\r\n", true),
    ];
    for (separator, hashbang) in cases {
        let current = if hashbang {
            format!(
                "#!x{separator}with(other) {{\nconst value=1;\ntarget={{key:value.use()}};\n//x{separator}}}\n"
            )
        } else {
            format!(
                "const value=1;\n//x{separator}with(other) {{\ntarget={{key:value.use()}};\n//x{separator}}}\n"
            )
        };
        let benign = current
            .replace("with(other) {", &" ".repeat("with(other) {".len()))
            .replace(&format!("//x{separator}}}"), &format!("//x{separator} "));
        assert_eq!(benign.len(), current.len());
        with_raw_entities(benign.as_bytes(), |lowering, entities, _| {
            assert_eq!(
                current_plan(lowering, entities, benign.as_bytes())
                    .bindings
                    .len(),
                1,
                "positive benign frame {benign:?}"
            );
            let source = SourceRef::new(
                lowering.full_source.repository(),
                lowering.full_source.generation(),
                lowering.full_source.span(),
                content_hash(current.as_bytes()),
                None,
            );
            let mut coherent = entities.clone();
            for entity in coherent.values_mut() {
                let evidence = entity.record.evidence.source.as_ref().unwrap();
                entity.record.evidence.source = Some(SourceRef::new(
                    source.repository(),
                    source.generation(),
                    evidence.span(),
                    source.content_hash(),
                    None,
                ));
            }
            let view = NativeSourceView::new(
                current.as_bytes(),
                "utf-8",
                1024 * 1024,
                16 * 1024 * 1024,
                &Cancellation::new(),
            )
            .unwrap();
            let plan = Targets::new(
                lowering.coordinates.facts(),
                &coherent,
                &source,
                &view,
                current.as_bytes(),
                lowering.request.source().path().as_str(),
                lowering.request.build_context(),
                2_000_000,
                &Cancellation::new(),
            )
            .unwrap();
            assert!(
                plan.bindings.is_empty(),
                "forged current partition {current:?}"
            );
        });
    }
}

#[test]
fn non_lf_hashbang_controls_pin_the_actual_clean_swallowed_carrier_then_refuse_it() {
    for separator in ["\r", "\u{2028}", "\u{2029}"] {
        let text =
            format!("#!x{separator}with(other) {{}}\nconst value=1; target={{key:value.use()}};\n");
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .unwrap();
        let tree = parser.parse(text.as_bytes(), None).unwrap();
        let root = tree.root_node();
        assert!(
            !root.has_error(),
            "the current carrier adversary must be TSX-clean: {text:?}"
        );
        let mut cursor = root.walk();
        let carrier = root
            .named_children(&mut cursor)
            .find(|node| node.kind() == "hash_bang_line")
            .unwrap();
        assert_eq!(carrier.start_byte(), 0);
        assert_eq!(carrier.end_byte(), text.find('\n').unwrap());
        assert!(
            std::str::from_utf8(&text.as_bytes()[carrier.byte_range()])
                .unwrap()
                .contains("with(other) {}")
        );
        let mut cursor = root.walk();
        assert!(
            !root
                .named_children(&mut cursor)
                .any(|node| node.kind() == "with_statement")
        );
        let model = Program {
            source: text.as_bytes(),
            words: Vec::new(),
            native_root: root.byte_range(),
            bindings: Vec::new(),
            receivers: Vec::new(),
            nonbinding: HashSet::new(),
        };
        let mut work = Work {
            remaining: 2_000_000,
            exhausted: false,
        };
        assert!(
            !model
                .admit_hashbang(carrier, &mut work, &Cancellation::new())
                .unwrap()
        );
        assert!(
            Program::new(text.as_bytes(), &mut work, &Cancellation::new())
                .unwrap()
                .is_none()
        );
    }
    for separator in ["\n", "\r\n"] {
        let text = format!("#!x{separator}const value=1; target={{key:value.use()}};\n");
        with_raw_entities(text.as_bytes(), |lowering, entities, _| {
            assert_eq!(
                current_plan(lowering, entities, text.as_bytes())
                    .bindings
                    .len(),
                1
            );
            let mut work = Work {
                remaining: 2_000_000,
                exhausted: false,
            };
            let model = Program::new(text.as_bytes(), &mut work, &Cancellation::new())
                .unwrap()
                .unwrap();
            assert_eq!(model.receivers.len(), 1);
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
                .unwrap();
            let tree = parser.parse(text.as_bytes(), None).unwrap();
            let mut cursor = tree.root_node().walk();
            let carrier = tree
                .root_node()
                .named_children(&mut cursor)
                .find(|node| node.kind() == "hash_bang_line")
                .unwrap();
            assert_eq!(carrier.end_byte(), text.find('\n').unwrap());
            assert_eq!(text.as_bytes().get(carrier.end_byte()), Some(&b'\n'));
            if separator == "\r\n" {
                assert_eq!(text.as_bytes().get(carrier.end_byte() - 1), Some(&b'\r'));
            }
            assert!(
                model
                    .admit_hashbang(carrier, &mut work, &Cancellation::new())
                    .unwrap()
            );
        });
    }
}

#[test]
fn tsx_clean_hashbang_forgery_with_current_hash_and_equal_width_markers_fails_the_carrier_boundary()
{
    for separator in ["\r", "\u{2028}", "\u{2029}"] {
        let current =
            format!("#!x{separator}with(other) {{}}\nconst value=1; target={{key:value.use()}};\n");
        let benign = current.replacen(separator, &" ".repeat(separator.len()), 1);
        assert_eq!(benign.len(), current.len());
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .unwrap();
        let current_tree = parser.parse(current.as_bytes(), None).unwrap();
        assert!(!current_tree.root_node().has_error());
        let benign_tree = parser.parse(benign.as_bytes(), None).unwrap();
        assert!(!benign_tree.root_node().has_error());
        let mut cursor = current_tree.root_node().walk();
        let current_carrier = current_tree
            .root_node()
            .named_children(&mut cursor)
            .find(|node| node.kind() == "hash_bang_line")
            .unwrap();
        let mut cursor = benign_tree.root_node().walk();
        let benign_carrier = benign_tree
            .root_node()
            .named_children(&mut cursor)
            .find(|node| node.kind() == "hash_bang_line")
            .unwrap();
        assert_eq!(current_carrier.byte_range(), benign_carrier.byte_range());
        with_raw_entities(benign.as_bytes(), |lowering, entities, _| {
            assert_eq!(
                current_plan(lowering, entities, benign.as_bytes())
                    .bindings
                    .len(),
                1,
                "genuine admitted native carrier positive"
            );
            let source = SourceRef::new(
                lowering.full_source.repository(),
                lowering.full_source.generation(),
                lowering.full_source.span(),
                content_hash(current.as_bytes()),
                None,
            );
            let mut coherent = entities.clone();
            for entity in coherent.values_mut() {
                let old = entity.record.evidence.source.as_ref().unwrap();
                entity.record.evidence.source = Some(SourceRef::new(
                    source.repository(),
                    source.generation(),
                    old.span(),
                    source.content_hash(),
                    None,
                ));
            }
            let view = NativeSourceView::new(
                current.as_bytes(),
                "utf-8",
                1024 * 1024,
                16 * 1024 * 1024,
                &Cancellation::new(),
            )
            .unwrap();
            let mut work = Work {
                remaining: 2_000_000,
                exhausted: false,
            };
            assert!(
                exact_projection(&view, current.as_bytes(), &mut work, &Cancellation::new())
                    .unwrap()
            );
            let model = Program {
                source: current.as_bytes(),
                words: Vec::new(),
                native_root: current_tree.root_node().byte_range(),
                bindings: Vec::new(),
                receivers: Vec::new(),
                nonbinding: HashSet::new(),
            };
            assert!(
                !model
                    .admit_hashbang(current_carrier, &mut work, &Cancellation::new())
                    .unwrap()
            );
            let plan = Targets::new(
                lowering.coordinates.facts(),
                &coherent,
                &source,
                &view,
                current.as_bytes(),
                lowering.request.source().path().as_str(),
                lowering.request.build_context(),
                2_000_000,
                &Cancellation::new(),
            )
            .unwrap();
            assert!(
                plan.bindings.is_empty(),
                "current clean carrier must not borrow the benign native certificate"
            );
        });
    }
}

#[test]
fn projected_native_root_clean_carrier_and_physical_bom_frame_are_joined_in_both_utf16_encodings() {
    let text = "const value=1; target={value:value.use()};";
    for (encoding, little) in [("utf-16le", true), ("utf-16be", false)] {
        let mut bytes = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        with_raw_entities_encoding(&bytes, encoding, |lowering, entities, _| {
            let view = lowering.source_view;
            let mut work = Work {
                remaining: 2_000_000,
                exhausted: false,
            };
            let program = Program::new(view.as_bytes(), &mut work, &Cancellation::new())
                .unwrap()
                .unwrap();
            assert!(exact_projection(view, &bytes, &mut work, &Cancellation::new()).unwrap());
            let root = lowering
                .coordinates
                .facts()
                .iter()
                .find(|fact| fact.kind() == SyntaxFactKind::Root)
                .unwrap();
            let clean = lowering
                .coordinates
                .facts()
                .iter()
                .find(|fact| fact.syntax_kind().as_str() == CLEAN)
                .unwrap();
            assert_eq!(root.span().start_byte(), 0);
            assert_eq!(
                usize::try_from(root.span().end_byte()).unwrap(),
                view.as_bytes().len()
            );
            assert_eq!(clean.span(), root.span());
            assert_eq!(
                view.physical_span(root.span()).unwrap(),
                lowering.full_source.span()
            );
            let native = span(root.span().file(), &program.native_root).unwrap();
            let prefix = SourceSpan::new(root.span().file(), 0, native.start_byte()).unwrap();
            let prefix = view.physical_span(prefix).unwrap();
            assert_eq!(prefix.start_byte(), 0);
            assert_eq!(
                prefix.end_byte(),
                if program.native_root.start == 0 { 0 } else { 2 }
            );
            assert_eq!(
                view.physical_span(native).unwrap().start_byte(),
                prefix.end_byte()
            );
            assert_eq!(
                lowering
                    .parse_output
                    .facts()
                    .iter()
                    .find(|fact| fact.syntax_kind().as_str() == CLEAN)
                    .unwrap()
                    .span(),
                lowering.full_source.span()
            );
            let plan = Targets::new(
                lowering.coordinates.facts(),
                entities,
                lowering.full_source,
                view,
                &bytes,
                lowering.request.source().path().as_str(),
                lowering.request.build_context(),
                2_000_000,
                &Cancellation::new(),
            )
            .unwrap();
            assert_eq!(plan.bindings.len(), 1);
            let mut changed = bytes.clone();
            changed[0] ^= 1;
            assert!(!exact_projection(view, &changed, &mut work, &Cancellation::new()).unwrap());
        });
    }
}

#[test]
fn missing_or_mismatched_utf16_bom_is_an_encoding_refusal_before_receiver_qualification() {
    let text = "const value=1; target={key:value.use()};";
    for (encoding, little) in [("utf-16le", true), ("utf-16be", false)] {
        let mut missing = Vec::new();
        for unit in text.encode_utf16() {
            missing.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let mut mismatched = if little {
            vec![0xfe, 0xff]
        } else {
            vec![0xff, 0xfe]
        };
        mismatched.extend_from_slice(&missing);
        for bytes in [missing, mismatched] {
            assert!(
                matches!(NativeSourceView::new(&bytes,encoding,1024*1024,16*1024*1024,&Cancellation::new()),Err(AdapterError::ProviderFailed { code }) if code.as_str()=="native-source-invalid-utf16")
            );
        }
    }
}

fn joined(
    lowering: &Lowering<'_, '_>,
    facts: &[SyntaxFact],
    entities: &HashMap<u64, MaterializedEntity>,
    source: &SourceRef,
    bytes: &[u8],
    maximum: usize,
    cancel: &Cancellation,
) -> Result<Targets, AdapterError> {
    let view = NativeSourceView::new(bytes, "utf-8", 1024 * 1024, 16 * 1024 * 1024, cancel)?;
    Targets::new(
        facts,
        entities,
        source,
        &view,
        bytes,
        lowering.request.source().path().as_str(),
        lowering.request.build_context(),
        maximum,
        cancel,
    )
}

fn replace(
    fact: &SyntaxFact,
    parent: Option<u64>,
    depth: usize,
    label: &str,
    range: SourceSpan,
) -> SyntaxFact {
    SyntaxFact::new(
        fact.local_id(),
        parent,
        fact.kind(),
        range,
        depth,
        SyntaxKindLabel::new(label).unwrap(),
    )
}

#[test]
fn missing_duplicate_reparented_or_relabelled_required_witnesses_withhold_binding() {
    with_raw_entities(ORIGINAL, |lowering, entities, _| {
        let original = lowering.coordinates.facts();
        let selected = original
            .iter()
            .enumerate()
            .filter(|(_, fact)| {
                metadata(fact)
                    || required(fact)
                    || matches!(
                        fact.kind(),
                        SyntaxFactKind::Root | SyntaxFactKind::Module | SyntaxFactKind::Declaration
                    )
                    || fact.syntax_kind().as_str() == "javascript.identifier.definition"
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for index in selected {
            for mutation in 0..5 {
                let mut facts = original.to_vec();
                let fact = &original[index];
                match mutation {
                    0 => {
                        facts.remove(index);
                    }
                    1 => {
                        facts.push(SyntaxFact::new(
                            original.iter().map(SyntaxFact::local_id).max().unwrap() + 1,
                            fact.parent(),
                            fact.kind(),
                            fact.span(),
                            fact.depth(),
                            fact.syntax_kind().clone(),
                        ));
                    }
                    2 => {
                        facts[index] = replace(
                            fact,
                            if fact.parent() == Some(1) {
                                Some(2)
                            } else {
                                Some(1)
                            },
                            fact.depth(),
                            fact.syntax_kind().as_str(),
                            fact.span(),
                        )
                    }
                    3 => {
                        facts[index] = replace(
                            fact,
                            fact.parent(),
                            fact.depth() + 1,
                            fact.syntax_kind().as_str(),
                            fact.span(),
                        )
                    }
                    _ => {
                        facts[index] = replace(
                            fact,
                            fact.parent(),
                            fact.depth(),
                            "javascript.const_receiver_counterfeit.signature",
                            fact.span(),
                        )
                    }
                }
                assert!(
                    joined(
                        lowering,
                        &facts,
                        entities,
                        lowering.full_source,
                        ORIGINAL,
                        2_000_000,
                        &Cancellation::new()
                    )
                    .unwrap()
                    .bindings
                    .is_empty(),
                    "index={index}, mutation={mutation}, original={fact:?}"
                );
            }
        }
    });
}

#[test]
fn current_physical_hash_and_complete_owner_grammar_reject_stale_or_forged_source() {
    with_raw_entities(ORIGINAL, |lowering, entities, _| {
        let facts = lowering.coordinates.facts();
        for (before, after) in [
            ("const", "var  "),
            ("path: path.resolve", "path? path.resolve"),
            ("require('path')", "require(`path`)"),
            (
                "path.resolve(__dirname, 'dist')",
                "path.resolve(__dirname, `dist`) ",
            ),
        ] {
            let text = std::str::from_utf8(ORIGINAL).unwrap();
            assert!(text.contains(before));
            let changed = text.replacen(before, after, 1).into_bytes();
            assert!(
                joined(
                    lowering,
                    facts,
                    entities,
                    lowering.full_source,
                    &changed,
                    2_000_000,
                    &Cancellation::new()
                )
                .unwrap()
                .bindings
                .is_empty()
            );
            let source = SourceRef::new(
                lowering.full_source.repository(),
                lowering.full_source.generation(),
                SourceSpan::new(
                    lowering.full_source.span().file(),
                    0,
                    u64::try_from(changed.len()).unwrap(),
                )
                .unwrap(),
                content_hash(&changed),
                None,
            );
            assert!(
                joined(
                    lowering,
                    facts,
                    entities,
                    &source,
                    &changed,
                    2_000_000,
                    &Cancellation::new()
                )
                .unwrap()
                .bindings
                .is_empty()
            );
        }
        for text in [
            "const value=1; target={value:flag?value.use():other};",
            "const value=1; value: target={key:value.use()};",
            "const value=1; ({value}=other); target={key:value.use()};",
            "const value=1; target={key:value.use()}; const value=2;",
        ] {
            let mut work = Work {
                remaining: 2_000_000,
                exhausted: false,
            };
            if let Some(program) =
                Program::new(text.as_bytes(), &mut work, &Cancellation::new()).unwrap()
            {
                for receiver in &program.receivers {
                    assert!(
                        program
                            .binding(receiver, &mut work, &Cancellation::new())
                            .unwrap()
                            .is_none(),
                        "{text}"
                    );
                }
            }
        }
    });
}

#[test]
fn foreign_materialized_variable_or_module_never_supplies_lexical_authority() {
    with_raw_entities(ORIGINAL, |lowering, entities, _| {
        let facts = lowering.coordinates.facts();
        for (&local, _) in entities.iter().filter(|(_, entity)| {
            matches!(
                entity.record.kind,
                EntityKind::Module | EntityKind::Variable
            )
        }) {
            for mutation in 0..9 {
                let mut changed = entities.clone();
                let entity = changed.get_mut(&local).unwrap();
                match mutation {
                    0 => entity.record.kind = EntityKind::Function,
                    1 => entity.record.language = "typescript".to_owned(),
                    2 => entity.record.generation = GenerationId::from_bytes([9; 20]),
                    3 => entity.record.provenance = FactId::from_bytes([9; 20]),
                    4 => entity.record.container = None,
                    5 => entity.record.id = SymbolId::from_bytes([9; 20]),
                    6 => {
                        let old = entity.record.evidence.source.as_ref().unwrap();
                        entity.record.evidence.source = Some(SourceRef::new(
                            old.repository(),
                            old.generation(),
                            old.span(),
                            content_hash(b"stale"),
                            None,
                        ));
                    }
                    7 => {
                        if entity.record.kind == EntityKind::Variable {
                            entity.definition_local_id = None;
                        } else {
                            entity.direct_parent =
                                ContainerRef::Entity(SymbolId::from_bytes([8; 20]));
                        }
                    }
                    _ => {
                        if entity.record.kind == EntityKind::Variable {
                            entity.record.canonical_name = "other".to_owned();
                        } else {
                            entity.record.repository =
                                rootlight_ids::RepositoryId::from_bytes([8; 16]);
                        }
                    }
                }
                assert!(
                    joined(
                        lowering,
                        facts,
                        &changed,
                        lowering.full_source,
                        ORIGINAL,
                        2_000_000,
                        &Cancellation::new()
                    )
                    .unwrap()
                    .bindings
                    .is_empty(),
                    "local={local}, mutation={mutation}"
                );
            }
        }
    });
}

#[test]
fn occurrence_boundary_rejects_candidate_provenance_span_role_and_text_forgeries() {
    with_raw_entities(ORIGINAL, |lowering, entities, document| {
        let facts = lowering.coordinates.facts();
        let plan = joined(
            lowering,
            facts,
            entities,
            lowering.full_source,
            ORIGINAL,
            2_000_000,
            &Cancellation::new(),
        )
        .unwrap();
        let fact = facts.iter().find(|fact| required(fact)).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == REFERENCE)
            .unwrap();
        for mutation in 0..8 {
            let mut changed = site.clone();
            changed.target = OccurrenceTarget::Unresolved {
                text_hash: site.syntactic_text_hash,
            };
            match mutation {
                0 => changed.provenance = FactId::from_bytes([8; 20]),
                1 => changed.role = OccurrenceRole::Read,
                2 => changed.enclosing = None,
                3 => changed.syntax_kind = "javascript.identifier.reference".to_owned(),
                4 => changed.syntactic_text_hash = content_hash(b"other"),
                5 => {
                    changed.target = OccurrenceTarget::Candidates {
                        symbols: vec![SymbolId::from_bytes([8; 20])],
                        total_count: 1,
                        completeness: rootlight_ir::CoverageStatus::Complete,
                    }
                }
                6 => changed.evidence.source = None,
                _ => changed.generation = GenerationId::from_bytes([8; 20]),
            }
            assert!(
                plan.resolve(fact, fact.span(), &changed, &Cancellation::new())
                    .unwrap()
                    .is_none(),
                "mutation={mutation}"
            );
        }
    });
}

#[test]
fn finite_consumer_work_and_cancellation_withhold_the_entire_partition() {
    let plain = b"const value=1; target={key:value.use(),other:value.other()};";
    let mut carrier = b"#!".to_vec();
    carrier.extend(std::iter::repeat_n(b'x', 2048));
    carrier.push(b'\n');
    carrier.extend_from_slice(plain);
    for text in [plain.as_slice(), carrier.as_slice()] {
        with_raw_entities(text, |lowering, entities, _| {
            let facts = lowering.coordinates.facts();
            for maximum in 0..1024 {
                let plan = joined(
                    lowering,
                    facts,
                    entities,
                    lowering.full_source,
                    text,
                    maximum,
                    &Cancellation::new(),
                )
                .unwrap();
                assert!(
                    plan.bindings.is_empty() || plan.bindings.len() == 2,
                    "maximum={maximum}"
                );
            }
            if text.starts_with(b"#!") {
                let mut maximum = Work {
                    remaining: 2_000_000,
                    exhausted: false,
                };
                let model = Program::new(text, &mut maximum, &Cancellation::new())
                    .unwrap()
                    .unwrap();
                let mut parser = tree_sitter::Parser::new();
                parser
                    .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
                    .unwrap();
                let tree = parser.parse(text, None).unwrap();
                let mut cursor = tree.root_node().walk();
                let hashbang = tree
                    .root_node()
                    .named_children(&mut cursor)
                    .find(|node| node.kind() == "hash_bang_line")
                    .unwrap();
                let mut small = Work {
                    remaining: 64,
                    exhausted: false,
                };
                assert!(
                    !model
                        .admit_hashbang(hashbang, &mut small, &Cancellation::new())
                        .unwrap()
                );
                assert!(small.exhausted);
                let cancelled = Cancellation::new();
                cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
                assert!(matches!(
                    model.admit_hashbang(hashbang, &mut maximum, &cancelled),
                    Err(AdapterError::Cancelled { .. })
                ));
            }
            let cancel = Cancellation::new();
            cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
            assert!(matches!(
                joined(
                    lowering,
                    facts,
                    entities,
                    lowering.full_source,
                    text,
                    2_000_000,
                    &cancel
                ),
                Err(AdapterError::Cancelled { .. })
            ));
        });
    }
}

fn native_provider() -> Arc<crate::TreeSitterProvider> {
    Arc::new(
        crate::TreeSitterProvider::new(
            crate::RuntimeConfig::new(
                1024 * 1024,
                16_384,
                128,
                16,
                64,
                1,
                16 * 1024 * 1024,
                crate::ParserSettings::new(256).unwrap(),
            )
            .unwrap(),
        )
        .unwrap(),
    )
}

fn with_raw_entities(
    raw: &[u8],
    operation: impl FnOnce(
        &Lowering<'_, '_>,
        &HashMap<u64, MaterializedEntity>,
        &rootlight_ir::NormalizedIrDocument,
    ),
) {
    with_raw_entities_encoding(raw, "utf-8", operation);
}

fn with_raw_entities_encoding(
    raw: &[u8],
    encoding: &str,
    operation: impl FnOnce(
        &Lowering<'_, '_>,
        &HashMap<u64, MaterializedEntity>,
        &rootlight_ir::NormalizedIrDocument,
    ),
) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("written.js"), raw).unwrap();
    let repository = derive_repository(b"javascript-const-consumer-qualification").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("written.js")).unwrap(),
            1024 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([7; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(raw.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = native_provider();
    let analyzer = TreeSitterAnalyzer::new(
        provider.clone(),
        ProducerIdentity::new(
            "javascript-const-consumer-qualification",
            "1.0",
            content_hash(b"javascript-const-config"),
        )
        .unwrap(),
        LanguageId::new("javascript").unwrap(),
        "javascript-const-consumer-qualification",
        content_hash(b"javascript-const-binary"),
    )
    .unwrap();
    let stream = || {
        StreamLimits::new(
            128,
            4096,
            16 * 1024 * 1024,
            128,
            128 * 1024,
            1024 * 1024,
            BatchThresholds::new(1, 1024 * 1024, 32, 16 * 1024).unwrap(),
        )
        .unwrap()
    };
    let limits = AnalysisLimits::new(
        1024 * 1024,
        16_384,
        128,
        16,
        16 * 1024 * 1024,
        stream(),
        stream(),
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("javascript").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"javascript-const-build")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let cancel = Cancellation::new();
    let parsed = execute_parse(
        provider.as_ref(),
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &cancel,
    )
    .unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        rootlight_ir::CoverageStatus::Complete
    );
    let output = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &cancel,
    )
    .unwrap();
    let view = NativeSourceView::new(
        raw,
        encoding,
        limits.max_source_bytes(),
        limits.max_reported_memory_bytes(),
        &cancel,
    )
    .unwrap();
    let lowering = Lowering::new(&analyzer, &request, &parsed, &view, &cancel).unwrap();
    let mut strings = 0;
    let plan = lowering.entity_drafts(&cancel, &mut strings).unwrap();
    let provenance = lowering.provenance().unwrap().id;
    let mut entities = HashMap::new();
    for draft in &plan.drafts {
        let entity = materialize_entity(
            draft,
            &source,
            &lowering.coordinates,
            draft
                .authored_display_span
                .map(|span| lowering.text_for_span(span))
                .transpose()
                .unwrap(),
            request.build_context(),
            provenance,
            analyzer.descriptor.tier(),
            limits.ir().max_string_bytes,
            &entities,
        )
        .unwrap();
        entities.insert(draft.local_id, entity);
    }
    let _excluded = exclude_ambiguous_entities(&mut entities);
    operation(&lowering, &entities, output.document());
}
