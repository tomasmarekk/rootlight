//! Handlebars block parameters through actual native parsing and validated IR.
//! Lexical value reads remain separate from helper dispatch and property lookup.

use super::*;

const HANDLEBARS: LanguageCase = LanguageCase {
    name: "handlebars",
    path: "written.hbs",
    frontend: "handlebars-native-source",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};
const ORIGINAL: &[u8] = include_bytes!("../fixtures/handlebars/open-color.inkscape.hbs");
const SKETCH_PALETTE: &[u8] = include_bytes!("../fixtures/handlebars/open-color.sketchpalette.hbs");
const OPENZEPPELIN_CONTRACT: &[u8] =
    include_bytes!("../fixtures/handlebars/openzeppelin/contract.hbs");
const OPENZEPPELIN_PAGE: &[u8] = include_bytes!("../fixtures/handlebars/openzeppelin/page.hbs");

fn output(raw: &[u8], encoding: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(HANDLEBARS, raw);
    let budget = limits();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(HANDLEBARS),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&provider, HANDLEBARS),
        &input,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files.len(), 1);
    assert_eq!(result.document().files[0].content_hash, content_hash(raw));
    assert_eq!(
        result.document().files[0].byte_length,
        u64::try_from(raw.len()).unwrap()
    );
    assert!(result.document().occurrences.iter().all(|site| {
        site.source.content_hash() == content_hash(raw)
            && site.source.span().end_byte() <= u64::try_from(raw.len()).unwrap()
    }));
    result
}

fn definition(
    document: &rootlight_ir::NormalizedIrDocument,
    start: u64,
    end: u64,
    name: &str,
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
    assert_eq!(
        sites.len(),
        1,
        "one parameter definition {name}@{start}..{end}"
    );
    assert_eq!(sites[0].syntax_kind, "handlebars.block_name.definition");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("a written block parameter must own its definition");
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, EntityKind::Parameter);
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.language, "handlebars");
    symbol
}

fn read(document: &rootlight_ir::NormalizedIrDocument, start: u64, end: u64, symbol: SymbolId) {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "one lexical Read@{start}..{end}");
    assert_eq!(sites[0].syntax_kind, "handlebars.block_value.reference");
    let entities: Vec<_> = document
        .entities
        .iter()
        .map(|entity| {
            (
                entity.kind,
                entity.canonical_name.as_str(),
                entity.container,
            )
        })
        .collect();
    assert_eq!(
        sites[0].target,
        OccurrenceTarget::Resolved { symbol },
        "Read@{start}..{end}, entities={entities:?}"
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(sites[0].id)
                    && edge.object == RelationEndpoint::Entity(symbol)
                    && edge.evidence.source.as_ref() == Some(&sites[0].source)
            })
            .count(),
        1
    );
}

fn no_resolved_read_at(document: &rootlight_ir::NormalizedIrDocument, start: usize) {
    assert!(!document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Read
            && site.source.span().start_byte() == u64::try_from(start).unwrap()
            && matches!(site.target, OccurrenceTarget::Resolved { .. })
    }));
}

fn unresolved_data_read(document: &rootlight_ir::NormalizedIrDocument, start: u64, end: u64) {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "one full-token data Read@{start}..{end}");
    assert_eq!(
        sites[0].syntax_kind,
        "handlebars.runtime_value_unavailable.reference"
    );
    assert!(matches!(
        sites[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(!document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Read
            && site.source.span().start_byte() < end
            && site.source.span().end_byte() > start
            && matches!(site.target, OccurrenceTarget::Resolved { .. })
    }));
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::RefersTo
            && (edge.subject == RelationEndpoint::Occurrence(sites[0].id)
                || edge.evidence.source.as_ref().is_some_and(|source| {
                    source.span().start_byte() < end && source.span().end_byte() > start
                }))
    }));
}

