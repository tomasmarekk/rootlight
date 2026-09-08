//! PowerShell native syntax qualification, independent of production capabilities.
//! Exact captures and incremental ranges guard the later grammar-to-IR boundary.

use tree_sitter::{InputEdit, Node, Parser, Point, Query, QueryCursor, StreamingIterator};

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_powershell::LANGUAGE.into())
        .unwrap();
    parser
}

fn nodes(root: Node<'_>) -> Vec<Node<'_>> {
    let mut pending = vec![root];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        result.push(node);
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    result.sort_by_key(|node| (node.start_byte(), node.end_byte(), node.kind_id()));
    result
}

fn point(source: &str, offset: usize) -> Point {
    let prefix = source.get(..offset).unwrap();
    Point::new(
        prefix.bytes().filter(|byte| *byte == b'\n').count(),
        prefix
            .rfind('\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

#[test]
fn powershell_declarations_parameters_and_invocations_have_exact_captures() {
    let source = r#"# source: 雪
function script:Get-Entry {
    param([string]$Name, [int]$Count = 1)
    Write-Output $Name
}
filter Select-Entry { $_ }
class Cache : BaseCache {
    [string]$Label
    Cache([string]$name) { $this.Label = $name }
    [string] Read([int]$slot) { return $this.Label }
}
enum Mode { Open = 1; Closed = 2 }
$item = [Cache]::new('雪')
$item.Read(0)
Get-Entry -Name 'entry' | Select-Entry
& $action 'value'
. './helpers.ps1'
"#;
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    assert_eq!(tree.root_node().byte_range(), 0..source.len());
    let query = Query::new(
        &tree_sitter_powershell::LANGUAGE.into(),
        r#"
        (function_statement (function_name) @function)
        (class_statement . (simple_name) @class)
        (class_property_definition (variable) @property)
        (class_method_definition (simple_name) @method)
        (script_parameter (variable) @parameter)
        (class_method_parameter (variable) @parameter)
        (enum_statement (simple_name) @enum)
        (enum_member (simple_name) @member)
        (command command_name: (command_name) @command)
        (invokation_expression (member_name) @invocation)
        "#,
    )
    .unwrap();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut actual = Vec::new();
    while let Some(found) = matches.next() {
        for capture in found.captures {
            actual.push((
                query.capture_names()[usize::try_from(capture.index).unwrap()],
                capture.node.utf8_text(source.as_bytes()).unwrap(),
            ));
        }
    }
    actual.sort_unstable();
    let mut expected = vec![
        ("function", "script:Get-Entry"),
        ("function", "Select-Entry"),
        ("class", "Cache"),
        ("property", "$Label"),
        ("method", "Cache"),
        ("method", "Read"),
        ("parameter", "$Name"),
        ("parameter", "$Count"),
        ("parameter", "$name"),
        ("parameter", "$slot"),
        ("enum", "Mode"),
        ("member", "Open"),
        ("member", "Closed"),
        ("command", "Write-Output"),
        ("command", "Get-Entry"),
        ("command", "Select-Entry"),
        ("invocation", "new"),
        ("invocation", "Read"),
    ];
    expected.sort_unstable();
    assert_eq!(actual, expected);
    for node in nodes(tree.root_node()) {
        assert!(source.get(node.byte_range()).is_some(), "{node:?}");
    }
}

#[test]
fn powershell_comments_and_strings_do_not_invent_declarations() {
    for literal in [
        "'function Hidden {}'",
        "\"class Hidden {} $value\"",
        "@'\nfunction Hidden {}\n雪\n'@",
        "@\"\nclass Hidden {} $value\n雪\n\"@",
        "\"escaped `\"function Hidden {}`\"\"",
    ] {
        let source = format!(
            "<# class BlockHidden {{}} #>\n# function LineHidden {{}}\nfunction Visible {{ $text = {literal}\n Write-Output $text }}\n"
        );
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        let all = nodes(tree.root_node());
        assert_eq!(
            all.iter()
                .filter(|node| node.kind() == "function_statement")
                .count(),
            1
        );
        assert_eq!(
            all.iter()
                .filter(|node| node.kind() == "class_statement")
                .count(),
            0
        );
        let name = all
            .iter()
            .find(|node| node.kind() == "function_name")
            .unwrap();
        assert_eq!(name.utf8_text(source.as_bytes()).unwrap(), "Visible");
    }
}

#[test]
fn powershell_incremental_edits_equal_fresh_trees_and_source_positions() {
    let mut source = "# source: 雪\r\nfunction Read-Entry { param($value)\r\n $text = \"before $value\"\r\n Write-Output $text\r\n}\r\nRead-Entry 1\r\n".to_owned();
    let mut parser = parser();
    let mut tree = parser.parse(&source, None).unwrap();
    assert!(!tree.root_node().has_error());
    for (old, new) in [
        ("$value)", "$value, $other = 20)"),
        (
            "\"before $value\"",
            "@\"\r\nbefore $($value + 1)\r\n雪\r\n\"@",
        ),
        ("Read-Entry", "script:Read-Record"),
        ("# source: 雪", "<# source: δ #>"),
        (
            " Write-Output $text\r\n",
            " Write-Output $text; Write-Output 2\r\n",
        ),
        ("$value + 1", "$value + 1000"),
        ("; Write-Output 2", ""),
        ("Read-Entry 1", "Read-Entry 1 {}"),
        ("{}", "{ Write-Output '雪' }"),
        ("{ Write-Output '雪' }", "{<# no action #>}"),
        ("{<# no action #>}", "{}"),
        ("{}", "{ param($argument) }"),
        ("{ param($argument) }", "{}"),
        ("Read-Entry 1 {}", "Read-Entry 1 {}; $bytes = 2kb"),
        ("2kb", "2kB"),
        ("2kB", "0X2LkB"),
        ("0X2LkB", "2E+2MB"),
        ("2E+2MB", "2e-2mb"),
        ("$bytes = 2e-2mb", "$bytes = $items.Apply{}"),
        ("Apply{}", "ForEach{ $_ }"),
        ("ForEach{ $_ }", "Where{ param($entry); $entry }"),
        ("$items.Where", "[Item]::Read"),
        ("[Item]::Read", "$items.$method"),
        ("$items.$method", "$items.\"Read\""),
        ("{ param($entry); $entry }", "{}"),
    ] {
        let start = source.find(old).unwrap();
        let end = start + old.len();
        let changed = source.replacen(old, new, 1);
        tree.edit(&InputEdit {
            start_byte: start,
            old_end_byte: end,
            new_end_byte: start + new.len(),
            start_position: point(&source, start),
            old_end_position: point(&source, end),
            new_end_position: point(&changed, start + new.len()),
        });
        let incremental = parser.parse(&changed, Some(&tree)).unwrap();
        let fresh = self::parser().parse(&changed, None).unwrap();
        assert!(
            !fresh.root_node().has_error(),
            "{changed}: {}",
            fresh.root_node().to_sexp()
        );
        assert_eq!(
            incremental.root_node().to_sexp(),
            fresh.root_node().to_sexp()
        );
        let shape = |root| {
            nodes(root)
                .into_iter()
                .map(|node| {
                    (
                        node.kind_id(),
                        node.byte_range(),
                        node.start_position(),
                        node.end_position(),
                        node.is_missing(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(shape(incremental.root_node()), shape(fresh.root_node()));
        source = changed;
        tree = incremental;
    }
}

#[test]
fn powershell_unfinished_syntax_retains_errors_and_bounded_ranges() {
    for source in [
        "function Broken {",
        "class Broken { [string] Read(",
        "$value = 'unfinished",
        "$value = @'\nunfinished",
        "$value = (1 +",
        "$action = {",
        "Invoke-Check {<# unfinished",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(tree.root_node().has_error(), "accepted {source}");
        for node in nodes(tree.root_node()) {
            assert!(
                source.get(node.byte_range()).is_some(),
                "{source}: {node:?}"
            );
        }
    }
}

#[test]
fn powershell_numeric_markers_preserve_case_insensitive_literal_ranges() {
    for base in ["2", "0x2", "0X2", "2.5", ".5", "2e2", "2E+2", ".5E-2"] {
        for multiplier in ["", "kb", "kB", "Kb", "KB", "mB", "GB", "tB", "Pb"] {
            let literal = format!("{base}{multiplier}");
            let source = format!("$value = {literal}\n");
            let tree = parser().parse(&source, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "{source}: {}",
                tree.root_node().to_sexp()
            );
            let literals: Vec<_> = nodes(tree.root_node())
                .into_iter()
                .filter(|node| matches!(node.kind(), "integer_literal" | "real_literal"))
                .map(|node| node.utf8_text(source.as_bytes()).unwrap())
                .collect();
            assert_eq!(literals, [literal.as_str()]);
        }
    }
    for literal in [
        "2l", "2L", "2d", "2D", "0x2l", "0X2L", "2LkB", "2DMB", "0X2LPb",
    ] {
        let source = format!("$value = {literal}\n");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        let literal_node = nodes(tree.root_node())
            .into_iter()
            .find(|node| node.kind() == "integer_literal")
            .unwrap();
        assert_eq!(literal_node.utf8_text(source.as_bytes()).unwrap(), literal);
    }
}

#[test]
fn powershell_numeric_markers_do_not_consume_barewords_or_delimiters() {
    for bareword in ["2GBL", "0XG", "2e+", "2XB", "2_GB"] {
        let source = format!("$value = {bareword}\n");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        assert!(
            !nodes(tree.root_node())
                .iter()
                .any(|node| matches!(node.kind(), "integer_literal" | "real_literal")),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
    }
    let source = "$range = 1 .. 2\n$text = '2GB'\n$size = (2L).ToString()\n";
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let literals: Vec<_> = nodes(tree.root_node())
        .into_iter()
        .filter(|node| matches!(node.kind(), "integer_literal" | "real_literal"))
        .map(|node| node.utf8_text(source.as_bytes()).unwrap())
        .collect();
    assert_eq!(literals, ["1", "2", "2L"]);
}

#[test]
fn powershell_script_block_method_arguments_retain_members_and_exact_ranges() {
    for (receiver, member) in [
        ("$items.", "Where"),
        ("$items.", "ForEach"),
        ("$items.", "Apply"),
        ("($items.Keys).", "Apply"),
        ("[Item]::", "Read"),
        ("$items.", "\"Read\""),
        ("$items.", "$method"),
    ] {
        for body in ["", "<# no action #>", "param($entry); $entry"] {
            let block = format!("{{{body}}}");
            let expression = format!("{receiver}{member}{block}");
            let source = format!("$result = {expression}\n");
            let tree = parser().parse(&source, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "{source}: {}",
                tree.root_node().to_sexp()
            );
            let invocation = nodes(tree.root_node())
                .into_iter()
                .find(|node| node.kind() == "invokation_expression")
                .unwrap();
            assert_eq!(invocation.utf8_text(source.as_bytes()).unwrap(), expression);
            let mut cursor = invocation.walk();
            let children: Vec<_> = invocation.named_children(&mut cursor).collect();
            let name = children
                .iter()
                .find(|node| node.kind() == "member_name")
                .unwrap();
            assert_eq!(name.utf8_text(source.as_bytes()).unwrap(), member);
            let argument = children
                .iter()
                .find(|node| node.kind() == "script_block_expression")
                .unwrap();
            assert_eq!(argument.utf8_text(source.as_bytes()).unwrap(), block);
        }
    }
}

#[test]
fn powershell_script_block_method_arguments_require_adjacent_complete_braces() {
    for source in [
        "$items.Apply { $_ }",
        "$items.ForEach { $_ }",
        "[Item]::Read { $_ }",
        "$items.Apply{ $_ }{ $_ }",
        "$items.Apply{",
        "$items.Apply{<# unfinished",
    ] {
        let tree = parser().parse(source, None).unwrap();
        assert!(tree.root_node().has_error(), "accepted {source}");
    }
}

#[test]
fn powershell_empty_script_blocks_retain_exact_expression_ranges() {
    for body in [
        "",
        " ",
        "\r\n",
        "<# no action #>",
        "# no action\n",
        "param()",
        "param($argument)",
    ] {
        let block = format!("{{{body}}}");
        for source in [
            format!("$action = {block}\n"),
            format!("Invoke-Check Write-Entry {block}\n"),
            format!("Invoke-Check -Action {block}\n"),
            format!("$actions = @({block}, {block})\n"),
        ] {
            let tree = parser().parse(&source, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "{source}: {}",
                tree.root_node().to_sexp()
            );
            let blocks: Vec<_> = nodes(tree.root_node())
                .into_iter()
                .filter(|node| node.kind() == "script_block_expression")
                .map(|node| node.utf8_text(source.as_bytes()).unwrap())
                .collect();
            assert_eq!(blocks, vec![block.as_str(); source.matches(&block).count()]);
        }
    }
}

#[test]
fn powershell_statement_boundaries_and_parser_reuse_are_stable() {
    let mut parser = parser();
    for separator in ["\n", "\r\n", ";", ";\n", " \t;\r\n"] {
        let source = format!("$value = 1{separator}$other = 2{separator}Write-Output $other");
        let tree = parser.parse(&source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{source}: {}",
            tree.root_node().to_sexp()
        );
        assert_eq!(
            nodes(tree.root_node())
                .iter()
                .filter(|node| node.kind() == "assignment_expression")
                .count(),
            2
        );
        parser.reset();
    }
    let language: tree_sitter::Language = tree_sitter_powershell::LANGUAGE.into();
    assert_eq!(language.abi_version(), 15);
    Query::new(&language, tree_sitter_powershell::HIGHLIGHTS_QUERY).unwrap();
}
