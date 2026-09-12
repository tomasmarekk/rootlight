//! Groovy declarations through the public analyzer and generation-bound source evidence.
//! Parsing alone cannot establish discoverable entities or exact definition ownership.

use super::*;

pub(super) const GROOVY: LanguageCase = LanguageCase {
    name: "groovy",
    path: "src/catalog.groovy",
    frontend: "dekobon-tree-sitter-groovy-0.2.0",
    source: "class Meter { String title; private int value; String read() { 'x' } }\nString render() { 'y' }\n",
    generated: false,
    body_before: "'x'",
    body_after: "'updated'",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(GROOVY, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, GROOVY),
        &request(&fixture.snapshot, &fixture.source, GROOVY, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

fn assert_lexical_reads(source: &str, name: &str, expected: &[Option<usize>]) {
    assert_named_reads(source, name, expected, "groovy.identifier.reference");
}

fn assert_type_reads(source: &str, name: &str, expected: &[Option<usize>]) {
    assert_named_reads(source, name, expected, "groovy.type_identifier.reference");
}

fn assert_named_reads(source: &str, name: &str, expected: &[Option<usize>], syntax: &str) {
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let mut reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.syntax_kind == syntax
                && source.get(
                    usize::try_from(occurrence.source.span().start_byte()).unwrap()
                        ..usize::try_from(occurrence.source.span().end_byte()).unwrap(),
                ) == Some(name)
        })
        .collect();
    reads.sort_by_key(|read| read.source.span().start_byte());
    let actual: Vec<_> = reads
        .iter()
        .map(|read| match read.target {
            OccurrenceTarget::Resolved { symbol } => {
                let definition = document
                    .occurrences
                    .iter()
                    .find(|occurrence| {
                        occurrence.role == OccurrenceRole::Definition
                            && occurrence.target == OccurrenceTarget::Resolved { symbol }
                    })
                    .unwrap();
                assert!(
                    document.relations.iter().any(|relation| {
                        relation.predicate == RelationPredicate::RefersTo
                            && relation.subject == RelationEndpoint::Occurrence(read.id)
                            && relation.object == RelationEndpoint::Entity(symbol)
                            && relation.evidence.source.as_ref() == Some(&read.source)
                    }),
                    "missing lexical relation for {read:?}"
                );
                Some(usize::try_from(definition.source.span().start_byte()).unwrap())
            }
            _ => None,
        })
        .collect();
    assert_eq!(actual, expected, "{name} in {source}\n{reads:?}");
}

#[test]
fn groovy_array_creation_preserves_generic_type_and_dimension_reads() {
    let source =
        "class Factory<T> { T[] allocate(int size) { def values = new T[size]; values } }\n";
    let owner = Some(source.find("<T>").unwrap() + 1);
    assert_type_reads(source, "T", &[owner; 2]);
    assert_lexical_reads(source, "size", &[Some(source.find("size)").unwrap())]);
    assert_lexical_reads(source, "values", &[Some(source.find("values =").unwrap())]);
}

#[test]
fn groovy_imported_type_evidence_preserves_native_alias_ownership() {
    for (import, name) in [
        ("import sample.Widget", "Widget"),
        ("@Marker\nimport sample.Widget", "Widget"),
        ("import sample /* owner */ . Widget as Selected", "Selected"),
    ] {
        let source = format!("{import}\nclass Holder {{ {name} item }}\n");
        let result = output(&source);
        let bindings: Vec<_> = result
            .document()
            .extensions
            .iter()
            .filter(|envelope| envelope.namespace == rootlight_ir::GROOVY_BINDING_NAMESPACE)
            .collect();
        assert_eq!(bindings.len(), 1, "{import}");
        let binding = rootlight_ir::decode_groovy_binding_envelope(bindings[0]).unwrap();
        assert_eq!(binding.target, content_hash(b"sample.Widget"));
        assert_eq!(
            binding.import.start_byte(),
            u64::try_from(source.find("sample").unwrap()).unwrap()
        );
        let site = bindings[0].evidence.source.as_ref().unwrap();
        assert_eq!(
            site.span().start_byte(),
            u64::try_from(source.find(&format!("{name} item")).unwrap()).unwrap()
        );
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .any(|occurrence| occurrence.source == *site
                    && occurrence.role == OccurrenceRole::TypeUse)
        );
    }
}

#[test]
fn groovy_qualified_import_evidence_uses_native_type_boundaries() {
    for (body, expected) in [
        (
            "class Holder { Selected.Inner item }",
            Some("sample.Widget.Inner"),
        ),
        (
            "class Holder { Selected /* name */ . Inner item }",
            Some("sample.Widget.Inner"),
        ),
        (
            "class Holder { Selected.Inner.Deep[] item }",
            Some("sample.Widget.Inner.Deep"),
        ),
        (
            "class Holder { Selected.Inner<String> item }",
            Some("sample.Widget.Inner"),
        ),
        (
            "class Holder<Selected> { Selected.Inner item }",
            Some("sample.Widget.Inner"),
        ),
        (
            "class Selected {}\nclass Holder { Selected.Inner item }",
            Some("sample.Widget.Inner"),
        ),
        (
            "class Holder extends Parent { Selected.Inner item }",
            Some("sample.Widget.Inner"),
        ),
        ("def value = Selected.Inner", None),
    ] {
        let source = format!("import sample.Widget as Selected\n{body}\n");
        let result = output(&source);
        assert!(result.document().diagnostics.is_empty(), "{body}");
        let bindings: Vec<_> = result
            .document()
            .extensions
            .iter()
            .filter(|envelope| envelope.namespace == rootlight_ir::GROOVY_BINDING_NAMESPACE)
            .map(|envelope| {
                rootlight_ir::decode_groovy_binding_envelope(envelope)
                    .unwrap()
                    .target
            })
            .collect();
        let expected: Vec<_> = expected
            .into_iter()
            .map(|name| content_hash(name.as_bytes()))
            .collect();
        assert_eq!(bindings, expected, "{body}");
    }
}

#[test]
fn groovy_inheritance_headers_exclude_generic_arguments_and_bounds() {
    let source = "class Base<T> {}\ninterface Face<T> {}\ninterface Extra {}\nclass Arg {}\nclass Holder<T extends Arg> extends Base<Arg[]> implements Face<Arg>, Extra {}";
    let result = output(source);
    let document = result.document();
    assert!(document.diagnostics.is_empty());
    let headers: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            matches!(
                site.role,
                OccurrenceRole::InheritanceUse | OccurrenceRole::ImplementationUse
            )
        })
        .collect();
    assert_eq!(headers.len(), 3);
    for (name, role) in [
        ("Base", OccurrenceRole::InheritanceUse),
        ("Face", OccurrenceRole::ImplementationUse),
        ("Extra", OccurrenceRole::ImplementationUse),
    ] {
        let start = u64::try_from(source.rfind(name).unwrap()).unwrap();
        let site = headers
            .iter()
            .find(|site| site.source.span().start_byte() == start)
            .unwrap();
        assert_eq!(site.role, role);
        assert!(matches!(site.target, OccurrenceTarget::Resolved { .. }));
    }
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|relation| matches!(
                relation.predicate,
                RelationPredicate::Extends | RelationPredicate::Implements
            ))
            .count(),
        3
    );
}

