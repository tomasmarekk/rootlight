//! Physical SAS declaration syntax for the reviewed native grammar candidate.
//! These synthetic fixtures establish written names, headers, and terminators;
//! they do not establish FCMP compilation, macro expansion, or runtime binding.

use tree_sitter::{Node, Parser, Tree};

fn parse(source: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_sas::LANGUAGE.into())
        .unwrap();
    parser.parse(source, None).unwrap()
}

fn nodes<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut pending = vec![root];
    let mut found = Vec::new();
    while let Some(node) = pending.pop() {
        if node.kind() == kind {
            found.push(node);
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    found.sort_by_key(Node::start_byte);
    found
}

fn text<'source>(node: Node<'_>, source: &'source str) -> &'source str {
    node.utf8_text(source.as_bytes()).unwrap()
}

fn field<'tree>(node: Node<'tree>, name: &str) -> Node<'tree> {
    node.child_by_field_name(name)
        .unwrap_or_else(|| panic!("missing {name} field: {}", node.to_sexp()))
}

fn owner<'tree>(mut node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    while let Some(parent) = node.parent() {
        if parent.kind() == kind {
            return Some(parent);
        }
        node = parent;
    }
    None
}

fn has_error_ancestor(mut node: Node<'_>) -> bool {
    while let Some(parent) = node.parent() {
        if parent.is_error() || parent.is_missing() {
            return true;
        }
        node = parent;
    }
    false
}

fn clean_declarations<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    nodes(root, kind)
        .into_iter()
        .filter(|node| !node.has_error() && !has_error_ancestor(*node))
        .collect()
}

#[test]
fn standalone_function_fragments_preserve_case_header_and_formal_owner() {
    for newline in ["\n", "\r\n"] {
        let source = format!(
            "FuNcTiOn Convert(Value, Label $);{newline}\
             Result = Value + 1;{newline}return Result;{newline}EnDsUb;"
        );
        let tree = parse(&source);
        let root = tree.root_node();
        assert!(!root.has_error(), "{}", root.to_sexp());
        let definitions = nodes(root, "fcmp_function_definition");
        assert_eq!(definitions.len(), 1);
        let definition = definitions[0];
        let header = field(definition, "header");
        assert_eq!(header.kind(), "fcmp_function_header");
        assert_eq!(text(header, &source), "FuNcTiOn Convert(Value, Label $);");
        assert_eq!(text(field(header, "name"), &source), "Convert");
        let parameters = nodes(field(header, "parameters"), "fcmp_parameter");
        assert_eq!(parameters.len(), 2);
        for (parameter, expected) in parameters.iter().zip(["Value", "Label"]) {
            assert_eq!(text(field(*parameter, "name"), &source), expected);
            assert_eq!(
                owner(*parameter, "fcmp_function_definition"),
                Some(definition)
            );
            assert!(parameter.end_byte() < header.end_byte());
        }
        assert!(parameters[0].child_by_field_name("character").is_none());
        assert_eq!(text(field(parameters[1], "character"), &source), "$");
        let body = field(definition, "body");
        assert_eq!(body.kind(), "fcmp_body");
        assert_eq!(nodes(body, "fcmp_unknown_statement").len(), 2);
        let end = field(definition, "end");
        assert_eq!(text(end, &source), "EnDsUb;");
        assert!(header.end_byte() <= body.start_byte());
        assert!(body.end_byte() <= end.start_byte());
        assert_eq!(text(definition, &source), source);
    }
}

