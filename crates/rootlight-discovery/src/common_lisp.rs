//! Bounded, written Common Lisp header evidence for conservative source routing.
//! Only the opening form is inspected; package, macro and reader bindings are
//! deliberately not inferred from a distinctive physical operator spelling.

#[derive(Clone, Copy)]
struct Cursor<'a> {
    rest: &'a str,
}

impl<'a> Cursor<'a> {
    fn skip_trivia(&mut self) -> Option<()> {
        loop {
            self.rest = self
                .rest
                .trim_start_matches(|character: char| character.is_ascii_whitespace());
            if let Some(comment) = self.rest.strip_prefix(';') {
                self.rest = comment
                    .find(['\r', '\n'])
                    .and_then(|at| comment.get(at..))
                    .unwrap_or("");
            } else if let Some(comment) = self.rest.strip_prefix("#|") {
                let mut rest = comment;
                let mut depth = 1_usize;
                while depth != 0 {
                    if let Some(after) = rest.strip_prefix("#|") {
                        depth = depth.checked_add(1)?;
                        rest = after;
                    } else if let Some(after) = rest.strip_prefix("|#") {
                        depth = depth.checked_sub(1)?;
                        rest = after;
                    } else {
                        rest = rest.get(rest.chars().next()?.len_utf8()..)?;
                    }
                }
                self.rest = rest;
            } else {
                return Some(());
            }
        }
    }

    fn symbol(&mut self) -> Option<&'a str> {
        self.skip_trivia()?;
        let end = self
            .rest
            .char_indices()
            .find_map(|(at, character)| {
                (character.is_ascii_whitespace()
                    || matches!(
                        character,
                        '(' | ')' | '"' | '\'' | '`' | ',' | ';' | '#' | '|' | '\\'
                    ))
                .then_some(at)
            })
            .unwrap_or(self.rest.len());
        let symbol = self.rest.get(..end)?;
        if symbol.is_empty() || symbol.chars().any(char::is_control) {
            return None;
        }
        self.rest = self.rest.get(end..)?;
        Some(symbol)
    }

    fn designator(&mut self) -> Option<()> {
        self.skip_trivia()?;
        if let Some(string) = self.rest.strip_prefix('"') {
            let mut escaped = false;
            for (at, character) in string.char_indices() {
                if character == '\0' {
                    return None;
                }
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    self.rest = string.get(at + character.len_utf8()..)?;
                    return Some(());
                }
            }
            return None;
        }
        if let Some(rest) = self.rest.strip_prefix("#:") {
            if !rest.chars().next().is_some_and(|character| {
                !character.is_ascii_whitespace() && !matches!(character, ';' | '#')
            }) {
                return None;
            }
            self.rest = rest;
            let symbol = self.symbol()?;
            return plain_symbol(symbol).then_some(());
        }
        let symbol = self.symbol()?;
        static_symbol(symbol, true).then_some(())
    }

    fn function_name(&mut self) -> Option<()> {
        self.skip_trivia()?;
        if let Some(rest) = self.rest.strip_prefix('(') {
            self.rest = rest;
            let operator = self.symbol()?;
            let operator = common_lisp_operator(operator).unwrap_or(operator);
            if !operator.eq_ignore_ascii_case("setf") || !static_symbol(self.symbol()?, false) {
                return None;
            }
            self.skip_trivia()?;
            self.rest = self.rest.strip_prefix(')')?;
            Some(())
        } else {
            static_symbol(self.symbol()?, false).then_some(())
        }
    }

    fn next_character(&mut self) -> Option<char> {
        self.skip_trivia()?;
        self.rest.chars().next()
    }
}

fn plain_symbol(symbol: &str) -> bool {
    let Some(first) = symbol.chars().next() else {
        return false;
    };
    if first.is_ascii_digit() || symbol == "." || symbol.contains(':') {
        return false;
    }
    // A numeric-looking token is not evidence of a written symbol name. Escaped
    // names are left to the real reader rather than interpreted by discovery.
    !matches!(first, '+' | '-' | '.')
        || !symbol
            .get(first.len_utf8()..)
            .and_then(|rest| rest.chars().next())
            .is_some_and(|character| character.is_ascii_digit() || character == '.')
}

fn static_symbol(symbol: &str, keyword: bool) -> bool {
    if let Some(rest) = symbol.strip_prefix(':') {
        return keyword && plain_symbol(rest);
    }
    if let Some((package, name)) = symbol.split_once(':') {
        return plain_symbol(package) && plain_symbol(name.strip_prefix(':').unwrap_or(name));
    }
    plain_symbol(symbol)
}