#[test]
fn groovy_qualified_local_types_do_not_hide_unresolved_namespaces() {
    for source in [
        "class Outer { static class Inner {} }\nclass Holder extends base.Parent { Outer.Inner item }",
        "import sample.*\nclass Outer { static class Inner {} }\nclass Holder { Outer.Inner item }",
        "import static sample.Container.Outer\nclass Outer { static class Inner {} }\nclass Holder { Outer.Inner item }",
        "import static sample.Container.*\nclass Outer { static class Inner {} }\nclass Holder { Outer.Inner item }",
        "class Outer { static class Inner {} }\nclass Holder { other.Outer.Inner item }",
        "class Outer { static class Inner {} }\nclass Holder { Outer.Missing item }",
    ] {
        let result = output(source);
        let reads: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "groovy.qualified_type.reference")
            .collect();
        assert!(!reads.is_empty(), "{source}");
        for site in reads {
            assert!(
                matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                "{source}: {site:?}"
            );
        }
    }
}

#[test]
fn groovy_local_type_precedence_matches_native_nested_class_order() {
    for (source, name, declaration) in [
        (
            "import sample.Widget as Selected\nclass Selected {}\nclass Holder { Selected item }",
            "Selected",
            "class Selected",
        ),
        (
            "class Outer { static class Selected {}\nstatic class Middle { static class Selected {}\nstatic class Holder { Selected item } } }",
            "Selected",
            "class Selected",
        ),
        (
            "class Outer { static class Selected {}\nstatic class Holder { static class Chosen {}\nChosen item } }",
            "Chosen",
            "class Chosen",
        ),
        (
            "class Holder extends base.Parent { Holder item }",
            "Holder",
            "class Holder",
        ),
    ] {
        let expected = Some(source.find(declaration).unwrap() + "class ".len());
        assert_type_reads(source, name, &[expected]);
    }
}

#[test]
fn groovy_imported_type_evidence_respects_shadowing_and_inheritance_uncertainty() {
    for (body, expected) in [
        ("class Holder<Selected> { Selected item }", 0),
        ("class Holder<Selected> { static Selected item }", 1),
        (
            "class Holder { static class Selected {}\nSelected item }",
            0,
        ),
        ("class Holder extends base.Parent { Selected item }", 0),
        (
            "interface Holder extends base.Parent { Selected read() }",
            0,
        ),
        (
            "class Other { static class Selected {} }\nclass Holder { Selected item }",
            1,
        ),
    ] {
        let source = format!("import sample.Widget as Selected\n{body}\n");
        let result = output(&source);
        assert!(result.document().diagnostics.is_empty(), "{body}");
        let count = result
            .document()
            .extensions
            .iter()
            .filter(|envelope| envelope.namespace == rootlight_ir::GROOVY_BINDING_NAMESPACE)
            .count();
        assert_eq!(count, expected, "{body}");
    }
}

#[test]
fn groovy_array_initializers_preserve_value_reads_without_inventing_closures() {
    let source = "def allocate(rows, columns, value) { def sized = new String[rows][columns][]; def initialized = new String[]{value}; def nested = new String[][]{new String[]{value}}; [sized, initialized, nested] }\n";
    assert_lexical_reads(source, "rows", &[Some(source.find("rows,").unwrap())]);
    assert_lexical_reads(source, "columns", &[Some(source.find("columns,").unwrap())]);
    assert_lexical_reads(source, "value", &[Some(source.find("value)").unwrap()); 2]);
    let result = output(source);
    let functions: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function)
        .collect();
    assert_eq!(functions.len(), 1);
    assert_eq!(functions[0].canonical_name, "allocate");
}

#[test]
fn groovy_import_fields_preserve_targets_kinds_and_authored_aliases() {
    for (declaration, target, kind, alias) in [
        ("import sample.Widget", "sample.Widget", "type_import", None),
        (
            "import sample.Widget as Alias",
            "sample.Widget",
            "type_import",
            Some("Alias"),
        ),
        ("import sample.*", "sample", "package_star_import", None),
        (
            "import static sample.Tools.run",
            "sample.Tools.run",
            "static_import",
            None,
        ),
        (
            "import static sample.Tools.run as perform",
            "sample.Tools.run",
            "static_import",
            Some("perform"),
        ),
        (
            "import static sample.Tools.*",
            "sample.Tools",
            "static_star_import",
            None,
        ),
    ] {
        let source = format!("@Marker\n{declaration}\nclass Widget {{}}\n");
        let result = output(&source);
        let doc = result.document();
        assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
        let targets: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == format!("groovy.{kind}.reference"))
            .collect();
        assert_eq!(targets.len(), 1, "{source}");
        let site = targets[0];
        assert_eq!(site.role, OccurrenceRole::ImportUse);
        assert_eq!(site.syntactic_text_hash, content_hash(target.as_bytes()));
        let start = source.find(target).unwrap();
        assert_eq!(
            site.source.span().start_byte(),
            u64::try_from(start).unwrap()
        );
        assert_eq!(
            site.source.span().end_byte(),
            u64::try_from(start + target.len()).unwrap()
        );
        assert_eq!(site.source.content_hash(), doc.files[0].content_hash);
        assert_eq!(site.source.generation(), doc.files[0].generation);
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        let imports: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Import)
            .collect();
        assert_eq!(imports.len(), usize::from(alias.is_some()), "{source}");
        if let Some(alias) = alias {
            assert_eq!(imports[0].canonical_name, alias);
            let definition = doc
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target
                            == OccurrenceTarget::Resolved {
                                symbol: imports[0].id,
                            }
                })
                .unwrap();
            let span = definition.source.span();
            assert_eq!(
                &source[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()],
                alias
            );
            assert!(
                !doc.occurrences.iter().any(
                    |site| site.role == OccurrenceRole::Reference && site.source.span() == span
                )
            );
        }
        assert!(!doc.relations.iter().any(|relation| matches!(
            relation.predicate,
            RelationPredicate::RefersTo | RelationPredicate::Calls
        )));
        assert!(
            doc.skipped_regions
                .iter()
                .any(|gap| gap.detail == "unresolved-import-target"),
            "{source}: gaps={:?}; occurrences={:?}",
            doc.skipped_regions,
            doc.occurrences
        );
    }
}

