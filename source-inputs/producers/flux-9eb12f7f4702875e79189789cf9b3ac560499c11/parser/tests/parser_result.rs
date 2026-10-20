use flux_source_parser::{
    ast::{walk::{self, Node, Visitor}, File},
    control::{Control, Limits, Progress, Stop},
    controlled_parser::ControlledParser,
    parser::{Parser, SourcePolicy},
};

const ORIGINAL: &str = include_str!("fixtures/original-scale_test.flux");

fn limits() -> Limits {
    Limits { source_bytes: 1_048_576, transitions: 4_194_304, tokens: 100_000, positions: 200_000,
             comments: 10_000, parser_depth: 512, ast_nodes: 100_000 }
}

fn legacy(source: &str, policy: SourcePolicy) -> File {
    Parser::with_source_policy(source, source.len(), policy).unwrap().parse_file("original.flux".to_owned())
}

fn controlled(source: &str, policy: SourcePolicy, bound: Limits) -> (Result<File, Stop>, Progress) {
    let mut never = |_| false;
    let mut parser = ControlledParser::new(source, Control::new(bound, &mut never), policy).unwrap();
    let result = parser.parse_file("original.flux".to_owned());
    (result, parser.progress())
}

struct Count(usize);
impl<'a> Visitor<'a> for Count {
    fn visit(&mut self, _: Node<'a>) -> bool { self.0 += 1; true }
}

#[test]
fn original_complete_native_ast_and_both_storage_policies_are_conserved() {
    assert_eq!(ORIGINAL.len(), 815);
    for policy in [SourcePolicy::LocationsOnly, SourcePolicy::Embedded] {
        let expected = legacy(ORIGINAL, policy);
        let (actual, progress) = controlled(ORIGINAL, policy, limits());
        let actual = actual.unwrap();
        assert_eq!(actual, expected);
        let mut count = Count(0); walk::walk(&mut count, Node::File(&actual));
        assert_eq!(count.0, 143);
        assert_eq!(progress.parser_depth, 0);
        assert!(progress.parser_steps > count.0);
        assert!(progress.ast_nodes >= count.0, "every native AST node needs an admission reservation");
        println!("policy={policy:?} original_bytes={} nodes={} reservations={} parser_steps={} total_work={}",
                 ORIGINAL.len(), count.0, progress.ast_nodes, progress.parser_steps, progress.transitions);
    }
}

#[test]
fn typed_stop_after_actual_ast_construction_discards_file_and_unwinds_depth() {
    let mut observed = false;
    let mut cancel = |p: Progress| { observed |= p.ast_nodes >= 8; observed };
    let mut parser = ControlledParser::new(ORIGINAL, Control::new(limits(), &mut cancel), SourcePolicy::LocationsOnly).unwrap();
    assert_eq!(parser.parse_file("original.flux".to_owned()), Err(Stop::Cancelled));
    assert_eq!(parser.progress().parser_depth, 0);
    assert!(parser.progress().ast_nodes >= 8);
    assert_eq!(parser.parse_file("retry.flux".to_owned()), Err(Stop::Cancelled));
    drop(parser);
    assert!(observed);
}

#[test]
fn active_long_lexical_cancellation_reaches_the_actual_parser_without_eof_success() {
    for source in [format!("value = \"{}\"", "a".repeat(32_768)), format!("//{}\nvalue = 1", "a".repeat(32_768))] {
        let mut observed = false;
        let mut cancel = |p: Progress| { observed |= p.byte_offset >= 4096; observed };
        let mut parser = ControlledParser::new(&source, Control::new(limits(), &mut cancel), SourcePolicy::LocationsOnly).unwrap();
        assert_eq!(parser.parse_file("long.flux".to_owned()), Err(Stop::Cancelled));
        assert_eq!(parser.progress().parser_depth, 0);
        assert!(parser.progress().byte_offset < source.len());
        drop(parser); assert!(observed);
    }
}

#[test]
fn node_and_work_authorities_fail_closed_at_the_observed_boundary() {
    let (_, progress) = controlled(ORIGINAL, SourcePolicy::LocationsOnly, limits());
    let mut bound = limits(); bound.ast_nodes = progress.ast_nodes;
    assert!(controlled(ORIGINAL, SourcePolicy::LocationsOnly, bound).0.is_ok());
    bound.ast_nodes -= 1;
    let (result, progress) = controlled(ORIGINAL, SourcePolicy::LocationsOnly, bound);
    assert_eq!(result, Err(Stop::Nodes)); assert_eq!(progress.parser_depth, 0);
    let mut bound = limits(); bound.transitions = 128;
    let (result, progress) = controlled(ORIGINAL, SourcePolicy::LocationsOnly, bound);
    assert_eq!(result, Err(Stop::Work)); assert_eq!(progress.parser_depth, 0);
    assert_eq!(progress.transitions, 128);
}

#[test]
fn recursive_monotype_paths_have_native_control_depth_authority() {
    let positive = format!("builtin value : {}int{}", "[".repeat(8), "]".repeat(8));
    let (actual, progress) = controlled(&positive, SourcePolicy::LocationsOnly, limits());
    assert_eq!(actual.unwrap(), legacy(&positive, SourcePolicy::LocationsOnly));
    assert_eq!(progress.parser_depth, 0);
    let deep = format!("builtin value : {}int{}", "[".repeat(256), "]".repeat(256));
    let mut bound = limits(); bound.parser_depth = 64;
    let (result, progress) = controlled(&deep, SourcePolicy::LocationsOnly, bound);
    assert_eq!(result, Err(Stop::Depth)); assert_eq!(progress.parser_depth, 0);
}

#[test]
fn ordinary_native_syntax_errors_and_unicode_are_not_promoted_to_control_stops() {
    for source in ["value = (", "value = 01", "value = \"bad\\q\"", "value = \"λ界\"\r\n", "f = (x) => x + 1\r\nresult = f(x: 2)"] {
        let (actual, progress) = controlled(source, SourcePolicy::LocationsOnly, limits());
        assert_eq!(actual.unwrap(), legacy(source, SourcePolicy::LocationsOnly), "{source:?}");
        assert_eq!(progress.parser_depth, 0);
    }
}

#[test]
fn active_native_string_conversion_cancellation_is_not_a_lexical_or_preflight_proxy() {
    let source = format!("value = \"{}\"", "a".repeat(8192));
    let mut reached_conversion = false;
    let mut cancel = |p: Progress| { reached_conversion |= p.conversion_steps >= 64; reached_conversion };
    let mut parser = ControlledParser::new(&source, Control::new(limits(), &mut cancel), SourcePolicy::LocationsOnly).unwrap();
    assert_eq!(parser.parse_file("conversion.flux".to_owned()), Err(Stop::Cancelled));
    assert_eq!(parser.progress().conversion_steps, 64);
    assert_eq!(parser.progress().parser_depth, 0);
    drop(parser);
    assert!(reached_conversion);
}

#[test]
fn native_conversion_values_and_syntax_diagnostics_remain_identical() {
    for source in [
        r#"value = "\xCE\xBB\n\$\\\"""#,
        r#"value = "\xFF""#,
        r#"value = "bad\q""#,
        "value = 1h2m3µs",
        "value = 2020-01-01",
        "value = 2020-01-01T00:00:00.123456789+03:00",
        "value = \"λ界\"\r\n",
        "value = \"before ${1 + 2} after\"",
    ] {
        let (actual, progress) = controlled(source, SourcePolicy::LocationsOnly, limits());
        assert_eq!(actual.unwrap(), legacy(source, SourcePolicy::LocationsOnly), "{source:?}");
        assert!(progress.conversion_steps > 0);
        assert_eq!(progress.parser_depth, 0);
    }
}

#[test]
fn actual_duration_and_unicode_conversion_loops_propagate_native_stops() {
    for source in [format!("value = {}", "1h".repeat(4096)), format!("value = \"{}\"", "λ界".repeat(4096))] {
        let mut observed = false;
        let mut cancel = |p: Progress| { observed |= p.conversion_steps >= 64; observed };
        let mut parser = ControlledParser::new(&source, Control::new(limits(), &mut cancel), SourcePolicy::LocationsOnly).unwrap();
        assert_eq!(parser.parse_file("conversion.flux".to_owned()), Err(Stop::Cancelled));
        assert_eq!(parser.progress().conversion_steps, 64);
        assert_eq!(parser.progress().parser_depth, 0);
        drop(parser); assert!(observed);
    }
}

#[test]
fn conversion_work_limit_is_reached_inside_the_actual_conversion_loop() {
    let source = format!("value = \"{}\"", "a".repeat(8192));
    let mut first_conversion_work = None;
    let mut observe = |p: Progress| { if p.conversion_steps == 1 { first_conversion_work.get_or_insert(p.transitions); } false };
    let mut parser = ControlledParser::new(&source, Control::new(limits(), &mut observe), SourcePolicy::LocationsOnly).unwrap();
    assert!(parser.parse_file("conversion.flux".to_owned()).is_ok()); drop(parser);
    let mut bound = limits(); bound.transitions = first_conversion_work.unwrap() + 31;
    let (result, progress) = controlled(&source, SourcePolicy::LocationsOnly, bound);
    assert_eq!(result, Err(Stop::Work));
    assert!(progress.conversion_steps > 1 && progress.conversion_steps < 8192);
    assert_eq!(progress.transitions, bound.transitions);
    assert_eq!(progress.parser_depth, 0);
}

#[test]
fn unsupported_validation_is_explicit_and_never_fabricates_a_native_file() {
    for (source, reason) in [
        ("value = /[a-z]+/", Stop::UnsupportedRegexValidation),
        ("value = 2020-01-01T00:00:00.12345678901234567890Z", Stop::UnsupportedTimePrecision),
    ] {
        let original = source.as_bytes().to_vec();
        let expected = legacy(source, SourcePolicy::LocationsOnly);
        assert!(!expected.body.is_empty());
        let (result, progress) = controlled(source, SourcePolicy::LocationsOnly, limits());
        assert_eq!(result, Err(reason));
        assert_eq!(progress.parser_depth, 0);
        assert_eq!(source.as_bytes(), original);
    }
}

#[test]
fn empty_file_and_parser_token_limits_also_have_native_admission_authority() {
    let (result, progress) = controlled("", SourcePolicy::LocationsOnly, limits());
    assert_eq!(result.unwrap(), legacy("", SourcePolicy::LocationsOnly));
    assert_eq!(progress.ast_nodes, 1);
    let mut bound = limits(); bound.ast_nodes = 0;
    assert_eq!(controlled("", SourcePolicy::LocationsOnly, bound).0, Err(Stop::Nodes));
    let mut bound = limits(); bound.tokens = 1;
    let (result, progress) = controlled("value = 1", SourcePolicy::LocationsOnly, bound);
    assert_eq!(result, Err(Stop::Tokens)); assert_eq!(progress.parser_depth, 0);
}
