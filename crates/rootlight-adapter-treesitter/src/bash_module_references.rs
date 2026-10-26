//! Bounded current-tree proof for companion shell variable References.
//! Producer and consumer invoke this policy on separate native trees and work
//! counters; no producer label, cached target or shell execution is authority.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::{Node, TreeCursor};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Binding {
    pub(crate) assignment: (usize, usize),
    pub(crate) definition: (usize, usize),
}

/// One decreasing budget covers every walk and context lookup of its owner.
pub(crate) struct Work {
    remaining: usize,
    exhausted: bool,
}

impl Work {
    pub(crate) const fn new(maximum: usize) -> Self {
        Self {
            remaining: maximum,
            exhausted: false,
        }
    }

    pub(crate) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    pub(crate) fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        match self.remaining.checked_sub(1) {
            Some(remaining) => {
                self.remaining = remaining;
                Ok(true)
            }
            None => {
                self.exhausted = true;
                Ok(false)
            }
        }
    }
}

fn raw<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a [u8]> {
    source.get(node.byte_range())
}

fn identifier(value: &[u8]) -> bool {
    value.len() <= 128
        && value.split_first().is_some_and(|(first, rest)| {
            (first.is_ascii_alphabetic() || *first == b'_')
                && rest
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        })
}

pub(crate) fn simple(node: Node<'_>, source: &[u8]) -> bool {
    if node.kind() != "variable_name" || !raw(node, source).is_some_and(identifier) {
        return false;
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.named_child_count() != 1 || parent.named_child(0) != Some(node) {
        return false;
    }
    let Some(value) = raw(parent, source) else {
        return false;
    };
    match parent.kind() {
        "simple_expansion" => value.len() == node.byte_range().len() + 1 && value.starts_with(b"$"),
        "expansion" => {
            value.len() == node.byte_range().len() + 3
                && value.starts_with(b"${")
                && value.ends_with(b"}")
        }
        _ => false,
    }
}

fn next(
    cursor: &mut TreeCursor<'_>,
    descend: bool,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    if !work.step(cancel)? {
        return Ok(false);
    }
    if descend && cursor.goto_first_child() {
        return Ok(true);
    }
    loop {
        if !work.step(cancel)? {
            return Ok(false);
        }
        if cursor.goto_next_sibling() {
            return Ok(true);
        }
        if !cursor.goto_parent() {
            return Ok(false);
        }
    }
}

fn field(
    parent: Node<'_>,
    child: Node<'_>,
    name: &str,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    for index in 0..parent.child_count() {
        if !work.step(cancel)? {
            return Ok(false);
        }
        let Ok(index) = u32::try_from(index) else {
            return Ok(false);
        };
        if parent.child(index) == Some(child) {
            return Ok(parent.field_name_for_child(index) == Some(name));
        }
    }
    Ok(false)
}

fn literal_operand(
    root: Node<'_>,
    source: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut cursor = root.walk();
    loop {
        if !work.step(cancel)? {
            return Ok(false);
        }
        let node = cursor.node();
        if node.is_named()
            && !matches!(
                node.kind(),
                "word"
                    | "number"
                    | "string"
                    | "raw_string"
                    | "string_content"
                    | "concatenation"
                    | "simple_expansion"
                    | "expansion"
                    | "variable_name"
                    | "special_variable_name"
            )
        {
            return Ok(false);
        }
        if node.kind() == "expansion"
            && !node.named_child(0).is_some_and(|name| simple(name, source))
        {
            return Ok(false);
        }
        if node.kind() == "word"
            && !raw(node, source).is_some_and(|value| {
                value.len() <= 256 && !value.iter().any(|byte| b"$`\\".contains(byte))
            })
        {
            return Ok(false);
        }
        if !next(&mut cursor, true, work, cancel)? {
            break;
        }
        if !root.byte_range().contains(&cursor.node().start_byte()) {
            return Ok(false);
        }
    }
    Ok(!work.exhausted())
}

fn decimal_operand(node: Node<'_>, source: &[u8]) -> bool {
    raw(node, source).is_some_and(|value| {
        (node.kind() == "number"
            && !value.is_empty()
            && value.len() <= 32
            && value.iter().all(u8::is_ascii_digit))
            || (node.kind() == "simple_expansion"
                && value == b"$#"
                && node.named_child_count() == 1
                && node
                    .named_child(0)
                    .is_some_and(|name| name.kind() == "special_variable_name"))
    })
}

fn test_ancestor<'tree>(
    mut node: Node<'tree>,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    while let Some(parent) = node.parent() {
        if !work.step(cancel)? {
            return Ok(None);
        }
        match parent.kind() {
            "test_command" => return Ok(Some(parent)),
            "binary_expression" | "unary_expression" | "string" | "concatenation" => node = parent,
            _ => return Ok(None),
        }
    }
    Ok(None)
}