#[test]
fn proc_bodies_retain_subroutines_arrays_and_character_return_headers() {
    let source = "proc fcmp outlib=work.Library.Group;\n\
        subroutine Adjust(Values[*], Count); Count=Count+1; endsub;\n\
        function Label(Value $) $; return Value; endsub;\n\
        function Empty(); endsub; quit;";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let procedures = nodes(root, "proc_step");
    assert_eq!(procedures.len(), 1);
    let procedure = procedures[0];
    let subroutines = nodes(root, "fcmp_subroutine_definition");
    assert_eq!(subroutines.len(), 1);
    let subroutine = subroutines[0];
    let header = field(subroutine, "header");
    assert_eq!(text(header, source), "subroutine Adjust(Values[*], Count);");
    assert_eq!(owner(subroutine, "proc_step"), Some(procedure));
    let parameters = nodes(header, "fcmp_parameter");
    assert_eq!(parameters.len(), 2);
    assert_eq!(text(field(parameters[0], "name"), source), "Values");
    assert_eq!(text(field(parameters[0], "dimensions"), source), "[*]");
    assert_eq!(text(field(parameters[1], "name"), source), "Count");
    assert_eq!(text(field(subroutine, "end"), source), "endsub;");
    let functions = nodes(root, "fcmp_function_definition");
    assert_eq!(functions.len(), 2);
    let label_header = field(functions[0], "header");
    assert_eq!(text(field(label_header, "return_type"), source), "$");
    assert_eq!(text(field(label_header, "name"), source), "Label");
    assert_eq!(owner(functions[0], "proc_step"), Some(procedure));
    assert!(functions[1].child_by_field_name("body").is_none());
    assert!(nodes(field(functions[1], "header"), "fcmp_parameter").is_empty());
}

#[test]
fn unknown_bodies_keep_quoted_and_commented_delimiters_inert() {
    let source = "function Outer(Value);\n\
        Text='function Fake(X); endsub; π';\n\
        /* subroutine Hidden(Y); endsub; */\n\
        * function Commented(Z);\n\
        %* endsub;\n\
        FunctionName=Value; EndsubSuffix=2; return FunctionName;\n\
        endsub;";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert_eq!(nodes(root, "fcmp_function_definition").len(), 1);
    assert!(nodes(root, "fcmp_subroutine_definition").is_empty());
    let definition = nodes(root, "fcmp_function_definition")[0];
    assert_eq!(
        text(field(field(definition, "header"), "name"), source),
        "Outer"
    );
    assert_eq!(text(field(definition, "end"), source), "endsub;");
    assert_eq!(nodes(root, "fcmp_parameter").len(), 1);
}

#[test]
fn missing_or_nested_declarations_cannot_borrow_a_sibling_endsub() {
    for (keyword, kind) in [
        ("function", "fcmp_function_definition"),
        ("subroutine", "fcmp_subroutine_definition"),
    ] {
        for separator in [";", ""] {
            let source = format!(
                "{keyword} Broken(X); X=X+1{separator}\n\
                 {keyword} Good(Y); Y=Y+1; endsub;"
            );
            let tree = parse(&source);
            let root = tree.root_node();
            for declaration in clean_declarations(root, kind) {
                let name = text(field(field(declaration, "header"), "name"), &source);
                assert_ne!(name, "Broken", "{}", root.to_sexp());
                assert_eq!(name, "Good");
                assert!(text(declaration, &source).starts_with(&format!("{keyword} Good")));
            }
            for body in nodes(root, "fcmp_body") {
                for statement in nodes(body, "fcmp_unknown_statement") {
                    assert!(!text(statement, &source).contains(&format!("{keyword} Good")));
                }
            }
        }
        let missing_end = format!("{keyword} Broken(X); X=X+1;");
        let tree = parse(&missing_end);
        assert!(clean_declarations(tree.root_node(), kind).is_empty());
        let missing_header_terminator = format!("{keyword} Broken(X) X=X+1; endsub;");
        let tree = parse(&missing_header_terminator);
        assert!(clean_declarations(tree.root_node(), kind).is_empty());
        let malformed_formals = format!("{keyword} Broken(X,,Y); X=X+1; endsub;");
        let tree = parse(&malformed_formals);
        assert!(clean_declarations(tree.root_node(), kind).is_empty());
    }
}