#[test]
fn original_parse_facts_preserve_the_required_parameter_and_read_topology() {
    use rootlight_adapter_sdk::SyntaxFactKind;

    let provider = Arc::new(provider());
    let fixture = Fixture::new(HANDLEBARS, ORIGINAL);
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, HANDLEBARS, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        provider.as_ref(),
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let selected: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| {
            matches!(fact.kind(), SyntaxFactKind::Root | SyntaxFactKind::Module)
                || matches!(
                    (fact.span().start_byte(), fact.span().end_byte()),
                    (86, 91) | (122, 125) | (165, 168) | (247, 252) | (156, 159) | (188, 191)
                )
        })
        .map(|fact| {
            (
                fact.kind(),
                fact.syntax_kind().as_str(),
                (fact.span().start_byte(), fact.span().end_byte()),
                fact.parent(),
                fact.depth(),
                fact.local_id(),
            )
        })
        .collect();
    let required = |kind, label: &str, start, end| {
        let matches: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == kind
                    && fact.syntax_kind().as_str() == label
                    && fact.span().start_byte() == start
                    && fact.span().end_byte() == end
            })
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "expected {label}@{start}..{end}: {selected:?}"
        );
        matches[0]
    };
    let root = required(SyntaxFactKind::Root, "handlebars.source_file.root", 0, 297);
    assert_eq!((root.parent(), root.depth()), (None, 0), "{selected:?}");
    let module = required(SyntaxFactKind::Module, "handlebars.file.module", 0, 297);
    assert_eq!(
        (module.parent(), module.depth()),
        (Some(root.local_id()), 1),
        "{selected:?}"
    );
    for (start, end) in [(86, 91), (122, 125), (165, 168)] {
        let declaration = required(
            SyntaxFactKind::Declaration,
            "handlebars.block_parameter.declaration",
            start,
            end,
        );
        assert_eq!(
            (declaration.parent(), declaration.depth()),
            (Some(module.local_id()), 2),
            "{selected:?}"
        );
        let definition = required(
            SyntaxFactKind::Occurrence,
            "handlebars.block_name.definition",
            start,
            end,
        );
        assert_eq!(
            (definition.parent(), definition.depth()),
            (Some(declaration.local_id()), 3),
            "{selected:?}"
        );
    }
    for (start, end) in [(247, 252), (156, 159), (188, 191)] {
        let read = required(
            SyntaxFactKind::Occurrence,
            "handlebars.block_value.reference",
            start,
            end,
        );
        assert_eq!(
            (read.parent(), read.depth()),
            (Some(module.local_id()), 2),
            "{selected:?}"
        );
    }
}

#[test]
fn original_template_retains_three_parameter_definitions_and_authored_read_relations() {
    assert_eq!(ORIGINAL.len(), 297);
    let result = output(ORIGINAL, "utf-8");
    let document = result.document();
    let color = definition(document, 86, 91, "color");
    let hex = definition(document, 122, 125, "hex");
    let rgb = definition(document, 165, 168, "rgb");
    assert_eq!(
        [color, hex, rgb].into_iter().collect::<BTreeSet<_>>().len(),
        3
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        3
    );
    read(document, 247, 252, color);
    read(document, 156, 159, hex);
    read(document, 188, 191, rgb);
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| { gap.detail.contains("handlebars") && gap.detail.contains("unavailable") })
    );
}

#[test]
fn original_sketch_palette_keeps_lexical_parameters_separate_from_parent_data() {
    assert_eq!(SKETCH_PALETTE.len(), 372);
    let result = output(SKETCH_PALETTE, "utf-8");
    let document = result.document();
    let color = definition(document, 78, 83, "color");
    let color_index = definition(document, 84, 94, "colorIndex");
    let hex = definition(document, 126, 129, "hex");
    let rgb = definition(document, 169, 172, "rgb");
    assert_eq!(
        [color, color_index, hex, rgb]
            .into_iter()
            .collect::<BTreeSet<_>>()
            .len(),
        4
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        4
    );
    for (start, end, symbol) in [
        (112, 117, color),
        (160, 163, hex),
        (203, 206, rgb),
        (229, 232, rgb),
        (254, 257, rgb),
    ] {
        read(document, start, end, symbol);
    }
    for (start, end) in [(291, 296), (297, 305)] {
        unresolved_data_read(document, start, end);
    }
}

#[test]
fn original_openzeppelin_templates_retain_modules_and_unresolved_runtime_values() {
    for (name, source) in [
        ("contract", OPENZEPPELIN_CONTRACT),
        ("page", OPENZEPPELIN_PAGE),
    ] {
        let result = output(source, "utf-8");
        let document = result.document();
        assert_eq!(
            document
                .entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Module)
                .count(),
            1,
            "{name}"
        );
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Parameter),
            "{name}"
        );
        assert!(
            document
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Read)
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. })),
            "{name}"
        );
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo),
            "{name}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail.contains("handlebars") && gap.detail.contains("unavailable")),
            "{name}"
        );
        if name == "contract" {
            let text = std::str::from_utf8(source).unwrap();
            let start = u64::try_from(text.find("{{name}}").unwrap() + 2).unwrap();
            unresolved_data_read(document, start, start + 4);
        }
    }
}

