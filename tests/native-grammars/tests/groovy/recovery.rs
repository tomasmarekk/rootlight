//! Exercises editor-style damage and parser reuse through the safe runtime API.
//! All inputs are bounded authored fragments; no corpus code is loaded or executed.
use std::ops::ControlFlow;
use tree_sitter::{InputEdit, Node, ParseOptions, Parser, Point, Tree};

const SOURCES: &[&str] = &[
    "def value = 1\nvalue\n/* a */\n/* b */\n.member()\nfinish()\n",
    "def value = 1\nvalue\n/* a */\n/* b */\nother()\nfinish()\n",
    "def text = / first\n${-> render()} last /\nfinish()\n",
    "def text = \"$λ.μέλος ${value / 2}\"\nfinish()\n",
    "def text = $/ first $λ last /$\nfinish()\n",
    "class Δelta { def λ(变量) { return 变量.μέλος() } }\nfinish()\n",
    "def run() { String first, second = 'x'\nfinish(first, second) }\n",
    "service.consume value then next\nfinish()\n",
    "if (ready) { run() } else { stop() }\nfinish()\n",
    "for (String item in items) { consume(item) }\nfinish()\n",
    "def c = { x -> x /* a */\n + 2 }\nfinish()\n",
    "try { run() } catch (Exception ex) { stop() } finally { finish() }\n",
    "def text = \"// literal\"\rfinish()\r",
    "def text = \"/* literal */ ${value /* note */ + 1}\"\nfinish()\n",
    "value\r// λ\r/* δ */\r.member()\rfinish()\r",
    "def text = /end\\/\nfinish()\n",
];

