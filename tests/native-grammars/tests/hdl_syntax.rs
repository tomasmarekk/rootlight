//! Native HDL contracts bind exact static ABI, physical syntax and parser state.
//! Native trees do not establish preprocessing, elaboration or resolved declarations.

use tree_sitter::{InputEdit, Parser, Point, Tree};

#[derive(Debug, PartialEq, Eq)]
struct Shape {
    kind: u16,
    range: std::ops::Range<usize>,
    points: (Point, Point),
    depth: usize,
    field: Option<String>,
    named: bool,
    missing: bool,
    error: bool,
    extra: bool,
}

fn shapes(tree: &Tree) -> Vec<Shape> {
    let mut output = Vec::new();
    let mut cursor = tree.walk();
    let mut depth = 0;
    loop {
        let node = cursor.node();
        output.push(Shape {
            kind: node.kind_id(),
            range: node.byte_range(),
            points: (node.start_position(), node.end_position()),
            depth,
            field: cursor.field_name().map(str::to_owned),
            named: node.is_named(),
            missing: node.is_missing(),
            error: node.is_error(),
            extra: node.is_extra(),
        });
        if cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return output;
            }
            depth -= 1;
        }
    }
}

fn point(bytes: &[u8], offset: usize) -> Point {
    let prefix = &bytes[..offset];
    Point::new(
        prefix.iter().filter(|byte| **byte == b'\n').count(),
        prefix
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(offset, |last| offset - last - 1),
    )
}

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_systemverilog::LANGUAGE_SYSTEMVERILOG.into())
        .unwrap();
    parser
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::ControlFlow;
    use tree_sitter::ParseOptions;

    #[test]
    fn runtime_accepts_the_exact_static_abi_and_fields() {
        let language: tree_sitter::Language =
            tree_sitter_systemverilog::LANGUAGE_SYSTEMVERILOG.into();
        assert_eq!(language.abi_version(), 15);
        assert_eq!(language.field_count(), 20);
        assert_eq!(language.name(), Some("systemverilog"));
        parser();
    }

    #[test]
    fn authored_language_constructs_keep_valid_written_syntax() {
        for source in [
            "module Top #(parameter WIDTH = 8)(input wire clk, output logic [WIDTH-1:0] value); assign value = '0; endmodule",
            "interface Link(input logic clk); logic data; modport Slave(input data); endinterface",
            "package Types; typedef logic [7:0] Byte; parameter int WIDTH = 8; endpackage",
            "class Box; int value; function int read(); return value; endfunction endclass",
            "program Test; initial begin $display(\"ready\"); end endprogram",
            "function automatic int add(input int x, input int y); return x+y; endfunction",
            "task automatic wait_one(input logic enable); if (enable) #1; endtask",
            "parameter WIDTH = 8; localparam DEPTH = 4; typedef logic [WIDTH-1:0] Word;",
            "`define WIDTH 8\n`ifdef ENABLE\nmodule Enabled; endmodule\n`endif\n",
            "module Parent; Child #(.WIDTH(8)) instance_name(.clk(clk)); generate for(genvar i=0;i<2;i=i+1) begin: lanes wire data; end endgenerate endmodule",
            "module \\escaped.module (input wire \\escaped.port ); endmodule",
            "primitive Invert(output result, input value); table 0:1; 1:0; endtable endprimitive",
        ] {
            let tree = parser().parse(source, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "{source}\n{}",
                tree.root_node().to_sexp()
            );
            assert!(
                shapes(&tree)
                    .iter()
                    .all(|node| node.range.end <= source.len())
            );
        }
    }

    #[test]
    fn malformed_owner_retains_exact_diagnostics_and_parser_reuse() {
        let source = "module Missing(input wire clk; assign value = clk;";
        let mut parser = parser();
        let tree = parser.parse(source, None).unwrap();
        assert!(tree.root_node().has_error());
        assert!(shapes(&tree).iter().any(|node| node.error || node.missing));
        let valid = parser.parse("module Clean; endmodule", None).unwrap();
        assert!(!valid.root_node().has_error());
    }

    #[test]
    fn cancellation_can_interrupt_and_reset_native_work() {
        let source = "module Item; wire a; endmodule\n".repeat(20_000);
        let mut parser = parser();
        let mut stopped = false;
        let mut progress = |_: &tree_sitter::ParseState| {
            stopped = true;
            ControlFlow::Break(())
        };
        let options = ParseOptions::new().progress_callback(&mut progress);
        let result = parser.parse_with_options(
            &mut |offset, _| &source.as_bytes()[offset..],
            None,
            Some(options),
        );
        assert!(result.is_none() && stopped);
        parser.reset();
        assert!(
            !parser
                .parse("module Clean; endmodule", None)
                .unwrap()
                .root_node()
                .has_error()
        );
    }
}

#[test]
fn edited_identifiers_and_restoration_match_complete_fresh_trees() {
    let source =
        "// λ\r\nmodule Written(input wire clk); wire value; assign value=clk; endmodule\r\n";
    let changed = source.replace("value", "long_value");
    let mut parser = parser();
    let original = parser.parse(source, None).unwrap();
    let mut previous = original.clone();
    previous.edit(&InputEdit {
        start_byte: 0,
        old_end_byte: source.len(),
        new_end_byte: changed.len(),
        start_position: Point::new(0, 0),
        old_end_position: point(source.as_bytes(), source.len()),
        new_end_position: point(changed.as_bytes(), changed.len()),
    });
    let mut edited = parser.parse(&changed, Some(&previous)).unwrap();
    assert_eq!(
        shapes(&edited),
        shapes(&parser.parse(&changed, None).unwrap())
    );
    edited.edit(&InputEdit {
        start_byte: 0,
        old_end_byte: changed.len(),
        new_end_byte: source.len(),
        start_position: Point::new(0, 0),
        old_end_position: point(changed.as_bytes(), changed.len()),
        new_end_position: point(source.as_bytes(), source.len()),
    });
    assert_eq!(
        shapes(&parser.parse(source, Some(&edited)).unwrap()),
        shapes(&original)
    );
}