#[test]
fn macro_headers_separate_formals_from_defaults_and_match_physical_closers() {
    let source = "%macro Compose(First, Mode=DefaultName, Text='a,b',\n\
        List=(One,Two), Ref=&External, Empty=) / secure;\n\
        %put &First.;\n\
        %mend cOmPoSe;";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let definitions = nodes(root, "macro_definition");
    assert_eq!(definitions.len(), 1);
    let definition = definitions[0];
    let header = field(definition, "header");
    assert_eq!(header.kind(), "macro_definition_header");
    assert!(text(header, source).starts_with("%macro Compose("));
    assert!(text(header, source).ends_with("/ secure;"));
    let name = text(field(header, "name"), source);
    assert_eq!(name, "Compose");
    let formals = nodes(field(header, "parameters"), "macro_parameter");
    assert_eq!(formals.len(), 6);
    for (parameter, expected) in formals
        .iter()
        .zip(["First", "Mode", "Text", "List", "Ref", "Empty"])
    {
        assert_eq!(text(field(*parameter, "name"), source), expected);
        assert_eq!(owner(*parameter, "macro_definition"), Some(definition));
        assert!(parameter.end_byte() < header.end_byte());
    }
    for (parameter, expected) in [
        (formals[1], "DefaultName"),
        (formals[2], "'a,b'"),
        (formals[3], "(One,Two)"),
        (formals[4], "&External"),
    ] {
        assert_eq!(text(field(parameter, "default"), source), expected);
    }
    assert!(formals[0].child_by_field_name("default").is_none());
    assert!(formals[5].child_by_field_name("default").is_none());
    assert_eq!(nodes(root, "macro_parameter").len(), 6);
    let end = field(definition, "end");
    assert_eq!(text(end, source), "%mend cOmPoSe;");
    let closing_name = text(field(end, "name"), source);
    assert!(name.eq_ignore_ascii_case(closing_name));
    assert_ne!(name, closing_name);
    assert!(header.end_byte() <= field(definition, "body").start_byte());
    assert!(field(definition, "body").end_byte() <= end.start_byte());
}

#[test]
fn macro_terminator_mismatch_is_written_syntax_not_identity_proof() {
    let source = "%macro Open(Value); %put &Value.; %mend Different;";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let definition = nodes(root, "macro_definition")[0];
    let opening = text(field(field(definition, "header"), "name"), source);
    let closing = text(field(field(definition, "end"), "name"), source);
    assert!(!opening.eq_ignore_ascii_case(closing));
    assert_eq!(nodes(root, "macro_parameter").len(), 1);
}

#[test]
fn data_headers_keep_whole_output_nodes_without_expansion_or_special_identity() {
    let source = "data work.Output; Value=1; run;\n\
        data work.Left work.Right; Value=2; run;\n\
        data work.&Name &Lib..&Member _NULL_; Value=3; run;";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let steps = nodes(root, "data_step");
    assert_eq!(steps.len(), 3);
    let outputs = |step| {
        let header = field(step, "header");
        let mut cursor = header.walk();
        header
            .children_by_field_name("output", &mut cursor)
            .collect::<Vec<_>>()
    };
    let single = outputs(steps[0]);
    assert_eq!(single.len(), 1);
    assert_eq!(single[0].kind(), "dataset_name");
    assert_eq!(text(single[0], source), "work.Output");
    assert_eq!(nodes(single[0], "identifier").len(), 2);
    let multiple = outputs(steps[1]);
    assert_eq!(multiple.len(), 2);
    assert_eq!(text(multiple[0], source), "work.Left");
    assert_eq!(text(multiple[1], source), "work.Right");
    let dynamic = outputs(steps[2]);
    assert_eq!(dynamic.len(), 3);
    assert_eq!(text(dynamic[0], source), "work.&Name");
    assert_eq!(text(dynamic[1], source), "&Lib..&Member");
    assert_eq!(text(dynamic[2], source), "_NULL_");
    assert!(!nodes(dynamic[0], "macro_variable_ref").is_empty());
    assert!(!nodes(dynamic[1], "macro_variable_ref").is_empty());
    assert!(nodes(dynamic[2], "macro_variable_ref").is_empty());
    for step in steps {
        assert_eq!(text(field(step, "end"), source), "run;");
    }
}