fn common_lisp_operator(operator: &str) -> Option<&str> {
    let (package, name) = operator.split_once(':')?;
    (package.eq_ignore_ascii_case("cl") || package.eq_ignore_ascii_case("common-lisp"))
        .then_some(name.strip_prefix(':').unwrap_or(name))
}

fn header(mut cursor: Cursor<'_>) -> Option<()> {
    cursor.skip_trivia()?;
    cursor.rest = cursor.rest.strip_prefix('(')?;
    let operator = cursor.symbol()?;
    let explicit = common_lisp_operator(operator);
    let name = explicit.unwrap_or(operator);
    if name.eq_ignore_ascii_case("in-package") {
        cursor.designator()?;
        return (cursor.next_character()? == ')').then_some(());
    }
    if name.eq_ignore_ascii_case("defpackage") {
        cursor.designator()?;
        return matches!(cursor.next_character()?, '(' | ')').then_some(());
    }
    if name.eq_ignore_ascii_case("defclass") {
        if !static_symbol(cursor.symbol()?, false) {
            return None;
        }
        return (cursor.next_character()? == '(').then_some(());
    }
    if name.eq_ignore_ascii_case("defgeneric")
        || explicit.is_some() && name.eq_ignore_ascii_case("defun")
    {
        cursor.function_name()?;
        return (cursor.next_character()? == '(').then_some(());
    }
    if explicit.is_some()
        && ["defmacro", "deftype"]
            .iter()
            .any(|kind| name.eq_ignore_ascii_case(kind))
    {
        if !static_symbol(cursor.symbol()?, false) {
            return None;
        }
        return (cursor.next_character()? == '(').then_some(());
    }
    if name.eq_ignore_ascii_case("defmethod") {
        cursor.function_name()?;
        loop {
            if cursor.next_character()? == '(' {
                return Some(());
            }
            if !static_symbol(cursor.symbol()?, true) {
                return None;
            }
        }
    }
    if explicit.is_some()
        && ["defvar", "defparameter", "defconstant"]
            .iter()
            .any(|kind| name.eq_ignore_ascii_case(kind))
    {
        if !static_symbol(cursor.symbol()?, false) {
            return None;
        }
        let next = cursor.next_character()?;
        return (name.eq_ignore_ascii_case("defvar") && next == ')'
            || next.is_alphanumeric()
            || matches!(next, '(' | '"' | '+' | '-' | '*' | ':'))
        .then_some(());
    }
    let (package, name) = operator.split_once(':')?;
    if !package.eq_ignore_ascii_case("asdf")
        || !name
            .strip_prefix(':')
            .unwrap_or(name)
            .eq_ignore_ascii_case("defsystem")
    {
        return None;
    }
    cursor.designator()?;
    if cursor.next_character()? == ')' {
        return Some(());
    }
    let option = cursor.symbol()?;
    (option.starts_with(':') && static_symbol(option, true) && !cursor.rest.is_empty())
        .then_some(())
}

/// Reports one distinctive physical opening header within the discovery sample.
///
/// Incomplete terminal UTF-8 scalars are ignored only for truncated samples;
/// malformed UTF-8, reader prefixes and incomplete header boundaries reject the hint.
pub(super) fn has_content_hint(source: &[u8], prefix_truncated: bool) -> bool {
    let source = match std::str::from_utf8(source) {
        Ok(source) => source,
        Err(error) if prefix_truncated && error.error_len().is_none() => {
            let Some(prefix) = source
                .get(..error.valid_up_to())
                .and_then(|prefix| std::str::from_utf8(prefix).ok())
            else {
                return false;
            };
            prefix
        }
        Err(_) => return false,
    };
    header(Cursor {
        rest: source.strip_prefix('\u{feff}').unwrap_or(source),
    })
    .is_some()
}

/// Limits content-based routing to audited, ambiguous or extensionless paths.
pub(super) fn is_content_candidate(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_none_or(|extension| {
            extension.to_str().is_some_and(|extension| {
                ["lisp", "asd", "cl", "lsp"]
                    .iter()
                    .any(|suffix| extension.eq_ignore_ascii_case(suffix))
            })
        })
}

#[cfg(test)]
mod tests {
    use super::{has_content_hint, is_content_candidate};

