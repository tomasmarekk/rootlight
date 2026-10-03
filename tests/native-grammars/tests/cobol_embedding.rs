//! Contiguous physical-line COBOL bodies retain original Markdown coordinates.
//! These native checks qualify neither deindented/disjoint ranges nor public
//! included-range admission, COPY expansion, or semantic binding.

use std::ops::{ControlFlow, Range as ByteRange};

use tree_sitter::{
    Language, ParseOptions, Parser, Point, Query, QueryCursor, StreamingIterator, Tree,
};

const QUERY: &str = include_str!("../../../crates/rootlight-adapter-treesitter/queries/cobol.scm");

#[derive(Clone, Copy, Debug)]
enum Profile {
    Fixed,
    Free,
}

impl Profile {
    fn language(self) -> Language {
        match self {
            Self::Fixed => tree_sitter_cobol::FIXED.into(),
            Self::Free => tree_sitter_cobol::FREE.into(),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Fixed => "cobol-fixed",
            Self::Free => "cobol-free",
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::Fixed => "000100 ",
            Self::Free => "",
        }
    }

    fn parser(self) -> Parser {
        let mut parser = Parser::new();
        parser.set_language(&self.language()).unwrap();
        parser
    }
}

fn point(source: &[u8], offset: usize) -> Point {
    let prefix = source.get(..offset).unwrap();
    Point::new(
        prefix.iter().filter(|byte| **byte == b'\n').count(),
        prefix
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

fn included_range(source: &[u8], body: ByteRange<usize>) -> tree_sitter::Range {
    let start_point = point(source, body.start);
    let end_point = point(source, body.end);
    assert_eq!(start_point.column, 0);
    assert!(body.end == source.len() || end_point.column == 0);
    tree_sitter::Range {
        start_byte: body.start,
        end_byte: body.end,
        start_point,
        end_point,
    }
}

fn bounded_parse(parser: &mut Parser, source: &[u8]) -> Tree {
    let mut steps = 0_usize;
    let mut watchdog = |_: &tree_sitter::ParseState| {
        steps = steps.saturating_add(1);
        if steps > 2_000 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    parser
        .parse_with_options(
            &mut |offset, _| source.get(offset..).unwrap_or_default(),
            None,
            Some(ParseOptions::new().progress_callback(&mut watchdog)),
        )
        .expect("native embedding fixture exceeded progress watchdog")
}

#[derive(Debug, PartialEq, Eq)]
struct NodeRecord {
    depth: usize,
    field: Option<&'static str>,
    kind: &'static str,
    bytes: Option<ByteRange<usize>>,
    children: usize,
    named: bool,
    extra: bool,
}

fn shape(tree: &Tree, source: &[u8], body: ByteRange<usize>) -> Vec<NodeRecord> {
    assert_eq!(tree.root_node().kind(), "start");
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let mut cursor = tree.walk();
    let mut depth = 0_usize;
    let mut result = Vec::new();
    loop {
        let node = cursor.node();
        assert!(!node.is_error() && !node.is_missing());
        assert!(node.start_byte() <= node.end_byte() && node.end_byte() <= source.len());
        assert_eq!(node.start_position(), point(source, node.start_byte()));
        assert_eq!(node.end_position(), point(source, node.end_byte()));
        assert_eq!(
            node.utf8_text(source).unwrap().as_bytes(),
            &source[node.byte_range()]
        );
        // The synthetic document root can include origin padding/EOF framing.
        // Authored children, not that wrapper, must retain exact body coordinates.
        let bytes = if depth == 0 {
            None
        } else {
            assert!(node.start_byte() >= body.start && node.end_byte() <= body.end);
            Some(node.start_byte() - body.start..node.end_byte() - body.start)
        };
        result.push(NodeRecord {
            depth,
            field: cursor.field_name(),
            kind: node.kind(),
            bytes,
            children: node.child_count(),
            named: node.is_named(),
            extra: node.is_extra(),
        });
        assert!(
            result.len() <= 4_096,
            "native fixture tree exceeded node bound"
        );
        if cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return result;
            }
            depth -= 1;
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CaptureRecord {
    pattern: usize,
    name: String,
    kind: &'static str,
    start: Option<usize>,
    end: Option<usize>,
    written: Option<String>,
}

fn captures(
    profile: Profile,
    tree: &Tree,
    source: &[u8],
    body: ByteRange<usize>,
) -> Vec<CaptureRecord> {
    let query = Query::new(&profile.language(), QUERY).unwrap();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source);
    let mut result = Vec::new();
    while let Some(matched) = matches.next() {
        for capture in matched.captures {
            let node = capture.node;
            assert_eq!(node.start_position(), point(source, node.start_byte()));
            assert_eq!(node.end_position(), point(source, node.end_byte()));
            let (start, end, written) = if node.id() == tree.root_node().id() {
                assert_eq!(node.kind(), "start");
                (None, None, None)
            } else {
                assert!(node.start_byte() >= body.start && node.end_byte() <= body.end);
                (
                    Some(node.start_byte() - body.start),
                    Some(node.end_byte() - body.start),
                    Some(node.utf8_text(source).unwrap().to_owned()),
                )
            };
            let name = query.capture_names()[usize::try_from(capture.index).unwrap()];
            result.push(CaptureRecord {
                pattern: matched.pattern_index,
                name: name.to_owned(),
                kind: node.kind(),
                start,
                end,
                written,
            });
            assert!(
                result.len() <= 4_096,
                "native fixture captures exceeded bound"
            );
        }
    }
    result.sort();
    result
}

fn fixture(profile: Profile, ending: &str) -> (String, String) {
    let prefix = profile.prefix();
    let (comment, literal) = match profile {
        Profile::Fixed => (
            "000650* indicator comment has no declaration".to_owned(),
            format!("\"first{ending}000800-    \"second\""),
        ),
        Profile::Free => (
            "*> free comment has no declaration".to_owned(),
            "\"first second\"".to_owned(),
        ),
    };
    let source = format!(
        "{prefix}IDENTIFICATION DIVISION.{ending}\
         {prefix}PROGRAM-ID. Embedded.{ending}\
         {prefix}DATA DIVISION.{ending}\
         {prefix}WORKING-STORAGE SECTION.{ending}\
         {prefix}01 Stored PIC X(16).{ending}\
         {prefix}PROCEDURE DIVISION.{ending}\
         {prefix}Entry-Point.{ending}\
         {comment}{ending}\
         {prefix}DISPLAY {literal}.{ending}\
         {prefix}MOVE Stored TO Stored.{ending}\
         {prefix}STOP RUN.{ending}\
         {prefix}END PROGRAM Embedded.{ending}"
    );
    (source, literal)
}

fn neighboring_program(profile: Profile, ending: &str, name: &str) -> String {
    let prefix = profile.prefix();
    format!(
        "{prefix}IDENTIFICATION DIVISION.{ending}\
         {prefix}PROGRAM-ID. {name}.{ending}\
         {prefix}PROCEDURE DIVISION.{ending}\
         {prefix}STOP RUN.{ending}"
    )
}

fn assert_equivalent(
    profile: Profile,
    source: &str,
    body: ByteRange<usize>,
    standalone: &str,
    embedded: &Tree,
) -> Vec<CaptureRecord> {
    assert_eq!(source.get(body.clone()).unwrap(), standalone);
    let expected = bounded_parse(&mut profile.parser(), standalone.as_bytes());
    assert_eq!(
        shape(embedded, source.as_bytes(), body.clone()),
        shape(&expected, standalone.as_bytes(), 0..standalone.len()),
        "{profile:?} native shape changed in host coordinates"
    );
    let actual = captures(profile, embedded, source.as_bytes(), body);
    assert_eq!(
        actual,
        captures(
            profile,
            &expected,
            standalone.as_bytes(),
            0..standalone.len()
        ),
        "{profile:?} capture inventory changed in host coordinates"
    );
    actual
}

#[test]
fn contiguous_markdown_bodies_match_standalone_trees_and_exact_captures() {
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            let (standalone, literal) = fixture(profile, ending);
            let before = neighboring_program(profile, ending, "Before-Body");
            let after = neighboring_program(profile, ending, "After-Body");
            let prefix = format!(
                "# Host λ😀{ending}```{}{ending}{before}```{ending}{ending}```{}{ending}",
                profile.label(),
                profile.label(),
            );
            let suffix = format!(
                "```{ending}{ending}```{}{ending}{after}```{ending}Host tail{ending}",
                profile.label(),
            );
            let source = format!("{prefix}{standalone}{suffix}");
            let body = prefix.len()..prefix.len() + standalone.len();
            let range = included_range(source.as_bytes(), body.clone());
            let mut parser = profile.parser();
            parser.set_included_ranges(&[range]).unwrap();
            let embedded = bounded_parse(&mut parser, source.as_bytes());
            assert_eq!(embedded.included_ranges(), vec![range]);
            let inventory = assert_equivalent(profile, &source, body, &standalone, &embedded);
            assert_eq!(
                inventory
                    .iter()
                    .filter(|item| item.name == "definition")
                    .count(),
                3
            );
            let strings: Vec<_> = inventory
                .iter()
                .filter(|item| item.name == "string")
                .collect();
            assert_eq!(strings.len(), 1);
            assert_eq!(strings[0].written.as_deref(), Some(literal.as_str()));
            let literal_start = standalone.find(&literal).unwrap();
            assert_eq!(strings[0].start, Some(literal_start));
            assert_eq!(strings[0].end, Some(literal_start + literal.len()));
            assert!(
                inventory
                    .iter()
                    .filter_map(|item| item.written.as_deref())
                    .all(|written| !written.contains("Before-Body")
                        && !written.contains("After-Body")
                        && !written.contains("```"))
            );
            assert!(
                inventory
                    .iter()
                    .any(|item| item.name == "reference"
                        && item.written.as_deref() == Some("Stored"))
            );
        }
    }
}

#[test]
fn contiguous_body_ending_at_original_host_eof_matches_standalone() {
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            for final_newline in [false, true] {
                let (mut standalone, _) = fixture(profile, ending);
                if !final_newline {
                    standalone.truncate(standalone.len() - ending.len());
                }
                let prefix = format!("# Host 雪{ending}```{}{ending}", profile.label());
                let source = format!("{prefix}{standalone}");
                let body = prefix.len()..source.len();
                let mut parser = profile.parser();
                parser
                    .set_included_ranges(&[included_range(source.as_bytes(), body.clone())])
                    .unwrap();
                let embedded = bounded_parse(&mut parser, source.as_bytes());
                assert_equivalent(profile, &source, body, &standalone, &embedded);
            }
        }
    }
}

