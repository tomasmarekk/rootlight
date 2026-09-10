//! Admission tests for the published Perl grammar against Rootlight's runtime.
//! Authored Perl is parsed only, including heredoc bodies that resemble code.

#[cfg(test)]
mod tests {
    use std::{
        ffi::{c_char, c_uint, c_void},
        ptr::NonNull,
    };
    use tree_sitter::{InputEdit, Node, Parser, Point, Tree};

    fn parser() -> Parser {
        let mut parser = Parser::new();
        let language = ts_parser_perl::LANGUAGE.into();
        parser.set_language(&language).unwrap();
        assert_eq!(language.abi_version(), 15);
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
        let prefix = &source[..offset];
        Point::new(
            prefix.bytes().filter(|b| *b == b'\n').count(),
            prefix
                .rfind('\n')
                .map_or(prefix.len(), |last| prefix.len() - last - 1),
        )
    }

    fn signature(tree: &Tree) -> Vec<(String, std::ops::Range<usize>, bool)> {
        nodes(tree.root_node())
            .iter()
            .map(|n| (n.kind().to_owned(), n.byte_range(), n.is_missing()))
            .collect()
    }

    fn valid(source: &str) -> Tree {
        let tree = parser().parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        for node in nodes(tree.root_node()) {
            assert!(source.get(node.byte_range()).is_some());
            assert_eq!(node.start_position(), point(source, node.start_byte()));
            assert_eq!(node.end_position(), point(source, node.end_byte()));
        }
        tree
    }

