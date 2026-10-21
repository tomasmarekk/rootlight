//! Source-bound controls for the retained Rocq native parser and its state.
//! Syntax recovery and generic tactics do not establish complete Coq semantics.

use tree_sitter::{InputEdit, Parser, Point, Tree};

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rocq::LANGUAGE.into())
        .unwrap();
    parser
}

fn shapes(tree: &Tree) -> Vec<(String, std::ops::Range<usize>, usize, bool, bool)> {
    let mut output = Vec::new();
    let mut pending = vec![(tree.root_node(), 0)];
    while let Some((node, depth)) = pending.pop() {
        output.push((
            node.kind().to_owned(),
            node.byte_range(),
            depth,
            node.is_error(),
            node.is_missing(),
        ));
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor).map(|child| (child, depth + 1)));
    }
    output
}

#[test]
fn runtime_loads_the_actual_static_rocq_abi_and_node_metadata() {
    let language: tree_sitter::Language = tree_sitter_rocq::LANGUAGE.into();
    assert_eq!(language.abi_version(), 15);
    assert_eq!(language.field_count(), 38);
    assert_eq!(language.name(), Some("rocq"));
    let metadata: serde_json::Value = serde_json::from_str(tree_sitter_rocq::NODE_TYPES).unwrap();
    assert!(
        metadata
            .as_array()
            .unwrap()
            .iter()
            .any(|row| { row["type"] == "definition_command" && row["named"] == true })
    );
    let tree = parser().parse("Definition value := 1.\n", None).unwrap();
    assert!(!tree.root_node().has_error());
    assert_eq!(tree.root_node().kind(), "source_file");
}

#[test]
fn ordinary_written_commands_keep_exact_nodes_names_and_physical_offsets() {
    let source = concat!(
        "(* λ😀 (* Definition decoy := 0. *) *)\n",
        "From Coq Require Import Init.Nat.\nModule Written.\n",
        "Definition résumé (n : nat) := n.\n",
        "Lemma statement : True. Proof. exact I. Qed.\n",
        "Inductive Flag : Type := | on | off.\n",
        "Fixpoint identity (n : nat) : nat := n.\nEnd Written.\n",
    );
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert_eq!(tree.root_node().byte_range(), 0..source.len());
    let nodes = shapes(&tree);
    for (kind, text) in [
        ("require_command", "From Coq Require Import Init.Nat"),
        ("module_command", "Module Written"),
        ("definition_command", "Definition résumé (n : nat) := n"),
        ("theorem_command", "Lemma statement : True"),
        ("inductive_command", "Inductive Flag : Type := | on | off"),
        ("fixpoint_command", "Fixpoint identity (n : nat) : nat := n"),
        ("end_command", "End Written"),
        ("ident_decl", "résumé"),
    ] {
        let start = source.find(text).unwrap();
        assert_eq!(
            nodes
                .iter()
                .filter(|node| node.0 == kind && node.1 == (start..start + text.len()))
                .count(),
            1,
            "missing exact {kind}: {text}: {}",
            tree.root_node().to_sexp(),
        );
    }
    assert_eq!(
        nodes
            .iter()
            .filter(|node| node.0 == "definition_command")
            .count(),
        1
    );
    assert!(
        nodes
            .iter()
            .all(|node| source.get(node.1.clone()).is_some())
    );
}

#[test]
fn incomplete_commands_and_invalid_physical_bytes_are_not_hidden() {
    for source in [
        b"Definition unfinished := (".as_slice(),
        b"Lemma unfinished :".as_slice(),
        b"(* unfinished nested (* comment *)".as_slice(),
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let nodes = shapes(&tree);
        assert!(nodes.iter().any(|node| node.3 || node.4));
        assert!(nodes.iter().all(|node| node.1.end <= source.len()));
    }
    let mut source = b"Definition value := ".to_vec();
    source.extend([0xff, b'.', b'\n']);
    assert!(std::str::from_utf8(&source).is_err());
    let tree = parser().parse(&source, None).unwrap();
    assert!(shapes(&tree).iter().all(|node| node.1.end <= source.len()));
}

#[test]
fn unsupported_command_forms_are_visible_without_a_dedicated_declaration_claim() {
    let language: tree_sitter::Language = tree_sitter_rocq::LANGUAGE.into();
    for (source, absent_kind) in [
        ("Declare ML Module \"custom.plugin\".\n", "plugin_command"),
        (
            "CoInductive Stream := More : Stream.\n",
            "coinductive_command",
        ),
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert_eq!(tree.root_node().byte_range(), 0..source.len());
        assert_eq!(language.id_for_node_kind(absent_kind, true), 0);
        assert!(!shapes(&tree).iter().any(|node| node.0 == absent_kind));
    }
    // Written notation syntax does not install a notation into later parsing.
    let source = "Notation \"custom\" := 1.\nDefinition value := custom.\n";
    let tree = parser().parse(source, None).unwrap();
    assert!(
        shapes(&tree)
            .iter()
            .any(|node| node.0 == "notation_command")
    );
    assert!(
        shapes(&tree)
            .iter()
            .any(|node| node.0 == "ident" && &source[node.1.clone()] == "custom")
    );
}

