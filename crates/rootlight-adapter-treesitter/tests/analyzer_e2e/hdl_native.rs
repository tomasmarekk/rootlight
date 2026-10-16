//! HDL product fixtures distinguish written ownership from hardware elaboration.
//! Exact definition bytes and unresolved references remain independently asserted.

use super::*;

const HDL: LanguageCase = LanguageCase {
    name: "systemverilog",
    path: "rtl/written.sv",
    frontend: "tree-sitter-systemverilog-0.4.1",
    source: "module pipe #(parameter WIDTH = 8)(input wire clk, input wire [WIDTH-1:0] data, output wire ready);\n wire active;\n function [WIDTH-1:0] transform(input [WIDTH-1:0] value); transform = value; endfunction\n assign ready = active;\n child #(.SIZE(WIDTH)) instance_0(.clk(clk));\n if (WIDTH > 0) begin : positive wire selected; end\nendmodule\n",
    generated: false,
    body_before: "transform = value",
    body_after: "transform = data",
};

fn output(source: &str) -> AnalysisOutput {
    output_for(HDL, source)
}

fn output_for(case: LanguageCase, source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, case),
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    );
    let document = result.document();
    validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
    for occurrence in &document.occurrences {
        let span = occurrence.source.span();
        let bytes = &source.as_bytes()[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert_eq!(occurrence.syntactic_text_hash, content_hash(bytes));
        assert_eq!(
            occurrence.source.content_hash(),
            content_hash(source.as_bytes())
        );
        assert_eq!(occurrence.source.repository(), document.repository);
        assert_eq!(occurrence.source.generation(), document.generation);
        if occurrence.role != OccurrenceRole::Definition {
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
    }
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    result
}

fn definition(
    document: &rootlight_ir::NormalizedIrDocument,
    source: &str,
    name: &str,
    kind: EntityKind,
) {
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == name && entity.kind == kind)
        .unwrap_or_else(|| {
            panic!(
                "missing {name} {kind:?}: {:?}",
                document
                    .entities
                    .iter()
                    .map(|entity| (&entity.canonical_name, entity.kind))
                    .collect::<Vec<_>>()
            )
        });
    let occurrence = document.occurrences.iter().find(|occurrence| occurrence.role == OccurrenceRole::Definition
        && matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == entity.id)).unwrap();
    let span = occurrence.source.span();
    let written = &source
        [usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap()];
    assert_eq!(written.strip_prefix('\\').unwrap_or(written), name);
}

fn contains(document: &rootlight_ir::NormalizedIrDocument, owner: &str, child: &str) {
    let parent = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == owner)
        .unwrap();
    let child = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == child)
        .unwrap();
    assert!(
        document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(parent.id)
                && relation.object == RelationEndpoint::Entity(child.id)),
        "missing {owner} Contains {}",
        child.canonical_name
    );
}

#[test]
fn hdl_modules_ports_parameters_functions_instances_and_generate_owners_are_written() {
    let result = output(HDL.source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, kind) in [
        ("pipe", EntityKind::Module),
        ("WIDTH", EntityKind::Constant),
        ("clk", EntityKind::Field),
        ("data", EntityKind::Field),
        ("ready", EntityKind::Field),
        ("active", EntityKind::Variable),
        ("transform", EntityKind::Function),
        ("value", EntityKind::Parameter),
        ("instance_0", EntityKind::Variable),
        ("positive", EntityKind::Namespace),
        ("selected", EntityKind::Variable),
    ] {
        definition(document, HDL.source, name, kind);
    }
    for (parent, child) in [
        ("pipe", "clk"),
        ("pipe", "WIDTH"),
        ("pipe", "transform"),
        ("transform", "value"),
        ("pipe", "instance_0"),
        ("pipe", "positive"),
        ("positive", "selected"),
    ] {
        contains(document, parent, child);
    }
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "child")
    );
}

