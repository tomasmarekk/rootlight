//! Written V evidence for suffixes shared with Verilog, Coq and GLSL.
//! The caller supplies a bounded prefix; comments and quoted data cannot supply
//! declaration tokens, and incomplete prefixes cannot finish a module directive.

#[derive(Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Name(&'a [u8]),
    Punctuation(u8),
    Newline,
    Literal,
}

struct Tokens<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (&byte, after) = self.rest.split_first()?;
            if byte == b'\n' {
                self.rest = after;
                return Some(Token::Newline);
            }
            if byte.is_ascii_whitespace() {
                self.rest = after;
                continue;
            }
            if self.rest.starts_with(b"//") || self.rest.starts_with(b"#!") {
                let length = self
                    .rest
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .unwrap_or(self.rest.len());
                self.rest = &self.rest[length..];
                continue;
            }
            let delimiters = if self.rest.starts_with(b"/*") {
                Some((b"/*", b"*/"))
            } else if self.rest.starts_with(b"(*") {
                Some((b"(*", b"*)"))
            } else {
                None
            };
            if let Some((open, close)) = delimiters {
                let mut depth = 1usize;
                self.rest = &self.rest[2..];
                while !self.rest.is_empty() && depth != 0 {
                    if self.rest.starts_with(open) {
                        depth += 1;
                        self.rest = &self.rest[2..];
                    } else if self.rest.starts_with(close) {
                        depth -= 1;
                        self.rest = &self.rest[2..];
                    } else {
                        self.rest = &self.rest[1..];
                    }
                }
                continue;
            }
            if matches!(byte, b'\'' | b'"' | b'`') {
                self.rest = after;
                while let Some((&next, after)) = self.rest.split_first() {
                    self.rest = after;
                    if next == b'\\' {
                        self.rest = self.rest.get(1..).unwrap_or_default();
                    } else if next == byte {
                        break;
                    }
                }
                return Some(Token::Literal);
            }
            if byte.is_ascii_alphabetic() || byte == b'_' {
                let length = self
                    .rest
                    .iter()
                    .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'_')
                    .count();
                let (name, after) = self.rest.split_at(length);
                self.rest = after;
                return Some(Token::Name(name));
            }
            self.rest = after;
            return Some(Token::Punctuation(byte));
        }
    }
}

pub(super) fn has_content_hint(source: &[u8], prefix_truncated: bool) -> bool {
    source_hint(source, prefix_truncated, false)
}

pub(super) fn has_script_hint(source: &[u8], prefix_truncated: bool) -> bool {
    source_hint(source, prefix_truncated, true)
}