fn readonly_test(
    test: Node<'_>,
    source: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let Some(value) = raw(test, source) else {
        return Ok(false);
    };
    let double = value.starts_with(b"[[") && value.ends_with(b"]]");
    if test.kind() != "test_command"
        || !(double || value.starts_with(b"[") && value.ends_with(b"]"))
    {
        return Ok(false);
    }
    let mut cursor = test.walk();
    loop {
        if !work.step(cancel)? {
            return Ok(false);
        }
        let node = cursor.node();
        if node.kind() == "binary_expression" {
            let (Some(left), Some(right), Some(operator)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
                node.child_by_field_name("operator")
                    .and_then(|operator| raw(operator, source)),
            ) else {
                return Ok(false);
            };
            let accepted = match operator {
                b"=" | b"==" | b"!=" => {
                    literal_operand(left, source, work, cancel)?
                        && literal_operand(right, source, work, cancel)?
                }
                b"=~" if double && right.kind() == "regex" => {
                    let Some(regex) = raw(right, source) else {
                        return Ok(false);
                    };
                    regex.len() <= 1024
                        && regex.iter().enumerate().all(|(index, byte)| {
                            *byte != b'`' && (*byte != b'$' || index + 1 == regex.len())
                        })
                        && literal_operand(left, source, work, cancel)?
                }
                b"-eq" | b"-ne" | b"-lt" | b"-le" | b"-gt" | b"-ge" => {
                    decimal_operand(left, source) && decimal_operand(right, source)
                }
                _ => false,
            };
            if !accepted {
                return Ok(false);
            }
        } else if node.kind() == "unary_expression" {
            let Some(operator) = node.child_by_field_name("operator") else {
                return Ok(false);
            };
            if !raw(operator, source).is_some_and(|value| {
                matches!(
                    value,
                    b"-n" | b"-z" | b"-f" | b"-d" | b"-e" | b"-r" | b"-w" | b"-x" | b"-s"
                )
            }) {
                return Ok(false);
            }
            let mut operand = None;
            let mut children = node.walk();
            for child in node.named_children(&mut children) {
                if !work.step(cancel)? {
                    return Ok(false);
                }
                if child != operator && operand.replace(child).is_some() {
                    return Ok(false);
                }
            }
            if !match operand {
                Some(operand) => literal_operand(operand, source, work, cancel)?,
                None => false,
            } {
                return Ok(false);
            }
        } else if node.is_named()
            && !matches!(
                node.kind(),
                "test_command"
                    | "test_operator"
                    | "word"
                    | "number"
                    | "regex"
                    | "string"
                    | "raw_string"
                    | "string_content"
                    | "simple_expansion"
                    | "special_variable_name"
                    | "variable_name"
                    | "expansion"
                    | "concatenation"
            )
        {
            return Ok(false);
        }
        if !next(&mut cursor, true, work, cancel)? {
            break;
        }
    }
    Ok(!work.exhausted())
}

fn guard(
    node: Node<'_>,
    source: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    if node.kind() == "test_command" {
        return readonly_test(node, source, work, cancel);
    }
    if node.kind() != "negated_command"
        || node.named_child_count() != 1
        || !raw(node, source).is_some_and(|value| value.starts_with(b"!"))
    {
        return Ok(false);
    }
    match node.named_child(0) {
        Some(test) => readonly_test(test, source, work, cancel),
        None => Ok(false),
    }
}

