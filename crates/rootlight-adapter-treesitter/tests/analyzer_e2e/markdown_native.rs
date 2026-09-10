//! Written document ownership through the native block parser and validated IR.
//! Heading text remains authored source, not a renderer-specific fragment ID.

use super::*;

const MARKDOWN: LanguageCase = LanguageCase {
    name: "markdown",
    path: "docs/storage.md",
    frontend: "tree-sitter-md-0.5.3",
    source: "# Storage *rules*\r\n\r\nIntro.\r\n\r\n## Shared cache\r\n\r\n[Guide]: <guide.md> \"Help\"\r\n\r\nBody.\r\n\r\nOther section\r\n=============\r\nTail.\r\n",
    generated: false,
    body_before: "Body.",
    body_after: "Longer replacement body.",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, MARKDOWN),
        &request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn markdown_matlab_bindings_keep_host_spans_and_separate_examples() {
    let source = "# Variables λ😀\r\n```matlab\r\nvalue = 1;\r\nvalue = value + 1;\r\n```\r\n```matlab\r\nvalue = 2;\r\nvalue = value + 2;\r\n```\r\n```matlab\r\nresult = value;\r\n```\r\n";
    let result = output(source);
    let variables: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.language == "matlab" && entity.canonical_name == "value")
        .collect();
    assert_eq!(variables.len(), 2);
    assert_ne!(variables[0].id, variables[1].id);
    for variable in variables {
        let sites: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.target
                    == OccurrenceTarget::Resolved {
                        symbol: variable.id,
                    }
            })
            .collect();
        assert_eq!(sites.len(), 3);
        for role in [
            OccurrenceRole::Definition,
            OccurrenceRole::Write,
            OccurrenceRole::Reference,
        ] {
            assert_eq!(sites.iter().filter(|site| site.role == role).count(), 1);
        }
        let definition = sites
            .iter()
            .find(|site| site.role == OccurrenceRole::Definition)
            .unwrap()
            .source
            .span()
            .start_byte();
        for site in sites {
            let span = site.source.span();
            assert_eq!(
                source.get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()
                ),
                Some("value")
            );
            assert!(span.start_byte() >= definition);
            assert!(span.start_byte() - definition < 32);
        }
    }
    let offset = source.find("result = value").unwrap() + "result = ".len();
    let read = result
        .document()
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == u64::try_from(offset).unwrap()
        })
        .unwrap();
    assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
}

#[test]
fn markdown_perl_bindings_keep_host_sources_and_separate_lexical_pads() {
    let source = "# Pads λ😀\r\n```perl\r\nmy $value = 1; print $value;\r\n```\r\n```perl\r\nprint $value;\r\n```\r\n";
    let result = output(source);
    let document = result.document();
    let target = document
        .entities
        .iter()
        .find(|entity| entity.language == "perl" && entity.canonical_name == "$value")
        .unwrap();
    let mut reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "perl.variable_name.reference")
        .collect();
    reads.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(reads.len(), 2);
    assert_eq!(
        reads[0].target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    assert!(matches!(
        reads[1].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    for read in reads {
        let span = read.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some("$value")
        );
        assert_eq!(read.syntactic_text_hash, content_hash(b"$value"));
    }
}

#[test]
fn markdown_perl_package_storage_does_not_escape_its_example() {
    let source = "# Packages λ😀\r\n```perl\r\npackage Harbor; our $value = 1; print $Harbor::value;\r\n```\r\n```perl\r\npackage Harbor; print $Harbor::value;\r\n```\r\n";
    let result = output(source);
    let document = result.document();
    let namespaces: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.language == "perl" && entity.canonical_name == "Harbor")
        .collect();
    assert_eq!(namespaces.len(), 2);
    assert_ne!(namespaces[0].id, namespaces[1].id);
    let variable = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "$value")
        .unwrap();
    let mut reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "perl.variable_name.reference")
        .collect();
    reads.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(reads.len(), 2);
    assert_eq!(
        reads[0].target,
        OccurrenceTarget::Resolved {
            symbol: variable.id
        }
    );
    assert!(matches!(
        reads[1].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    for read in reads {
        let span = read.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some("$Harbor::value")
        );
        assert_eq!(read.syntactic_text_hash, content_hash(b"$Harbor::value"));
    }
}

#[test]
fn markdown_perl_bare_calls_do_not_borrow_later_or_neighbor_declarations() {
    let source = "# Functions λ😀\r\n```perl\r\npackage Harbor; sub value { 13 } my $result = value;\r\n```\r\n```perl\r\npackage Harbor; my $before = value; sub value { 29 } my $after = value;\r\n```\r\n";
    let result = output(source);
    let document = result.document();
    let mut definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.syntactic_text_hash == content_hash(b"value")
        })
        .collect();
    definitions.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(definitions.len(), 2);
    assert_ne!(definitions[0].target, definitions[1].target);
    let mut calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "perl.bare_function_name.reference")
        .collect();
    calls.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(calls.len(), 3);
    for (site, target) in calls.into_iter().zip([Some(0), None, Some(1)]) {
        let span = site.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some("value")
        );
        assert_eq!(site.syntactic_text_hash, content_hash(b"value"));
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.predicate == RelationPredicate::Calls
            })
            .collect();
        if let Some(target) = target {
            assert_eq!(site.target, definitions[target].target);
            assert_eq!(site.role, OccurrenceRole::CallSite);
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].evidence.source.as_ref(), Some(&site.source));
        } else {
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
            assert!(edges.is_empty());
        }
    }
}

#[test]
fn markdown_perl_qualified_storage_and_root_aliases_stay_inside_their_example() {
    let source = "# Functions λ😀\r\n```perl\r\nsub main::Cove::value { 13 } ::Cove::value();\r\n```\r\n```perl\r\nsub Cove::value { 29 } main::Cove::value();\r\n```\r\n```perl\r\nCove::value();\r\n```\r\n";
    let result = output(source);
    let document = result.document();
    let functions: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function)
        .collect();
    assert_eq!(functions.len(), 2);
    assert_ne!(functions[0].id, functions[1].id);
    assert!(
        functions.iter().all(
            |entity| entity.canonical_name == "value" && entity.qualified_name == "Cove::value"
        )
    );
    let mut definitions: Vec<_> = document.occurrences.iter().filter(|site| {
        site.role == OccurrenceRole::Definition && matches!(site.target,
            OccurrenceTarget::Resolved { symbol } if functions.iter().any(|entity| entity.id == symbol))
    }).collect();
    definitions.sort_by_key(|site| site.source.span().start_byte());
    let mut calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "perl.static_function_name.reference")
        .collect();
    calls.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(definitions.len(), 2);
    assert_eq!(calls.len(), 3);
    for (site, (name, target)) in calls.iter().zip([
        ("::Cove::value", Some(0)),
        ("main::Cove::value", Some(1)),
        ("Cove::value", None),
    ]) {
        assert_eq!(site.syntactic_text_hash, content_hash(name.as_bytes()));
        let span = site.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(name)
        );
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.predicate == RelationPredicate::Calls
            })
            .collect();
        if let Some(target) = target {
            assert_eq!(site.target, definitions[target].target);
            assert_eq!(site.role, OccurrenceRole::CallSite);
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].evidence.source.as_ref(), Some(&site.source));
        } else {
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
            assert!(edges.is_empty());
        }
    }
}