#[test]
fn hdl_moduleless_fragments_preserve_tasks_typedefs_and_macro_names() {
    let source = "parameter COUNT = 4;\ntypedef enum logic [1:0] {IDLE, BUSY} state_t;\n`define ENABLED(x) ((x) != 0)\ntask prot_task(input [1:0] a, input [1:0] b, output reg [1:0] out); out = a + b; endtask\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, kind) in [
        ("COUNT", EntityKind::Constant),
        ("state_t", EntityKind::TypeAlias),
        ("IDLE", EntityKind::Constant),
        ("BUSY", EntityKind::Constant),
        ("ENABLED", EntityKind::Constant),
        ("prot_task", EntityKind::Function),
        ("a", EntityKind::Parameter),
        ("b", EntityKind::Parameter),
        ("out", EntityKind::Parameter),
    ] {
        definition(document, source, name, kind);
    }
    contains(document, "prot_task", "a");
    contains(document, "prot_task", "out");
}

#[test]
fn hdl_nonansi_ports_multiple_declarators_and_escaped_identifiers_keep_exact_spans() {
    let source = "module legacy(a, b, z);\ninput a, b; output z;\nwire first, second;\nreg \\data[0] , plain;\nassign z = a & b;\nendmodule\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, kind) in [
        ("legacy", EntityKind::Module),
        ("a", EntityKind::Field),
        ("b", EntityKind::Field),
        ("z", EntityKind::Field),
        ("first", EntityKind::Variable),
        ("second", EntityKind::Variable),
        ("data[0]", EntityKind::Variable),
        ("plain", EntityKind::Variable),
    ] {
        definition(document, source, name, kind);
    }
}

#[test]
fn hdl_interfaces_packages_classes_and_programs_keep_authored_owners() {
    let source = "package values; parameter LIMIT = 8; typedef int index_t; endpackage\ninterface bus(input logic clk); logic ready; modport producer(output ready, input clk); endinterface\nclass box; int value; function int get(); return value; endfunction endclass\nprogram check; int count; endprogram\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, kind) in [
        ("values", EntityKind::Namespace),
        ("LIMIT", EntityKind::Constant),
        ("index_t", EntityKind::TypeAlias),
        ("bus", EntityKind::Interface),
        ("producer", EntityKind::Namespace),
        ("box", EntityKind::Class),
        ("get", EntityKind::Function),
        ("check", EntityKind::Module),
    ] {
        definition(document, source, name, kind);
    }
    contains(document, "values", "index_t");
    contains(document, "bus", "producer");
    contains(document, "box", "get");
}

#[test]
fn hdl_inert_text_and_malformed_owners_never_create_valid_children() {
    let source = "// module hidden; endmodule\nmodule valid; string message = \"task phantom; endtask\"; endmodule\nmodule ; input promoted; endmodule\n";
    let result = output(source);
    let document = result.document();
    assert!(!document.diagnostics.is_empty());
    definition(document, source, "valid", EntityKind::Module);
    for name in ["hidden", "phantom", "promoted"] {
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "fabricated {name}"
        );
    }
    assert!(!document.skipped_regions.is_empty());
}

#[test]
fn hdl_written_references_survive_identity_capture_partitioning() {
    let source = "module Written; wire source; wire sink; assign sink=source; child instance_0(.port(source)); endmodule\n";
    for case in [
        HDL,
        LanguageCase {
            name: "verilog",
            path: "rtl/written.v",
            frontend: "tree-sitter-verilog-0.4.1",
            ..HDL
        },
    ] {
        let result = output_for(case, source);
        let document = result.document();
        assert!(document.diagnostics.is_empty());
        for (offset, text) in [
            (source.find("sink=source").unwrap(), "sink"),
            (source.find("sink=source").unwrap() + 5, "source"),
            (source.find("child").unwrap(), "child"),
            (source.rfind("source").unwrap(), "source"),
        ] {
            let matches: Vec<_> = document
                .occurrences
                .iter()
                .filter(|occurrence| {
                    let span = occurrence.source.span();
                    occurrence.role == OccurrenceRole::Reference
                        && span.start_byte() == u64::try_from(offset).unwrap()
                        && span.end_byte() == u64::try_from(offset + text.len()).unwrap()
                })
                .collect();
            assert_eq!(
                matches.len(),
                1,
                "{} missing written reference {text} at {offset}",
                case.name
            );
            assert!(matches!(
                matches[0].target,
                OccurrenceTarget::Unresolved { .. }
            ));
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source == matches[0].source
                        && gap.domain == FactDomain::Relations)
            );
        }
    }
}