#[test]
fn groovy_import_target_identity_ignores_trivia_but_keeps_exact_source() {
    for (prefix, kind, target, canonical) in [
        (
            "static ",
            "static_import",
            "sample /* static .* as */ . Tools . run",
            "sample.Tools.run",
        ),
        ("static ", "static_import", "sample.λ.run", "sample.λ.run"),
        (
            "",
            "type_import",
            "sample /* static .* as */ . Widget",
            "sample.Widget",
        ),
    ] {
        let source = format!("@Marker(value='static .* as')\nimport {prefix}{target} as renamed\n");
        let result = output(&source);
        let doc = result.document();
        assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
        let site = doc
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == format!("groovy.{kind}.reference"))
            .expect("native import target");
        assert_eq!(site.syntactic_text_hash, content_hash(canonical.as_bytes()));
        let span = site.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            target
        );
        assert_eq!(site.role, OccurrenceRole::ImportUse);
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    }
}

#[test]
fn groovy_import_aliases_are_not_lexical_value_definitions() {
    assert_lexical_reads(
        "import sample.Widget as Alias\ndef read() { Alias }\n",
        "Alias",
        &[None],
    );
    assert_lexical_reads(
        "import static sample.Tools.run as perform\ndef read() { perform }\n",
        "perform",
        &[None],
    );
}

#[test]
fn groovy_annotated_imports_preserve_unresolved_source_evidence() {
    for declaration in [
        "import sample.Widget",
        "import sample.Widget as Alias",
        "import sample.*",
        "import static sample.Tools.run",
        "import static sample.Tools.run as perform",
        "import static sample.Tools.*",
    ] {
        let import = format!("@Marker\r\n// λ\r\n@sample.Other\n{declaration}");
        let source = format!("{import}\n@interface Marker {{}}\n");
        let result = output(&source);
        let doc = result.document();
        assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
        let gaps: Vec<_> = doc
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "unresolved-import-target")
            .collect();
        assert_eq!(gaps.len(), 1, "{source}");
        let span = gaps[0].source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            import
        );
        let mut annotations: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.syntax_kind == "groovy.qualified_identifier.reference")
            .collect();
        annotations.sort_by_key(|occurrence| occurrence.source.span().start_byte());
        assert_eq!(annotations.len(), 2, "{source}");
        for (occurrence, name) in annotations.into_iter().zip(["Marker", "sample.Other"]) {
            let span = occurrence.source.span();
            assert_eq!(
                &source[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()],
                name
            );
            assert_eq!(
                occurrence.syntactic_text_hash,
                content_hash(name.as_bytes())
            );
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
            assert_eq!(occurrence.repository, doc.files[0].repository);
            assert_eq!(occurrence.generation, doc.files[0].generation);
        }
        assert!(
            !doc.relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::RefersTo)
        );
    }
}

#[test]
fn groovy_generic_parameters_bind_bounds_members_and_shadowing_methods() {
    let source = "class Outer<T extends Number> { T field; T plain(T argument) { T local = argument; local }; <T extends CharSequence> T shadow(T argument) { T local = argument; local }; <U extends T> U bound(U value) { value }; class Inner { T field }; static <V> V factory(V item) { item } }\n";
    let outer = Some(source.find("T extends Number").unwrap());
    let shadow = Some(source.find("T extends CharSequence").unwrap());
    assert_type_reads(
        source,
        "T",
        &[
            outer, outer, outer, outer, shadow, shadow, shadow, outer, outer,
        ],
    );
    assert_type_reads(source, "U", &[Some(source.find("U extends").unwrap()); 2]);
    assert_type_reads(source, "V", &[Some(source.find("V>").unwrap()); 2]);
    assert_lexical_reads(source, "item", &[Some(source.find("item)").unwrap())]);
}

#[test]
fn groovy_generic_parameters_are_visible_in_recursive_and_forward_bounds() {
    let source = "class Forward<T extends U, U extends Number> { T field; U other }; class Recursive<V extends Comparable<V>> { V field }\n";
    assert_type_reads(source, "U", &[Some(source.find("U extends").unwrap()); 2]);
    assert_type_reads(source, "T", &[Some(source.find("T extends").unwrap())]);
    assert_type_reads(source, "V", &[Some(source.find("V extends").unwrap()); 2]);
}

#[test]
fn groovy_def_prefixed_generics_keep_type_and_value_bindings() {
    for (header, type_reads) in [("def <T> T read", 3), ("def <T> read", 2)] {
        let source = format!(
            "class Outer<T> {{ {header}(T value) {{ T local = value; local }}; T after }}\n"
        );
        let method_type = Some(source.find("def <T>").unwrap() + 5);
        let mut expected = vec![method_type; type_reads];
        expected.push(Some(source.find("<T>").unwrap() + 1));
        assert_type_reads(&source, "T", &expected);
        assert_lexical_reads(&source, "value", &[Some(source.find("value)").unwrap())]);
        assert_lexical_reads(&source, "local", &[Some(source.find("local =").unwrap())]);
    }
}

#[test]
fn groovy_extended_closure_parameters_keep_authored_bindings() {
    for (header, name) in [
        ("var", "var"),
        ("String var", "var"),
        ("String... remaining", "remaining"),
        ("@Deprecated final String value", "value"),
        ("final def value", "value"),
        ("String value = seed", "value"),
    ] {
        let source = format!("def seed = 'λ'; def action = {{ {header} -> sink({name}) }}\n");
        let parameter_start = source.find(header).unwrap() + header.find(name).unwrap();
        assert_lexical_reads(&source, name, &[Some(parameter_start)]);
        if header.contains("seed") {
            assert_lexical_reads(&source, "seed", &[Some(source.find("seed =").unwrap())]);
        }
        let result = output(&source);
        let doc = result.document();
        let parameter = doc
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        let closure = doc
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "<anonymous>")
            .unwrap();
        assert_eq!(parameter.kind, EntityKind::Parameter);
        assert_eq!(
            parameter.container,
            Some(rootlight_ir::ContainerRef::Entity(closure.id))
        );
    }
}

#[test]
fn groovy_annotation_and_list_trivia_preserve_source_bindings() {
    let source = "class Reader<T> { @Deprecated\r\n// λ\r\nT read(T value) { def items = [[value],\n\n// boundary\n[value],\n]; value } }\n";
    assert_type_reads(source, "T", &[Some(source.find("<T>").unwrap() + 1); 2]);
    assert_lexical_reads(source, "value", &[Some(source.find("value)").unwrap()); 3]);
    let result = output(source);
    let method = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "read")
        .unwrap();
    assert_eq!(method.kind, EntityKind::Method);
    let span = method.evidence.source.as_ref().unwrap().span();
    let authored = &source
        [usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap()];
    assert!(authored.starts_with("@Deprecated\r\n// λ\r\nT read"));
    assert!(authored.ends_with("; value }"));
}