fn parser() -> Parser {
    let mut p = Parser::new();
    p.set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
        .unwrap();
    p
}
fn nodes(root: Node<'_>) -> Vec<Node<'_>> {
    let mut pending = vec![root];
    let mut result = vec![];
    while let Some(n) = pending.pop() {
        let mut c = n.walk();
        pending.extend(n.children(&mut c));
        result.push(n);
    }
    result
}
fn point(source: &[u8], offset: usize) -> Point {
    let prefix = &source[..offset];
    Point::new(
        prefix.iter().filter(|b| **b == b'\n').count(),
        prefix
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(prefix.len(), |p| prefix.len() - p - 1),
    )
}
fn checked(p: &mut Parser, source: &[u8], old: Option<&Tree>) -> Tree {
    let mut steps = 0;
    let mut progress = |_: &tree_sitter::ParseState| {
        steps += 1;
        if steps > 10_000 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let tree = p
        .parse_with_options(
            &mut |offset, _| source.get(offset..).unwrap_or_default(),
            old,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        )
        .expect("bounded fixture exceeded runtime operation watchdog");
    // Precompute lines once: rescanning the document for every node would make
    // coordinate verification quadratic and obscure actual parser behavior.
    let lines: Vec<_> = std::iter::once(0)
        .chain(
            source
                .iter()
                .enumerate()
                .filter_map(|(i, b)| (*b == b'\n').then_some(i + 1)),
        )
        .collect();
    let coordinate = |offset| {
        let row = lines.partition_point(|start| *start <= offset) - 1;
        Point::new(row, offset - lines[row])
    };
    for n in nodes(tree.root_node()) {
        assert!(n.start_byte() <= n.end_byte() && n.end_byte() <= source.len());
        assert_eq!(n.start_position(), coordinate(n.start_byte()));
        assert_eq!(n.end_position(), coordinate(n.end_byte()));
        if let Some(parent) = n.parent() {
            assert!(parent.start_byte() <= n.start_byte() && n.end_byte() <= parent.end_byte());
        }
    }
    tree
}
fn signature(tree: &Tree) -> Vec<(String, std::ops::Range<usize>, Point, Point, bool)> {
    nodes(tree.root_node())
        .into_iter()
        .map(|n| {
            (
                n.kind().to_owned(),
                n.byte_range(),
                n.start_position(),
                n.end_position(),
                n.is_missing(),
            )
        })
        .collect()
}
fn valid(source: &str) -> Tree {
    let tree = checked(&mut parser(), source.as_bytes(), None);
    assert!(
        !tree.root_node().has_error(),
        "{source:?}\n{}",
        tree.root_node().to_sexp()
    );
    tree
}
fn edit(tree: &mut Tree, before: &str, after: &str, start: usize, old_end: usize, new_end: usize) {
    tree.edit(&InputEdit {
        start_byte: start,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: point(before.as_bytes(), start),
        old_end_position: point(before.as_bytes(), old_end),
        new_end_position: point(after.as_bytes(), new_end),
    });
}

#[test]
fn every_scalar_prefix_can_be_completed_without_stale_scanner_state() {
    let mut count = 0;
    for source in SOURCES {
        let expected = signature(&valid(source));
        for cut in source
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(source.len()))
        {
            let prefix = &source[..cut];
            let mut p = parser();
            let mut old = checked(&mut p, prefix.as_bytes(), None);
            edit(&mut old, prefix, source, cut, cut, source.len());
            let actual = checked(&mut p, source.as_bytes(), Some(&old));
            assert_eq!(
                signature(&actual),
                expected,
                "completion cut={cut} source={source:?}"
            );
            count += 1;
        }
    }
    println!("runtime-work kind=prefix cases={count}");
}

#[test]
fn single_scalar_damage_and_repair_match_fresh_parses() {
    let mut count = 0;
    for source in SOURCES {
        let initial = valid(source);
        for (start, ch) in source.char_indices() {
            for replacement in ["", "\n", "/", "*", "'", "}", "$", "λ", "\0"] {
                let old_end = start + ch.len_utf8();
                let mut changed = source.to_string();
                changed.replace_range(start..old_end, replacement);
                let mut old = initial.clone();
                edit(
                    &mut old,
                    source,
                    &changed,
                    start,
                    old_end,
                    start + replacement.len(),
                );
                let mut p = parser();
                let mut actual = checked(&mut p, changed.as_bytes(), Some(&old));
                let fresh = checked(&mut parser(), changed.as_bytes(), None);
                assert_eq!(
                    signature(&actual),
                    signature(&fresh),
                    "mutation start={start} replacement={replacement:?} source={source:?}"
                );
                edit(
                    &mut actual,
                    &changed,
                    source,
                    start,
                    start + replacement.len(),
                    old_end,
                );
                let repaired = checked(&mut p, source.as_bytes(), Some(&actual));
                assert_eq!(
                    signature(&repaired),
                    signature(&initial),
                    "repair start={start} replacement={replacement:?} source={source:?}"
                );
                count += 1;
            }
        }
    }
    println!("runtime-work kind=mutation cases={count}");
}

#[test]
fn invalid_utf8_and_nul_inputs_do_not_poison_reused_parser() {
    let expected_source = "def λ = / text /\nfinish(λ)\n";
    let expected = signature(&valid(expected_source));
    let mut p = parser();
    let mut count = 0;
    for byte in 0_u8..=255 {
        for prefix in [
            b"def ".as_slice(),
            b"/*",
            b"def text = /",
            b"def text = \"${",
        ] {
            let mut source = prefix.to_vec();
            source.extend([byte, b'\n', b'/', b'*', 0]);
            checked(&mut p, &source, None);
            p.reset();
            assert_eq!(
                signature(&checked(&mut p, expected_source.as_bytes(), None)),
                expected
            );
            count += 1;
        }
    }
    println!("runtime-work kind=bytes cases={count}");
}

#[test]
fn cancelled_parse_resumes_and_reset_accepts_a_different_document() {
    let source = "def λ = / ${-> render()} /\n".repeat(4096);
    let expected = signature(&valid(&source));
    for resume in [true, false] {
        let mut p = parser();
        let mut calls = 0;
        let mut cancel = |_: &tree_sitter::ParseState| {
            calls += 1;
            ControlFlow::Break(())
        };
        let result = p.parse_with_options(
            &mut |offset, _| &source.as_bytes()[offset..],
            None,
            Some(ParseOptions::new().progress_callback(&mut cancel)),
        );
        assert!(result.is_none() && calls > 0);
        if resume {
            assert_eq!(
                signature(&checked(&mut p, source.as_bytes(), None)),
                expected
            );
        } else {
            p.reset();
            let other = "def fresh = 1\nfinish(fresh)\n";
            assert_eq!(
                signature(&checked(&mut p, other.as_bytes(), None)),
                signature(&valid(other))
            );
        }
    }
    println!("runtime-work kind=cancellation cases=2");
}

#[test]
fn independent_parser_threads_and_cloned_trees_keep_state_isolated() {
    let expected: Vec<_> = SOURCES.iter().map(|s| signature(&valid(s))).collect();
    std::thread::scope(|scope| {
        let mut handles = vec![];
        for lane in 0..4 {
            let expected = &expected;
            handles.push(scope.spawn(move || {
                let mut p = parser();
                for iteration in 0..32 {
                    let index = (iteration + lane) % SOURCES.len();
                    let source = SOURCES[index];
                    let tree = checked(&mut p, source.as_bytes(), None);
                    let cloned = tree.clone();
                    drop(tree);
                    assert_eq!(signature(&cloned), expected[index]);
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
    });
    println!("runtime-work kind=threads cases=128");
}