#[test]
fn incremental_edits_and_reused_parser_state_match_a_fresh_tree() {
    let before = "Definition value := 1.\nLemma statement : True. Proof. exact I. Qed.\n";
    let after = before.replacen('1', "12", 1);
    let start = before.find('1').unwrap();
    let mut parser = parser();
    let mut old = parser.parse(before, None).unwrap();
    old.edit(&InputEdit {
        start_byte: start,
        old_end_byte: start + 1,
        new_end_byte: start + 2,
        start_position: Point::new(0, start),
        old_end_position: Point::new(0, start + 1),
        new_end_position: Point::new(0, start + 2),
    });
    let incremental = parser.parse(&after, Some(&old)).unwrap();
    let fresh = self::parser().parse(&after, None).unwrap();
    assert_eq!(shapes(&incremental), shapes(&fresh));
    parser.parse("Definition broken := (", None).unwrap();
    assert_eq!(shapes(&parser.parse(&after, None).unwrap()), shapes(&fresh));
}

#[test]
fn written_module_signatures_functors_and_aliases_keep_named_structure() {
    for (source, kind, name) in [
        (
            "Module Type Signature.\n",
            "module_type_command",
            "Signature",
        ),
        (
            "Module Type QuotationOfTerm (T : Term).\n",
            "module_type_command",
            "QuotationOfTerm",
        ),
        ("Module Export FSets.\n", "module_command", "FSets"),
        ("Module Alias := Parent.Child.\n", "module_command", "Alias"),
        (
            "Module Applied := F.Make Arg Other.\n",
            "module_command",
            "Applied",
        ),
        (
            "Module Functor (Import T : Term) (E : EnvironmentSig T).\n",
            "module_command",
            "Functor",
        ),
        (
            "Module Checked <: Signature Arg.\n",
            "module_command",
            "Checked",
        ),
        (
            "Module Sealed : Signature := Make Arg.\n",
            "module_command",
            "Sealed",
        ),
        (
            "Module Type Alias := Parent.Signature.\n",
            "module_type_command",
            "Alias",
        ),
        (
            "Module Atom := (Parent.Make) Arg.\n",
            "module_command",
            "Atom",
        ),
        (
            "Module Typed : (Signature Arg).\n",
            "module_command",
            "Typed",
        ),
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        let command = tree
            .root_node()
            .named_child(0)
            .unwrap()
            .named_child(0)
            .unwrap();
        assert_eq!(command.kind(), kind, "{source}");
        let written = command.child_by_field_name("name").unwrap();
        assert_eq!(&source[written.byte_range()], name);
        assert!(
            !shapes(&tree)
                .iter()
                .any(|node| node.0 == "generic_tactic_body")
        );
    }
    let source = "Include Base <+ Extra.\n";
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert!(shapes(&tree).iter().any(|node| node.0 == "include_command"));
    for source in [
        "Module := Base.",
        "Module Invalid (T :).",
        "Module Invalid :=.",
        "Include.",
        "Module Bad := ! F [inline at level 1].",
        "Module Bad := (F X).",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn context_binders_and_generalizing_constraints_are_written_nodes() {
    for (source, expected_constraints) in [
        ("Context (A : Type).\n", vec![]),
        ("Context {A : Type}.\n", vec![]),
        ("Context {T} {M : Monad T}.\n", vec![]),
        (
            "Context `{cf : checker_flags}.\n",
            vec![(Some("cf"), "checker_flags")],
        ),
        ("Context `{!EqDec A}.\n", vec![(None, "EqDec A")]),
        (
            "Context `(A : Type) `{H : !EqDec A, Other A}.\n",
            vec![
                (Some("A"), "Type"),
                (Some("H"), "EqDec A"),
                (None, "Other A"),
            ],
        ),
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        let command = tree
            .root_node()
            .named_child(0)
            .unwrap()
            .named_child(0)
            .unwrap();
        assert_eq!(command.kind(), "context_command");
        assert_eq!(command.byte_range(), 0..source.trim_end().len() - 1);
        assert!(
            !shapes(&tree)
                .iter()
                .any(|node| node.0 == "generic_tactic_body")
        );
        if source.contains('`') {
            assert!(
                shapes(&tree)
                    .iter()
                    .any(|node| node.0 == "generalizing_binder")
            );
        }
        let mut pending = vec![tree.root_node()];
        let mut constraints = Vec::new();
        while let Some(node) = pending.pop() {
            if node.kind() == "generalizing_constraint" {
                constraints.push(node);
            }
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor));
        }
        constraints.sort_by_key(|node| node.start_byte());
        let actual = constraints
            .iter()
            .map(|node| {
                (
                    node.child_by_field_name("name")
                        .map(|name| &source[name.byte_range()]),
                    &source[node.child_by_field_name("type").unwrap().byte_range()],
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected_constraints, "{source}");
    }
    for source in ["Context.", "Context {A :}.", "Context `{H : !}."] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
    }
}

fn named_nodes<'tree>(root: tree_sitter::Node<'tree>, kind: &str) -> Vec<tree_sitter::Node<'tree>> {
    let mut pending = vec![root];
    let mut found = Vec::new();
    while let Some(node) = pending.pop() {
        if node.kind() == kind {
            found.push(node);
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    found.sort_by_key(tree_sitter::Node::start_byte);
    found
}

#[test]
fn instance_commands_keep_original_names_types_and_distinct_term_nodes() {
    // Exact original excerpts and their MIT notice are retained beside these tests.
    // Syntax controls do not establish that the standalone excerpts typecheck.
    for (source, name, body_kind, expected_type) in [
        (
            include_str!("fixtures/coq-instances/dirpath_eqdec.v"),
            "dirpath_eqdec",
            None,
            "Classes.EqDec dirpath",
        ),
        (
            include_str!("fixtures/coq-instances/reflect_prim_int.v"),
            "reflect_prim_int",
            Some("instance_field_body"),
            "ReflectEq Numbers.Cyclic.Int63.Uint63.int",
        ),
        (
            include_str!("fixtures/coq-instances/subrel_config_impl_cmp.v"),
            "subrel_config_impl_cmp",
            None,
            "config.impl cf1 cf2 ->\r\n  RelationClasses.subrelation (@compare_sort cf1 cs pb) (@compare_sort cf2 cs pb)",
        ),
        (
            include_str!("fixtures/coq-instances/subst_instance_list.v"),
            "subst_instance_list",
            Some("generalizing_binder"),
            "UnivSubst (list A)",
        ),
        (
            include_str!("fixtures/coq-instances/quote_name.v"),
            "quote_name",
            Some("ltac_term"),
            "ground_quotable name",
        ),
        (
            include_str!("fixtures/coq-instances/ascii_eqdec.v"),
            "ascii_eqdec",
            None,
            "EqDec ascii",
        ),
        (
            include_str!("fixtures/coq-instances/Monad_result.v"),
            "Monad_result",
            Some("record_construction"),
            "Monad (fun T => result T E)",
        ),
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{name}: {}",
            tree.root_node().to_sexp()
        );
        let commands = named_nodes(tree.root_node(), "instance_command");
        assert_eq!(commands.len(), 1, "{name}: {}", tree.root_node().to_sexp());
        let command = commands[0];
        let written = command.child_by_field_name("name").unwrap();
        assert_eq!(&source[written.byte_range()], name);
        assert_eq!(written.start_byte(), source.find(name).unwrap());
        assert_eq!(command.start_byte(), source.find("Instance").unwrap());
        assert_eq!(command.end_byte(), source.len() - 1);
        assert_eq!(written.kind(), "ident_decl");
        assert_eq!(
            &source[command.child_by_field_name("type").unwrap().byte_range()],
            expected_type
        );
        if let Some(kind) = body_kind {
            assert!(!named_nodes(command, kind).is_empty(), "{name}: {kind}");
        }
        if name == "subrel_config_impl_cmp" {
            assert!(command.child_by_field_name("body").is_none());
        }
        if matches!(name, "reflect_prim_int" | "Monad_result") {
            let assignments = named_nodes(command, "field_assignment");
            let field_names = assignments
                .iter()
                .map(|field| &source[field.child_by_field_name("name").unwrap().byte_range()])
                .collect::<Vec<_>>();
            assert_eq!(
                field_names,
                if name == "reflect_prim_int" {
                    vec!["eqb"]
                } else {
                    vec!["ret", "bind"]
                }
            );
            if name == "reflect_prim_int" {
                assert_eq!(
                    &source[assignments[0]
                        .child_by_field_name("value")
                        .unwrap()
                        .byte_range()],
                    "Numbers.Cyclic.Int63.Uint63.eqb"
                );
            }
        }
    }
}