fn conditional_edge(
    parent: Node<'_>,
    child: Node<'_>,
    source: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let (condition, opening, closing) = match parent.kind() {
        "if_statement" => (parent.child_by_field_name("condition"), "then", Some("fi")),
        "elif_clause" => (parent.named_child(0), "then", None),
        "else_clause" => (None, "else", None),
        _ => return Ok(false),
    };
    if let Some(condition) = condition {
        if !guard(condition, source, work, cancel)? {
            return Ok(false);
        }
        if condition == child {
            return Ok(true);
        }
    } else if parent.kind() != "else_clause" {
        return Ok(false);
    }
    let mut lower = None;
    let mut upper = parent.end_byte();
    let mut found = false;
    let mut cursor = parent.walk();
    for item in parent.children(&mut cursor) {
        if !work.step(cancel)? {
            return Ok(false);
        }
        found |= item == child;
        if item.kind() == opening {
            lower = Some(item.end_byte())
        }
        if closing == Some(item.kind()) {
            upper = item.start_byte()
        }
    }
    Ok(found
        && lower.is_some_and(|lower| lower <= child.start_byte())
        && child.end_byte() <= upper
        && matches!(
            child.kind(),
            "command" | "redirected_statement" | "if_statement" | "elif_clause" | "else_clause"
        ))
}

fn numeric_redirect(
    node: Node<'_>,
    source: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let Some(body) = node.child_by_field_name("body") else {
        return Ok(false);
    };
    if node.kind() != "redirected_statement" || body.kind() != "command" {
        return Ok(false);
    }
    let mut count = 0usize;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !work.step(cancel)? {
            return Ok(false);
        }
        if child == body {
            continue;
        }
        let Some(value) = raw(child, source) else {
            return Ok(false);
        };
        if child.kind() != "file_redirect" || value.len() > 32 {
            return Ok(false);
        }
        let Some(at) = value.windows(2).position(|part| part == b">&") else {
            return Ok(false);
        };
        if !value
            .get(..at)
            .is_some_and(|part| part.iter().all(u8::is_ascii_digit))
            || !value
                .get(at + 2..)
                .is_some_and(|part| !part.is_empty() && part.iter().all(u8::is_ascii_digit))
        {
            return Ok(false);
        }
        count += 1;
    }
    Ok(count > 0)
}

fn context(
    mut node: Node<'_>,
    root: Node<'_>,
    source: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    while let Some(parent) = node.parent() {
        if !work.step(cancel)? {
            return Ok(false);
        }
        match parent.kind() {
            "program" => return Ok(parent == root),
            "simple_expansion" | "expansion" | "string" | "concatenation" => {}
            "command" if field(parent, node, "argument", work, cancel)? => {}
            "binary_expression" | "unary_expression" => {
                let Some(test) = test_ancestor(parent, work, cancel)? else {
                    return Ok(false);
                };
                if !readonly_test(test, source, work, cancel)? {
                    return Ok(false);
                }
            }
            "test_command" if readonly_test(parent, source, work, cancel)? => {}
            "negated_command" if guard(parent, source, work, cancel)? => {}
            "redirected_statement"
                if parent.child_by_field_name("body") == Some(node)
                    && numeric_redirect(parent, source, work, cancel)? => {}
            "if_statement" | "elif_clause" | "else_clause"
                if conditional_edge(parent, node, source, work, cancel)? => {}
            _ => return Ok(false),
        }
        node = parent;
    }
    Ok(false)
}

// printf can assign with both -v and %n. Admit only a bounded literal format
// whose directives are %s or %%; GNU Bash Builtins documents both write forms.
fn readonly_printf(
    node: Node<'_>,
    source: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut cursor = node.walk();
    for argument in node.named_children(&mut cursor) {
        if !work.step(cancel)? {
            return Ok(false);
        }
        if !field(node, argument, "argument", work, cancel)? {
            continue;
        }
        let Some(mut value) = raw(argument, source) else {
            return Ok(false);
        };
        if value.len() > 256 {
            return Ok(false);
        }
        match argument.kind() {
            "raw_string" if value.starts_with(b"'") && value.ends_with(b"'") => {
                let Some(inner) = value.get(1..value.len().saturating_sub(1)) else {
                    return Ok(false);
                };
                value = inner;
            }
            "string" if value.starts_with(b"\"") && value.ends_with(b"\"") => {
                let Some(inner) = value.get(1..value.len().saturating_sub(1)) else {
                    return Ok(false);
                };
                value = inner;
            }
            "word" => {}
            _ => return Ok(false),
        }
        if value.starts_with(b"-") || value.iter().any(|byte| b"$`\\".contains(byte)) {
            return Ok(false);
        }
        let mut index = 0;
        while let Some(byte) = value.get(index) {
            if !work.step(cancel)? {
                return Ok(false);
            }
            index += 1;
            if *byte == b'%' {
                if !matches!(value.get(index), Some(b's' | b'%')) {
                    return Ok(false);
                }
                index += 1;
            }
        }
        return Ok(true);
    }
    Ok(false)
}

