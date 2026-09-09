//! Retains the pinned upstream Astro grammar and query contract tests.
//! These syntax checks complement the source-boundary and scanner-state regressions.

use tree_sitter_astro_next::{HIGHLIGHTS_QUERY, INJECTIONS_QUERY, LANGUAGE};

#[cfg(test)]
mod tests {
    #[test]
    fn test_can_load_grammar() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&super::LANGUAGE.into())
            .expect("Error loading Astro parser");
    }

    #[test]
    fn test_can_parse_astro() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&super::LANGUAGE.into())
            .expect("Error loading Astro parser");

        let code = r#"
---
import Layout from '../layouts/Layout.astro';
const title = "Hello";
const items = [1, 2, 3];
---

<Layout title={title}>
    <h1 class="heading">{title}</h1>

    <ul>
        {items.map((item) => (
            <li>{item}</li>
        ))}
    </ul>

    <script>
        console.log('Hello from Astro!');
    </script>

    <style>
        .heading {
            color: red;
            font-weight: bold;
        }
    </style>
</Layout>
"#;

        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();
        assert!(
            !root.has_error(),
            "Parse tree has errors: {}",
            root.to_sexp()
        );
    }

    #[test]
    fn test_highlights_query_is_valid() {
        let language: tree_sitter::Language = super::LANGUAGE.into();
        tree_sitter::Query::new(&language, super::HIGHLIGHTS_QUERY)
            .expect("HIGHLIGHTS_QUERY should be a valid query for the Astro grammar");
    }

    #[test]
    fn test_highlights_query_matches_html_nodes() {
        use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

        let language: tree_sitter::Language = super::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();

        let code = r#"<h1 class="title">Hello</h1>"#;
        let tree = parser.parse(code, None).unwrap();

        let query = Query::new(&language, super::HIGHLIGHTS_QUERY).unwrap();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), code.as_bytes());

        let mut capture_names: Vec<String> = vec![];
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let name = query.capture_names()[cap.index as usize].to_string();
                if !capture_names.contains(&name) {
                    capture_names.push(name);
                }
            }
        }

        assert!(
            capture_names.contains(&"tag".to_string()),
            "should match tag_name as @tag"
        );
        assert!(
            capture_names.contains(&"property".to_string()),
            "should match attribute_name as @property"
        );
        assert!(
            capture_names.contains(&"string".to_string()),
            "should match attribute_value as @string"
        );
    }

    #[test]
    fn test_injections_query_is_valid() {
        let language: tree_sitter::Language = super::LANGUAGE.into();
        tree_sitter::Query::new(&language, super::INJECTIONS_QUERY)
            .expect("INJECTIONS_QUERY should be a valid query for the Astro grammar");
    }

    #[test]
    fn test_injections_query_matches_script_and_style() {
        use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

        let language: tree_sitter::Language = super::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();

        let code = r#"
<script>
    let x: number = 1;
</script>
<style>
    h1 { color: red; }
</style>
"#;
        let tree = parser.parse(code, None).unwrap();
        let query = Query::new(&language, super::INJECTIONS_QUERY).unwrap();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), code.as_bytes());

        let mut matched_languages: Vec<String> = vec![];
        while let Some(m) = matches.next() {
            for prop in query.property_settings(m.pattern_index) {
                if prop.key.as_ref() == "injection.language"
                    && let Some(val) = &prop.value
                {
                    matched_languages.push(val.to_string());
                }
            }
        }

        assert!(
            matched_languages.contains(&"typescript".to_string()),
            "should inject TypeScript for script content"
        );
        assert!(
            matched_languages.contains(&"css".to_string()),
            "should inject CSS for style content"
        );
    }

    #[test]
    fn test_html_interpolation_structure() {
        use tree_sitter::Parser;

        let language: tree_sitter::Language = super::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();

        let code = r#"{isProduction ? 'Production' : 'Development'}"#;
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        println!("AST for interpolation: {}", root.to_sexp());

        assert!(!root.has_error(), "Parse tree should not have errors");
    }

    #[test]
    fn test_injections_query_matches_html_interpolations() {
        use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

        let language: tree_sitter::Language = super::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();

        let code = r#"<p>{isProduction ? 'Production' : 'Development'}</p>"#;
        let tree = parser.parse(code, None).unwrap();

        let query = Query::new(&language, super::INJECTIONS_QUERY).unwrap();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), code.as_bytes());

        let mut found_typescript_injection = false;
        while let Some(m) = matches.next() {
            for prop in query.property_settings(m.pattern_index) {
                if prop.key.as_ref() == "injection.language"
                    && let Some(val) = &prop.value
                    && val.as_ref() == "typescript"
                {
                    found_typescript_injection = true;
                }
            }
        }

        assert!(
            found_typescript_injection,
            "should inject TypeScript for HTML interpolation content"
        );
    }
}
