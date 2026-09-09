//! Source-boundary qualification of a locally patched Astro grammar candidate.
//! Native host ranges must preserve complete expressions before injection.

#[cfg(test)]
mod tests {
    // SAFETY: These signatures match the candidate scanner's C exports.
    unsafe extern "C" {
        fn tree_sitter_astro_external_scanner_create() -> *mut std::ffi::c_void;
        fn tree_sitter_astro_external_scanner_destroy(payload: *mut std::ffi::c_void);
        fn tree_sitter_astro_external_scanner_deserialize(
            payload: *mut std::ffi::c_void,
            buffer: *const std::ffi::c_char,
            length: u32,
        );
        fn tree_sitter_astro_external_scanner_serialize(
            payload: *mut std::ffi::c_void,
            buffer: *mut std::ffi::c_char,
        ) -> u32;
    }

    #[test]
    fn scanner_restore_does_not_read_past_advertised_frame() {
        let _: tree_sitter::Language = tree_sitter_astro_next::LANGUAGE.into();
        // The allocation is padded to make this a deterministic contract probe,
        // not a process crash: bytes beyond the advertised prefix are inaccessible input.
        let mut input = [0_u8; 1024];
        input[2..4].copy_from_slice(&1_u16.to_ne_bytes());
        let mut before = [0_u8; 1024];
        let mut after = [0_u8; 1024];
        // SAFETY: The constructor creates one owned scanner; both output buffers
        // meet the 1024-byte ABI size, and even the candidate's over-read stays
        // inside our initialized allocation. The scanner is destroyed before assertions.
        let (before_len, after_len) = unsafe {
            let scanner = tree_sitter_astro_external_scanner_create();
            assert!(!scanner.is_null());
            let before_len =
                tree_sitter_astro_external_scanner_serialize(scanner, before.as_mut_ptr().cast());
            tree_sitter_astro_external_scanner_deserialize(scanner, input.as_ptr().cast(), 1);
            let after_len =
                tree_sitter_astro_external_scanner_serialize(scanner, after.as_mut_ptr().cast());
            tree_sitter_astro_external_scanner_destroy(scanner);
            (before_len, after_len)
        };
        assert_eq!(
            after_len, before_len,
            "a one-byte truncated frame must reset to empty"
        );
        assert_eq!(before, after);
    }

    fn token(source: &str, kind: &str, expected: &str) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_astro_next::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let mut nodes = vec![root];
        let mut captured = Vec::new();
        while let Some(node) = nodes.pop() {
            if node.kind() == kind {
                captured.push(node.utf8_text(source.as_bytes()).unwrap());
            }
            let mut cursor = node.walk();
            nodes.extend(node.children(&mut cursor));
        }
        assert!(
            !root.has_error(),
            "source {source:?}; tree {}",
            root.to_sexp()
        );
        assert!(
            captured.contains(&expected),
            "expected {expected:?}; actual {captured:?}; tree {}",
            root.to_sexp()
        );
    }

    #[test]
    fn frontmatter_retains_typescript_source() {
        token(
            "---\nconst title: string = 'Guide';\n---\n<h1>{title}</h1>",
            "frontmatter_js_block",
            "\nconst title: string = 'Guide';",
        );
    }

    #[test]
    fn attributes_retain_object_expression() {
        token(
            "<Card data={{enabled: true}} />",
            "attribute_js_expr",
            "{enabled: true}",
        );
    }

    #[test]
    fn attributes_retain_regex_braces() {
        token(
            "<Card value={/}/.test(text)} />",
            "attribute_js_expr",
            "/}/.test(text)",
        );
    }

    #[test]
    fn attributes_retain_regex_comment_markers() {
        token(
            "<Card value={/[/*}]/.test(text)} />",
            "attribute_js_expr",
            "/[/*}]/.test(text)",
        );
    }

    #[test]
    fn template_strings_retain_escaped_backticks() {
        token(
            "<Card value={`escaped \\` tail`} />",
            "attribute_js_expr",
            "`escaped \\` tail`",
        );
    }

    #[test]
    fn nested_templates_retain_closing_delimiters() {
        token(
            "<Card value={`outer ${`inner ${value}`} tail`} />",
            "attribute_js_expr",
            "`outer ${`inner ${value}`} tail`",
        );
    }

    #[test]
    fn script_prefix_is_not_a_closing_tag() {
        token(
            "<script>const value = '</scripted>';\nfinish();</script>",
            "raw_text",
            "const value = '</scripted>';\nfinish();",
        );
    }

    #[test]
    fn style_prefix_is_not_a_closing_tag() {
        token(
            "<style>.item::after { content: '</stylex>'; }</style>",
            "raw_text",
            ".item::after { content: '</stylex>'; }",
        );
    }
}