#[test]
fn markdown_perl_function_imports_and_storage_stay_inside_their_example() {
    let source = "# Functions λ😀\r\n```perl\r\npackage Harbor; use subs qw(reverse); sub reverse { 29 } reverse('abc');\r\n```\r\n```perl\r\npackage Harbor; sub reverse { 41 } reverse('abc'); &reverse();\r\n```\r\n";
    assert_perl_function_example_isolation(source);
}

#[test]
fn markdown_perl_grouped_imports_stay_inside_their_example() {
    let source = "# Functions λ😀\r\n```perl\r\npackage Harbor; use subs ((('reverse'))); sub reverse { 29 } reverse('abc');\r\n```\r\n```perl\r\npackage Harbor; sub reverse { 41 } reverse('abc'); &reverse();\r\n```\r\n";
    assert_perl_function_example_isolation(source);
}

fn assert_perl_function_example_isolation(source: &str) {
    let result = output(source);
    let document = result.document();
    let mut definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.syntactic_text_hash == content_hash(b"reverse")
        })
        .collect();
    definitions.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(definitions.len(), 2);
    assert_ne!(definitions[0].target, definitions[1].target);
    let mut calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            matches!(
                site.syntax_kind.as_str(),
                "perl.importable_function_name.reference" | "perl.amper_function_name.reference"
            )
        })
        .collect();
    calls.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].target, definitions[0].target);
    assert!(matches!(
        calls[1].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_eq!(calls[2].target, definitions[1].target);
    for (call, written) in calls.into_iter().zip(["reverse", "reverse", "&reverse"]) {
        let span = call.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(written)
        );
        assert_eq!(call.syntactic_text_hash, content_hash(written.as_bytes()));
        if let OccurrenceTarget::Resolved { symbol } = call.target {
            assert_eq!(call.role, OccurrenceRole::CallSite);
            let edges: Vec<_> = document
                .relations
                .iter()
                .filter(|edge| {
                    edge.subject == RelationEndpoint::Occurrence(call.id)
                        && edge.predicate == RelationPredicate::Calls
                })
                .collect();
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].object, RelationEndpoint::Entity(symbol));
            assert_eq!(edges[0].evidence.source.as_ref(), Some(&call.source));
        }
    }
}

#[test]
fn markdown_matlab_local_calls_do_not_escape_their_example() {
    let source = "# Calls λ😀\r\n```matlab\r\nfunction result = entry(value)\r\nresult = helper(value);\r\nend\r\nfunction result = helper(value)\r\nresult = value;\r\nend\r\n```\r\n```matlab\r\nresult = helper(1);\r\n```\r\n";
    let result = output(source);
    let calls: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(calls.len(), 1);
    let span = calls[0].source.span();
    assert_eq!(
        source.get(
            usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap()
        ),
        Some("helper(value)")
    );
    let offset = u64::try_from(source.find("helper(1)").unwrap()).unwrap();
    let unresolved: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| site.source.span().start_byte() == offset)
        .collect();
    assert_eq!(unresolved.len(), 2);
    assert!(
        unresolved
            .iter()
            .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
}

#[test]
fn markdown_nix_merged_sets_preserve_recursion_and_host_definition_sites() {
    let source = "# Sets λ😀\r\n```nix\r\nlet a = rec { field = 1; }; a.added = field; in a\r\n```\r\n```nix\r\nlet a = rec { field = 2; }; a.added = field; in a\r\n```\r\n";
    let result = output(source);
    let fields: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.language == "nix" && entity.canonical_name == "field")
        .collect();
    assert_eq!(fields.len(), 2);
    assert_ne!(fields[0].id, fields[1].id);
    for field in fields {
        let span = field.evidence.source.as_ref().unwrap().span();
        let reads: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|item| {
                item.role == OccurrenceRole::Reference
                    && item.target == (OccurrenceTarget::Resolved { symbol: field.id })
            })
            .collect();
        assert_eq!(reads.len(), 1);
        let read = reads[0].source.span();
        assert_eq!(
            source.get(
                usize::try_from(read.start_byte()).unwrap()
                    ..usize::try_from(read.end_byte()).unwrap()
            ),
            Some("field")
        );
        assert!(read.start_byte() > span.end_byte());
        assert!(read.start_byte() - span.end_byte() < 24);
    }
}

#[test]
fn markdown_nix_literal_keys_preserve_host_spelling_and_example_scope() {
    let source = "# Keys λ😀\r\n```nix\r\nlet ${\"name\"} = 1; in name\r\n```\r\n```nix\r\nlet ${\"name\"} = 2; in name\r\n```\r\n";
    let result = output(source);
    let names: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|item| item.language == "nix" && item.canonical_name == r#"${"name"}"#)
        .collect();
    assert_eq!(names.len(), 2);
    assert_ne!(names[0].id, names[1].id);
    for name in names {
        let definition = result
            .document()
            .occurrences
            .iter()
            .find(|item| {
                item.role == OccurrenceRole::Definition
                    && item.target == (OccurrenceTarget::Resolved { symbol: name.id })
            })
            .unwrap();
        let span = definition.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(r#"${"name"}"#)
        );
        let reads: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|item| {
                item.role == OccurrenceRole::Reference
                    && item.target == (OccurrenceTarget::Resolved { symbol: name.id })
            })
            .collect();
        assert_eq!(reads.len(), 1);
        let read = reads[0].source.span();
        assert_eq!(
            source.get(
                usize::try_from(read.start_byte()).unwrap()
                    ..usize::try_from(read.end_byte()).unwrap()
            ),
            Some("name")
        );
        assert!(read.start_byte() > span.end_byte());
        assert!(read.start_byte() - span.end_byte() < 16);
    }
}