#[test]
fn groovy_assert_message_and_map_trivia_keep_value_reads() {
    for separator in [",", ":"] {
        let source = format!(
            "def run(value) {{ assert value != null{separator} value; def entries = [\nfirst: value,\n// λ\nsecond: [value],\n]; value }}\n"
        );
        assert_lexical_reads(&source, "value", &[Some(source.find("value)").unwrap()); 5]);
    }
}

#[test]
fn groovy_bodyless_methods_keep_generic_owners_across_trivia() {
    for tail in ["\n\n", "\r\n// boundary\r\n", ";\n"] {
        let source = format!("interface Reader<T> {{ T read(T value){tail}T next(){tail}}}\n");
        let owner = Some(source.find("<T>").unwrap() + 1);
        assert_type_reads(&source, "T", &[owner; 3]);
        let result = output(&source);
        let mut methods: Vec<_> = result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Method)
            .map(|entity| entity.canonical_name.as_str())
            .collect();
        methods.sort_unstable();
        assert_eq!(methods, ["next", "read"]);
    }
}

#[test]
fn groovy_static_contexts_do_not_capture_outer_generic_parameters() {
    let source = "class Holder<T> { static T field; static T read(T value) { value }; static class Nested { T field }; static { T local }; static <T> T own(T value) { value }; class Inner<T extends CharSequence> { T field } }\n";
    let own = Some(source.find("<T> T own").unwrap() + 1);
    let inner = Some(source.find("T extends").unwrap());
    assert_type_reads(
        source,
        "T",
        &[None, None, None, None, None, own, own, inner],
    );
}

#[test]
fn groovy_type_and_value_namespaces_do_not_share_targets() {
    let source = "class Holder<T> { def examine() { def T = 1; T local; sink(T) } }\n";
    assert_type_reads(source, "T", &[Some(source.find("<T>").unwrap() + 1)]);
    assert_lexical_reads(source, "T", &[Some(source.find("T =").unwrap())]);
    let source = "class Holder<T> { pkg.T external; List<T[]> typed; T[] array; <T> T shadow(T value) { value } }\n";
    let outer = Some(source.find("<T>").unwrap() + 1);
    let inner = Some(source.find("<T> T shadow").unwrap() + 1);
    assert_type_reads(source, "T", &[outer, outer, inner, inner]);
    let result = output(source);
    let qualified = u64::try_from(source.find("pkg.T").unwrap() + 4).unwrap();
    let reference = result
        .document()
        .occurrences
        .iter()
        .find(|site| site.source.span().start_byte() == qualified)
        .unwrap();
    assert!(matches!(
        reference.target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn groovy_nested_type_bindings_distinguish_implicit_static_members() {
    let source = "interface Holder<T> { T field = null; T read(T value) }; class Outer<U> { interface Inner { U read() }; trait Nested { U read() { null } }; @interface Marker { U value() }; record Entry(U value) {}; enum State { READY; U read() { null } } }\n";
    let generic = Some(source.find("<T>").unwrap() + 1);
    assert_type_reads(source, "T", &[None, generic, generic]);
    let outer = Some(source.find("<U>").unwrap() + 1);
    assert_type_reads(source, "U", &[outer, outer, outer, outer, None]);
}

#[test]
fn groovy_catch_parameters_bind_untyped_modified_and_multicatch_headers() {
    for header in [
        "problem",
        "def problem",
        "final problem",
        "final def problem",
        "Exception problem",
        "final Exception problem",
        "IOException | RuntimeException problem",
        "@Deprecated Exception problem",
        "@Deprecated final def problem",
        "final @Deprecated Exception problem",
    ] {
        let source = format!(
            "def examine() {{ try {{ work() }} catch ({header}) {{ sink(problem); def deferred = {{ sink(problem) }} }} finally {{ sink(problem) }}; sink(problem) }}\n"
        );
        let parameter = Some(source.find("problem)").unwrap());
        assert_lexical_reads(&source, "problem", &[parameter, parameter, None, None]);
        let result = output(&source);
        let parameters: Vec<_> = result
            .document()
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Parameter && entity.canonical_name == "problem"
            })
            .collect();
        assert_eq!(parameters.len(), 1);
    }
}

#[test]
fn groovy_modified_formal_parameters_bind_body_and_captured_reads() {
    for header in [
        "String value",
        "@Deprecated String value",
        "final String value",
        "@Deprecated final String value",
        "final @Deprecated String value",
        "def value",
        "final def value",
        "final value",
    ] {
        let source = format!(
            "class Holder {{ def examine({header}) {{ sink(value); def deferred = {{ sink(value) }} }}; def outside() {{ sink(value) }} }}\n"
        );
        let parameter = Some(source.find("value)").unwrap());
        assert_lexical_reads(&source, "value", &[parameter, parameter, None]);
        let result = output(&source);
        let parameters: Vec<_> = result
            .document()
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Parameter && entity.canonical_name == "value"
            })
            .collect();
        assert_eq!(parameters.len(), 1);
    }
}

#[test]
fn groovy_lexical_reads_respect_initializers_and_closure_parameters() {
    let source = "def examine(input) { sink(value); def value = value; sink(value); def action = { argument -> sink(value, input, argument) }; def implicit = { sink(it, value) }; def explicit = { -> sink(it, value) } }\n";
    let value = Some(source.find("value =").unwrap());
    assert_lexical_reads(source, "value", &[None, None, value, value, value, value]);
    assert_lexical_reads(source, "input", &[Some(source.find("input").unwrap())]);
    assert_lexical_reads(
        source,
        "argument",
        &[Some(source.find("argument").unwrap())],
    );
    assert_lexical_reads(source, "it", &[None, None]);
}

#[test]
fn groovy_closure_arrow_comments_preserve_parameter_and_capture_bindings() {
    for header in ["->", "value ->", "String value ->"] {
        for trivia in [" // note\n", "\n// first\r\n/* second */\r\n"] {
            let source = format!(
                "def value = 'outer'; def action = {{ {header}{trivia}sink(value); def nested = {{ -> sink(value) }} }}; sink(value)\n"
            );
            let outer = Some(source.find("value =").unwrap());
            let inner = source.find("value ->").or(outer);
            assert_lexical_reads(&source, "value", &[inner, inner, outer]);
        }
    }
}