#[test]
fn triple_values_preserve_shadowing_and_zero_argument_block_scope() {
    let source = "{{item.name}}{{#each values as |item|}}{{{item.name}}}{{#with item.value as |item|}}{{~{item.name}~}}{{/with}}{{#noop}}{{{item.name}}}{{/noop}}{{item.name}}{{/each}}{{item.name}}";
    let result = output(source.as_bytes(), "utf-8");
    let document = result.document();
    let declarations: Vec<_> = source
        .match_indices("as |item|")
        .map(|(start, _)| u64::try_from(start + 4).unwrap())
        .collect();
    let outer = definition(document, declarations[0], declarations[0] + 4, "item");
    let inner = definition(document, declarations[1], declarations[1] + 4, "item");
    assert_ne!(outer, inner);
    let uses: Vec<_> = source
        .match_indices("item.name")
        .map(|(start, _)| start)
        .collect();
    assert_eq!(uses.len(), 6);
    no_resolved_read_at(document, uses[0]);
    no_resolved_read_at(document, uses[5]);
    for (position, target) in [
        (uses[1], outer),
        (uses[2], inner),
        (uses[3], outer),
        (uses[4], outer),
    ] {
        let start = u64::try_from(position).unwrap();
        read(document, start, start + 4, target);
        no_resolved_read_at(document, position + 5);
    }
    let header = u64::try_from(source.find("with item.value").unwrap() + 5).unwrap();
    read(document, header, header + 4, outer);
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        2
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn escaped_triple_values_stay_literal_beside_a_real_parameter_read() {
    for (prefix, literal) in [("", false), (r"\", true), (r"\\", false), (r"\\\", false)] {
        let source = [
            "{{#each values as |item|}}",
            prefix,
            "{{{item.name}}} {{item.name}}{{/each}}",
        ]
        .concat();
        let result = output(source.as_bytes(), "utf-8");
        let document = result.document();
        let declaration = u64::try_from(source.find("as |item|").unwrap() + 4).unwrap();
        let item = definition(document, declaration, declaration + 4, "item");
        let first = u64::try_from(source.find("item.name").unwrap()).unwrap();
        let genuine = u64::try_from(source.rfind("item.name").unwrap()).unwrap();
        read(document, genuine, genuine + 4, item);
        if !literal {
            read(document, first, first + 4, item);
            continue;
        }
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() < first + 4
                    && site.source.span().end_byte() > first),
            "{source}"
        );
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo
                    && edge.evidence.source.as_ref().is_some_and(|source| source
                        .span()
                        .start_byte()
                        < first + 4
                        && source.span().end_byte() > first)),
            "{source}"
        );
    }
}

#[test]
fn data_paths_stay_unresolved_beside_same_named_lexical_parameters() {
    let source = "{{#each values as |last index|}}{{last}} {{index}} {{@last}} {{@index}} {{@../last}} {{@../../index}}{{/each}}";
    let result = output(source.as_bytes(), "utf-8");
    let document = result.document();
    let declaration = u64::try_from(source.find("as |last index|").unwrap() + 4).unwrap();
    let last = definition(document, declaration, declaration + 4, "last");
    let index = definition(document, declaration + 5, declaration + 10, "index");
    for (token, symbol) in [("{{last}}", last), ("{{index}}", index)] {
        let start = u64::try_from(source.find(token).unwrap() + 2).unwrap();
        read(
            document,
            start,
            start + u64::try_from(token.len() - 4).unwrap(),
            symbol,
        );
    }
    for data in ["@last", "@index", "@../last", "@../../index"] {
        let start = u64::try_from(source.find(data).unwrap()).unwrap();
        unresolved_data_read(document, start, start + u64::try_from(data.len()).unwrap());
    }
}

#[test]
fn malformed_data_paths_refuse_lexical_binding_for_the_file() {
    for data in ["@", "@../", "@.. /last", "@../ last", "@@last"] {
        let source = [
            "{{#each values as |item|}}{{",
            data,
            "}} {{item.name}}{{/each}}",
        ]
        .concat();
        let result = output(source.as_bytes(), "utf-8");
        let document = result.document();
        assert!(
            !document.occurrences.iter().any(|site| {
                site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })
            }),
            "{source}"
        );
        assert!(
            document.skipped_regions.iter().any(|gap| {
                gap.detail.contains("handlebars") && gap.detail.contains("unavailable")
            }),
            "{source}"
        );
    }
}