#[test]
fn markdown_nix_inherit_from_fields_keep_source_and_local_example_identities() {
    let source = "# Settings λ😀\r\n```nix\r\nlet inherit ({ port = 1; }) port; in port\r\n```\r\n```nix\r\nlet inherit ({ port = 2; }) port; in port\r\n```\r\n";
    let result = output(source);
    let fields: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|item| item.language == "nix" && item.canonical_name == "port")
        .collect();
    assert_eq!(fields.len(), 4);
    for field in fields {
        let definition = field.evidence.source.as_ref().unwrap().span();
        let reads: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|item| {
                item.role == OccurrenceRole::Reference
                    && item.target == (OccurrenceTarget::Resolved { symbol: field.id })
            })
            .collect();
        assert_eq!(reads.len(), 1);
        let read = reads[0].source.span();
        assert_eq!(
            source.get(
                usize::try_from(read.start_byte()).unwrap()
                    ..usize::try_from(read.end_byte()).unwrap()
            ),
            Some("port")
        );
        assert!(read.start_byte() > definition.end_byte());
        assert!(read.start_byte() - definition.end_byte() < 24);
    }
}

#[test]
fn markdown_nix_selected_attributes_keep_each_example_and_host_source() {
    let source = "# Settings λ😀\r\n```nix\r\nlet settings = { port = 1; }; in settings.port\r\n```\r\n```nix\r\nlet settings = { port = 2; }; in settings.port\r\n```\r\n";
    let result = output(source);
    let fields: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|item| item.language == "nix" && item.canonical_name == "port")
        .collect();
    assert_eq!(fields.len(), 2);
    assert_ne!(fields[0].id, fields[1].id);
    for field in fields {
        let definition = field.evidence.source.as_ref().unwrap().span();
        let reads: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|item| {
                item.role == OccurrenceRole::Reference
                    && item.target == (OccurrenceTarget::Resolved { symbol: field.id })
            })
            .collect();
        assert_eq!(reads.len(), 1);
        let read = reads[0].source.span();
        assert_eq!(
            source.get(
                usize::try_from(read.start_byte()).unwrap()
                    ..usize::try_from(read.end_byte()).unwrap()
            ),
            Some("port")
        );
        assert!(read.start_byte() > definition.end_byte());
        assert!(read.start_byte() - definition.end_byte() < 32);
    }
}

#[test]
fn markdown_nix_implicit_path_roots_keep_host_sources_and_separate_examples() {
    let source = "# Roots λ😀\r\n```nix\r\nlet a.b = 1; a.c = 2; in a\r\n```\r\n```nix\r\nlet a.d = 3; in a\r\n```\r\n";
    let result = output(source);
    let roots: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.language == "nix" && entity.canonical_name == "a")
        .collect();
    assert_eq!(roots.len(), 2);
    assert_ne!(roots[0].id, roots[1].id);
    for root in roots {
        let evidence = root.evidence.source.as_ref().unwrap();
        assert_eq!(
            source.get(
                usize::try_from(evidence.span().start_byte()).unwrap()
                    ..usize::try_from(evidence.span().end_byte()).unwrap()
            ),
            Some("a")
        );
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role == OccurrenceRole::Reference
                    && occurrence.target == (OccurrenceTarget::Resolved { symbol: root.id }))
        );
    }
}

#[test]
fn markdown_nix_bindings_retain_host_coordinates_and_separate_example_owners() {
    let source = "# Examples λ😀\r\n\r\n```nix\r\nlet identity = value: value; in identity 1\r\n```\r\n\r\n```nix\r\nlet identity = value: value; in identity 2\r\n```\r\n";
    let result = output(source);
    for (name, kind) in [
        ("identity", EntityKind::Function),
        ("value", EntityKind::Parameter),
    ] {
        let entities: Vec<_> = result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
            .collect();
        assert_eq!(entities.len(), 2, "{:?}", result.document().entities);
        assert_ne!(entities[0].id, entities[1].id);
        for entity in entities {
            assert_eq!(entity.language, "nix");
            let definition = result
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.role == OccurrenceRole::Definition
                        && occurrence.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                })
                .unwrap();
            let span = definition.source.span();
            assert_eq!(
                source.get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()
                ),
                Some(name)
            );
        }
    }
}

#[test]
fn markdown_nix_lexical_bindings_do_not_cross_example_boundaries() {
    let source = "# Examples λ😀\r\n\r\n```nix\r\nlet value = 1; in value\r\n```\r\n\r\n```nix\r\nvalue\r\n```\r\n";
    assert_markdown_nix_lexical_boundaries(source, "value");
}

#[test]
fn markdown_nix_quoted_lexical_bindings_do_not_cross_example_boundaries() {
    let source = "# Examples λ😀\r\n\r\n```nix\r\nlet \"\\value\" = 1; in value\r\n```\r\n\r\n```nix\r\nvalue\r\n```\r\n";
    assert_markdown_nix_lexical_boundaries(source, r#""\value""#);
}

fn assert_markdown_nix_lexical_boundaries(source: &str, definition: &str) {
    let result = output(source);
    let document = result.document();
    let target = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == definition)
        .unwrap();
    let mut reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "nix.identifier.reference")
        .collect();
    reads.sort_by_key(|occurrence| occurrence.source.span().start_byte());
    assert_eq!(reads.len(), 2);
    assert_eq!(
        reads[0].target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    assert!(matches!(
        reads[1].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    for read in reads {
        let span = read.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some("value")
        );
    }
}

#[test]
fn markdown_objective_c_generic_bindings_retain_host_sources_and_distinct_owners() {
    let fixture = include_str!("../../../../tests/fixtures/objective-c/generics.m");
    let source = format!(
        "# Generic types\n\n```objective-c\n{fixture}\n```\n\n```objective-c\n{fixture}\n```\n"
    );
    let result = output(&source);
    let parameters: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::TypeParameter)
        .collect();
    assert_eq!(parameters.len(), 14);
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Method && entity.canonical_name == "extra")
            .count(),
        2
    );
    assert_eq!(
        parameters
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        14
    );
    for parameter in parameters {
        assert_eq!(parameter.language, "objective-c");
        let reference = parameter.evidence.source.as_ref().unwrap();
        let span = reference.span();
        let written = source
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap();
        assert_eq!(written, parameter.canonical_name);
        let occurrence = result
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target
                        == OccurrenceTarget::Resolved {
                            symbol: parameter.id,
                        }
            })
            .unwrap();
        assert_eq!(&occurrence.source, reference);
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(written.as_bytes())
        );
    }
}

