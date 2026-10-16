//! Bounded HDL code evidence for the `.v` suffix shared with V and Coq.
//! Comments, strings and ordinary V module directives cannot establish HDL routing.

#[derive(Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Name(&'a [u8]),
    Punctuation(u8),
    HdlDefine,
}

struct Tokens<'a> {
    source: &'a [u8],
}

fn sized_literal_length(source: &[u8]) -> Option<usize> {
    if !source.first()?.is_ascii_digit() {
        return None;
    }
    let size = source
        .iter()
        .take_while(|byte| byte.is_ascii_digit() || **byte == b'_')
        .count();
    let value = source.get(size..)?.trim_ascii_start();
    let length = based_literal_length(value)?;
    Some(source.len() - value.len() + length)
}

fn based_literal_length(source: &[u8]) -> Option<usize> {
    let mut base = source.strip_prefix(b"'")?;
    if matches!(base.first(), Some(b's' | b'S')) {
        base = base.get(1..)?;
    }
    let (&radix, value) = base.split_first()?;
    let radix = radix.to_ascii_lowercase();
    if !matches!(radix, b'b' | b'o' | b'd' | b'h') {
        return None;
    }
    let value = value.trim_ascii_start();
    let unknown = |byte: u8| matches!(byte, b'x' | b'X' | b'z' | b'Z' | b'?');
    let digit = |byte: u8| match radix {
        b'b' => matches!(byte, b'0' | b'1') || unknown(byte),
        b'o' => matches!(byte, b'0'..=b'7') || unknown(byte),
        b'd' => byte.is_ascii_digit() || unknown(byte),
        b'h' => byte.is_ascii_hexdigit() || unknown(byte),
        _ => false,
    };
    let first = *value.first()?;
    if !digit(first) {
        return None;
    }
    let length = if radix == b'd' && unknown(first) {
        1 + value[1..].iter().take_while(|byte| **byte == b'_').count()
    } else {
        value
            .iter()
            .take_while(|byte| {
                **byte == b'_'
                    || if radix == b'd' {
                        byte.is_ascii_digit()
                    } else {
                        digit(**byte)
                    }
            })
            .count()
    };
    let remaining = value.get(length..)?;
    if remaining
        .first()
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'\''))
    {
        return None;
    }
    Some(source.len() - remaining.len())
}

fn macro_default_length(source: &[u8]) -> Option<usize> {
    let mut depth = [0usize; 3];
    let mut quoted = false;
    let mut escaped = false;
    for (index, &byte) in source.iter().enumerate() {
        if byte == b'\n' {
            return None;
        }
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b',' | b')' if depth == [0; 3] => return Some(index),
            b'(' => depth[0] += 1,
            b'[' => depth[1] += 1,
            b'{' => depth[2] += 1,
            b')' => depth[0] = depth[0].checked_sub(1)?,
            b']' => depth[1] = depth[1].checked_sub(1)?,
            b'}' => depth[2] = depth[2].checked_sub(1)?,
            _ => {}
        }
    }
    None
}

fn macro_formals_length(source: &[u8]) -> Option<usize> {
    let source = source.split(|byte| *byte == b'\n').next()?;
    let mut rest = source.strip_prefix(b"(")?.trim_ascii_start();
    if rest.starts_with(b")") {
        return Some(source.len() - rest.len() + 1);
    }
    loop {
        if !rest
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        {
            return None;
        }
        let name = rest
            .iter()
            .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
            .count();
        rest = rest.get(name..)?.trim_ascii_start();
        if let Some(default) = rest.strip_prefix(b"=") {
            rest = default.get(macro_default_length(default)?..)?;
        }
        let (&separator, after) = rest.split_first()?;
        match separator {
            b')' => {
                let length = source.len() - after.len();
                return Some(length);
            }
            b',' => rest = after.trim_ascii_start(),
            _ => return None,
        }
    }
}