    #[test]
    fn distinctive_headers_use_whole_operators_and_static_names() {
        for source in [
            "(defpackage sample)",
            "(defpackage \"sample\\\"quoted\" (:use :cl))",
            "(in-package sample)",
            "(in-package \"sample\")",
            "(cl:in-package :sample)",
            "(COMMON-LISP::DEFCLASS widget ())",
            "(defclass λ ())",
            "(defgeneric (setf measure) (value))",
            "(defmethod measure around :before (value) value)",
            "(defmethod (CL:SETF sample:measure) (value) value)",
            "(cl:defun (setf measure) (value) value)",
            "(cl:deftype sample () '(integer 0 5))",
            "(cl:defvar *state*)",
            "(cl:defparameter *state* 4)",
            "(COMMON-LISP:DEFCONSTANT +limit+ 8)",
            "(asdf:defsystem :sample)",
            "(asdf:defsystem sample :components ())",
            "(defpackage#| operator boundary |#:sample)",
        ] {
            assert!(has_content_hint(source.as_bytes(), false), "{source}");
        }
    }

    #[test]
    fn reader_contexts_and_later_forms_do_not_supply_opening_evidence() {
        for source in [
            "'(defpackage :sample)",
            "`(defpackage :sample)",
            ",@(defpackage :sample)",
            "#'(defgeneric sample ())",
            "#.(defpackage :sample)",
            "#+feature (defpackage :sample)",
            "#-feature (defpackage :sample)",
            "#;(defpackage :sample) (in-package :sample)",
            "\"(defpackage :sample)\"",
            "(quote (defpackage :sample))",
            "(progn (defpackage :sample))",
            "(print :sample) (defpackage :sample)",
            "(defun sample (value) value)",
            "(defmacro sample (value) value)",
            "(pkg:defclass widget ())",
            "(pkg:defpackage :sample)",
            "(defsystem :sample)",
            "(asdf-user:defsystem :sample)",
            "(|defpackage| :sample)",
            "(def\\package :sample)",
        ] {
            assert!(!has_content_hint(source.as_bytes(), false), "{source}");
        }
    }

    #[test]
    fn comments_require_complete_nested_boundaries() {
        for source in [
            "; (defpackage :phantom)\r\n(defclass written ())",
            "#| (defpackage :phantom) #| nested |# tail |# (in-package :written)",
            "\u{feff}#| #| #| inside |# |# |# (defgeneric written ())",
            "(defpackage #| #| nested |# tail |# :written)",
        ] {
            assert!(has_content_hint(source.as_bytes(), false), "{source}");
        }
        for source in [
            "; (defpackage :phantom)",
            "#| (defpackage :phantom)",
            "#| outer #| inner |# (defpackage :phantom)",
            "#| complete |# |# (defpackage :phantom)",
            "(defpackage #| outer #| nested |# :phantom)",
        ] {
            assert!(!has_content_hint(source.as_bytes(), false), "{source}");
        }
    }

    #[test]
    fn malformed_names_and_header_boundaries_are_not_evidence() {
        for source in [
            "(defpackag :sample)",
            "(defpackage-extra :sample)",
            "(defpackage:sample)",
            "(defpackage)",
            "(defpackage :)",
            "(defpackage 123)",
            "(defpackage .9)",
            "(defpackage \"unclosed)",
            "(defpackage #: sample)",
            "(defpackage #:#| comment |#sample)",
            "(defpackage :sample unexpected)",
            "(in-package :sample extra)",
            "(in-package :sample",
            "(in-package 'sample)",
            "(defclass :keyword ())",
            "(defclass 23 ())",
            "(defclass +23 ())",
            "(defclass pkg:::name ())",
            "(defclass \"name\" ())",
            "(defclass written",
            "(defclass written wrong)",
            "(defgeneric sample)",
            "(defgeneric (setf) ())",
            "(defgeneric (setf sample extra) ())",
            "(defmethod sample :before)",
            "(defmethod sample 42 ())",
            "(cl:defmacro (setf sample) ())",
            "(cl:deftype (setf sample) ())",
            "(cl:defparameter *state*)",
            "(cl:defconstant *state* #.(evaluate))",
            "(asdf:defsystem :sample unexpected)",
            "(asdf:defsystem :sample :)",
            "(asdf:defsystem :sample :components",
            "(asdf:defsystem #.(compute-name))",
        ] {
            assert!(!has_content_hint(source.as_bytes(), false), "{source}");
        }
    }

    #[test]
    fn path_candidates_do_not_widen_into_other_lisp_dialects() {
        for path in [
            "file.lisp",
            "file.ASD",
            "file.CL",
            "file.LSP",
            "entry",
            "src/entry",
        ] {
            assert!(is_content_candidate(path), "{path}");
        }
        for path in [
            "file.scm",
            "file.ss",
            "file.sld",
            "file.el",
            "file.cl.bak",
            "src/lisp.rs",
        ] {
            assert!(!is_content_candidate(path), "{path}");
        }
    }
}