#[test]
fn instance_proof_priority_and_existing_registration_remain_distinct() {
    for (source, command_kind, name) in [
        ("Instance pending : C.\n", "instance_command", "pending"),
        (
            "Instance prioritized : C | 7 := value.\n",
            "instance_command",
            "prioritized",
        ),
        (
            "Declare Instance required : C | 2.\n",
            "declare_instance_command",
            "required",
        ),
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let commands = named_nodes(tree.root_node(), command_kind);
        assert_eq!(
            commands.len(),
            1,
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        assert_eq!(
            &source[commands[0]
                .child_by_field_name("name")
                .unwrap()
                .byte_range()],
            name
        );
        if source.contains('|') {
            let priorities = named_nodes(commands[0], "instance_priority");
            assert_eq!(priorities.len(), 1, "{}", tree.root_node().to_sexp());
            assert_eq!(
                &source[priorities[0].byte_range()],
                if name == "prioritized" { "| 7" } else { "| 2" }
            );
        }
    }
    for source in [
        "Existing Instance prior.\n",
        "Existing Instance Module.prior | 3.\n",
        "Existing Instances first Module.second | 5.\n",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        assert_eq!(
            named_nodes(tree.root_node(), "existing_instance_command").len(),
            1
        );
        assert!(named_nodes(tree.root_node(), "instance_command").is_empty());
        assert!(named_nodes(tree.root_node(), "ident_decl").is_empty());
    }
}

#[test]
fn malformed_instance_terms_and_noncommand_decoys_cannot_be_clean_named_commands() {
    for source in [
        "Instance missing_type := 0.",
        "Instance missing_body : C := .",
        "Instance unclosed : C := ltac:(exact I.",
        "Instance bad_field : C := { field := }.",
        "Instance bad_record : C := {| field := |}.",
        "Instance empty_priority : C | := value.",
        "(* Instance comment_decoy : C := _. *)",
        "Definition text := \"Instance string_decoy : C := _.\".",
    ] {
        let tree = parser().parse(source, None).unwrap();
        let clean = named_nodes(tree.root_node(), "instance_command")
            .into_iter()
            .filter(|command| {
                !command.has_error()
                    && command.parent().is_some_and(|sentence| {
                        sentence.kind() == "sentence"
                            && !sentence.has_error()
                            && sentence
                                .parent()
                                .is_some_and(|root| root.kind() == "source_file")
                    })
            })
            .count();
        assert_eq!(clean, 0, "{source}: {}", tree.root_node().to_sexp());
    }
    for source in [
        "Existing Instance \"not_a_reference\".",
        "Existing Instance.",
        "Declare Instance missing_type.",
        "Existing Class \"not_a_reference\".",
        "Existing Class.",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn record_field_types_belong_in_binders_not_bare_assignment_annotations() {
    for source in [
        "Instance methods : C := { field (x : nat) := x }.",
        "Definition methods := {| field (x : nat) := x |}.",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let assignments = named_nodes(tree.root_node(), "field_assignment");
        assert_eq!(assignments.len(), 1);
        let field = assignments[0];
        assert_eq!(
            &source[field.child_by_field_name("name").unwrap().byte_range()],
            "field"
        );
        assert_eq!(
            &source[field.child_by_field_name("value").unwrap().byte_range()],
            "x"
        );
        let binders = named_nodes(field, "binder");
        assert_eq!(binders.len(), 1);
        assert_eq!(&source[binders[0].byte_range()], "(x : nat)");
    }
    let invalid = [
        "Instance bad : C := { field : nat := 0 }.",
        "Definition bad := {| field : nat := 0 |}.",
    ];
    let outcomes = invalid.map(|source| {
        let tree = parser().parse(source, None).unwrap();
        (tree.root_node().has_error(), tree.root_node().to_sexp())
    });
    assert_eq!(
        outcomes.each_ref().map(|(error, _)| *error),
        [true, true],
        "{outcomes:?}"
    );
}

#[test]
fn existing_class_registration_preserves_original_plugin_neighbors() {
    let source = include_str!("fixtures/coq-instances/tmExistingInstance.v");
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let commands = named_nodes(tree.root_node(), "existing_class_command");
    assert_eq!(commands.len(), 1);
    let registration = commands[0];
    assert_eq!(&source[registration.byte_range()], "Existing Class True");
    assert_eq!(registration.byte_range(), 220..239);
    assert!(registration.child_by_field_name("name").is_none());
    assert!(named_nodes(registration, "ident_decl").is_empty());
    let references = named_nodes(registration, "reference");
    assert_eq!(references.len(), 1);
    assert_eq!(&source[references[0].byte_range()], "True");
    assert_eq!(references[0].byte_range(), 235..239);
    let mut retained = shapes(&tree)
        .into_iter()
        .filter(|(kind, _, _, _, _)| {
            matches!(
                kind.as_str(),
                "require_command" | "import_command" | "evaluation_command"
            )
        })
        .map(|(kind, range, _, _, _)| (kind, range))
        .collect::<Vec<_>>();
    retained.sort_by_key(|(_, range)| range.start);
    assert_eq!(
        retained,
        vec![
            ("require_command".into(), 0..40),
            ("require_command".into(), 43..84),
            ("import_command".into(), 87..107),
            ("evaluation_command".into(), 290..310),
        ]
    );
    let qualified = "Existing Class Module.carrier.";
    let tree = parser().parse(qualified, None).unwrap();
    assert!(!tree.root_node().has_error());
    let registrations = named_nodes(tree.root_node(), "existing_class_command");
    assert_eq!(registrations.len(), 1);
    assert!(named_nodes(registrations[0], "ident_decl").is_empty());
    let references = named_nodes(registrations[0], "reference");
    assert_eq!(references.len(), 1);
    assert_eq!(&qualified[references[0].byte_range()], "Module.carrier");
}

#[test]
fn record_class_original_declarations_keep_heads_fields_and_written_constructors() {
    for (source, kind, name, constructor, fields) in [
        (
            include_str!("fixtures/coq-records/binder_annot.v"),
            "record_command",
            "binder_annot",
            Some("mkBindAnn"),
            vec!["binder_name", "binder_relevance"],
        ),
        (
            include_str!("fixtures/coq-records/checker_flags.v"),
            "class_command",
            "checker_flags",
            None,
            vec![
                "check_univs",
                "prop_sub_type",
                "indices_matter",
                "lets_in_constructor_types",
            ],
        ),
        (
            include_str!("fixtures/coq-records/retroknowledge.v"),
            "record_command",
            "t",
            Some("mk_retroknowledge"),
            vec![
                "retro_int63",
                "retro_float64",
                "retro_string",
                "retro_array",
            ],
        ),
        (
            include_str!("fixtures/coq-records/listable.v"),
            "class_command",
            "Listable",
            None,
            vec!["list_all", "find_index", "cardinality"],
        ),
        (
            include_str!("fixtures/coq-records/dependent_sigma.v"),
            "record_command",
            "Sigma",
            None,
            vec!["fst", "snd"],
        ),
        (
            include_str!("fixtures/coq-records/sigma.v"),
            "record_command",
            "sigma",
            Some("mk_sig"),
            vec!["π1", "π2"],
        ),
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        let commands = named_nodes(tree.root_node(), kind);
        assert_eq!(commands.len(), 1, "{source}");
        let command = commands[0];
        assert_eq!(
            &source[command.child_by_field_name("name").unwrap().byte_range()],
            name
        );
        let actual_fields = named_nodes(command, "record_field");
        assert_eq!(
            actual_fields
                .iter()
                .map(|field| &source[field.child_by_field_name("name").unwrap().byte_range()])
                .collect::<Vec<_>>(),
            fields
        );
        let constructors = named_nodes(command, "record_constructor");
        assert_eq!(
            constructors
                .iter()
                .map(|node| &source[node.child_by_field_name("name").unwrap().byte_range()])
                .collect::<Vec<_>>(),
            constructor.into_iter().collect::<Vec<_>>()
        );
        assert!(named_nodes(command, "field_assignment").is_empty());
        assert!(named_nodes(command, "constructor").is_empty());
    }
}

#[test]
fn record_class_singleton_projection_is_not_a_written_constructor() {
    let source = include_str!("fixtures/coq-records/debug_opt.v");
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let commands = named_nodes(tree.root_node(), "class_command");
    assert_eq!(commands.len(), 1);
    let fields = named_nodes(commands[0], "class_singleton_field");
    assert_eq!(fields.len(), 1);
    assert_eq!(
        &source[fields[0].child_by_field_name("name").unwrap().byte_range()],
        "debug"
    );
    assert!(named_nodes(commands[0], "constructor").is_empty());
    assert!(named_nodes(commands[0], "record_constructor").is_empty());
}

#[test]
fn record_class_field_values_and_structure_alias_keep_precise_boundaries() {
    for source in [
        "Structure Pair : Type := make_pair { left : nat; right : nat }.",
        "Record Methods := { method (x : nat) : nat := x; _ : nat; _value : nat }.",
        "Class Ranked := { parent :: C | 3; coerce :> nat; both ::> C; value := body }.",
        "Record Named := { field : nat } as argument.",
        "Record Notated := { op : nat -> nat -> nat where \"x +++ y\" := (op x y); seed : nat }.",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        let fields = named_nodes(tree.root_node(), "record_field");
        assert!(!fields.is_empty());
        if source.starts_with("Class Ranked") {
            let priorities = named_nodes(tree.root_node(), "instance_priority");
            assert_eq!(priorities.len(), 1, "{}", tree.root_node().to_sexp());
            assert_eq!(&source[priorities[0].byte_range()], "| 3");
        }
        if source.starts_with("Record Notated") {
            assert_eq!(named_nodes(tree.root_node(), "decl_notations").len(), 1);
        }
        for value in named_nodes(tree.root_node(), "record_field_value") {
            let text = &source[value.byte_range()];
            assert!(matches!(text, "x" | "body"), "{text}");
            assert!(!text.contains(';') && !text.contains('|'));
        }
    }
    let source = "Definition made := {| field := value |}.";
    let tree = parser().parse(source, None).unwrap();
    assert!(!tree.root_node().has_error());
    assert!(named_nodes(tree.root_node(), "record_field").is_empty());
    assert_eq!(named_nodes(tree.root_node(), "field_assignment").len(), 1);
}

#[test]
fn record_class_malformed_commands_cannot_be_clean_rooted_declarations() {
    for source in [
        "Record Bad := { field : }.",
        "Class Bad := { field := }.",
        "Structure Bad := make_bad { field : nat.",
        "Record Bad := { field : nat } where \"bad\" := field.",
        "Class Bad := projection : .",
        "(* Record Decoy := { fake : nat }. *)",
        "Definition text := \"Class Decoy := { fake : nat }.\".",
    ] {
        let tree = parser().parse(source, None).unwrap();
        for kind in ["record_command", "class_command"] {
            assert!(
                !named_nodes(tree.root_node(), kind)
                    .iter()
                    .any(|node| !node.has_error()
                        && node
                            .parent()
                            .is_some_and(|sentence| sentence.kind() == "sentence"
                                && !sentence.has_error()
                                && sentence
                                    .parent()
                                    .is_some_and(|root| root.kind() == "source_file"))),
                "{source}: {}",
                tree.root_node().to_sexp()
            );
        }
    }
    // The retained generic operator grammar can read this as an unknown type
    // notation. It must never become a Record priority or semantic evidence.
    let source = "Record Ambiguous := { field : nat | 3 }.";
    let tree = parser().parse(source, None).unwrap();
    assert!(named_nodes(tree.root_node(), "instance_priority").is_empty());
    assert_eq!(named_nodes(tree.root_node(), "custom_operation").len(), 1);
}

#[test]
fn term_projection_keeps_authentic_singleton_class_and_following_definition() {
    let source = concat!(
        include_str!("fixtures/coq-projections/singleton.v"),
        "\n",
        include_str!("fixtures/coq-projections/neighbor.v"),
        "\n",
    );
    let tree = parser().parse(source, None).unwrap();
    let classes = named_nodes(tree.root_node(), "class_command");
    assert_eq!(classes.len(), 1, "{}", tree.root_node().to_sexp());
    assert!(!classes[0].has_error());
    assert_eq!(classes[0].start_byte(), 0);
    let start = source.find("Definition extends_eprogram").unwrap();
    let neighbors: Vec<_> = named_nodes(tree.root_node(), "definition_command")
        .into_iter()
        .filter(|node| node.start_byte() == start)
        .collect();
    assert_eq!(neighbors.len(), 1, "{}", tree.root_node().to_sexp());
    assert!(!neighbors[0].has_error());
    let names = named_nodes(neighbors[0], "ident_decl");
    assert_eq!(names.len(), 1);
    assert_eq!(&source[names[0].byte_range()], "extends_eprogram");
    assert_eq!(named_nodes(classes[0], "term_projection").len(), 4);

    let reduced = "Class t := obs : forall p p' (pr : o.(pre) p) (pr' : o.(pre) p'), pre p.\nDefinition kept := 0.\n";
    let tree = parser().parse(reduced, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let definitions = named_nodes(tree.root_node(), "definition_command");
    assert_eq!(definitions.len(), 1);
    assert_eq!(
        &reduced[definitions[0].byte_range()],
        "Definition kept := 0"
    );
}

#[test]
fn term_projection_fields_arguments_and_chains_keep_written_boundaries() {
    for (body, count, object, projection) in [
        ("value.(Module.field)", 1, "value", "Module.field"),
        (
            "value.(@Module.field parameter)",
            1,
            "value",
            "Module.field",
        ),
        ("value.(field (argument := written))", 1, "value", "field"),
        ("value.(field (argument := @written))", 1, "value", "field"),
        ("value.(field parameter)", 1, "value", "field"),
        ("value.(field).(next)", 2, "value.(field)", "next"),
        ("(make value).(field)", 1, "(make value)", "field"),
        ("value .( field )", 1, "value", "field"),
    ] {
        let source = format!("Definition result := {body}.\n");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{body}: {}",
            tree.root_node().to_sexp()
        );
        let nodes = named_nodes(tree.root_node(), "term_projection");
        assert_eq!(nodes.len(), count, "{body}");
        let outer = nodes
            .iter()
            .max_by_key(|node| node.end_byte() - node.start_byte())
            .unwrap();
        assert_eq!(&source[outer.byte_range()], body);
        assert_eq!(
            &source[outer.child_by_field_name("object").unwrap().byte_range()],
            object
        );
        assert_eq!(
            &source[outer
                .child_by_field_name("projection")
                .unwrap()
                .byte_range()],
            projection
        );
        assert!(shapes(&tree).iter().all(|node| node.1.end <= source.len()));
    }
}

#[test]
fn term_projection_invalid_delimiters_and_literal_decoys_are_visible() {
    for body in [
        "value.()",
        "value.(field",
        "value.(@)",
        "value.(@field (argument := written))",
    ] {
        let source = format!("Definition result := {body}.\n");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "{body}: {}",
            tree.root_node().to_sexp()
        );
    }
    let source = "(* value.(hidden) *)\nDefinition literal := \"value.(hidden)\".\nDefinition ordinary := Module.field.\n";
    let tree = parser().parse(source, None).unwrap();
    assert!(!tree.root_node().has_error());
    assert!(named_nodes(tree.root_node(), "term_projection").is_empty());
    assert_eq!(named_nodes(tree.root_node(), "definition_command").len(), 2);

    let separate = "Definition result := value. (field).\n";
    let tree = parser().parse(separate, None).unwrap();
    assert!(named_nodes(tree.root_node(), "term_projection").is_empty());
    let definitions = named_nodes(tree.root_node(), "definition_command");
    assert_eq!(definitions.len(), 1);
    assert_eq!(
        &separate[definitions[0].byte_range()],
        "Definition result := value"
    );
}

#[test]
fn tactic_notation_keeps_written_items_precedence_and_bounded_body() {
    for (source, expected_items, level, body) in [
        ("Tactic Notation \"written\" := idtac.", 1, None, "idtac"),
        (
            "Tactic Notation (at level 3) \"written\" ident(H) \"as\" simple_intropattern(X) := destruct H as X.",
            4,
            Some("3"),
            "destruct H as X",
        ),
        (
            "Tactic Notation \"written\" constr_list_sep(xs, \",\") := idtac xs.",
            2,
            None,
            "idtac xs",
        ),
        (
            "Tactic Notation \"written\" := match goal with | |- True => idtac end.",
            1,
            None,
            "match goal with | |- True => idtac end",
        ),
        (
            "Tactic Notation \"written\" ident(H) := match type of H with | True => idtac | _ => idtac end.",
            2,
            None,
            "match type of H with | True => idtac | _ => idtac end",
        ),
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        let commands = named_nodes(tree.root_node(), "tactic_notation_command");
        assert_eq!(commands.len(), 1, "{source}");
        let command = commands[0];
        assert_eq!(&source[command.byte_range()], source.trim_end_matches('.'));
        assert_eq!(
            named_nodes(command, "tactic_notation_item").len(),
            expected_items
        );
        assert_eq!(
            command
                .child_by_field_name("level")
                .map(|node| &source[node.byte_range()]),
            level
        );
        assert_eq!(
            &source[command.child_by_field_name("body").unwrap().byte_range()],
            body
        );
        assert!(named_nodes(command, "ident_decl").is_empty());
    }
}

#[test]
fn tactic_notation_malformed_syntax_and_literal_decoys_stay_visible() {
    for source in [
        "Tactic Notation := idtac.",
        "Tactic Notation written := idtac.",
        "Tactic Notation \"written\" idtac.",
        "Tactic Notation \"written\" := .",
        "Tactic Notation \"written := idtac.",
        "Tactic Notation (at level 6) \"written\" := idtac.",
        "Tactic Notation \"written\" := match goal with | |- True => idtac.",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
    }
    let source = "(* Tactic Notation \"hidden\" := idtac. *)\nDefinition literal := \"Tactic Notation\".\nDefinition kept := 0.\n";
    let tree = parser().parse(source, None).unwrap();
    assert!(!tree.root_node().has_error());
    assert!(named_nodes(tree.root_node(), "tactic_notation_command").is_empty());
    assert_eq!(named_nodes(tree.root_node(), "definition_command").len(), 2);

    let incomplete = "Tactic Notation \"written\" := match type of H with | True => idtac.\nDefinition kept := 0.\n";
    let tree = parser().parse(incomplete, None).unwrap();
    assert!(tree.root_node().has_error());
    assert!(
        named_nodes(tree.root_node(), "tactic_notation_command")
            .iter()
            .all(|command| command.has_error())
    );
}

#[test]
fn tactic_notation_match_values_and_patterns_keep_explicit_fields() {
    for value in ["subject", "type of H"] {
        for keyword in ["match", "lazymatch", "multimatch"] {
            let body = format!("{keyword} {value} with | True => idtac | _ => idtac end");
            let source = format!("Tactic Notation \"written\" := {body}.\nDefinition kept := 0.\n");
            let tree = parser().parse(&source, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "{}",
                tree.root_node().to_sexp()
            );
            let matches = named_nodes(tree.root_node(), "match_tactic");
            assert_eq!(matches.len(), 1);
            assert_eq!(&source[matches[0].byte_range()], body);
            assert_eq!(
                &source[matches[0]
                    .child_by_field_name("value")
                    .unwrap()
                    .byte_range()],
                value
            );
            let cases = named_nodes(matches[0], "match_tactic_case");
            assert_eq!(cases.len(), 2);
            for (case, pattern) in cases.iter().zip(["True", "_"]) {
                assert_eq!(
                    &source[case.child_by_field_name("pattern").unwrap().byte_range()],
                    pattern
                );
                assert_eq!(
                    &source[case.child_by_field_name("body").unwrap().byte_range()],
                    "idtac"
                );
            }
            assert_eq!(named_nodes(tree.root_node(), "definition_command").len(), 1);
        }
    }
}

#[test]
fn tactic_notation_original_mrutils_retains_all_five_exact_clean_lemma_heads() {
    let source = include_str!("fixtures/coq-tactic-notations/MRUtils.v");
    assert_eq!(source.len(), 7039);
    let tree = parser().parse(source, None).unwrap();
    let commands = named_nodes(tree.root_node(), "theorem_command");
    let expected = [
        (5885..5982, "iff_forall"),
        (6012..6087, "iff_ex"),
        (6117..6181, "if_true_false"),
        (6220..6285, "iff_is_true_eq_bool"),
        (6429..6482, "uip_bool"),
    ];
    let mut retained = 0;
    for (range, name) in expected {
        let found = commands
            .iter()
            .find(|node| node.byte_range() == range && !node.has_error());
        eprintln!(
            "original Lemma {name} {}..{} clean={}",
            range.start,
            range.end,
            found.is_some()
        );
        if let Some(node) = found {
            assert_eq!(node.parent().unwrap().kind(), "sentence");
            assert_eq!(
                node.parent().unwrap().parent().unwrap().kind(),
                "source_file"
            );
            let names = named_nodes(*node, "ident_decl");
            assert_eq!(names.len(), 1);
            assert_eq!(&source[names[0].byte_range()], name);
            retained += 1;
        }
    }
    assert_eq!(retained, 5, "all original written Lemma heads must survive");
}

#[test]
fn abbreviations_and_numeric_notation_preserve_original_and_authored_neighbors() {
    let original = include_str!("fixtures/coq-tactic-notations/record-notation-neighbor.v");
    assert_eq!(original.len(), 219);
    let authored = "Notation x := (fun x : A => {| g := x.1 |}).\nLemma kept : True.\n";
    assert_eq!(authored.len(), 64);
    for (source, expected) in [
        (original, "Lemma list_map_swap_eq"),
        (authored, "Lemma kept"),
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        assert_eq!(
            named_nodes(tree.root_node(), "abbreviation_command").len(),
            1
        );
        let projections = named_nodes(tree.root_node(), "numeric_projection_notation");
        assert_eq!(projections.len(), if source == original { 2 } else { 1 });
        for projection in projections {
            assert_eq!(
                projection
                    .child_by_field_name("object")
                    .unwrap()
                    .utf8_text(source.as_bytes())
                    .unwrap(),
                "x"
            );
            assert!(matches!(
                projection
                    .child_by_field_name("notation")
                    .unwrap()
                    .utf8_text(source.as_bytes())
                    .unwrap(),
                ".1" | ".2"
            ));
        }
        assert!(
            named_nodes(tree.root_node(), "tactic_notation_level").is_empty(),
            "{}",
            tree.root_node().to_sexp()
        );
        let start = source.find(expected).unwrap();
        let end = start + source[start..].find('.').unwrap();
        let commands = named_nodes(tree.root_node(), "theorem_command");
        assert!(
            commands
                .iter()
                .any(|node| node.byte_range() == (start..end) && !node.has_error()),
            "{}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn tactic_notation_levels_stay_bounded_and_do_not_replace_ordinary_numbers() {
    for level in 0..=5 {
        let source = format!(
            "Tactic Notation (at level {level}) \"written\" := idtac.\nDefinition ordinary : nat := {level}.\nDefinition larger := 19.\n"
        );
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{level}: {}",
            tree.root_node().to_sexp()
        );
        let levels = named_nodes(tree.root_node(), "tactic_notation_level");
        assert_eq!(levels.len(), 1);
        assert_eq!(&source[levels[0].byte_range()], level.to_string());
        let commands = named_nodes(tree.root_node(), "tactic_notation_command");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].child_by_field_name("level"), Some(levels[0]));
        assert_eq!(named_nodes(tree.root_node(), "definition_command").len(), 2);
    }
    for level in ["6", "9", "10", "-1"] {
        let source = format!("Tactic Notation (at level {level}) \"written\" := idtac.");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
    }
    let source = "(* at level 3 *)\nDefinition literal := \"at level 3\".\nDefinition typed (x : nat) : nat := 3.\n";
    let tree = parser().parse(source, None).unwrap();
    assert!(!tree.root_node().has_error());
    assert!(named_nodes(tree.root_node(), "tactic_notation_level").is_empty());
}

#[test]
fn abbreviation_and_numeric_notation_reject_decoys_and_keep_malformed_neighbors() {
    let decoy = "Definition wording := \"x.1\".\n(* y.2 *) Lemma kept : True.\n";
    let tree = parser().parse(decoy, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert!(named_nodes(tree.root_node(), "numeric_projection_notation").is_empty());
    for source in [
        "Notation := (fun x : A => x).\nLemma kept : True.\n",
        "Notation named := .\nLemma kept : True.\n",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let start = source.find("Lemma kept").unwrap();
        assert!(
            named_nodes(tree.root_node(), "theorem_command")
                .iter()
                .any(|node| node.start_byte() == start && !node.has_error()),
            "{}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn abbreviations_keep_parameters_and_numeric_suffixes_separate_from_decimals() {
    for command in ["Notation", "Abbreviation"] {
        let source = format!(
            "{command} written argument := (argument).\nDefinition decimal := 3.14.\nDefinition projected := pair.1.2.\n"
        );
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let abbreviations = named_nodes(tree.root_node(), "abbreviation_command");
        assert_eq!(abbreviations.len(), 1);
        assert_eq!(
            abbreviations[0]
                .child_by_field_name("name")
                .unwrap()
                .utf8_text(source.as_bytes())
                .unwrap(),
            "written"
        );
        assert_eq!(
            abbreviations[0]
                .child_by_field_name("parameter")
                .unwrap()
                .utf8_text(source.as_bytes())
                .unwrap(),
            "argument"
        );
        let suffixes = named_nodes(tree.root_node(), "numeric_projection_suffix");
        assert_eq!(suffixes.len(), 2);
        assert!(
            suffixes
                .iter()
                .all(|suffix| matches!(suffix.utf8_text(source.as_bytes()).unwrap(), ".1" | ".2"))
        );
        let decimals = named_nodes(tree.root_node(), "number");
        assert_eq!(decimals.len(), 1);
        assert_eq!(decimals[0].utf8_text(source.as_bytes()).unwrap(), "3.14");
    }
}