fn numeric_define(source: &[u8]) -> bool {
    let Some(after_directive) = source.strip_prefix(b"`define") else {
        return false;
    };
    let horizontal = |byte: u8| matches!(byte, b' ' | b'\t' | b'\r');
    let name = after_directive.trim_ascii_start();
    if !after_directive
        .first()
        .is_some_and(|byte| horizontal(*byte))
        || after_directive[..after_directive.len() - name.len()].contains(&b'\n')
        || !name
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
    {
        return false;
    }
    let length = name
        .iter()
        .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
        .count();
    let mut after_name = &name[length..];
    let has_formals = after_name.starts_with(b"(");
    if has_formals {
        let Some(length) = macro_formals_length(after_name) else {
            return false;
        };
        after_name = &after_name[length..];
    }
    let value = after_name.trim_ascii_start();
    if (!has_formals && !after_name.first().is_some_and(|byte| horizontal(*byte)))
        || after_name[..after_name.len() - value.len()].contains(&b'\n')
    {
        return false;
    }
    let value = value
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    if based_literal_length(value).is_some() || sized_literal_length(value).is_some() {
        return true;
    }
    matches!(
        value.get(..2),
        Some(b"'0" | b"'1" | b"'x" | b"'X" | b"'z" | b"'Z")
    ) && !value
        .get(2)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'\''))
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            self.source = self.source.trim_ascii_start();
            let (&first, rest) = self.source.split_first()?;
            if let Some(comment) = self.source.strip_prefix(b"//") {
                self.source = comment
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .and_then(|end| comment.get(end + 1..))
                    .unwrap_or_default();
            } else if self.source.starts_with(b"/*") || self.source.starts_with(b"(*") {
                let (open, close) = if self.source.starts_with(b"/*") {
                    (b"/*", b"*/")
                } else {
                    (b"(*", b"*)")
                };
                self.source = self.source.get(2..).unwrap_or_default();
                let mut depth = 1usize;
                while depth != 0 && !self.source.is_empty() {
                    if self.source.starts_with(open) {
                        depth += 1;
                        self.source = self.source.get(2..).unwrap_or_default();
                    } else if self.source.starts_with(close) {
                        depth -= 1;
                        self.source = self.source.get(2..).unwrap_or_default();
                    } else {
                        self.source = self.source.get(1..).unwrap_or_default();
                    }
                }
            } else if numeric_define(self.source) {
                // A real macro directive distinguishes unsized literals from V strings.
                self.source = rest;
                return Some(Token::HdlDefine);
            } else if let Some(length) = sized_literal_length(self.source) {
                // The numeric size disambiguates an HDL apostrophe from a V string.
                self.source = self.source.get(length..).unwrap_or_default();
            } else if matches!(first, b'"' | b'\'') {
                self.source = rest;
                while let Some((&byte, rest)) = self.source.split_first() {
                    self.source = rest;
                    if byte == first {
                        break;
                    }
                    if byte == b'\\' {
                        self.source = self.source.get(1..).unwrap_or_default();
                    }
                }
            } else if first.is_ascii_alphabetic() || matches!(first, b'_' | b'\\') {
                let length = if first == b'\\' {
                    self.source
                        .iter()
                        .position(u8::is_ascii_whitespace)
                        .unwrap_or(self.source.len())
                } else {
                    self.source
                        .iter()
                        .take_while(|byte| {
                            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
                        })
                        .count()
                };
                let (name, rest) = self.source.split_at(length);
                self.source = rest;
                return Some(Token::Name(name));
            } else {
                self.source = rest;
                return Some(Token::Punctuation(first));
            }
        }
    }
}