#[test]
fn missing_data_run_cannot_borrow_a_following_header_terminator() {
    for separator in [";", ""] {
        let source = format!(
            "data Bad; Value=1{separator}\n\
             data Good; Value=2; run;"
        );
        let tree = parse(&source);
        let root = tree.root_node();
        for step in clean_declarations(root, "data_step") {
            let header = field(step, "header");
            assert_eq!(text(header, &source), "data Good;");
            assert!(text(step, &source).starts_with("data Good;"));
        }
        for statement in nodes(root, "generic_statement") {
            if owner(statement, "data_step").is_some() {
                assert!(!text(statement, &source).contains("data Good"));
            }
        }
    }
}

#[test]
fn physical_closers_exclude_leading_trivia() {
    for trivia in [" ", "\t", "\n  ", "\r\n\t", "\u{000c}"] {
        for body in ["", "return Value;"] {
            let source = format!("function Measure(Value);{body}{trivia}endsub;");
            let tree = parse(&source);
            let root = tree.root_node();
            assert!(!root.has_error(), "{}", root.to_sexp());
            let definitions = nodes(root, "fcmp_function_definition");
            assert_eq!(definitions.len(), 1);
            let end = field(definitions[0], "end");
            assert_eq!(text(end, &source), "endsub;");
            assert_eq!(end.start_byte(), source.find("endsub;").unwrap());
        }
        let source = format!("data Result; Value=1;{trivia}run;");
        let tree = parse(&source);
        let root = tree.root_node();
        assert!(!root.has_error(), "{}", root.to_sexp());
        let steps = nodes(root, "data_step");
        assert_eq!(steps.len(), 1);
        let end = field(steps[0], "end");
        assert_eq!(text(end, &source), "run;");
        assert_eq!(end.start_byte(), source.find("run;").unwrap());
    }
}

#[test]
fn opaque_bodies_preserve_quote_and_punctuation_starts() {
    for statement in ["'function Hidden(); endsub;';", "+Value;", "/Value;"] {
        let source = format!("function Measure(Value);\n  {statement}\n\tendsub;");
        let tree = parse(&source);
        let root = tree.root_node();
        assert!(!root.has_error(), "{}", root.to_sexp());
        let definitions = nodes(root, "fcmp_function_definition");
        assert_eq!(definitions.len(), 1);
        let unknown = nodes(field(definitions[0], "body"), "fcmp_unknown_statement");
        assert_eq!(unknown.len(), 1);
        assert_eq!(text(unknown[0], &source), statement);
        assert_eq!(text(field(definitions[0], "end"), &source), "endsub;");
    }
}

#[test]
fn dataset_macro_continuations_preserve_adjacency_and_whitespace_boundaries() {
    let source = "data Prefix&Suffix &Library..&Member &First&Second; run;";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let steps = nodes(root, "data_step");
    assert_eq!(steps.len(), 1);
    let header = field(steps[0], "header");
    let mut cursor = header.walk();
    let outputs = header
        .children_by_field_name("output", &mut cursor)
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 3);
    for (output, (expected, references)) in outputs.iter().zip([
        ("Prefix&Suffix", 1),
        ("&Library..&Member", 2),
        ("&First&Second", 2),
    ]) {
        assert_eq!(text(*output, source), expected);
        assert_eq!(nodes(*output, "macro_variable_ref").len(), references);
    }

    for separator in [" ", "\t", "\r\n"] {
        for first in ["First", "&First"] {
            let source = format!("data {first}{separator}&Next; run;");
            let tree = parse(&source);
            let root = tree.root_node();
            assert!(!root.has_error(), "{}", root.to_sexp());
            let steps = nodes(root, "data_step");
            assert_eq!(steps.len(), 1);
            let header = field(steps[0], "header");
            let mut cursor = header.walk();
            let outputs = header
                .children_by_field_name("output", &mut cursor)
                .collect::<Vec<_>>();
            assert_eq!(outputs.len(), 2, "{source}: {}", root.to_sexp());
            assert_eq!(text(outputs[0], &source), first);
            assert_eq!(text(outputs[1], &source), "&Next");
            assert_eq!(nodes(outputs[1], "macro_variable_ref").len(), 1);
        }
    }
}