#[test]
fn groovy_lexical_reads_keep_control_bindings_inside_their_visibility() {
    let source = "def examine(items) { for (item in items) { sink(item) }; sink(item); for (int index = 0; index < 2; index++) { sink(index) }; sink(index); try (Reader handle = open(); Reader second = wrap(handle)) { sink(handle, second) } catch (IOException problem) { sink(handle, second, problem) } finally { sink(handle, second, problem) } }\n";
    assert_lexical_reads(source, "items", &[Some(source.find("items").unwrap())]);
    assert_lexical_reads(
        source,
        "item",
        &[Some(source.find("item in").unwrap()), None],
    );
    let index = Some(source.find("index =").unwrap());
    assert_lexical_reads(source, "index", &[index, index, index, None]);
    let handle = Some(source.find("handle =").unwrap());
    assert_lexical_reads(source, "handle", &[handle, handle, None, None]);
    assert_lexical_reads(
        source,
        "second",
        &[Some(source.find("second =").unwrap()), None, None],
    );
    assert_lexical_reads(
        source,
        "problem",
        &[Some(source.find("problem)").unwrap()), None],
    );
    let source = "def loop(items) { for (item in items) def local = local; sink(local) }\n";
    assert_lexical_reads(source, "local", &[None, None]);
}

#[test]
fn groovy_lexical_reads_exclude_member_names_and_literal_keys() {
    let source = "def examine(value, receiver) { sink([value: value, (value): value], receiver.value, receiver.value(value), receiver?.value, receiver*.value, receiver.@value, receiver.&value, receiver::value) }\n";
    let value = Some(source.find("value,").unwrap());
    assert_lexical_reads(source, "value", &[value, value, value, value]);
    let receiver = Some(source.find("receiver)").unwrap());
    assert_lexical_reads(source, "receiver", &[receiver; 7]);
    let command = "def commands(value, then) { send value then value }\n";
    assert_lexical_reads(
        command,
        "value",
        &[Some(command.find("value,").unwrap()); 2],
    );
    assert_lexical_reads(command, "then", &[]);
}

#[test]
fn groovy_lexical_reads_observe_binding_order_and_noncapturing_methods() {
    let source = "def outer = 1\ndef examine(input) { def recursive = { sink(recursive) }; def first = 1, second = first; def (left, right) = [left, right]; def captured = { sink(later) }; def later = 2; sink(outer); def explicit = { -> sink(input) }; sink(second, left, right) }\n";
    assert_lexical_reads(source, "recursive", &[None]);
    assert_lexical_reads(source, "first", &[Some(source.find("first =").unwrap())]);
    assert_lexical_reads(source, "left", &[None, Some(source.find("left,").unwrap())]);
    assert_lexical_reads(
        source,
        "right",
        &[None, Some(source.find("right)").unwrap())],
    );
    assert_lexical_reads(source, "later", &[None]);
    assert_lexical_reads(source, "outer", &[None]);
    assert_lexical_reads(source, "input", &[Some(source.find("input)").unwrap())]);
}

#[test]
fn groovy_lexical_reads_do_not_confuse_siblings_or_generated_parameters() {
    let source = "def examine(it, items) { for (item in items) { sink(item) }; for (item in items) { sink(item) }; def implicit = { sink(it) }; def explicit = { -> sink(it) }; for (value in value) sink(value) }\n";
    let mut items = source
        .match_indices("item in")
        .map(|(start, _)| Some(start));
    assert_lexical_reads(
        source,
        "item",
        &[items.next().unwrap(), items.next().unwrap()],
    );
    assert_lexical_reads(source, "it", &[None, Some(source.find("it,").unwrap())]);
    assert_lexical_reads(
        source,
        "value",
        &[None, Some(source.find("value in").unwrap())],
    );
}

#[test]
fn groovy_lazy_interpolation_keeps_explicit_parameter_visibility() {
    let source =
        "def examine(it) { sink(\"${-> it}\", \"${writer -> writer.append(it)}\", \"${it}\") }\n";
    assert_lexical_reads(source, "it", &[Some(source.find("it)").unwrap()); 3]);
    assert_lexical_reads(source, "writer", &[Some(source.find("writer ->").unwrap())]);
    let source = "def dollars(value, $value) { sink(\"$value\", \"$value.name\", \"${value}\") }\n";
    assert_lexical_reads(source, "value", &[Some(source.find("value,").unwrap()); 3]);
    assert_lexical_reads(source, "$value", &[]);
}

#[test]
fn groovy_bounded_capture_plans_do_not_guess_lexical_bindings() {
    let source = format!(
        "def examine(value) {{ sink({}) }}",
        vec!["value"; 80].join(", ")
    );
    let provider = Arc::new(provider());
    let fixture = Fixture::new(GROOVY, source.as_bytes());
    let budget = limits_with_syntax_records(64);
    let result = analyze(
        &analyzer(&provider, GROOVY),
        &request(&fixture.snapshot, &fixture.source, GROOVY, &budget),
        &ExtensionSupport::default(),
    );
    assert_eq!(result.report().coverage().status(), CoverageStatus::Bounded);
    let reads: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Reference)
        .collect();
    assert!(!reads.is_empty());
    assert!(
        reads
            .iter()
            .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::RefersTo)
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
}

#[test]
fn groovy_control_bindings_keep_distinct_names_sources_and_owners() {
    for (body, name, kind, count) in [
        (
            "try { work() } catch (IOException problem) { sink(problem, 1) } catch (RuntimeException problem) { sink(problem, 1) }",
            "problem",
            EntityKind::Parameter,
            2,
        ),
        (
            "for (item in items) { sink(item, 1) }; for (String item : items) { sink(item, 1) }",
            "item",
            EntityKind::Variable,
            2,
        ),
        (
            "try (Reader handle = open()) { sink(handle, 1) } finally {}; try (Reader handle = open()) { sink(handle, 1) } finally {}",
            "handle",
            EntityKind::Variable,
            2,
        ),
        (
            "for (int index = 0; index < 2; index++) { sink(index, 1) }; for (int index = 0; index < 2; index++) { sink(index, 1) }",
            "index",
            EntityKind::Variable,
            2,
        ),
        (
            "def (left, right) = pair; sink(left, 1)",
            "left",
            EntityKind::Variable,
            1,
        ),
    ] {
        let source = format!("def run() {{ {body} }}\n");
        let result = output(&source);
        let doc = result.document();
        assert!(
            doc.diagnostics.is_empty(),
            "{source}: {:?}",
            doc.diagnostics
        );
        let owner = doc
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "run")
            .unwrap();
        let bindings: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name)
            .collect();
        assert_eq!(
            bindings.len(),
            count,
            "{source}: {:?}; {:?}",
            doc.entities,
            doc.skipped_regions
        );
        assert_eq!(
            bindings
                .iter()
                .map(|entity| entity.id)
                .collect::<BTreeSet<_>>()
                .len(),
            count
        );
        for binding in bindings {
            assert_eq!(binding.kind, kind);
            assert_eq!(
                binding.container,
                Some(rootlight_ir::ContainerRef::Entity(owner.id))
            );
            let definitions: Vec<_> = doc
                .occurrences
                .iter()
                .filter(|occurrence| {
                    occurrence.role == OccurrenceRole::Definition
                        && occurrence.target == OccurrenceTarget::Resolved { symbol: binding.id }
                })
                .collect();
            assert_eq!(definitions.len(), 1);
            let span = definitions[0].source.span();
            assert_eq!(
                source.get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()
                ),
                Some(name)
            );
        }
        let changed = output(&source.replace(", 1)", ", 12345)"));
        let identities = |output: &AnalysisOutput| {
            output
                .document()
                .entities
                .iter()
                .map(|entity| (entity.id, entity.container))
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(identities(&result), identities(&changed));
    }
}