pub(super) fn has_content_hint(source: &[u8]) -> bool {
    let mut tokens = Tokens { source };
    while let Some(token) = tokens.next() {
        if token == Token::HdlDefine {
            return true;
        }
        if !matches!(token, Token::Name(b"module" | b"macromodule")) {
            continue;
        }
        let mut name = tokens.next();
        if matches!(name, Some(Token::Name(b"automatic" | b"static"))) {
            name = tokens.next();
        }
        if !matches!(name, Some(Token::Name(_))) {
            continue;
        }
        match tokens.next() {
            Some(Token::Punctuation(b'(')) => return true,
            Some(Token::Punctuation(b'#')) if tokens.next() == Some(Token::Punctuation(b'(')) => {
                return true;
            }
            Some(Token::Punctuation(b';')) => {
                // A semicolon alone cannot distinguish a foreign module directive.
                while let Some(token) = tokens.next() {
                    if matches!(token, Token::Name(b"endmodule") | Token::HdlDefine) {
                        return true;
                    }
                    if matches!(
                        token,
                        Token::Name(b"parameter" | b"localparam" | b"reg" | b"wire" | b"logic")
                    ) {
                        let mut name = tokens.next();
                        if matches!(name, Some(Token::Name(b"integer" | b"real" | b"time"))) {
                            name = tokens.next();
                        }
                        if matches!(name, Some(Token::Name(_)))
                            && tokens.next() == Some(Token::Punctuation(b'='))
                        {
                            return true;
                        }
                    }
                }
                return false;
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdl_headers_require_a_port_parameter_or_closed_empty_module() {
        for source in [
            "module written(input wire clk); endmodule",
            "module written; endmodule",
            "module automatic written # (parameter N=4)(); endmodule",
            "/* declaration */ module \\written.name (input wire clk); endmodule",
            "`timescale 1ns/1ps\nmodule written(input wire clk); endmodule",
            "module broken(input wire clk;",
            "module bench; parameter WIDTH = 8;",
            "module bench; localparam integer DEPTH = 16;",
            "module bench; reg clk = 0;",
        ] {
            assert!(has_content_hint(source.as_bytes()), "{source}");
        }
    }

    #[test]
    fn v_coq_and_inert_hdl_names_do_not_select_verilog() {
        for source in [
            "module main\nfn main() {}",
            "module main\n#include <stdio.h>\nfn main() {}",
            "module main\n#flag -I include\n",
            "module main; fn main() {}",
            "Module Written. Definition value := 1. End Written.",
            "(* module hidden(); endmodule *) Definition value := 1.",
            "(* outer (* inner *) module hidden(); endmodule *)",
            "/* outer /* inner */ module hidden(); endmodule */",
            "// module hidden(); endmodule\nmodule main\n",
            "/* module hidden(); endmodule */",
            "message := 'module hidden(); endmodule'",
            "message := \"module hidden(); endmodule\"",
            "module main #include <stdio.h>\n",
            "module main #flag -I lib\n",
        ] {
            assert!(!has_content_hint(source.as_bytes()), "{source}");
        }
    }

    #[test]
    fn sized_hdl_literals_before_headers_do_not_start_v_strings() {
        for literal in [
            "1'b1",
            "8'hff",
            "32'shDEAD_BEEF",
            "8 'o7",
            "12'D23",
            "4'bx_z?1",
        ] {
            let source =
                format!("`define ENABLED {literal}\nmodule written(input wire clk); endmodule");
            assert!(has_content_hint(source.as_bytes()), "{source}");
        }
        for source in [
            "message := 'b1 module hidden(); endmodule'",
            "message := 'hff\nmodule hidden(); endmodule'",
            "message := '1\\'b1 module hidden(); endmodule'",
            "message := \"1'b1 module hidden(); endmodule\"",
        ] {
            assert!(!has_content_hint(source.as_bytes()), "{source}");
        }
    }

    #[test]
    fn hdl_define_numeric_bodies_disambiguate_unsized_apostrophes() {
        for literal in ["'1", "'0", "'x", "'z", "'hFF", "'d1", "'sb10", "'o7"] {
            for suffix in ["", "\nmodule written(input wire clk); endmodule"] {
                let source = format!("`define ENABLED {literal}{suffix}");
                assert!(has_content_hint(source.as_bytes()), "{source}");
            }
        }
        for source in [
            "message := \"`define ENABLED '1\\nmodule hidden(); endmodule\"",
            "message := '`define ENABLED \\'hFF module hidden(); endmodule'",
            "/* `define ENABLED '1 module hidden(); endmodule */",
            "(* `define ENABLED '1 module hidden(); endmodule *)",
            "`define ENABLED 'h",
            "`define ENABLED 'b2",
            "`define ENABLED '1suffix",
            "`define ENABLED\n'1",
            "`define ENABLED 'h\nFF",
            "`defined ENABLED '1",
        ] {
            assert!(!has_content_hint(source.as_bytes()), "{source}");
        }
    }

    #[test]
    fn hdl_numeric_macros_preserve_written_formal_argument_boundaries() {
        for parameters in [
            "()",
            "(x)",
            "(x, y)",
            "(x = 1, y)",
            "(x = call(1, 2))",
            "(x = \"),\")",
            "(x = {1, 2})",
        ] {
            let source =
                format!("`define MASK{parameters} 'b1\nmodule written(input wire clk); endmodule");
            assert!(has_content_hint(source.as_bytes()), "{source}");
            let prefix = format!("module bench;\n`define MASK{parameters}'b1");
            assert!(has_content_hint(prefix.as_bytes()), "{prefix}");
        }
        for source in [
            "`define MASK(x 'b1",
            "`define MASK(1) 'b1",
            "`define MASK(x,) 'b1",
            "`define MASK(\n) 'b1",
            "`define MASK(x = call(1) 'b1",
            "`define MASK(x = \"open) 'b1",
            "message := \"`define MASK(x) 'b1 module hidden(); endmodule\"",
        ] {
            assert!(!has_content_hint(source.as_bytes()), "{source}");
        }
    }
}