#[test]
fn cancelled_included_body_then_reset_keeps_the_next_original_source_exact() {
    for profile in [Profile::Fixed, Profile::Free] {
        for ending in ["\n", "\r\n"] {
            let prefix = profile.prefix();
            let large = format!(
                "{prefix}IDENTIFICATION DIVISION.{ending}\
                 {prefix}PROGRAM-ID. Large-Body.{ending}\
                 {prefix}PROCEDURE DIVISION.{ending}\
                 {}{prefix}STOP RUN.{ending}",
                format!("{prefix}DISPLAY 1.{ending}").repeat(10_000),
            );
            let host_prefix = format!("# Host{ending}```{}{ending}", profile.label());
            let source = format!("{host_prefix}{large}```{ending}");
            let body = host_prefix.len()..host_prefix.len() + large.len();
            let mut parser = profile.parser();
            parser
                .set_included_ranges(&[included_range(source.as_bytes(), body)])
                .unwrap();
            let mut called = false;
            let mut cancel = |_: &tree_sitter::ParseState| {
                called = true;
                ControlFlow::Break(())
            };
            let cancelled = parser.parse_with_options(
                &mut |offset, _| source.as_bytes().get(offset..).unwrap_or_default(),
                None,
                Some(ParseOptions::new().progress_callback(&mut cancel)),
            );
            assert!(called && cancelled.is_none());
            parser.reset();

            let (standalone, _) = fixture(profile, ending);
            let host_prefix = format!("# Fresh λ{ending}{ending}```{}{ending}", profile.label());
            let source = format!("{host_prefix}{standalone}```{ending}");
            let body = host_prefix.len()..host_prefix.len() + standalone.len();
            parser
                .set_included_ranges(&[included_range(source.as_bytes(), body.clone())])
                .unwrap();
            let embedded = bounded_parse(&mut parser, source.as_bytes());
            assert_equivalent(profile, &source, body, &standalone, &embedded);
        }
    }
}