#[test]
fn nested_same_name_parameters_bind_headers_before_their_own_body_and_end_at_close() {
    let source = "{{item.name}}{{#each values as |item|}}{{item.name}}{{#with item.value as |item|}}{{item.name}}{{/with}}{{item.name}}{{/each}}{{item.name}}";
    let result = output(source.as_bytes(), "utf-8");
    let document = result.document();
    let declarations: Vec<_> = source
        .match_indices("as |item|")
        .map(|(start, _)| u64::try_from(start + 4).unwrap())
        .collect();
    let outer = definition(document, declarations[0], declarations[0] + 4, "item");
    let inner = definition(document, declarations[1], declarations[1] + 4, "item");
    assert_ne!(outer, inner);
    let uses: Vec<_> = source
        .match_indices("{{item.name}}")
        .map(|(start, _)| start + 2)
        .collect();
    assert_eq!(uses.len(), 5);
    no_resolved_read_at(document, uses[0]);
    no_resolved_read_at(document, uses[4]);
    for (position, target) in [(uses[1], outer), (uses[2], inner), (uses[3], outer)] {
        let start = u64::try_from(position).unwrap();
        read(document, start, start + 4, target);
    }
    let header = u64::try_from(source.find("with item.value").unwrap() + 5).unwrap();
    read(document, header, header + 4, outer);
}

#[test]
fn helper_heads_property_suffixes_and_data_variables_do_not_acquire_lexical_targets() {
    let source = "{{#each values as |item|}}{{item item.name}}{{other.item}}{{item.name}}{{@index}}{{/each}}";
    let result = output(source.as_bytes(), "utf-8");
    let document = result.document();
    let declaration = u64::try_from(source.find("as |item|").unwrap() + 4).unwrap();
    let item = definition(document, declaration, declaration + 4, "item");
    for (position, _) in source.match_indices("item.name") {
        let start = u64::try_from(position).unwrap();
        read(document, start, start + 4, item);
    }
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })
            })
            .count(),
        2
    );
    assert!(
        document
            .relations
            .iter()
            .all(|edge| edge.predicate != RelationPredicate::Calls)
    );
}

#[test]
fn raw_host_text_keeps_only_handlebars_parameter_and_read_semantics() {
    for (host, body) in [
        ("css", ".swatch { color: {{item~}}; }"),
        ("json", r#"{"shade":"{{item.name}}","value":1}"#),
        ("typescript", "const palette = { shade: '{{item.name}}' };"),
        ("tex", r"\textcolor{blue}{ {{item.name}} }"),
    ] {
        let source = format!("{{{{#each values as |item|}}}}{body}{{{{/each}}}}");
        let result = output(source.as_bytes(), "utf-8");
        let document = result.document();
        let declaration = u64::try_from(source.find("as |item|").unwrap() + 4).unwrap();
        let item = definition(document, declaration, declaration + 4, "item");
        let usage = u64::try_from(source.find("{{item").unwrap() + 2).unwrap();
        read(document, usage, usage + 4, item);
        assert_eq!(document.files[0].language, "handlebars", "{host}");
        assert_eq!(
            document
                .entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Parameter)
                .count(),
            1,
            "{host}"
        );
    }
}

#[test]
fn escaped_mustache_stays_literal_while_the_following_value_read_resolves() {
    for (prefix, suffix, literal) in [
        ("", "", false),
        (r"\", "", true),
        (r"\\", "", false),
        (r"\\\", "", false),
        (r"\\\\", "", false),
        (r"\ ", "", false),
        (r"\\ ", "", false),
        (r#"<span data-name="\"#, r#""></span>"#, true),
    ] {
        let source = [
            "{{#each values as |item|}}",
            prefix,
            "{{item.name}}",
            suffix,
            " {{item.name}}{{/each}}",
        ]
        .concat();
        let result = output(source.as_bytes(), "utf-8");
        let document = result.document();
        let declaration = u64::try_from(source.find("as |item|").unwrap() + 4).unwrap();
        let item = definition(document, declaration, declaration + 4, "item");
        let first = u64::try_from(source.find("item.name").unwrap()).unwrap();
        let genuine = u64::try_from(source.rfind("item.name").unwrap()).unwrap();
        read(document, genuine, genuine + 4, item);
        if !literal {
            read(document, first, first + 4, item);
            continue;
        }
        let overlaps_literal =
            |span: SourceSpan| span.start_byte() < first + 4 && first < span.end_byte();
        assert!(
            !document.occurrences.iter().any(|site| {
                site.role == OccurrenceRole::Read && overlaps_literal(site.source.span())
            }),
            "escaped mustache must remain literal text: {source}"
        );
        assert!(
            !document.relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| overlaps_literal(source.span()))
            }),
            "escaped mustache must not acquire a lexical relation: {source}"
        );
    }
}