#[test]
fn groovy_type_parameters_and_record_components_have_authored_owners() {
    let source = "class Store<T extends Number> { <U> U convert(U value) { value } }\nrecord Entry(String title, int size) {}\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    for (name, kind, owner) in [
        ("T", EntityKind::TypeParameter, "Store"),
        ("U", EntityKind::TypeParameter, "convert"),
        ("title", EntityKind::Property, "Entry"),
        ("size", EntityKind::Property, "Entry"),
    ] {
        let entity = doc
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap_or_else(|| panic!("missing {name}: {:?}", doc.entities));
        let parent = doc
            .entities
            .iter()
            .find(|entity| entity.canonical_name == owner)
            .unwrap();
        assert_eq!(entity.kind, kind);
        assert_eq!(
            entity.container,
            Some(rootlight_ir::ContainerRef::Entity(parent.id))
        );
        let definition = doc
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
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

#[test]
fn groovy_package_owns_top_level_types_without_stealing_members() {
    let source = "// header\npackage sample.catalog\nclass Meter { int read() { 1 } }\ninterface Named {}\ntrait Stored {}\nenum Mode { OPEN }\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    let named = |name: &str| {
        doc.entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap_or_else(|| panic!("missing {name}: {:?}", doc.entities))
    };
    let package = named("sample.catalog");
    assert_eq!(package.kind, EntityKind::Module);
    for name in ["Meter", "Named", "Stored", "Mode"] {
        let entity = named(name);
        assert_eq!(
            entity.container,
            Some(rootlight_ir::ContainerRef::Entity(package.id)),
            "{name}"
        );
        assert!(doc.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(package.id)
                && relation.object == RelationEndpoint::Entity(entity.id)
        }));
    }
    assert_eq!(
        named("read").container,
        Some(rootlight_ir::ContainerRef::Entity(named("Meter").id))
    );
    assert_eq!(
        named("OPEN").container,
        Some(rootlight_ir::ContainerRef::Entity(named("Mode").id))
    );
    let definitions: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target == OccurrenceTarget::Resolved { symbol: package.id }
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    let span = definitions[0].source.span();
    assert_eq!(
        source.get(
            usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap()
        ),
        Some("sample.catalog")
    );
    let edited = output(&source.replace("{ 1 }", "{ 12345 }"));
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| (entity.id, entity.container))
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(identities(&result), identities(&edited));
    let renamed = output(&source.replace("sample.catalog", "sample.storage"));
    let renamed_class = renamed
        .document()
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Meter")
        .unwrap();
    assert_ne!(named("Meter").id, renamed_class.id);
}

#[test]
fn groovy_declarations_keep_exact_definition_sources() {
    let result = output(GROOVY.source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    for (name, kind) in [
        ("Meter", EntityKind::Class),
        ("title", EntityKind::Property),
        ("value", EntityKind::Field),
        ("read", EntityKind::Method),
        ("render", EntityKind::Function),
    ] {
        let entities: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
            .collect();
        assert_eq!(entities.len(), 1, "{name}/{kind:?}: {:?}", doc.entities);
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target
                        == (OccurrenceTarget::Resolved {
                            symbol: entities[0].id,
                        })
            })
            .collect();
        assert_eq!(definitions.len(), 1, "{name}");
        let span = definitions[0].source.span();
        assert_eq!(
            GROOVY.source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(name)
        );
    }
}

#[test]
fn groovy_package_trivia_keeps_identity_and_exact_definition_bytes() {
    let mut expected = None;
    for (prefix, name, separator) in [
        ("", "sample.catalog", "\n"),
        ("/** Package documentation. */\n", "sample.catalog", "\n"),
        ("// header\n", "sample /* component */ . catalog", "; "),
        ("#!/usr/bin/env groovy\n", "sample.catalog", ";\n"),
    ] {
        let source =
            format!("{prefix}package {name}{separator}class Meter {{ int read() {{ 1 }} }}\n");
        let result = output(&source);
        let doc = result.document();
        assert!(
            doc.diagnostics.is_empty(),
            "{source}: {:?}",
            doc.diagnostics
        );
        let package = doc
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "sample.catalog")
            .unwrap();
        let class = doc
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Meter")
            .unwrap();
        assert_eq!(
            class.container,
            Some(rootlight_ir::ContainerRef::Entity(package.id))
        );
        let definition = doc
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: package.id }
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
        let identities = doc
            .entities
            .iter()
            .map(|entity| (entity.id, entity.container))
            .collect::<BTreeSet<_>>();
        if let Some(expected) = &expected {
            assert_eq!(&identities, expected);
        } else {
            expected = Some(identities);
        }
    }
    let default_package = output("class Meter {}\n");
    let doc = default_package.document();
    let class = doc
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Meter")
        .unwrap();
    let file = doc
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Module)
        .unwrap();
    assert_eq!(file.canonical_name, GROOVY.path);
    assert_eq!(
        class.container,
        Some(rootlight_ir::ContainerRef::Entity(file.id))
    );
    assert_eq!(
        doc.entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Module)
            .count(),
        1
    );
}

