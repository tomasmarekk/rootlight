//! Prevent authored query captures from exceeding the pinned native step array.
//! This source check is test-only: pattern quantifiers include captures that the
//! native compiler silently omits, so the closed capture-name roster is insufficient.

use super::*;

const MAX_STEP_CAPTURES: usize = 3;

#[derive(Clone, Copy, Debug)]
struct Token<'a> {
    text: &'a str,
    offset: usize,
}

fn query_tokens(source: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut offset = 0;
    while offset < source.len() {
        let start = offset;
        let character = source[offset..].chars().next().unwrap();
        if character.is_whitespace() {
            offset += character.len_utf8();
            continue;
        }
        match character {
            ';' => {
                offset = source[offset..]
                    .find('\n')
                    .map_or(source.len(), |end| offset + end + 1);
                continue;
            }
            '"' => {
                offset += 1;
                loop {
                    let byte = *source.as_bytes().get(offset).expect("closed query string");
                    offset += 1;
                    if byte == b'"' {
                        break;
                    }
                    if byte == b'\\' {
                        assert!(offset < source.len(), "complete query string escape");
                        offset += 1;
                    }
                }
            }
            '(' | ')' | '[' | ']' | ':' | '?' | '+' | '*' | '!' => offset += 1,
            '@' => {
                offset += 1;
                while let Some(character) = source[offset..].chars().next() {
                    if !character.is_alphanumeric() && !matches!(character, '_' | '-' | '.') {
                        break;
                    }
                    offset += character.len_utf8();
                }
                assert!(offset > start + 1, "capture has a written name");
            }
            _ => {
                while let Some(character) = source[offset..].chars().next() {
                    if character.is_whitespace() || "()[]:;\"@?+*!".contains(character) {
                        break;
                    }
                    offset += character.len_utf8();
                }
                assert!(offset > start, "query token advances");
            }
        }
        tokens.push(Token {
            text: &source[start..offset],
            offset: start,
        });
    }
    tokens
}

#[derive(Debug, PartialEq, Eq)]
struct StepCaptures {
    offset: usize,
    count: usize,
}

struct StepWalk<'a> {
    tokens: Vec<Token<'a>>,
    next: usize,
    steps: Vec<StepCaptures>,
}

impl<'a> StepWalk<'a> {
    fn peek(&self) -> Option<&'a str> {
        self.tokens.get(self.next).map(|token| token.text)
    }

    fn consume(&mut self, expected: &str) {
        assert_eq!(self.peek(), Some(expected), "valid native query structure");
        self.next += 1;
    }

    fn step(&mut self, offset: usize) -> Vec<usize> {
        let id = self.steps.len();
        self.steps.push(StepCaptures { offset, count: 0 });
        vec![id]
    }

    fn sequence(&mut self, end: &str, alternatives: bool) -> Vec<usize> {
        let mut targets = Vec::new();
        while self.peek() != Some(end) {
            match self.peek().expect("closed native query structure") {
                "." => self.next += 1,
                "!" => {
                    self.next += 1;
                    assert!(self.peek().is_some(), "negated field name");
                    self.next += 1;
                }
                _ => {
                    let child = self.pattern();
                    if alternatives || targets.is_empty() {
                        targets.extend(child);
                    }
                }
            }
        }
        self.consume(end);
        targets
    }

    fn pattern(&mut self) -> Vec<usize> {
        let token = self.tokens.get(self.next).copied().expect("query pattern");
        self.next += 1;
        let targets = match token.text {
            "[" => self.sequence("]", true),
            "(" => match self.peek().expect("query node or group") {
                name if name.starts_with('#') || name.starts_with('.') => {
                    let mut depth = 1;
                    while depth != 0 {
                        match self.peek().expect("closed query predicate") {
                            "(" => depth += 1,
                            ")" => depth -= 1,
                            _ => {}
                        }
                        self.next += 1;
                    }
                    return Vec::new();
                }
                "(" | "[" => self.sequence(")", false),
                name if name.starts_with('"') => self.sequence(")", false),
                _ => {
                    let name = self.peek().unwrap();
                    self.next += 1;
                    if (name == "MISSING" && self.peek() != Some(")")) || name.ends_with('/') {
                        self.next += 1;
                    }
                    let targets = self.step(token.offset);
                    self.sequence(")", false);
                    targets
                }
            },
            "_" => self.step(token.offset),
            text if text.starts_with('"') => self.step(token.offset),
            _ => {
                self.consume(":");
                self.pattern()
            }
        };
        // Native suffixes attach to the first step of a group, or every first
        // branch step of an alternation; neither creates another capture slot.
        while let Some(suffix) = self.peek() {
            if suffix.starts_with('@') {
                assert!(!targets.is_empty(), "capture has a native target step");
                for &id in &targets {
                    self.steps[id].count += 1;
                }
            } else if !matches!(suffix, "?" | "+" | "*") {
                break;
            }
            self.next += 1;
        }
        targets
    }
}

