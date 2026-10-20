use flux_source_parser::ast::{
    self,
    walk::{self, Node, Visitor},
};
use flux_source_parser::control::{checked_source_len, Control, Limits, Stop};
use flux_source_parser::parser::{Parser, SourcePolicy};
use flux_source_parser::scanner::{controlled::ControlledScanner, Scanner, TokenType};

const ORIGINAL: &str = include_str!("fixtures/original-scale_test.flux");

fn limits() -> Limits {
    Limits {
        source_bytes: 1_048_576,
        transitions: 4_194_304,
        tokens: 100_000,
        positions: 200_000,
        comments: 10_000,
        parser_depth: 512,
        ast_nodes: 100_000,
    }
}

#[test]
fn original_tokens_and_physical_offsets_match_the_unchanged_native_scanner() {
    assert_eq!(ORIGINAL.len(), 815);
    let mut never = |_| false;
    let mut controlled =
        ControlledScanner::new(ORIGINAL, Control::new(limits(), &mut never)).unwrap();
    let mut original = Scanner::new(ORIGINAL);
    let mut count = 0;
    loop {
        let expected = original.scan_with_regex();
        let actual = controlled.scan_with_regex().unwrap();
        assert_eq!(actual, expected);
        if actual.tok == TokenType::Eof {
            break;
        }
        assert_eq!(
            &ORIGINAL[actual.start_offset as usize..actual.end_offset as usize],
            actual.lit
        );
        count += 1;
    }
    assert!(count > 100);
    assert!(controlled.progress().transitions >= ORIGINAL.len());
}

#[test]
fn source_lengths_are_checked_without_allocating_gigabytes() {
    assert_eq!(checked_source_len(815, 815), Ok(815));
    assert_eq!(checked_source_len(815, 814), Err(Stop::SourceLength));
    assert_eq!(
        checked_source_len(i32::MAX as usize - 1, usize::MAX),
        Ok(i32::MAX - 1)
    );
    assert_eq!(
        checked_source_len(i32::MAX as usize, usize::MAX),
        Err(Stop::SourceLength)
    );
    assert_eq!(
        checked_source_len(i32::MAX as usize + 1, usize::MAX),
        Err(Stop::SourceLength)
    );
    let mut never = |_| false;
    let mut bound = limits();
    bound.source_bytes = 814;
    assert!(matches!(
        ControlledScanner::new(ORIGINAL, Control::new(bound, &mut never)),
        Err(Stop::SourceLength)
    ));
    assert!(matches!(
        Parser::with_source_policy(ORIGINAL, 814, SourcePolicy::LocationsOnly),
        Err(Stop::SourceLength)
    ));
}

#[test]
fn native_transitions_stop_inside_each_long_lexical_region_without_a_token() {
    let long = "a".repeat(32_768);
    let cases = [
        (long.clone(), 0),
        (format!("//{long}\nvalue"), 0),
        (format!("\"{long}\""), 0),
        (format!("/{long}/"), 1),
        (" ".repeat(32_768), 0),
        (format!("{long}${{value}}"), 2),
    ];
    for (text, mode) in cases {
        let mut callback_count = 0;
        let mut cancel = |p: flux_source_parser::control::Progress| {
            callback_count += 1;
            p.transitions >= 64
        };
        let mut scanner =
            ControlledScanner::new(&text, Control::new(limits(), &mut cancel)).unwrap();
        assert_eq!(
            scanner.scan_with_comments(mode),
            Err(Stop::Cancelled),
            "mode={mode}"
        );
        assert_eq!(scanner.progress().transitions, 64);
        assert!(scanner.progress().byte_offset < 128);
        assert_eq!(scanner.progress().tokens, 0);
        assert!(scanner.comments.is_empty());
        assert_eq!(scanner.scan_with_comments(mode), Err(Stop::Cancelled));
        drop(scanner);
        assert!(callback_count > 64);
    }
}

#[test]
fn native_work_is_finite_and_unread_does_not_refund_it() {
    let text = format!("/{}/", "a".repeat(1024));
    let mut never = |_| false;
    let mut bound = limits();
    bound.transitions = 31;
    let mut scanner = ControlledScanner::new(&text, Control::new(bound, &mut never)).unwrap();
    assert_eq!(scanner.scan().unwrap().tok, TokenType::Div);
    let used = scanner.progress().transitions;
    assert!(used > 0);
    scanner.unread();
    assert_eq!(scanner.progress().transitions, used);
    assert_eq!(scanner.scan_with_regex(), Err(Stop::Work));
    assert_eq!(scanner.progress().transitions, 31);
    scanner.unread();
    assert_eq!(scanner.scan(), Err(Stop::Work));
}

#[test]
fn token_comment_and_position_limits_are_typed_and_sticky() {
    for (text, kind) in [
        ("a b", Stop::Tokens),
        ("//one\n//two\na", Stop::Comments),
        ("a b", Stop::Positions),
    ] {
        let mut bound = limits();
        match kind {
            Stop::Tokens => bound.tokens = 1,
            Stop::Comments => bound.comments = 1,
            Stop::Positions => bound.positions = 2,
            _ => unreachable!(),
        }
        let mut never = |_| false;
        let mut scanner = ControlledScanner::new(text, Control::new(bound, &mut never)).unwrap();
        let result = if kind == Stop::Comments {
            scanner.scan()
        } else {
            scanner.scan().unwrap();
            scanner.scan()
        };
        assert_eq!(result, Err(kind));
        assert_eq!(scanner.scan(), Err(kind));
    }
}