#[test]
fn groovy_invalid_package_positions_report_source_scoped_ownership_gaps() {
    for (source, expected_headers) in [
        (
            "class Before {}\npackage late\nclass After {}\n",
            vec!["package late"],
        ),
        (
            "package first\npackage second\nclass After {}\n",
            vec!["package first", "package second"],
        ),
        ("def run() { package nested; 1 }\n", vec!["package nested"]),
    ] {
        let result = output(source);
        let doc = result.document();
        let headers: BTreeSet<_> = doc
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "groovy-invalid-package-ownership-unavailable")
            .map(|gap| {
                let span = gap.source.span();
                source
                    .get(
                        usize::try_from(span.start_byte()).unwrap()
                            ..usize::try_from(span.end_byte()).unwrap(),
                    )
                    .unwrap()
            })
            .collect();
        assert_eq!(
            headers,
            expected_headers.into_iter().collect(),
            "{source}: {:?}",
            doc.skipped_regions
        );
        if let Some(after) = doc
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "After")
        {
            let owner = doc
                .entities
                .iter()
                .find(|entity| {
                    Some(rootlight_ir::ContainerRef::Entity(entity.id)) == after.container
                })
                .unwrap();
            assert_eq!(owner.canonical_name, GROOVY.path);
        }
    }
}

#[test]
fn groovy_package_unavailable_name_is_not_silently_dropped() {
    let source = "package sample.\n catalog\nclass Meter {}\n";
    let result = output(source);
    let doc = result.document();
    let gap = doc
        .skipped_regions
        .iter()
        .find(|gap| gap.detail == "groovy-package-name-unavailable")
        .unwrap_or_else(|| {
            panic!(
                "missing name gap: {:?}; {:?}",
                doc.skipped_regions, doc.diagnostics
            )
        });
    let span = gap.source.span();
    assert_eq!(
        source.get(
            usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap()
        ),
        Some("sample.\n catalog")
    );
    assert!(
        !doc.entities
            .iter()
            .any(|entity| entity.canonical_name == "sample.catalog")
    );
    assert!(
        !doc.entities
            .iter()
            .any(|entity| entity.canonical_name == "Meter"),
        "an unavailable package cannot silently become the default package"
    );
}

#[test]
fn groovy_multiple_bindings_and_parameters_have_distinct_owners() {
    let source = "trait Named { String label() { 'name' } }\nclass Meter { private int first, second = 2; Meter(int seed) { first = seed }\nint read(int delta) { int left = 1, right = 2; return left + right + delta } }\nenum Mode { OPEN, CLOSED }\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    for (name, kind) in [
        ("Named", EntityKind::Trait),
        ("label", EntityKind::Method),
        ("Meter", EntityKind::Class),
        ("Meter", EntityKind::Constructor),
        ("first", EntityKind::Field),
        ("second", EntityKind::Field),
        ("seed", EntityKind::Parameter),
        ("read", EntityKind::Method),
        ("delta", EntityKind::Parameter),
        ("left", EntityKind::Variable),
        ("right", EntityKind::Variable),
        ("Mode", EntityKind::Enum),
        ("OPEN", EntityKind::Constant),
        ("CLOSED", EntityKind::Constant),
    ] {
        let entities: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
            .collect();
        assert_eq!(
            entities.len(),
            1,
            "{name}/{kind:?}: {:?}; {:?}",
            doc.entities,
            doc.skipped_regions
        );
        assert!(entities[0].container.is_some(), "{name}");
    }
}

#[test]
fn groovy_body_edits_preserve_declaration_identity() {
    let before = output(GROOVY.source);
    let after = output(&GROOVY.source.replace(GROOVY.body_before, GROOVY.body_after));
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| (entity.canonical_name.clone(), entity.id))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(identities(&before), identities(&after));
}

#[test]
fn groovy_properties_and_explicit_fields_keep_exact_authored_identity() {
    let source = "class Ledger { String title; private int secret; protected int guarded; public int exposed; static int count; final int version = 1; String first, second; @Deprecated String legacy }\ntrait Named { String label; private int slot }\ninterface Limits { int MAX = 10 }\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    for (name, kind, owner) in [
        ("title", EntityKind::Property, "Ledger"),
        ("secret", EntityKind::Field, "Ledger"),
        ("guarded", EntityKind::Field, "Ledger"),
        ("exposed", EntityKind::Field, "Ledger"),
        ("count", EntityKind::Property, "Ledger"),
        ("version", EntityKind::Property, "Ledger"),
        ("first", EntityKind::Property, "Ledger"),
        ("second", EntityKind::Property, "Ledger"),
        ("legacy", EntityKind::Property, "Ledger"),
        ("label", EntityKind::Property, "Named"),
        ("slot", EntityKind::Field, "Named"),
        ("MAX", EntityKind::Field, "Limits"),
    ] {
        let entities: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name)
            .collect();
        assert_eq!(entities.len(), 1, "{name}: {entities:?}");
        let entity = entities[0];
        assert_eq!(entity.kind, kind, "{name}");
        let container = doc
            .entities
            .iter()
            .find(|candidate| {
                Some(rootlight_ir::ContainerRef::Entity(candidate.id)) == entity.container
            })
            .unwrap();
        assert_eq!(container.canonical_name, owner, "{name}");
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1, "{name}");
        let span = definitions[0].source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(name)
        );
    }
    assert!(
        !doc.entities
            .iter()
            .any(|entity| matches!(entity.kind, EntityKind::Method | EntityKind::Function)),
        "implicit accessors have no authored declarations"
    );
}

#[test]
fn groovy_dynamic_receiver_is_not_resolved_by_matching_method_name() {
    let source = "class Meter { int read() { 1 } }\ndef run(target) { target.read() }\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    let calls: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(calls.len(), 1);
    assert!(!matches!(
        calls[0].target,
        OccurrenceTarget::Resolved { .. }
    ));
    assert!(
        !doc.relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    assert!(doc.skipped_regions.iter().any(|gap| {
        gap.detail == "groovy-import-inheritance-and-runtime-dispatch-resolution-unavailable"
    }));
}

#[test]
fn groovy_quoted_method_names_keep_decoded_identity_and_exact_written_definitions() {
    for (written, name) in [
        ("'with space'", "with space"),
        ("\"double space\"", "double space"),
        ("'''triple space'''", "triple space"),
        ("\"\"\"triple double\"\"\"", "triple double"),
        ("/slash name/", "slash name"),
        ("$/dollar name/$", "dollar name"),
        (r"'uni\u0063ode'", "unicode"),
        (r"'quote\'name'", "quote'name"),
        (r"'surrogate\uD83D\uDE80'", "surrogate🚀"),
        (r"'line\nname'", r#""line\u000aname""#),
        (r"'lone\uD800'", r#""lone\ud800""#),
        ("''", "\"\""),
    ] {
        let source = format!("class Sample {{ def {written}() {{ 1 }} }}\n");
        let result = output(&source);
        let doc = result.document();
        assert!(
            doc.diagnostics.is_empty(),
            "{written}: {:?}",
            doc.diagnostics
        );
        let methods: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Method)
            .collect();
        assert_eq!(
            methods.len(),
            1,
            "{written}: {:?}; {:?}",
            doc.entities,
            doc.skipped_regions
        );
        assert_eq!(methods[0].canonical_name, name, "{written}");
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target
                        == OccurrenceTarget::Resolved {
                            symbol: methods[0].id,
                        }
            })
            .collect();
        assert_eq!(definitions.len(), 1, "{written}");
        let span = definitions[0].source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(written)
        );
        assert_eq!(
            definitions[0].syntactic_text_hash,
            content_hash(written.as_bytes())
        );
        assert!(
            !doc.skipped_regions
                .iter()
                .any(|gap| gap.detail == "groovy-quoted-name-unavailable")
        );
    }
}