fn overflowing_steps(source: &str) -> Vec<StepCaptures> {
    let mut walk = StepWalk {
        tokens: query_tokens(source),
        next: 0,
        steps: Vec::new(),
    };
    while walk.peek().is_some() {
        walk.pattern();
    }
    walk.steps
        .into_iter()
        .filter(|step| step.count > MAX_STEP_CAPTURES)
        .collect()
}

#[test]
fn source_step_limit_matches_the_pinned_native_query_compiler() {
    let native = include_str!("../../../../source-inputs/cargo/tree-sitter-0.26.11/src/query.c");
    assert!(
        native
            .lines()
            .any(|line| line.trim() == "#define MAX_STEP_CAPTURE_COUNT 3")
    );
    assert_eq!(MAX_STEP_CAPTURES, 3);
}

#[test]
fn fourth_capture_is_rejected_even_when_its_name_is_repeated() {
    for source in [
        "(identifier) @a @b @c @d",
        "(identifier) @same @same @same @same",
        "(identifier)@a@b?@c+@d",
    ] {
        assert_eq!(
            overflowing_steps(source),
            [StepCaptures {
                offset: 0,
                count: 4
            }]
        );
    }
    assert!(overflowing_steps("(identifier) @a @b @c").is_empty());
    assert!(overflowing_steps("(identifier) @a @b @c\n(identifier) @d").is_empty());
}

#[test]
fn groups_alternations_and_fields_preserve_the_same_native_step_target() {
    for source in [
        "((identifier) @a @b @c (#eq? @a \"x\")) @d",
        "[(identifier) @a @b @c (string_literal)] @d",
        "(function_item name: (identifier) @a @b @c @d)",
        "((identifier) @a @b @c) @d",
    ] {
        let overflow = overflowing_steps(source);
        assert_eq!(overflow.len(), 1, "{source}");
        assert_eq!(overflow[0].count, 4, "{source}");
    }
    for source in [
        "((identifier) @a @b (identifier) @c @d) @first",
        "(function_item name: (identifier) @a @b body: (block) @c @d) @owner",
        "[(identifier) @a @b (string_literal) @c @d] @branch",
        "(MISSING identifier) @a @b @c",
        "(MISSING \";\") @a @b @c",
        "\"(\" @a @b @c",
        "_ @a @b @c",
    ] {
        assert!(overflowing_steps(source).is_empty(), "{source}");
    }
}

#[test]
fn comments_strings_and_predicate_capture_references_do_not_add_attachments() {
    let source = r#"
; @decoy @decoy @decoy @decoy (ignored)
((identifier) @a @b @c
 (#match? @a "@x @y @z @w \"quoted\" ; [(")
 (#eq? @a @b)
 (.eq? @a @b)
 (#set! "note" "@x @y @z @w"))
(identifier) @a ; a comment does not reset an attachment target
 @b @c
"#;
    assert!(overflowing_steps(source).is_empty());
    assert_eq!(
        overflowing_steps("(identifier) @a @b ; comment\n @c @d")[0].count,
        4
    );
}

#[test]
fn native_pattern_quantifiers_do_not_reveal_the_fourth_same_step_capture() {
    let language = language_for(GrammarFamily::Rust);
    let three = Query::new(&language, "(identifier) @same @same @same").unwrap();
    let four = Query::new(&language, "(identifier) @same @same @same @same").unwrap();
    assert_eq!(three.capture_names(), four.capture_names());
    assert_eq!(three.capture_quantifiers(0), four.capture_quantifiers(0));
    assert!(overflowing_steps("(identifier) @same @same @same").is_empty());
    assert_eq!(
        overflowing_steps("(identifier) @same @same @same @same").len(),
        1
    );
}

#[test]
fn every_builtin_native_query_step_fits_the_native_capture_array() {
    let registry = QueryPackRegistry::catalogue();
    let mut violations = Vec::new();
    for entry in registry
        .packs
        .iter()
        .map(|(_, entry)| entry)
        .chain(std::iter::once(&registry.typescript_tsx))
    {
        for step in overflowing_steps(entry.source) {
            let line = entry.source[..step.offset]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1;
            violations.push(format!(
                "{:?}/{:?}:{line}: {} capture attachments",
                entry.family, entry.native, step.count
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "native capture array overflow:\n{}",
        violations.join("\n")
    );
}