#[test]
fn markdown_swift_extension_examples_keep_distinct_stable_method_identities() {
    let fixture = "struct Entry {}\nextension Entry { func copy() -> Entry { return self } }\n";
    let source = format!("# Extensions\n\n```swift\n{fixture}\n```\n\n```swift\n{fixture}\n```\n");
    let initial = output(&source);
    let methods: Vec<_> = initial
        .document()
        .entities
        .iter()
        .filter(|entity| {
            entity.language == "swift"
                && entity.kind == EntityKind::Method
                && entity.canonical_name == "copy"
        })
        .collect();
    assert_eq!(methods.len(), 2);
    let ids: BTreeSet<_> = methods.iter().map(|entity| entity.id).collect();
    assert_eq!(ids.len(), 2);
    for method in methods {
        let reference = method.evidence.source.as_ref().unwrap();
        let span = reference.span();
        assert_eq!(
            source
                .get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()
                )
                .unwrap(),
            "func copy() -> Entry { return self }"
        );
    }
    let changed = output(&source.replace("return self", "return  self"));
    assert_eq!(
        ids,
        changed
            .document()
            .entities
            .iter()
            .filter(|entity| entity.language == "swift"
                && entity.kind == EntityKind::Method
                && entity.canonical_name == "copy")
            .map(|entity| entity.id)
            .collect()
    );
}

#[test]
fn markdown_objective_c_forward_declarations_keep_host_sources_and_separate_examples() {
    let fixture = include_str!("../../../../tests/fixtures/objective-c/forwards.m");
    let source = format!(
        "# Forward types\n\n```objective-c\n{fixture}\n```\n\n```objective-c\n{fixture}\n```\n"
    );
    let result = output(&source);
    let declarations: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Declaration)
        .collect();
    assert_eq!(declarations.len(), 14);
    let mut identities = BTreeSet::new();
    for declaration in declarations {
        let OccurrenceTarget::Resolved { symbol } = declaration.target else {
            panic!("embedded forward declaration has its written identity");
        };
        identities.insert(symbol);
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(entity.language, "objective-c");
        let span = declaration.source.span();
        let written = source
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap();
        assert_eq!(written, entity.canonical_name);
        assert_eq!(
            declaration.syntactic_text_hash,
            content_hash(written.as_bytes())
        );
    }
    assert_eq!(identities.len(), 14);
}

#[test]
fn markdown_lua_bindings_keep_visibility_and_example_boundaries() {
    let source = "# Examples\r\n\r\n~~~lua\r\nlocal outer = 1\r\ndo\r\n  local outer = outer\r\n  local function capture(parameter)\r\n    local snapshot = outer\r\n    local recursive = capture\r\n    return parameter\r\n  end\r\nend\r\nreturn outer\r\n~~~\r\n\r\n~~~lua\r\nlocal first = outer\r\nlocal outer = 2\r\nlocal second = outer\r\nlocal assigned = function() return assigned end\r\n~~~\r\n\r\n~~~rust\r\nfn foreign() {}\r\n~~~\r\n\r\n~~~lua\r\nlocal third = outer\r\nlocal missing = foreign\r\n~~~\r\n";
    let result = assert_lua_reference_bindings_in(
        MARKDOWN,
        source,
        &[
            ("local outer = outer", "outer", Some("local outer = 1")),
            (
                "local snapshot = outer",
                "outer",
                Some("local outer = outer"),
            ),
            (
                "local recursive = capture",
                "capture",
                Some("local function capture"),
            ),
            ("return parameter", "parameter", Some("parameter)")),
            ("return outer", "outer", Some("local outer = 1")),
            ("local first = outer", "outer", None),
            ("local second = outer", "outer", Some("local outer = 2")),
            ("return assigned", "assigned", None),
            ("local third = outer", "outer", None),
            ("local missing = foreign", "foreign", None),
        ],
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "markdown-code-semantics-unavailable")
    );
    assert_eq!(result.report().coverage().status(), CoverageStatus::Bounded);
}

#[test]
fn markdown_lua_bounded_plans_keep_references_without_guessing_targets() {
    let source = format!(
        "> paragraph\n{}\n~~~lua\nlocal value = 1\nreturn value\n~~~\n",
        "> continued\n".repeat(33)
    );
    let result =
        assert_lua_reference_bindings_in(MARKDOWN, &source, &[("return value", "value", None)]);
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "markdown-inline-budget-unavailable")
    );
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == rootlight_ir::RelationPredicate::RefersTo)
    );
}

#[test]
fn markdown_lua_binding_artifacts_rebind_exact_relations() {
    let source = "~~~lua\nlocal value = 1\nreturn value\n~~~\n\n~~~lua\nlocal value = 2\nreturn value\n~~~\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, MARKDOWN);
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let changed = fixture.next_generation();
    let next = request(&changed.snapshot, &changed.source, MARKDOWN, &budget);
    let replay = analyzer
        .analyze_from_artifact(
            &next,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(&analyzer, &next, &ExtensionSupport::default());
    assert_eq!(replay.document(), fresh.document());
    assert_eq!(replay.report(), fresh.report());
    let references: Vec<_> = first
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "lua.identifier.reference")
        .collect();
    assert_eq!(references.len(), 2);
    assert_ne!(references[0].target, references[1].target);
    for occurrence in references {
        assert!(matches!(
            occurrence.target,
            OccurrenceTarget::Resolved { .. }
        ));
        let current = replay
            .document()
            .occurrences
            .iter()
            .find(|item| {
                item.syntax_kind == occurrence.syntax_kind
                    && item.source.span() == occurrence.source.span()
            })
            .unwrap();
        assert_eq!(current.target, occurrence.target);
        assert_eq!(current.source.generation(), changed.source.generation());
    }
    let relations: Vec<_> = replay
        .document()
        .relations
        .iter()
        .filter(|relation| relation.predicate == rootlight_ir::RelationPredicate::RefersTo)
        .collect();
    assert_eq!(relations.len(), 2);
    for relation in relations {
        assert_eq!(
            relation.evidence.source.as_ref().unwrap().generation(),
            changed.source.generation()
        );
    }
}

#[test]
fn markdown_code_labels_require_declared_languages_without_content_inference() {
    for (label, supported) in [
        ("rust", true),
        ("rs", true),
        ("", false),
        ("text", false),
        ("rusty", false),
        ("rust&#32;", false),
    ] {
        let source = format!("~~~{label}\nfn example() {{}}\n~~~\n");
        let result = output(&source);
        assert_eq!(
            result
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Function
                    && entity.canonical_name == "example"),
            supported,
            "{label}"
        );
        assert_eq!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "markdown-embedded-analysis-unavailable"),
            !supported,
            "{label}"
        );
    }
}