fn source_hint(source: &[u8], prefix_truncated: bool, shared_shader: bool) -> bool {
    let tokens: Vec<_> = Tokens { rest: source }.collect();
    if shared_shader && written_shader(&tokens) {
        return false;
    }
    if tokens.windows(2).enumerate().any(|(index, pair)| {
        (index == 0 || matches!(tokens[index - 1], Token::Newline | Token::Punctuation(b'.')))
            && matches!(
                pair,
                [
                    Token::Name(
                        b"From"
                            | b"Require"
                            | b"Definition"
                            | b"Inductive"
                            | b"CoInductive"
                            | b"Lemma"
                            | b"Theorem"
                            | b"Corollary"
                            | b"Proposition"
                            | b"Fixpoint"
                            | b"CoFixpoint"
                            | b"Record"
                            | b"Section"
                            | b"Ltac"
                            | b"Notation"
                    ),
                    Token::Name(_) | Token::Literal
                ]
            )
    }) {
        // Coq record fields and proof-local bindings also use :=. Written Coq
        // commands fence those overlapping tokens from V script admission.
        return false;
    }
    for (index, token) in tokens.iter().enumerate() {
        let after = &tokens[index + 1..];
        match token {
            Token::Name(b"fn") => {
                if matches!(after, [Token::Name(_), Token::Punctuation(b'(' | b'['), ..]) {
                    return true;
                }
                if let [Token::Punctuation(b'('), tail @ ..] = after {
                    let mut depth = 1usize;
                    for (end, token) in tail.iter().enumerate() {
                        match token {
                            Token::Punctuation(b'(') => depth += 1,
                            Token::Punctuation(b')') => depth -= 1,
                            _ => {}
                        }
                        if depth == 0 {
                            if matches!(
                                &tail[end + 1..],
                                [Token::Name(_), Token::Punctuation(b'(' | b'['), ..]
                            ) {
                                return true;
                            }
                            break;
                        }
                    }
                }
            }
            Token::Name(b"struct" | b"enum" | b"interface")
                if !shared_shader || *token != Token::Name(b"struct") =>
            {
                if matches!(after, [Token::Name(_), Token::Punctuation(b'{' | b'['), ..]) {
                    return true;
                }
                if *token == Token::Name(b"enum")
                    && matches!(
                        after,
                        [
                            Token::Name(_),
                            Token::Name(b"as"),
                            Token::Name(_),
                            Token::Punctuation(b'{'),
                            ..
                        ]
                    )
                {
                    return true;
                }
            }
            Token::Name(b"type" | b"const") => {
                if matches!(after, [Token::Name(_), Token::Punctuation(b'='), ..]) {
                    return true;
                }
            }
            Token::Name(b"module" | b"import") => {
                let mut tail = after;
                if !matches!(tail.first(), Some(Token::Name(_))) {
                    continue;
                }
                tail = &tail[1..];
                while let [Token::Punctuation(b'.'), Token::Name(_), rest @ ..] = tail {
                    tail = rest;
                }
                if matches!(tail.first(), Some(Token::Newline))
                    || (tail.is_empty() && !prefix_truncated)
                {
                    return true;
                }
            }
            Token::Name(b"println" | b"eprintln" | b"print" | b"eprint") => {
                if matches!(after.first(), Some(Token::Punctuation(b'('))) {
                    return true;
                }
            }
            Token::Name(_)
                if matches!(
                    after,
                    [Token::Punctuation(b':'), Token::Punctuation(b'='), ..]
                ) && (index == 0
                    || matches!(
                        tokens[index - 1],
                        Token::Newline | Token::Punctuation(b'{') | Token::Name(b"mut")
                    )) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

fn written_shader(tokens: &[Token<'_>]) -> bool {
    let mut braces = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        match token {
            Token::Punctuation(b'{') => braces += 1,
            Token::Punctuation(b'}') => braces = braces.saturating_sub(1),
            _ => {}
        }
        let tail = &tokens[index..];
        if matches!(
            tail,
            [Token::Punctuation(b'#'), Token::Name(b"version"), ..]
        ) {
            return true;
        }
        if braces == 0
            && let [
                Token::Name(return_type),
                Token::Name(_),
                Token::Punctuation(b'('),
                ..,
            ] = tail
            && !matches!(
                *return_type,
                b"fn"
                    | b"pub"
                    | b"unsafe"
                    | b"return"
                    | b"assert"
                    | b"defer"
                    | b"go"
                    | b"spawn"
                    | b"if"
                    | b"for"
                    | b"match"
                    | b"else"
                    | b"or"
                    | b"is"
                    | b"in"
                    | b"mut"
            )
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_declarations_and_scripts_supply_v_evidence() {
        for source in [
            "module main\n",
            "import net.http\n",
            "pub fn written[T](value T) T { return value }",
            "fn (box &Box) written() {}",
            "struct Box { value int }",
            "enum Mode { first }",
            "interface Reader { read() int }",
            "enum Section as u8 { first }",
            "struct Record { id int }\nfn run() { select from Record where id == 1 }",
            "type Number = int",
            "const answer = 42",
            "value := 'authored'\n",
            "mut value := 1\n",
            "println_value := 1;",
            "println('authored')\n",
            "/* outer /* nested */ comment */ fn written() {}",
            "#!/usr/bin/env v\nprintln(args.len)\n",
        ] {
            assert!(has_content_hint(source.as_bytes(), false), "{source}");
        }
    }

    #[test]
    fn shader_structures_and_typed_functions_do_not_override_glsl() {
        for source in [
            "struct Material { float value; };",
            "float println(float value) { return value; } void main() { println(1.0); }",
            "struct Color { float value; }; Color print(Color value) { return value; }",
            "#version 450\nvoid main() {}",
        ] {
            assert!(!has_script_hint(source.as_bytes(), false), "{source}");
        }
        assert!(has_script_hint(b"println(args.len > 0)\n", false));
        assert!(has_script_hint(
            b"struct Record { callback fn (int) }\nfn main() {}",
            false
        ));
        assert!(has_script_hint(
            b"fn (box &Box) read() int { return 1 }",
            false
        ));
    }

    #[test]
    fn inert_data_and_foreign_declarations_do_not_supply_v_evidence() {
        for source in [
            "",
            "// fn fake() {}\n",
            "/* outer /* fn fake() {} */ comment */",
            "(* outer (* fn fake() {} *) comment *)",
            "'fn fake() {}'",
            "\"struct Fake {}\"",
            "module written(input wire a); endmodule",
            "module written; endmodule",
            "Definition written := 1.",
            "Record Box := {\nvalue := 1\n}.",
            "Require Import Init.\nvalue := 1\n",
            "Module Written. End Written.",
            "#version 450\nvoid main() { gl_Position = vec4(0.0); }",
        ] {
            assert!(!has_content_hint(source.as_bytes(), false), "{source}");
        }
        assert!(!has_content_hint(b"module main", true));
        assert!(has_content_hint(b"module main\n", true));
    }
}