#[test]
fn comment_bridged_dataset_nodes_retain_raw_span_and_each_macro_leaf() {
    // Named comment extras can bridge immediate tokens. The composite is raw
    // source evidence, not a static dataset name or proof of runtime outputs.
    for first in ["First", "&First"] {
        let written = format!("{first}/* gap */&Next");
        let source = format!("data {written}; run;");
        let tree = parse(&source);
        let root = tree.root_node();
        assert!(!root.has_error(), "{}", root.to_sexp());
        let outputs = nodes(root, "dataset_name");
        let composite = outputs
            .iter()
            .find(|node| text(**node, &source) == written)
            .copied()
            .expect("the complete comment-bridged span must remain available");
        let start = source.find(&written).unwrap();
        assert_eq!(composite.byte_range(), start..start + written.len());
        let comments = nodes(composite, "block_comment");
        assert_eq!(comments.len(), 1);
        assert_eq!(text(comments[0], &source), "/* gap */");
        let references = nodes(composite, "macro_variable_ref");
        let expected = if first.starts_with('&') {
            vec!["&First", "&Next"]
        } else {
            vec!["&Next"]
        };
        assert_eq!(references.len(), expected.len());
        for (reference, expected) in references.iter().zip(expected) {
            let start = source.find(expected).unwrap();
            assert_eq!(reference.byte_range(), start..start + expected.len());
            assert_eq!(text(*reference, &source), expected);
        }
    }
}

#[test]
fn character_return_lengths_remain_written_header_fields() {
    for separator in ["", " ", "\t", "\r\n"] {
        for length in ["16", "32", "2048"] {
            let header = format!("function Label(Value $) ${separator}{length};");
            let source = format!("{header} return Value; endsub;");
            let tree = parse(&source);
            let root = tree.root_node();
            assert!(!root.has_error(), "{}", root.to_sexp());
            let definitions = nodes(root, "fcmp_function_definition");
            assert_eq!(definitions.len(), 1);
            let native_header = field(definitions[0], "header");
            assert_eq!(text(native_header, &source), header);
            assert_eq!(text(field(native_header, "return_type"), &source), "$");
            assert_eq!(text(field(native_header, "return_length"), &source), length);
            let parameters = nodes(native_header, "fcmp_parameter");
            assert_eq!(parameters.len(), 1);
            assert_eq!(text(field(parameters[0], "name"), &source), "Value");
            assert_eq!(text(field(definitions[0], "end"), &source), "endsub;");
        }
    }
    for invalid_length in ["-16", "16.5", "Value", "16 32"] {
        let source = format!("function Broken(Value $) $ {invalid_length}; endsub;");
        let tree = parse(&source);
        assert!(clean_declarations(tree.root_node(), "fcmp_function_definition").is_empty());
    }
    let tree = parse("function Broken(Value $) 16; endsub;");
    assert!(clean_declarations(tree.root_node(), "fcmp_function_definition").is_empty());
}