#[test]
fn markdown_foreign_code_keeps_its_range_budget_after_native_paragraphs() {
    let source = format!(
        "{}{}",
        "Paragraph.\n\n".repeat(31),
        (0..33)
            .map(|index| format!("~~~rust\nfn item_{index}() {{}}\n~~~\n\n"))
            .collect::<String>()
    );
    let result = output(&source);
    let names: BTreeSet<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function)
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    assert_eq!(names.len(), 32);
    assert!(names.contains("item_0"));
    assert!(!names.contains("item_32"));
    let gaps: Vec<_> = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "markdown-code-budget-unavailable")
        .collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].reason, SkippedRegionReason::ResourceLimit);
    let span = gaps[0].source.span();
    assert_eq!(
        &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()],
        "~~~rust\nfn item_32() {}\n~~~\n"
    );
}

#[test]
fn markdown_code_parse_errors_preserve_healthy_neighboring_examples() {
    let source = "~~~rust\nfn broken( {\n~~~\n\n~~~rust\nfn healthy() {}\n~~~\n";
    let result = output(source);
    assert!(result.document().entities.iter().any(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "healthy"));
    let gap = result
        .document()
        .skipped_regions
        .iter()
        .find(|gap| gap.detail == "markdown-code-parse-unavailable")
        .unwrap();
    assert_eq!(gap.reason, SkippedRegionReason::ParseError);
    let span = gap.source.span();
    assert_eq!(
        &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()],
        "~~~rust\nfn broken( {\n~~~\n"
    );
}

#[test]
fn markdown_fenced_definitions_match_standalone_language_evidence() {
    let provider = Arc::new(provider());
    let budget = limits();
    let cases: Vec<_> = CASES
        .into_iter()
        .chain([
            CSS_CASE,
            JSON_CASE,
            TOML_CASE,
            yaml_native::YAML,
            html_native::HTML,
            astro_native::ASTRO,
            sql_native::SQL,
            r_native::R,
            solidity_native::SOLIDITY,
            scala_native::SCALA,
            dart_native::DART,
            powershell_native::POWERSHELL,
            objective_c_native::OBJECTIVE_C,
            nix_native::NIX,
            matlab_native::MATLAB,
            perl_native::PERL,
        ])
        .collect();
    let expected_languages: BTreeSet<_> = rootlight_adapter_treesitter::GrammarRegistry::audited()
        .unwrap()
        .descriptors()
        .iter()
        .filter(|descriptor| descriptor.language().as_str() != "markdown")
        .map(|descriptor| descriptor.language().as_str().to_owned())
        .collect();
    assert_eq!(
        cases
            .iter()
            .map(|case| case.name.to_owned())
            .collect::<BTreeSet<_>>(),
        expected_languages
    );
    for case in cases {
        let fixture = Fixture::new(case, case.source.as_bytes());
        let standalone = analyze(
            &analyzer(&provider, case),
            &request(&fixture.snapshot, &fixture.source, case, &budget),
            &ExtensionSupport::default(),
        );
        let source = format!(
            "# Example\n\n~~~~~~~~{}\n{}\n~~~~~~~~\n",
            case.name, case.source
        );
        let embedded = output(&source);
        let definitions = |result: &AnalysisOutput, source: &str| {
            let mut values: Vec<_> = result
                .document()
                .occurrences
                .iter()
                .filter_map(|occurrence| {
                    if occurrence.role != OccurrenceRole::Definition {
                        return None;
                    }
                    let OccurrenceTarget::Resolved { symbol } = occurrence.target else {
                        return None;
                    };
                    let entity = result
                        .document()
                        .entities
                        .iter()
                        .find(|entity| entity.id == symbol)
                        .unwrap();
                    if entity.language != case.name {
                        return None;
                    }
                    let span = occurrence.source.span();
                    let text = &source[usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()];
                    assert_eq!(
                        occurrence.syntactic_text_hash,
                        content_hash(text.as_bytes())
                    );
                    Some((entity.kind, entity.canonical_name.clone(), text.to_owned()))
                })
                .collect();
            values.sort();
            values
        };
        let expected = definitions(&standalone, case.source);
        assert!(!expected.is_empty(), "{}", case.name);
        assert_eq!(definitions(&embedded, &source), expected, "{}", case.name);
    }
}

#[test]
fn markdown_objective_c_preserves_unary_dispatch_signatures() {
    let source = "~~~objective-c\n@interface Sample\n- (int)value;\n+ (int)value;\n@end\n~~~\n";
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &provider(),
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let signatures: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "objective_c.method.signature")
        .collect();
    assert_eq!(signatures.len(), 2, "{signatures:?}");
    for signature in &signatures {
        let parent = parsed
            .facts()
            .iter()
            .find(|fact| Some(fact.local_id()) == signature.parent())
            .unwrap();
        assert_eq!(
            parent.syntax_kind().as_str(),
            "objective_c.method.declaration",
            "{signature:?}; {parent:?}"
        );
    }
    let definitions: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "objective_c.method.definition")
        .collect();
    for definition in definitions {
        let parts: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.syntax_kind().as_str() == "objective_c.selector.definition_part"
                    && fact.parent() == definition.parent()
            })
            .collect();
        assert_eq!(
            rootlight_adapter_sdk::canonical_objective_c_selector(definition, &parts, source, 1024)
                .as_deref(),
            Some("value"),
            "{definition:?}; {parts:?}"
        );
    }
    let result = output(source);
    let methods: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.language == "objective-c" && entity.kind == EntityKind::Method)
        .collect();
    assert_eq!(
        methods.len(),
        2,
        "gaps: {:?}; {signatures:?}",
        result
            .document()
            .skipped_regions
            .iter()
            .map(|gap| &gap.detail)
            .collect::<Vec<_>>()
    );
    assert_ne!(methods[0].id, methods[1].id);
}