#[test]
fn escaped_block_headers_and_closes_do_not_change_real_outer_parameter_scope() {
    for body in [
        r"\{{#with other as |ghost|}}\{{/with}}\{{/each}}",
        r"\{{~#with other as |ghost|~}}\{{~/with~}}\{{~/each~}}",
    ] {
        let source = [
            "{{#each values as |item|}}{{item.name}} ",
            body,
            " {{item.name}}{{/each}}",
        ]
        .concat();
        let result = output(source.as_bytes(), "utf-8");
        let document = result.document();
        let declaration = u64::try_from(source.find("as |item|").unwrap() + 4).unwrap();
        let item = definition(document, declaration, declaration + 4, "item");
        let parameters: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .map(|entity| entity.canonical_name.as_str())
            .collect();
        assert_eq!(parameters, ["item"], "{source}");
        for (position, _) in source.match_indices("item.name") {
            let start = u64::try_from(position).unwrap();
            read(document, start, start + 4, item);
        }
    }
}

#[test]
fn invalid_ambiguous_and_inverse_blocks_keep_source_without_resolved_parameter_reads() {
    for source in [
        "{{#each values as |item|}}{{item.name}}{{/with}}",
        "{{#each values as |item item|}}{{item.name}}{{/each}}",
        "{{#each values as |item|}}{{item.name}}{{else}}{{item.name}}{{/each}}",
        "{{#each values as |item|}}{{item.name}}",
        "{{#each values as |item|}}{{item.name}{{/each}}",
        "{{#each values as |item|}}{{{item.name}}{{/each}}",
        "{{#each values as |item|}}{{{}}}{{item.name}}{{/each}}",
        "{{#each values as |item|}}{{{~item.name}}}{{/each}}",
        "{{#each values as |item|}}{{{item.name~}}}{{/each}}",
        "{{#each values as |item|}}{{#noop}}{{item.name}}{{/other}}{{/each}}",
        "{{#each values as |item item|}}{{{item.name}}}{{/each}}",
        "{{#each values as |item|}}{{{item.name}}}{{else}}{{item.name}}{{/each}}",
        "<!DOCTYPE palette [<!ENTITY shade \"blue\">]><palette>{{#each values as |item|}}{{item.name}}{{/each}}</palette>",
    ] {
        let result = output(source.as_bytes(), "utf-8");
        let document = result.document();
        assert!(
            !document.occurrences.iter().any(|site| {
                site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })
            }),
            "{source}"
        );
        assert!(
            document.skipped_regions.iter().any(|gap| {
                gap.detail.contains("handlebars") && gap.detail.contains("unavailable")
            }),
            "{source}"
        );
    }
}

#[test]
fn utf16_both_orders_preserve_exact_physical_parameter_and_read_bytes() {
    let text = "é🧭\r\n{{#each values as |item|}}{{item.name}}{{/each}}";
    for (encoding, little) in [("utf-16le", true), ("utf-16be", false)] {
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
        let result = output(&raw, encoding);
        let declaration = text.find("as |item|").unwrap() + 4;
        let usage = text.find("item.name").unwrap();
        let physical =
            |offset| 2 + u64::try_from(text[..offset].encode_utf16().count() * 2).unwrap();
        let symbol = definition(
            result.document(),
            physical(declaration),
            physical(declaration) + 8,
            "item",
        );
        read(
            result.document(),
            physical(usage),
            physical(usage) + 8,
            symbol,
        );
    }
}

#[test]
fn cancelled_template_analysis_returns_cancellation_before_publication() {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(HANDLEBARS, ORIGINAL);
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, HANDLEBARS, &budget);
    let cancellation = Cancellation::new();
    assert!(cancellation.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        execute_analysis(
            &analyzer(&provider, HANDLEBARS),
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancellation,
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
}