#[test]
fn data_verbs_retain_direct_dataset_leaves_after_initial_comments() {
    for (prefix, separator) in [
        ("", " "),
        ("/* initial comment */ ", " "),
        ("%* initial comment; ", " "),
        ("", "/* verb comment */"),
        ("", "%* verb comment; "),
    ] {
        let source = format!(
            "data Work.Result; \
             {prefix}sEt{separator}Work.Input; \
             {prefix}merge{separator}Work.Left Work.Right; \
             {prefix}update{separator}Work.Master Work.Change; \
             {prefix}output{separator}Work.Other; \
             {prefix}output; run;"
        );
        let tree = parse(&source);
        let root = tree.root_node();
        assert!(!root.has_error(), "{prefix:?}: {}", root.to_sexp());
        let steps = nodes(root, "data_step");
        assert_eq!(steps.len(), 1, "{}", root.to_sexp());
        let step = steps[0];
        assert_eq!(text(step, &source), source);
        assert_eq!(text(field(step, "header"), &source), "data Work.Result;");
        assert_eq!(text(field(step, "end"), &source), "run;");
        assert!(
            nodes(step, "generic_statement").is_empty(),
            "{}",
            root.to_sexp()
        );

        for (kind, expected) in [
            ("set_statement", vec![vec!["Work.Input"]]),
            ("merge_statement", vec![vec!["Work.Left", "Work.Right"]]),
            ("update_statement", vec![vec!["Work.Master", "Work.Change"]]),
            ("output_statement", vec![vec!["Work.Other"], vec![]]),
        ] {
            let statements = nodes(step, kind);
            assert_eq!(statements.len(), expected.len(), "{}", root.to_sexp());
            for (statement, expected_names) in statements.into_iter().zip(expected) {
                assert_eq!(statement.parent(), Some(step));
                assert_eq!(owner(statement, "data_step"), Some(step));
                let mut cursor = statement.walk();
                let leaves = statement
                    .named_children(&mut cursor)
                    .filter(|node| node.kind() == "dataset_name")
                    .collect::<Vec<_>>();
                assert_eq!(leaves.len(), expected_names.len());
                for (leaf, expected_name) in leaves.into_iter().zip(expected_names) {
                    let start = source.find(expected_name).unwrap();
                    assert_eq!(leaf.byte_range(), start..start + expected_name.len());
                    assert_eq!(text(leaf, &source), expected_name);
                    assert_eq!(leaf.parent(), Some(statement));
                    assert_eq!(owner(leaf, "data_step"), Some(step));
                }
            }
        }
        let header = field(step, "header");
        let mut cursor = header.walk();
        let outputs = header
            .children_by_field_name("output", &mut cursor)
            .collect::<Vec<_>>();
        assert_eq!(outputs.len(), 1);
        assert_eq!(text(outputs[0], &source), "Work.Result");
        assert_eq!(nodes(root, "dataset_name").len(), 7);
    }
}

#[test]
fn data_keyword_variables_and_later_boundary_words_remain_opaque() {
    let source = "data Work.Result; set=1; merge=2; update=3; output=4; \
        setter=5; put SET MERGE UPDATE OUTPUT; OUTPUT+1; SET[1]=2; \
        subroutineValue=6; SET/* operator comment */[1]=2; \
        OUTPUT%* operator comment; +1; run;";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let steps = nodes(root, "data_step");
    assert_eq!(steps.len(), 1, "{}", root.to_sexp());
    let step = steps[0];
    assert_eq!(text(step, source), source);
    let statements = nodes(step, "generic_statement");
    assert_eq!(statements.len(), 11);
    for (statement, expected) in statements.iter().zip([
        "set=1;",
        "merge=2;",
        "update=3;",
        "output=4;",
        "setter=5;",
        "put SET MERGE UPDATE OUTPUT;",
        "OUTPUT+1;",
        "SET[1]=2;",
        "subroutineValue=6;",
        "SET/* operator comment */[1]=2;",
        "OUTPUT%* operator comment; +1;",
    ]) {
        assert_eq!(text(*statement, source), expected);
        assert_eq!(statement.parent(), Some(step));
    }
    for kind in [
        "set_statement",
        "merge_statement",
        "update_statement",
        "output_statement",
    ] {
        assert!(nodes(root, kind).is_empty(), "{}", root.to_sexp());
    }
    let datasets = nodes(root, "dataset_name");
    assert_eq!(datasets.len(), 1);
    assert_eq!(text(datasets[0], source), "Work.Result");
    assert_eq!(datasets[0].parent(), Some(field(step, "header")));
}