#[test]
fn unicode_crlf_bad_tokens_and_regex_reentry_preserve_standard_native_results() {
    for (text, mode) in [
        ("é = 3\r\n// λ\r\nvalue = /α+/", 1),
        ("\"text ${value}\"", 0),
        ("? ! `", 0),
        ("/unclosed", 1),
        ("x${value}", 2),
    ] {
        let mut never = |_| false;
        let mut controlled =
            ControlledScanner::new(text, Control::new(limits(), &mut never)).unwrap();
        let mut original = Scanner::new(text);
        for _ in 0..100 {
            let expected = original.scan_with_comments(mode);
            let actual = controlled.scan_with_comments(mode).unwrap();
            assert_eq!(actual, expected, "{text:?} mode={mode}");
            if actual.tok == TokenType::Eof {
                break;
            }
        }
        assert_eq!(
            controlled.scan_with_comments(mode).unwrap().tok,
            TokenType::Eof
        );
    }
}

#[derive(Default)]
struct NativeSnapshot {
    nodes: Vec<(String, ast::Position, ast::Position)>,
    identifiers: Vec<(String, usize, usize)>,
    copied_source_bytes: usize,
    copied_filenames: usize,
    errors: usize,
}

fn physical_offset(position: ast::Position) -> usize {
    assert!(position.is_valid());
    let line_start = if position.line == 1 {
        0
    } else {
        ORIGINAL
            .match_indices('\n')
            .nth(position.line as usize - 2)
            .unwrap()
            .0
            + 1
    };
    line_start + position.column as usize - 1
}

impl<'a> Visitor<'a> for NativeSnapshot {
    fn visit(&mut self, node: Node<'a>) -> bool {
        let base = node.base();
        self.nodes
            .push((node.to_string(), base.location.start, base.location.end));
        self.copied_source_bytes += base.location.source.as_ref().map_or(0, String::len);
        self.copied_filenames += usize::from(base.location.file.is_some());
        self.errors += base.errors.len();
        if let Node::Identifier(id) = node {
            self.identifiers.push((
                id.name.clone(),
                physical_offset(base.location.start),
                physical_offset(base.location.end),
            ));
        }
        true
    }
}

#[test]
fn locations_only_preserves_original_native_structure_and_all_required_leaf_witnesses() {
    let mut embedded = Parser::with_source_policy(ORIGINAL, 815, SourcePolicy::Embedded).unwrap();
    let mut locations =
        Parser::with_source_policy(ORIGINAL, 815, SourcePolicy::LocationsOnly).unwrap();
    let embedded = embedded.parse_file("original.flux".to_owned());
    let locations = locations.parse_file("original.flux".to_owned());
    let mut a = NativeSnapshot::default();
    let mut b = NativeSnapshot::default();
    walk::walk(&mut a, Node::File(&embedded));
    walk::walk(&mut b, Node::File(&locations));
    assert_eq!(a.nodes, b.nodes);
    assert_eq!(a.identifiers, b.identifiers);
    assert_eq!(a.errors, 0);
    assert_eq!(b.errors, 0);
    assert!(a.copied_source_bytes > ORIGINAL.len());
    assert!(a.copied_filenames > 0);
    assert_eq!(b.copied_source_bytes, 0);
    assert_eq!(b.copied_filenames, 0);
    println!("original_bytes={} native_nodes={} embedded_source_bytes={} embedded_filename_copies={} locations_source_bytes={} locations_filename_copies={}", ORIGINAL.len(), a.nodes.len(), a.copied_source_bytes, a.copied_filenames, b.copied_source_bytes, b.copied_filenames);
    for (name, start, end) in [
        ("scaleTime", 117, 126),
        ("d", 130, 131),
        ("n", 133, 134),
        ("d", 176, 177),
        ("n", 182, 183),
        ("scaleTime", 285, 294),
    ] {
        assert_eq!(
            b.identifiers
                .iter()
                .filter(|item| item.0 == name && item.1 == start && item.2 == end)
                .count(),
            1
        );
        assert_eq!(&ORIGINAL[start..end], name);
    }
    let assignment = locations
        .body
        .iter()
        .find_map(|statement| match statement {
            ast::Statement::Variable(value) if value.id.name == "scaleTime" => Some(value),
            _ => None,
        })
        .unwrap();
    let ast::Expression::Function(function) = &assignment.init else {
        panic!("native assignment is not a function")
    };
    assert_eq!(function.params.len(), 2);
    for (parameter, expected) in function
        .params
        .iter()
        .zip([("d", 130, 131), ("n", 133, 134)])
    {
        let ast::PropertyKey::Identifier(id) = &parameter.key else {
            panic!("native parameter key is not an identifier")
        };
        assert_eq!(
            (
                id.name.as_str(),
                physical_offset(id.base.location.start),
                physical_offset(id.base.location.end)
            ),
            expected
        );
    }
    assert_eq!(
        ORIGINAL
            .match_indices("scaleTime")
            .map(|(offset, _)| offset)
            .collect::<Vec<_>>(),
        vec![117, 285, 336, 387, 438]
    );
}

#[test]
fn cancellation_eof_and_invalid_mode_never_convert_a_stop_to_success() {
    let mut cancelled = |_| true;
    assert!(matches!(
        ControlledScanner::new("", Control::new(limits(), &mut cancelled)),
        Err(Stop::Cancelled)
    ));
    let mut never = |_| false;
    let mut scanner = ControlledScanner::new("", Control::new(limits(), &mut never)).unwrap();
    assert_eq!(scanner.scan().unwrap().tok, TokenType::Eof);
    assert_eq!(scanner.scan_with_comments(3), Err(Stop::InvalidMode));
    assert_eq!(scanner.scan(), Err(Stop::InvalidMode));
}