// These names receive shell-owned state changes or assignment semantics. A
// lexical assignment is insufficient authority for an ordinary variable here.
fn ordinary_name(name: &[u8]) -> bool {
    !matches!(
        name,
        b"_" | b"RANDOM"
            | b"SRANDOM"
            | b"SECONDS"
            | b"LINENO"
            | b"EPOCHREALTIME"
            | b"EPOCHSECONDS"
            | b"BASH_MONOSECONDS"
            | b"BASHPID"
            | b"BASH_SUBSHELL"
            | b"PIPESTATUS"
            | b"BASH_COMMAND"
            | b"BASH_ARGC"
            | b"BASH_ARGV"
            | b"BASH_LINENO"
            | b"BASH_SOURCE"
            | b"FUNCNAME"
            | b"SHELLOPTS"
            | b"BASHOPTS"
            | b"DIRSTACK"
            | b"GROUPS"
            | b"BASH_ALIASES"
            | b"BASH_CMDS"
            | b"UID"
            | b"EUID"
            | b"PPID"
            | b"BASH_VERSINFO"
    )
}

fn command_word<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a [u8]> {
    let name = node.child_by_field_name("name")?;
    if name.kind() != "command_name" || name.named_child_count() != 1 {
        return None;
    }
    let word = name.named_child(0)?;
    let value = raw(word, source)?;
    (word.kind() == "word"
        && !value.is_empty()
        && value.len() <= 128
        && !value
            .iter()
            .any(|byte| byte.is_ascii_whitespace() || b"$`\\'\"".contains(byte)))
    .then_some(value)
}

pub(crate) fn eligible(
    root: Node<'_>,
    node: Node<'_>,
    source: &[u8],
    complete_input: bool,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    if !complete_input || !simple(node, source) {
        return Ok(false);
    }
    context(node, root, source, work, cancel)
}