#[test]
fn groovy_unavailable_method_names_report_exact_gaps_without_inventing_local_owners() {
    for written in [r#""${name}""#, r#""$name""#, r"'bad\q'"] {
        let source = format!(
            "class Sample {{ def {written}(parameter) {{ def localValue = 1 }}\ndef valid() {{ 2 }} }}\n"
        );
        let result = output(&source);
        let doc = result.document();
        assert!(
            doc.diagnostics.is_empty(),
            "{written}: {:?}",
            doc.diagnostics
        );
        let methods: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Method)
            .map(|entity| entity.canonical_name.as_str())
            .collect();
        assert_eq!(methods, ["valid"], "{written}");
        assert!(
            !doc.entities
                .iter()
                .any(|entity| matches!(entity.canonical_name.as_str(), "parameter" | "localValue"))
        );
        let gaps: Vec<_> = doc
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "groovy-quoted-name-unavailable")
            .collect();
        assert_eq!(gaps.len(), 1, "{written}: {:?}", doc.skipped_regions);
        let span = gaps[0].source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(written)
        );
    }
}

#[test]
fn groovy_closures_own_explicit_parameters_and_locals_without_written_names() {
    for (source, callable, header, bindings) in [
        (
            "def transform = { String value -> def local = value; local }\n",
            "{ String value -> def local = value; local }",
            "{ String value ->",
            vec!["value", "local"],
        ),
        (
            "class Store { def transform = { value -> value } }\n",
            "{ value -> value }",
            "{ value ->",
            vec!["value"],
        ),
        (
            "def run() { consume { value -> value } }\n",
            "{ value -> value }",
            "{ value ->",
            vec!["value"],
        ),
        (
            "def message = \"${writer -> writer << 'ready'}\"\n",
            "writer -> writer << 'ready'",
            "writer ->",
            vec!["writer"],
        ),
        ("def callback = { -> 1 }\n", "{ -> 1 }", "{ ->", vec![]),
        ("def callback = { it }\n", "{ it }", "{", vec![]),
    ] {
        let result = output(source);
        let doc = result.document();
        assert!(
            doc.diagnostics.is_empty(),
            "{source}: {:?}",
            doc.diagnostics
        );
        let functions: Vec<_> = doc
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == "<anonymous>")
            .collect();
        assert_eq!(functions.len(), 1, "{source}: {:?}", doc.entities);
        let function = functions[0];
        assert_eq!(function.kind, EntityKind::Function);
        assert!(
            function
                .flags
                .contains(&rootlight_ir::EntityFlag::Synthetic)
        );
        let signatures: Vec<_> = doc
            .extensions
            .iter()
            .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
            .filter_map(|extension| {
                let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
                if lexical.kind() != rootlight_ir::LexicalEvidenceKind::Signature
                    || lexical.subject() != rootlight_ir::FactRef::Entity(function.id)
                {
                    return None;
                }
                let span = extension.evidence.source.as_ref().unwrap().span();
                assert_eq!(
                    source.get(
                        usize::try_from(span.start_byte()).unwrap()
                            ..usize::try_from(span.end_byte()).unwrap()
                    ),
                    Some(lexical.text())
                );
                Some(lexical.text().to_owned())
            })
            .collect();
        assert_eq!(signatures, [header]);
        let span = function.evidence.source.as_ref().unwrap().span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(callable)
        );
        assert!(!doc.occurrences.iter().any(|occurrence| occurrence.role
            == OccurrenceRole::Definition
            && occurrence.target
                == OccurrenceTarget::Resolved {
                    symbol: function.id
                }));
        for name in bindings {
            let binding = doc
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name)
                .unwrap();
            assert_eq!(
                binding.container,
                Some(rootlight_ir::ContainerRef::Entity(function.id)),
                "{name}"
            );
            assert!(
                doc.relations
                    .iter()
                    .any(|relation| relation.predicate == RelationPredicate::Contains
                        && relation.subject == RelationEndpoint::Entity(function.id)
                        && relation.object == RelationEndpoint::Entity(binding.id))
            );
        }
        assert!(
            !doc.entities
                .iter()
                .any(|entity| entity.canonical_name == "it"),
            "implicit parameters have no authored definition"
        );
    }
}

#[test]
fn groovy_sibling_and_nested_closures_keep_distinct_body_independent_identities() {
    let source =
        "def run() { consume({ value -> value }, { value -> { nested -> nested + value } }) }\n";
    let before = output(source);
    let doc = before.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    let closures: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "<anonymous>")
        .collect();
    assert_eq!(closures.len(), 3, "{:?}", doc.entities);
    assert_eq!(
        closures
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
    for (name, owner_source) in [
        ("value", "{ value -> value }"),
        ("value", "{ value -> { nested -> nested + value } }"),
        ("nested", "{ nested -> nested + value }"),
    ] {
        let owner = closures
            .iter()
            .find(|entity| {
                let span = entity.evidence.source.as_ref().unwrap().span();
                source.get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap(),
                ) == Some(owner_source)
            })
            .unwrap();
        assert_eq!(
            doc.entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Parameter
                    && entity.canonical_name == name
                    && entity.container == Some(rootlight_ir::ContainerRef::Entity(owner.id)))
                .count(),
            1
        );
    }
    let after = output(&source.replace("nested + value", "nested + value + 12345"));
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| (entity.id, entity.canonical_name.clone(), entity.container))
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(identities(&before), identities(&after));
}

#[test]
fn groovy_disjoint_block_variables_keep_distinct_source_identities() {
    let source = "def run() { if (true) { def local = 1 }; if (false) { def local = 2 } }\n";
    let result = output(source);
    let doc = result.document();
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    let locals: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "local")
        .collect();
    assert_eq!(
        locals.len(),
        2,
        "{:?}; {:?}",
        doc.entities,
        doc.skipped_regions
    );
    assert_ne!(locals[0].id, locals[1].id);
    let after = output(&source.replace("= 1", "= 12345"));
    let ids = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(ids(&result), ids(&after));
}