#[test]
fn markdown_data_examples_keep_addresses_and_alias_bindings_local() {
    let source = concat!(
        "~~~yaml\nname: &entry first\ncopy: *entry\n~~~\n\n",
        "~~~yaml\nname: &entry second\ncopy: *entry\n~~~\n\n",
        "~~~yaml\ncopy: *entry\n~~~\n\n",
        "~~~toml\n[config]\nname = 'first'\n~~~\n\n",
        "~~~toml\n[config]\nname = 'second'\n~~~\n",
    );
    let result = output(source);
    let document = result.document();
    let aliases: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "yaml.alias.reference")
        .collect();
    assert_eq!(aliases.len(), 3);
    let mut targets = BTreeSet::new();
    for alias in aliases {
        let start = usize::try_from(alias.source.span().start_byte()).unwrap();
        let fence_start = source[..start].rfind("~~~yaml").unwrap();
        if fence_start == source.rfind("~~~yaml").unwrap() {
            assert!(matches!(alias.target, OccurrenceTarget::Unresolved { .. }));
            assert!(document.skipped_regions.iter().any(|gap| {
                gap.detail == "yaml-alias-target-unavailable"
                    && gap.source.span() == alias.source.span()
            }));
            continue;
        }
        let OccurrenceTarget::Resolved { symbol } = alias.target else {
            panic!("declared alias must bind within its code example");
        };
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        let span = entity.evidence.source.as_ref().unwrap().span();
        assert!(span.start_byte() >= u64::try_from(fence_start).unwrap());
        assert!(span.end_byte() < alias.source.span().start_byte());
        targets.insert(symbol);
    }
    assert_eq!(targets.len(), 2);
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.detail == "yaml-duplicate-mapping-key" || gap.detail.contains("toml-duplicate")
    }));
    let properties: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.language == "toml" && entity.kind == EntityKind::Property)
        .collect();
    assert_eq!(properties.len(), 2);
    assert_ne!(properties[0].id, properties[1].id);
    assert_eq!(properties[0].canonical_name, properties[1].canonical_name);
    let edited = output(&source.replace("first", "a longer replacement value"));
    let data_identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .filter(|entity| matches!(entity.language.as_str(), "yaml" | "toml"))
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(data_identities(&result), data_identities(&edited));
}

#[test]
fn markdown_fenced_examples_keep_distinct_owners_and_original_coordinates() {
    let source = "# Examples\r\n\r\n> ~~~rust\r\n> fn greet() {}\r\n> ~~~\r\n\r\n~~~rust\r\nfn greet() {}\r\n~~~\r\n";
    let result = output(source);
    let functions: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "greet")
        .collect();
    assert_eq!(functions.len(), 2);
    assert_ne!(functions[0].id, functions[1].id);
    let mut owners = BTreeSet::new();
    for entity in functions {
        assert_eq!(entity.language, "rust");
        let source_ref = entity.evidence.source.as_ref().unwrap();
        assert_eq!(source_ref.content_hash(), content_hash(source.as_bytes()));
        let span = source_ref.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            "fn greet() {}"
        );
        let relation = result
            .document()
            .relations
            .iter()
            .find(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.object == rootlight_ir::RelationEndpoint::Entity(entity.id)
            })
            .unwrap();
        owners.insert(relation.subject);
    }
    assert_eq!(owners.len(), 2);
}

#[test]
fn markdown_named_links_and_images_bind_the_first_document_definition() {
    let source = "[text][Guide] [GUIDE][] [guide] ![alt][Guide] ![guide][] ![GUIDE]\n\n> [Guide]: first.md\n\n[guide]: second.md\n";
    let result = output(source);
    let target = result
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::LinkDefinition && entity.canonical_name == "[Guide]"
        })
        .unwrap();
    let references: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .collect();
    assert_eq!(references.len(), 6, "{references:#?}");
    for reference in references {
        assert_eq!(
            reference.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        let span = reference.source.span();
        assert_eq!(
            reference.syntactic_text_hash,
            content_hash(
                &source.as_bytes()[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()]
            )
        );
        assert!(result.document().relations.iter().any(|relation| {
            relation.predicate == rootlight_ir::RelationPredicate::RefersTo
                && relation.subject == rootlight_ir::RelationEndpoint::Occurrence(reference.id)
                && relation.object == rootlight_ir::RelationEndpoint::Entity(target.id)
        }));
    }
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::LinkDefinition)
            .count(),
        2
    );
}

#[test]
fn markdown_reference_comparison_preserves_unicode_and_container_source() {
    let source = "> [text][Straße\r\n> label] [É] [e\u{301}] [A\u{a0}B] [a b]\r\n\r\n[STRASSE label]: one.md\r\n[é]: two.md\r\n[a b]: three.md\r\n";
    let result = output(source);
    for (written, expected) in [
        ("[Straße\r\n> label]", Some("[STRASSE label]")),
        ("[É]", Some("[é]")),
        ("[e\u{301}]", None),
        ("[A\u{a0}B]", None),
        ("[a b]", Some("[a b]")),
    ] {
        let occurrence = result
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Reference
                    && occurrence.syntactic_text_hash == content_hash(written.as_bytes())
            })
            .unwrap_or_else(|| panic!("missing {written:?}"));
        let target = expected.map(|name| {
            result
                .document()
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name)
                .unwrap()
                .id
        });
        if let Some(symbol) = target {
            assert_eq!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol },
                "{written}"
            );
        } else {
            assert!(
                matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }),
                "{written}"
            );
        }
    }
}

#[test]
fn markdown_bounded_plans_do_not_guess_reference_targets() {
    let source = format!(
        "> paragraph\n{}\n{}\n[ref]: target.md\n",
        "> continued\n".repeat(33),
        "[ref]\n\n".repeat(34)
    );
    let result = output(&source);
    assert_eq!(result.report().coverage().status(), CoverageStatus::Bounded);
    assert!(result.document().skipped_regions.iter().any(|gap| {
        gap.detail == "markdown-inline-budget-unavailable"
            && gap.reason == SkippedRegionReason::ResourceLimit
    }));
    let references: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .collect();
    assert_eq!(references.len(), 34);
    for occurrence in references {
        assert!(matches!(
            occurrence.target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.detail == "markdown-reference-target-unavailable"
                && gap.source.span() == occurrence.source.span()
        }));
    }
}

#[test]
fn markdown_named_references_after_many_paragraphs_keep_exact_targets() {
    let source = format!("{}\n[ref]: target.md\n", "[ref]\n\n".repeat(257));
    let result = output(&source);
    let definition = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::LinkDefinition)
        .unwrap();
    let references: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "markdown.shortcut_link.reference")
        .collect();
    assert_eq!(references.len(), 257);
    for occurrence in references {
        assert_eq!(
            occurrence.target,
            OccurrenceTarget::Resolved {
                symbol: definition.id
            }
        );
        assert_eq!(occurrence.syntactic_text_hash, content_hash(b"[ref]"));
    }
    assert!(!result.document().skipped_regions.iter().any(|gap| {
        matches!(
            gap.detail.as_str(),
            "markdown-inline-budget-unavailable" | "markdown-reference-target-unavailable"
        )
    }));
}