/// Prove one ordinary expansion from the complete current source without buffers.
/// All walks share the caller's counter; exhaustion refuses the whole helper frame.
pub(crate) fn prove(
    root: Node<'_>,
    node: Node<'_>,
    source: &[u8],
    complete_input: bool,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<Binding>, AdapterError> {
    cancel.check()?;
    if !complete_input
        || !simple(node, source)
        || root.kind() != "program"
        || root.has_error()
        || root.end_byte() != source.len()
        || !matches!(source.get(..root.start_byte()), Some(b"" | b"\xef\xbb\xbf"))
        || !context(node, root, source, work, cancel)?
    {
        return Ok(None);
    }
    let Some(name) = raw(node, source) else {
        return Ok(None);
    };
    if !ordinary_name(name) {
        return Ok(None);
    }
    let mut contains_functions = false;
    let mut declarations = root.walk();
    loop {
        if !work.step(cancel)? {
            return Ok(None);
        }
        if declarations.node().kind() == "function_definition" {
            contains_functions = true;
            break;
        }
        if !next(&mut declarations, true, work, cancel)? {
            break;
        }
    }
    let mut definition = None;
    let mut duplicate = false;
    let mut blocked = false;
    let mut exact_leaf = 0usize;
    let mut cursor = root.walk();
    loop {
        if !work.step(cancel)? {
            return Ok(None);
        }
        let current = cursor.node();
        let function = current.kind() == "function_definition";
        if current.kind() == "variable_name" && current.byte_range() == node.byte_range() {
            exact_leaf += 1;
        }
        if current.kind() == "&" {
            blocked = true
        }
        if current.kind() == "variable_assignment" {
            let written_name = current.child_by_field_name("name");
            if written_name.and_then(|name| raw(name, source)) == Some(name) {
                let ordinary = current.parent() == Some(root)
                    && written_name.is_some_and(|name| name.kind() == "variable_name")
                    && current.child_by_field_name("value").is_some_and(|value| {
                        value.kind() != "array"
                            && written_name.is_some_and(|name| {
                                source.get(name.end_byte()..value.start_byte()) == Some(b"=")
                            })
                    });
                if ordinary {
                    let Some(written_name) = written_name else {
                        return Ok(None);
                    };
                    duplicate |= definition
                        .replace(Binding {
                            assignment: (current.start_byte(), current.end_byte()),
                            definition: (written_name.start_byte(), written_name.end_byte()),
                        })
                        .is_some();
                } else if current.start_byte() <= node.start_byte() {
                    blocked = true
                }
            } else if !written_name.is_some_and(|name| name.kind() == "variable_name")
                && current.start_byte() <= node.start_byte()
            {
                blocked = true
            }
        }
        if current.start_byte() <= node.start_byte()
            && matches!(
                current.kind(),
                "declaration_command"
                    | "unset_command"
                    | "arithmetic_expansion"
                    | "postfix_expression"
                    | "for_statement"
                    | "while_statement"
                    | "until_statement"
                    | "case_statement"
                    | "pipeline"
                    | "subshell"
                    | "process_substitution"
                    | "command_substitution"
                    | "heredoc_redirect"
            )
        {
            blocked = true
        }
        // Array indices and substring offsets may evaluate stored arithmetic
        // without an arithmetic CST node. Only plain ${NAME} is admitted here;
        // array construction, indexing and all complex expansions stay unproved.
        if current.start_byte() <= node.start_byte()
            && (matches!(current.kind(), "array" | "subscript")
                || current.kind() == "expansion"
                    && !current
                        .named_child(0)
                        .is_some_and(|name| simple(name, source)))
        {
            blocked = true;
        }
        if current.kind() == "redirected_statement"
            && current.start_byte() <= node.start_byte()
            && !numeric_redirect(current, source, work, cancel)?
        {
            blocked = true
        }
        if matches!(current.kind(), "binary_expression" | "unary_expression") {
            let test = test_ancestor(current, work, cancel)?;
            let readonly = match test {
                Some(test) => readonly_test(test, source, work, cancel)?,
                None => false,
            };
            if current.start_byte() <= node.start_byte()
                && (!readonly
                    || name == b"BASH_REMATCH"
                        && current
                            .child_by_field_name("operator")
                            .and_then(|operator| raw(operator, source))
                            == Some(b"=~"))
            {
                blocked = true;
            }
        }
        if current.kind() == "command" && current.start_byte() <= node.start_byte() {
            let word = command_word(current, source);
            let unsafe_name = word.is_none()
                || word.is_some_and(|name| {
                    matches!(
                        name,
                        b"eval"
                            | b"."
                            | b"source"
                            | b"read"
                            | b"mapfile"
                            | b"readarray"
                            | b"let"
                            | b"alias"
                            | b"unalias"
                            | b"trap"
                            | b"enable"
                            | b"builtin"
                            | b"command"
                            | b"exec"
                    )
                });
            if unsafe_name && current.start_byte() <= node.start_byte() {
                blocked = true
            }
            if let Some(word) = word {
                if word == b"printf" && !readonly_printf(current, source, work, cancel)? {
                    blocked = true;
                }
                if contains_functions {
                    let mut declarations = root.walk();
                    loop {
                        if !work.step(cancel)? {
                            return Ok(None);
                        }
                        let candidate = declarations.node();
                        if candidate.kind() == "function_definition"
                            && candidate
                                .child_by_field_name("name")
                                .and_then(|name| raw(name, source))
                                == Some(word)
                            && current.start_byte() <= node.start_byte()
                        {
                            blocked = true
                        }
                        if blocked {
                            break;
                        }
                        if !next(
                            &mut declarations,
                            candidate.kind() != "function_definition",
                            work,
                            cancel,
                        )? {
                            break;
                        }
                    }
                }
                // Unknown command effects are fenced after argument expansion.
                // Literal builtins below do not mutate ordinary variable bindings.
                if !matches!(
                    word,
                    b":" | b"echo" | b"printf" | b"true" | b"false" | b"exit" | b"set"
                ) && current.end_byte() <= node.start_byte()
                {
                    blocked = true
                }
            }
        }
        if blocked {
            return Ok(None);
        }
        if !next(&mut cursor, !function, work, cancel)? {
            break;
        }
    }
    Ok(definition.filter(|definition| {
        !duplicate
            && !blocked
            && exact_leaf == 1
            && definition.assignment.1 <= node.start_byte()
            && !work.exhausted()
    }))
}