    fn definitions<'a>(tree: &Tree, source: &'a str) -> Vec<&'a str> {
        nodes(tree.root_node())
            .iter()
            .filter(|n| n.kind() == "subroutine_declaration_statement")
            .map(|n| {
                n.child_by_field_name("name")
                    .unwrap()
                    .utf8_text(source.as_bytes())
                    .unwrap()
            })
            .collect()
    }

    const SOURCE: &str = "package Measure;\nuse strict;\nuse warnings;\nsub adjust ($value) { return $value + 1; }\nmy $text = qq{λ😀};\nmy @items = (1, 2);\nmy %items = (first => 1);\nmy $answer = adjust($items[0]);\n";

    #[test]
    fn declarations_and_utf8_ranges_are_source_exact() {
        for newline in ["\n", "\r\n"] {
            let source = SOURCE.replace('\n', newline);
            let tree = valid(&source);
            assert_eq!(definitions(&tree, &source), ["adjust"]);
            assert_eq!(tree.root_node().end_byte(), source.len());
        }
    }

    #[test]
    fn comments_quotes_pod_and_heredocs_do_not_invent_subroutines() {
        let source = "# sub hidden {}\nmy $text = q{sub fake {}};\n=pod\nsub phantom {}\n=cut\nmy $body = <<'END';\nsub hidden_again {}\nEND\nsub actual { 1 }\n";
        let tree = valid(source);
        assert_eq!(definitions(&tree, source), ["actual"]);
    }

    #[test]
    fn heredoc_delimiters_with_equal_prefix_and_length_are_distinct() {
        let source =
            "my $body = <<'ABCDEFGHA';\nABCDEFGHB\nsub phantom {}\nABCDEFGHA\nsub actual { 1 }\n";
        let tree = valid(source);
        assert_eq!(definitions(&tree, source), ["actual"]);
        let ends: Vec<_> = nodes(tree.root_node())
            .iter()
            .filter(|n| n.kind() == "heredoc_end")
            .map(|n| n.utf8_text(source.as_bytes()).unwrap())
            .collect();
        assert_eq!(ends, ["ABCDEFGHA"]);
    }

    #[test]
    fn incremental_edits_match_fresh_trees() {
        for (before, after) in [("λ😀", "δ🌍"), ("+ 1", "+ 23"), ("adjust", "transform")] {
            let start = SOURCE.find(before).unwrap();
            let mut changed = SOURCE.to_owned();
            changed.replace_range(start..start + before.len(), after);
            let mut parser = parser();
            let mut old = parser.parse(SOURCE, None).unwrap();
            old.edit(&InputEdit {
                start_byte: start,
                old_end_byte: start + before.len(),
                new_end_byte: start + after.len(),
                start_position: point(SOURCE, start),
                old_end_position: point(SOURCE, start + before.len()),
                new_end_position: point(&changed, start + after.len()),
            });
            let incremental = parser.parse(&changed, Some(&old)).unwrap();
            assert_eq!(signature(&incremental), signature(&valid(&changed)));
        }
    }

    #[test]
    fn malformed_input_remains_incomplete_and_parser_reusable() {
        let expected = valid(SOURCE);
        let mut parser = parser();
        for source in ["sub broken {", "my $x = ;", "my $x = q{unterminated"] {
            let tree = parser.parse(source, None).unwrap();
            assert!(
                tree.root_node().has_error(),
                "{source}: {}",
                tree.root_node().to_sexp()
            );
            parser.reset();
            assert_eq!(
                signature(&parser.parse(SOURCE, None).unwrap()),
                signature(&expected)
            );
        }
    }

    #[test]
    fn exact_delimiters_survive_utf8_crlf_and_incremental_edits() {
        for prefix in [
            "A".repeat(8),
            "A".repeat(255),
            "A".repeat(1015),
            "λ😀".repeat(40),
        ] {
            for newline in ["\n", "\r\n"] {
                let delimiter = format!("{prefix}A");
                let impostor = format!("{prefix}B");
                let source = format!("my $body = <<'{delimiter}';\n{impostor}\nsub phantom {{}}\n{delimiter}\nsub actual {{ 1 }}\n").replace('\n', newline);
                let tree = valid(&source);
                assert_eq!(definitions(&tree, &source), ["actual"]);
                let start = source.find("phantom").unwrap();
                let mut changed = source.clone();
                changed.replace_range(start..start + 7, "not_code");
                let mut reused = parser();
                let mut old = reused.parse(&source, None).unwrap();
                old.edit(&InputEdit {
                    start_byte: start,
                    old_end_byte: start + 7,
                    new_end_byte: start + 8,
                    start_position: point(&source, start),
                    old_end_position: point(&source, start + 7),
                    new_end_position: point(&changed, start + 8),
                });
                let incremental = reused.parse(&changed, Some(&old)).unwrap();
                assert_eq!(signature(&incremental), signature(&valid(&changed)));
                assert_eq!(definitions(&incremental, &changed), ["actual"]);
            }
        }
    }

    #[test]
    fn queued_heredocs_preserve_separate_exact_delimiters() {
        for count in [1, 2, 8] {
            let delimiters: Vec<_> = (0..count).map(|i| format!("ABCDEFGH{i}")).collect();
            let heads = delimiters
                .iter()
                .map(|d| format!("<<'{d}'"))
                .collect::<Vec<_>>()
                .join(", ");
            let bodies = delimiters
                .iter()
                .map(|d| format!("sub phantom {{}}\n{d}\n"))
                .collect::<String>();
            let source = format!("my @bodies = ({heads});\n{bodies}sub actual {{ 1 }}\n");
            let tree = valid(&source);
            assert_eq!(definitions(&tree, &source), ["actual"]);
            assert_eq!(
                nodes(tree.root_node())
                    .iter()
                    .filter(|n| n.kind() == "heredoc_end")
                    .count(),
                count
            );
        }
    }

    #[test]
    fn unrepresentable_delimiters_are_errors_not_silent_approximations() {
        for size in [1017, 1024, 1025, 4096] {
            let delimiter = "A".repeat(size);
            let source =
                format!("my $body = <<'{delimiter}';\ntext\n{delimiter}\nsub actual {{ 1 }}\n");
            let tree = parser().parse(&source, None).unwrap();
            assert!(
                tree.root_node().has_error(),
                "silently accepted {size} delimiter bytes"
            );
        }
    }

    #[test]
    fn full_heredoc_queue_is_not_silently_overwritten() {
        let delimiters: Vec<_> = (0..9).map(|i| format!("END{i}")).collect();
        let heads = delimiters
            .iter()
            .map(|d| format!("<<'{d}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let bodies = delimiters
            .iter()
            .map(|d| format!("text\n{d}\n"))
            .collect::<String>();
        let source = format!("my @bodies = ({heads});\n{bodies}sub actual {{ 1 }}\n");
        let tree = parser().parse(&source, None).unwrap();
        assert!(
            tree.root_node().has_error(),
            "silently accepted an overwritten queue"
        );
    }

    #[test]
    fn quote_nesting_fails_closed_when_state_cannot_round_trip() {
        for depth in [8, 32, 64] {
            let source = format!(
                "my $text = {}1{};\nsub actual {{ 1 }}\n",
                "qq{${\\ ".repeat(depth),
                "}}".repeat(depth)
            );
            let tree = parser().parse(&source, None).unwrap();
            if depth <= 32 {
                assert!(
                    !tree.root_node().has_error(),
                    "depth {depth}: {}",
                    tree.root_node().to_sexp()
                );
                assert_eq!(definitions(&tree, &source), ["actual"]);
            } else {
                assert!(
                    tree.root_node().has_error(),
                    "silently accepted a truncated quote stack"
                );
            }
        }
    }

    #[test]
    fn included_ranges_keep_host_coordinates() {
        let prefix = "# Host λ😀\r\n```perl\r\n";
        let source = format!("{prefix}{SOURCE}\n```\n");
        let mut parser = parser();
        let start = prefix.len();
        let end = start + SOURCE.len();
        parser
            .set_included_ranges(&[tree_sitter::Range {
                start_byte: start,
                end_byte: end,
                start_point: point(&source, start),
                end_point: point(&source, end),
            }])
            .unwrap();
        let tree = parser.parse(&source, None).unwrap();
        assert!(!tree.root_node().has_error());
        assert_eq!(definitions(&tree, &source), ["adjust"]);
        for node in nodes(tree.root_node()) {
            assert!(node.start_byte() >= start && node.end_byte() <= end);
            assert_eq!(node.start_position(), point(&source, node.start_byte()));
            assert_eq!(node.end_position(), point(&source, node.end_byte()));
        }
    }

    #[test]
    fn cancellation_and_reset_preserve_fresh_parse() {
        let source = "my $value = qq{text};\n".repeat(10_000);
        let mut parser = parser();
        let mut called = false;
        let mut cancel = |_: &tree_sitter::ParseState| {
            called = true;
            std::ops::ControlFlow::Break(())
        };
        let result = parser.parse_with_options(
            &mut |offset, _| source.as_bytes().get(offset..).unwrap_or_default(),
            None,
            Some(tree_sitter::ParseOptions::new().progress_callback(&mut cancel)),
        );
        assert!(called && result.is_none());
        parser.reset();
        assert_eq!(
            signature(&parser.parse(SOURCE, None).unwrap()),
            signature(&valid(SOURCE))
        );
    }

    // SAFETY: These declarations match the published scanner's C entry points.
    unsafe extern "C" {
        fn tree_sitter_perl_external_scanner_create() -> *mut c_void;
        fn tree_sitter_perl_external_scanner_destroy(payload: *mut c_void);
        fn tree_sitter_perl_external_scanner_serialize(
            payload: *mut c_void,
            buffer: *mut c_char,
        ) -> c_uint;
        fn tree_sitter_perl_external_scanner_deserialize(
            payload: *mut c_void,
            buffer: *const c_char,
            length: c_uint,
        );
    }

    struct Scanner(NonNull<c_void>);
    impl Scanner {
        fn new() -> Self {
            let _ = parser();
            // SAFETY: This owner receives a fresh allocation and destroys it once.
            Self(NonNull::new(unsafe { tree_sitter_perl_external_scanner_create() }).unwrap())
        }
        fn restore(&mut self, declared: usize, recovery: u8) {
            // Padding keeps the original defective length handling inside readable
            // storage, so this negative control does not depend on undefined reads.
            let mut frame = [0_u8; 1024];
            frame[3] = recovery;
            // SAFETY: Live scanner and readable backing frame for the entire native call.
            unsafe {
                tree_sitter_perl_external_scanner_deserialize(
                    self.0.as_ptr(),
                    frame.as_ptr().cast(),
                    c_uint::try_from(declared).unwrap(),
                )
            };
        }
        fn restore_frame(&mut self, bytes: &[u8]) {
            assert!(bytes.len() <= 1024);
            let mut padded = [0_u8; 1024];
            padded[..bytes.len()].copy_from_slice(bytes);
            // SAFETY: Live scanner and initialized readable backing bytes cover this call.
            unsafe {
                tree_sitter_perl_external_scanner_deserialize(
                    self.0.as_ptr(),
                    padded.as_ptr().cast(),
                    c_uint::try_from(bytes.len()).unwrap(),
                )
            };
        }
        fn bytes(&mut self) -> Vec<u8> {
            let mut frame = [0xa5_u8; 1026];
            // SAFETY: Live scanner receives the required writable 1024-byte buffer.
            let len = unsafe {
                tree_sitter_perl_external_scanner_serialize(
                    self.0.as_ptr(),
                    frame[1..1025].as_mut_ptr().cast(),
                )
            };
            let len = usize::try_from(len).unwrap();
            assert!(len <= 1024);
            assert_eq!(frame[0], 0xa5);
            assert!(frame[len + 1..].iter().all(|b| *b == 0xa5));
            frame[1..len + 1].to_vec()
        }
    }
    impl Drop for Scanner {
        fn drop(&mut self) {
            // SAFETY: Unique live allocation is freed through its original allocator.
            unsafe { tree_sitter_perl_external_scanner_destroy(self.0.as_ptr()) };
        }
    }

    #[test]
    fn empty_reset_clears_previous_recovery_state() {
        let mut scanner = Scanner::new();
        assert_eq!(scanner.bytes(), [0; 4]);
        scanner.restore(4, 1);
        assert_eq!(scanner.bytes(), [0, 0, 0, 1]);
        scanner.restore(0, 0);
        assert_eq!(scanner.bytes(), [0; 4]);
    }

    #[test]
    fn truncated_frames_do_not_consume_undeclared_bytes() {
        let mut scanner = Scanner::new();
        for len in 1..4 {
            scanner.restore(len, 1);
            assert_eq!(scanner.bytes(), [0; 4], "declared frame length {len}");
        }
    }

    #[test]
    fn complete_delimiter_frames_round_trip_and_truncations_reset() {
        let mut scanner = Scanner::new();
        for delimiter in ["", "END", "λ😀", &"X".repeat(1016)] {
            let bytes = delimiter.as_bytes();
            let len = u16::try_from(bytes.len()).unwrap().to_le_bytes();
            let mut frame = vec![0, 1, 1, 1, 0, len[0], len[1]];
            frame.extend_from_slice(bytes);
            frame.push(1);
            scanner.restore_frame(&frame);
            assert_eq!(scanner.bytes(), frame);
            assert_eq!(scanner.bytes(), frame);
            for end in 0..frame.len() {
                scanner.restore_frame(&frame);
                scanner.restore_frame(&frame[..end]);
                assert_eq!(scanner.bytes(), [0; 4], "truncated at {end}");
            }
        }
    }

    #[test]
    fn malformed_frames_reset_without_touching_buffer_guards() {
        let mut scanner = Scanner::new();
        for byte in 0..=255 {
            for len in [1, 2, 3, 5, 32, 255, 1024] {
                scanner.restore(4, 1);
                scanner.restore_frame(&vec![byte; len]);
                assert_eq!(scanner.bytes(), [0; 4], "byte {byte} length {len}");
            }
        }
        for frame in [
            vec![0, 5, 0, 0],
            vec![0, 0, 1, 0, 0, 0, 0, 0],
            vec![0, 1, 0, 0],
            vec![0, 0, 0, 2],
            vec![0, 1, 1, 2, 0, 0, 0, 0],
        ] {
            scanner.restore_frame(&frame);
            assert_eq!(scanner.bytes(), [0; 4]);
        }
    }
}