#[test]
fn markdown_resolved_reference_artifacts_rebind_without_changing_targets() {
    let source = "> [text][first\n> label]\n\n> [FIRST\n> LABEL]: target.md\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, MARKDOWN);
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let changed = fixture.next_generation();
    let next = request(&changed.snapshot, &changed.source, MARKDOWN, &budget);
    let replay = analyzer
        .analyze_from_artifact(
            &next,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(&analyzer, &next, &ExtensionSupport::default());
    assert_eq!(fresh.document(), replay.document());
    assert_eq!(fresh.report(), replay.report());
    let original = first
        .document()
        .occurrences
        .iter()
        .find(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .unwrap();
    assert!(matches!(original.target, OccurrenceTarget::Resolved { .. }));
    let current = replay
        .document()
        .occurrences
        .iter()
        .find(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .unwrap();
    assert_eq!(current.target, original.target);
    assert_eq!(current.source.generation(), changed.source.generation());
    assert_eq!(
        current.syntactic_text_hash,
        content_hash(b"[first\n> label]")
    );
}

#[test]
fn markdown_inline_evidence_preserves_written_spans_and_hashes() {
    let source = "# Links\r\n\r\n> é [full][Guide] [Guide][] [Guide] [direct](guide.md)\r\n> <https://example.test> and `code [opaque](hidden.md)`\r\n\r\n[Guide]: target.md\r\n";
    let result = output(source);
    let actual: BTreeSet<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role != OccurrenceRole::Definition)
        .map(|occurrence| {
            let span = occurrence.source.span();
            let text = &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()];
            assert_eq!(
                occurrence.syntactic_text_hash,
                content_hash(text.as_bytes())
            );
            assert_eq!(
                occurrence.source.content_hash(),
                content_hash(source.as_bytes())
            );
            (occurrence.syntax_kind.as_str(), text)
        })
        .collect();
    for expected in [
        ("markdown.reference_label.reference", "[Guide]"),
        ("markdown.collapsed_link.reference", "[Guide][]"),
        ("markdown.shortcut_link.reference", "[Guide]"),
        ("markdown.link_destination.reference", "guide.md"),
        (
            "markdown.link_destination.reference",
            "<https://example.test>",
        ),
        ("markdown.code_span.string", "`code [opaque](hidden.md)`"),
    ] {
        assert!(
            actual.contains(&expected),
            "missing {expected:?}: {actual:#?}"
        );
    }
    assert!(
        !actual
            .iter()
            .any(|(kind, text)| kind.ends_with(".reference") && text.contains("hidden.md"))
    );
}

#[test]
fn markdown_inline_delimiters_do_not_cross_paragraphs() {
    let source = "[opening\n\nclosing](hidden.md)\n\n`[literal](also-hidden.md)`\n\n```text\n[opaque](fenced.md)\n```\n";
    let result = output(source);
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .all(|occurrence| { occurrence.syntax_kind != "markdown.link_destination.reference" })
    );
}

#[test]
fn markdown_inline_embedded_syntax_remains_an_exact_scoped_gap() {
    let source = "Written <i title='x'>word</i> and $x+y$.\n";
    let result = output(source);
    let gaps: BTreeSet<_> = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "markdown-inline-embedded-unavailable")
        .map(|gap| {
            assert_eq!(gap.domain, FactDomain::Entities);
            let span = gap.source.span();
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()]
        })
        .collect();
    assert_eq!(gaps, BTreeSet::from(["<i title='x'>", "</i>", "$x+y$"]));
}

#[test]
fn markdown_native_paragraphs_do_not_consume_foreign_language_ranges() {
    let source = "[direct](guide.md)\n\n".repeat(257);
    let result = output(&source);
    assert_eq!(
        result
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.syntax_kind == "markdown.link_destination.reference"
            })
            .count(),
        257
    );
    let gaps: Vec<_> = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "markdown-inline-budget-unavailable")
        .collect();
    assert!(gaps.is_empty());
    assert_eq!(
        result.report().coverage().status(),
        output("[direct](guide.md)\n\n")
            .report()
            .coverage()
            .status()
    );
}

#[test]
fn markdown_many_quoted_paragraphs_keep_exact_unicode_source_ranges() {
    let source = "> é [direct](guide.md)\r\n> continued 🚀\r\n\r\n".repeat(257);
    let result = output(&source);
    let references: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "markdown.link_destination.reference")
        .collect();
    assert_eq!(references.len(), 257);
    for reference in references {
        let span = reference.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            "guide.md"
        );
        assert_eq!(
            reference.source.content_hash(),
            content_hash(source.as_bytes())
        );
        assert_eq!(reference.syntactic_text_hash, content_hash(b"guide.md"));
    }
    assert!(
        !result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "markdown-inline-budget-unavailable")
    );
}

#[test]
fn markdown_single_block_fragment_limit_keeps_other_paragraphs_available() {
    let source = format!(
        "> [blocked](blocked.md)\n{}\n[healthy](healthy.md)\n",
        "> continued\n".repeat(33)
    );
    let result = output(&source);
    let references: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "markdown.link_destination.reference")
        .collect();
    assert_eq!(references.len(), 1);
    assert_eq!(
        references[0].syntactic_text_hash,
        content_hash(b"healthy.md")
    );
    let gaps: Vec<_> = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "markdown-inline-budget-unavailable")
        .collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].reason, SkippedRegionReason::ResourceLimit);
    assert!(
        gaps[0].source.span().end_byte()
            < u64::try_from(source.find("[healthy]").unwrap()).unwrap()
    );
}

#[test]
fn markdown_blocks_preserve_authored_names_and_kinds() {
    let result = output(MARKDOWN.source);
    let document = result.document();
    let names: BTreeSet<_> = document
        .entities
        .iter()
        .map(|entity| (entity.kind, entity.canonical_name.as_str()))
        .collect();
    assert_eq!(
        names,
        BTreeSet::from([
            (EntityKind::Module, MARKDOWN.path),
            (EntityKind::DocumentSection, "Storage *rules*"),
            (EntityKind::DocumentSection, "Shared cache"),
            (EntityKind::DocumentSection, "Other section"),
            (EntityKind::LinkDefinition, "[Guide]"),
        ])
    );
    assert!(
        document
            .entities
            .iter()
            .all(|entity| entity.language == "markdown")
    );
    assert_eq!(document.version, rootlight_ir::NormalizedIrVersion::V1_5);
    for entity in document
        .entities
        .iter()
        .filter(|entity| entity.kind != EntityKind::Module)
    {
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let span = definitions[0].source.span();
        let text = &MARKDOWN.source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert_eq!(text, entity.canonical_name);
        assert_eq!(
            definitions[0].syntactic_text_hash,
            content_hash(text.as_bytes())
        );
        assert_eq!(
            definitions[0].source.content_hash(),
            content_hash(MARKDOWN.source.as_bytes())
        );
    }
}

#[test]
fn markdown_section_ownership_follows_written_levels_including_setext() {
    let result = output(MARKDOWN.source);
    let document = result.document();
    for (name, owner, exact_text) in [
        (
            "Storage *rules*",
            MARKDOWN.path,
            MARKDOWN.source.split("Other section").next().unwrap(),
        ),
        (
            "Shared cache",
            "Storage *rules*",
            "## Shared cache\r\n\r\n[Guide]: <guide.md> \"Help\"\r\n\r\nBody.\r\n\r\n",
        ),
        (
            "[Guide]",
            "Shared cache",
            "[Guide]: <guide.md> \"Help\"\r\n",
        ),
        (
            "Other section",
            MARKDOWN.path,
            "Other section\r\n=============\r\nTail.\r\n",
        ),
    ] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        let parent = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == owner)
            .unwrap();
        assert!(
            document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(parent.id)
                    && relation.object == RelationEndpoint::Entity(entity.id)),
            "{name} -> {owner}"
        );
        let span = entity.evidence.source.as_ref().unwrap().span();
        assert_eq!(
            &MARKDOWN.source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            exact_text
        );
    }
}

#[test]
fn markdown_nested_blocks_do_not_extend_sections_into_following_containers() {
    let source =
        "# Outer 雪\n\n> ## Quoted\n> text\n\n- ## Listed\n  item text\n\n## Following\nend\n";
    let result = output(source);
    for (name, expected) in [
        ("Quoted", "## Quoted\n> text\n"),
        ("Listed", "## Listed\n  item text\n\n"),
        ("Following", "## Following\nend\n"),
    ] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        let span = entity.evidence.source.as_ref().unwrap().span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            expected,
            "{name}"
        );
    }
}

#[test]
fn markdown_empty_headings_own_sections_without_inventing_a_written_definition() {
    let result = output("#\n\nBody\n\n#\n\nOther\n");
    let headings: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::DocumentSection)
        .collect();
    assert_eq!(headings.len(), 2);
    assert_ne!(headings[0].id, headings[1].id);
    assert!(
        headings
            .iter()
            .all(|entity| entity.canonical_name == "<untitled>")
    );
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::Definition)
    );
}

#[test]
fn markdown_opaque_blocks_do_not_invent_document_headings_or_code_symbols() {
    let source = "# Written\n\n```markdown\n# Fence\n[hidden]: fake.md\n```\n\n    # Indented\n\n<!--\n# Comment\n-->\n\nParagraph with # text.\n";
    let result = output(source);
    let names: BTreeSet<_> = result
        .document()
        .entities
        .iter()
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    assert_eq!(
        names,
        BTreeSet::from([MARKDOWN.path, "Written", "<code-block>"])
    );
    assert_eq!(
        result
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "markdown-embedded-analysis-unavailable")
            .count(),
        3
    );
}

#[test]
fn markdown_excessive_heading_name_remains_a_source_scoped_gap() {
    let source = format!(
        "# {}\n\nBody.\n\n## Visible\nTail.\n",
        "a".repeat(IrLimits::default().max_string_bytes + 1)
    );
    let result = output(&source);
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Visible")
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "<untitled>")
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == FactDomain::Entities
                && gap.detail == "declaration-name-unavailable"
                && gap.source.span().start_byte() == 0
                && usize::try_from(gap.source.span().end_byte()).unwrap() == source.len())
    );
}

#[test]
fn markdown_required_replay_preserves_ownership_and_scoped_unavailable_analysis() {
    let source =
        "# Guide\n\nUse [entry][ref].\n\n```rust\nfn embedded() {}\n```\n\n[ref]: entry.rs\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, MARKDOWN);
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
    let demand = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        demand,
        artifact.required_syntax_fact_count(&deadline()).unwrap()
    );
    let bounded = limits_with_syntax_records(demand);
    let changed = fixture.next_generation();
    let next = request(&changed.snapshot, &changed.source, MARKDOWN, &bounded);
    let (fresh, retained) = analyzer
        .analyze_and_capture(
            &next,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let replay = analyzer
        .analyze_from_artifact(
            &next,
            &retained,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(fresh.document(), replay.document());
    assert_eq!(fresh.report(), replay.report());
    let gaps = |result: &AnalysisOutput| {
        result
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail.starts_with("markdown-"))
            .map(|gap| (gap.detail.clone(), gap.domain, gap.source.span()))
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(gaps(&first), gaps(&replay));
    for (detail, domain, text) in [
        (
            "markdown-link-resolution-unavailable",
            FactDomain::Relations,
            source,
        ),
        (
            "markdown-code-semantics-unavailable",
            FactDomain::Relations,
            "```rust\nfn embedded() {}\n```\n",
        ),
    ] {
        assert!(
            replay.document().skipped_regions.iter().any(|gap| {
                let span = gap.source.span();
                gap.detail == detail
                    && gap.domain == domain
                    && &source[usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()]
                        == text
            }),
            "{detail}"
        );
    }
    assert!(first.document().occurrences.iter().any(|occurrence| {
        occurrence.syntax_kind == "markdown.reference_label.reference"
            && occurrence.syntactic_text_hash == content_hash(b"[ref]")
    }));
    assert!(!replay.document().skipped_regions.iter().any(|gap| {
        gap.detail == "markdown-inline-analysis-unavailable"
            || gap.detail == "markdown-inline-parse-unavailable"
    }));
    assert!(
        replay
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "embedded"
                && entity.language == "rust"
                && entity.kind == EntityKind::Function)
    );
    assert!(
        replay
            .document()
            .skipped_regions
            .iter()
            .all(|gap| gap.source.generation() == changed.source.generation())
    );
}

#[test]
fn markdown_repeated_headings_remain_distinct_and_body_edits_preserve_identity() {
    let source = "# Cache\n\nText.\n\n## Entry\n\nFirst.\n\n## Entry\n\nSecond.\n\n[ref]: one.md\n[ref]: two.md\n";
    let before = output(source);
    let after = output(&source.replace("First.", "A longer body with é."));
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| (entity.id, entity.kind, entity.canonical_name.clone()))
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        before.document().entities.len(),
        6,
        "{:#?}",
        before.document()
    );
    assert_eq!(identities(&before).len(), 6);
    assert_eq!(identities(&before), identities(&after));
}
