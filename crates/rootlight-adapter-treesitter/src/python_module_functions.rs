//! Prove the first guarded use of an ordinary written module function.
//! The current source is reparsed; producer facts never authorize this lookup.

use std::ops::ControlFlow;

use rootlight_adapter_sdk::{AdapterError, DiagnosticCode, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::{Node, ParseOptions};

type Span = (usize, usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Proof {
    pub(crate) declaration: Span,
    pub(crate) name: Span,
    pub(crate) site: Span,
}

struct Work(usize);

impl Work {
    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(next) = self.0.checked_sub(1) else {
            return Ok(false);
        };
        self.0 = next;
        Ok(true)
    }

    fn bytes(&mut self, raw: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
        for _ in raw.chunks(64) {
            if !self.step(cancel)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn span(node: Node<'_>) -> Span {
    (node.start_byte(), node.end_byte())
}

fn name<'a>(
    node: Node<'_>,
    source: &'a [u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<&'a [u8]>, AdapterError> {
    let Some(raw) = source.get(node.byte_range()) else {
        return Ok(None);
    };
    if node.kind() != "identifier"
        || !work.bytes(raw, cancel)?
        || !raw
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        || !raw.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_')
    {
        return Ok(None);
    }
    Ok(Some(raw))
}

fn only_child<'a>(
    node: Node<'a>,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<Option<Node<'a>>, AdapterError> {
    let mut one = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !work.step(cancel)? {
            return Ok(None);
        }
        if child.kind() == "comment" {
            continue;
        }
        if one.is_some() {
            return Ok(None);
        }
        one = Some(child);
    }
    Ok(one)
}

/// Select only the distinct Reference leaf; Call and CallName captures are unchanged.
pub(crate) fn candidate_site(node: Node<'_>) -> bool {
    let Some(call) = node.parent() else {
        return false;
    };
    let Some(statement) = call.parent() else {
        return false;
    };
    let Some(block) = statement.parent() else {
        return false;
    };
    let Some(guard) = block.parent() else {
        return false;
    };
    node.kind() == "identifier"
        && call.kind() == "call"
        && call.child_by_field_name("function") == Some(node)
        && statement.kind() == "expression_statement"
        && block.kind() == "block"
        && guard.kind() == "if_statement"
        && guard.child_by_field_name("consequence") == Some(block)
        && guard
            .parent()
            .is_some_and(|root| root.kind() == "module" && root.parent().is_none())
}

/// Return no certificate when any module operation can precede or replace the binding.
pub(crate) fn source_proof(
    source: &[u8],
    maximum: usize,
    cancel: &Cancellation,
) -> Result<Option<Proof>, AdapterError> {
    cancel.check()?;
    if maximum == 0 {
        return Ok(None);
    }
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .map_err(|_| AdapterError::ProviderFailed {
            code: DiagnosticCode::new("python-module-function-grammar")
                .expect("fixed grammar diagnostic is valid"),
        })?;
    let mut parse_work = maximum;
    let mut exhausted = false;
    let mut progress = |_: &tree_sitter::ParseState| {
        if cancel.check().is_err() {
            return ControlFlow::Break(());
        }
        match parse_work.checked_sub(1) {
            Some(next) => {
                parse_work = next;
                ControlFlow::Continue(())
            }
            None => {
                exhausted = true;
                ControlFlow::Break(())
            }
        }
    };
    let mut input = |at: usize, _: tree_sitter::Point| source.get(at..).unwrap_or_default();
    let tree = parser.parse_with_options(
        &mut input,
        None,
        Some(ParseOptions::new().progress_callback(&mut progress)),
    );
    cancel.check()?;
    let Some(tree) = tree else { return Ok(None) };
    let root = tree.root_node();
    if exhausted
        || root.kind() != "module"
        || root.has_error()
        || root.is_missing()
        || root.end_byte() != source.len()
        || !matches!(source.get(..root.start_byte()), Some(b"" | b"\xef\xbb\xbf"))
    {
        return Ok(None);
    }
    let mut work = Work(maximum);
    // These operations escape lexical source authority even inside a deferred body.
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if !work.step(cancel)? {
            return Ok(None);
        }
        if matches!(
            node.kind(),
            "global_statement"
                | "nonlocal_statement"
                | "delete_statement"
                | "import_from_statement"
                | "wildcard_import"
                | "interpolation"
        ) {
            return Ok(None);
        }
        if node.kind() == "identifier" {
            let Some(raw) = source.get(node.byte_range()) else {
                return Ok(None);
            };
            if !work.bytes(raw, cancel)? {
                return Ok(None);
            }
            if matches!(
                raw,
                b"exec"
                    | b"eval"
                    | b"globals"
                    | b"locals"
                    | b"vars"
                    | b"getattr"
                    | b"setattr"
                    | b"delattr"
                    | b"__import__"
                    | b"__dict__"
                    | b"__getattr__"
                    | b"__getattribute__"
                    | b"__setattr__"
            ) {
                return Ok(None);
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                break;
            }
        }
        if cursor.node() == root {
            break;
        }
    }
    let mut definitions: Vec<(Span, Span)> = Vec::new();
    let mut imports: Vec<Span> = Vec::new();
    let mut selected = None;
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if !work.step(cancel)? {
            return Ok(None);
        }
        match child.kind() {
            "comment" => {}
            "expression_statement" if definitions.is_empty() && selected.is_none() => {
                let Some(value) = only_child(child, &mut work, cancel)? else {
                    return Ok(None);
                };
                let Some(raw) = source.get(value.byte_range()) else {
                    return Ok(None);
                };
                if value.kind() != "string"
                    || !matches!(raw.first(), Some(b'\'' | b'"'))
                    || !work.bytes(raw, cancel)?
                {
                    return Ok(None);
                }
            }
            "import_statement" if definitions.is_empty() && selected.is_none() => {
                let Some(raw) = source.get(child.byte_range()) else {
                    return Ok(None);
                };
                if !work.bytes(raw, cancel)? {
                    return Ok(None);
                }
                let Some(word) = raw.strip_prefix(b"import ") else {
                    return Ok(None);
                };
                let word = word.trim_ascii();
                if !word.first().is_some_and(u8::is_ascii_alphabetic)
                    || !word.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_')
                {
                    return Ok(None);
                }
                let Some(import_name) = child.child_by_field_name("name") else {
                    return Ok(None);
                };
                imports
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                imports.push(span(import_name));
            }
            "function_definition" if selected.is_none() => {
                let Some(identifier) = child.child_by_field_name("name") else {
                    return Ok(None);
                };
                if name(identifier, source, &mut work, cancel)?.is_none() {
                    return Ok(None);
                }
                let Some(parameters) = child.child_by_field_name("parameters") else {
                    return Ok(None);
                };
                let Some(body) = child.child_by_field_name("body") else {
                    return Ok(None);
                };
                let Some(prefix) = source.get(child.start_byte()..identifier.start_byte()) else {
                    return Ok(None);
                };
                let Some(prefix) = prefix.strip_prefix(b"def") else {
                    return Ok(None);
                };
                if prefix.is_empty()
                    || !prefix.iter().all(u8::is_ascii_whitespace)
                    || !work.bytes(prefix, cancel)?
                    || parameters.named_child_count() != 0
                    || body.kind() != "block"
                {
                    return Ok(None);
                }
                let mut inner = child.walk();
                for field in child.named_children(&mut inner) {
                    if !work.step(cancel)? {
                        return Ok(None);
                    }
                    if field != identifier
                        && field != parameters
                        && field != body
                        && field.kind() != "comment"
                    {
                        return Ok(None);
                    }
                }
                definitions
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                definitions.push((span(child), span(identifier)));
            }
            "if_statement" if selected.is_none() && !definitions.is_empty() => {
                let Some(condition) = child.child_by_field_name("condition") else {
                    return Ok(None);
                };
                let Some(body) = child.child_by_field_name("consequence") else {
                    return Ok(None);
                };
                let Some(raw) = source.get(condition.byte_range()) else {
                    return Ok(None);
                };
                if !work.bytes(raw, cancel)? {
                    return Ok(None);
                }
                let Some(left) = condition.named_child(0) else {
                    return Ok(None);
                };
                let Some(right) = condition.named_child(1) else {
                    return Ok(None);
                };
                if condition.kind() != "comparison_operator"
                    || condition.named_child_count() != 2
                    || left.kind() != "identifier"
                    || source.get(left.byte_range()) != Some(b"__name__".as_slice())
                    || right.kind() != "string"
                    || !matches!(
                        source.get(right.byte_range()),
                        Some(b"\"__main__\"" | b"'__main__'")
                    )
                {
                    return Ok(None);
                }
                let compact = || raw.iter().copied().filter(|b| !b.is_ascii_whitespace());
                if !compact().eq(b"__name__==\"__main__\"".iter().copied())
                    && !compact().eq(b"__name__=='__main__'".iter().copied())
                {
                    return Ok(None);
                }
                let mut inner = child.walk();
                for field in child.named_children(&mut inner) {
                    if !work.step(cancel)? {
                        return Ok(None);
                    }
                    if field != condition && field != body && field.kind() != "comment" {
                        return Ok(None);
                    }
                }
                let Some(statement) = only_child(body, &mut work, cancel)? else {
                    return Ok(None);
                };
                let Some(call) = only_child(statement, &mut work, cancel)? else {
                    return Ok(None);
                };
                if statement.kind() != "expression_statement" || call.kind() != "call" {
                    return Ok(None);
                }
                let Some(function) = call.child_by_field_name("function") else {
                    return Ok(None);
                };
                let Some(arguments) = call.child_by_field_name("arguments") else {
                    return Ok(None);
                };
                if arguments.named_child_count() != 0 || !candidate_site(function) {
                    return Ok(None);
                }
                let Some(written) = name(function, source, &mut work, cancel)? else {
                    return Ok(None);
                };
                let mut target = None;
                for &(declaration, identifier) in &definitions {
                    let raw = &source[identifier.0..identifier.1];
                    if !work.bytes(raw, cancel)? {
                        return Ok(None);
                    }
                    if raw == written {
                        if target.is_some() {
                            return Ok(None);
                        }
                        target = Some((declaration, identifier));
                    }
                }
                let Some((declaration, identifier)) = target else {
                    return Ok(None);
                };
                for import in &imports {
                    let raw = &source[import.0..import.1];
                    if !work.bytes(raw, cancel)? || raw == written {
                        return Ok(None);
                    }
                }
                selected = Some(Proof {
                    declaration,
                    name: identifier,
                    site: span(function),
                });
            }
            _ => return Ok(None),
        }
    }
    Ok(selected)
}

pub(crate) struct Cache<'a> {
    source: &'a [u8],
    maximum: usize,
    complete: bool,
    proof: Option<Option<Proof>>,
}

impl<'a> Cache<'a> {
    pub(crate) const fn new(source: &'a [u8], maximum: usize, complete: bool) -> Self {
        Self {
            source,
            maximum,
            complete,
            proof: None,
        }
    }

    pub(crate) fn certified(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancel.check()?;
        if !self.complete || !candidate_site(node) {
            return Ok(false);
        }
        if self.proof.is_none() {
            self.proof = Some(source_proof(self.source, self.maximum, cancel)?);
        }
        Ok(self
            .proof
            .flatten()
            .is_some_and(|proof| proof.site == span(node)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &[u8] = include_bytes!("../tests/fixtures/python_module_function/repl.py");

    #[test]
    fn complete_original_has_exact_function_field_and_guarded_reference() {
        let proof = source_proof(ORIGINAL, 16_384, &Cancellation::new())
            .unwrap()
            .unwrap();
        assert_eq!(ORIGINAL.len(), 1818);
        assert_eq!(proof.name, (1572, 1576));
        assert_eq!(proof.site, (1810, 1814));
        assert!(ORIGINAL[proof.declaration.0..proof.declaration.1].starts_with(b"def main():"));
        assert_eq!(&ORIGINAL[proof.site.0..proof.site.1], b"main");
    }

    #[test]
    fn ordinary_renamed_entry_and_deferred_local_imports_keep_written_ownership() {
        let source = b"import sys\n\ndef launch():\n    import worker\n    worker.run()\n\nif __name__ == '__main__':\n    launch()\n";
        let proof = source_proof(source, 16_384, &Cancellation::new())
            .unwrap()
            .unwrap();
        assert_eq!(&source[proof.name.0..proof.name.1], b"launch");
        assert_eq!(&source[proof.site.0..proof.site.1], b"launch");
        assert!(proof.name.1 < proof.site.0);
    }

    #[test]
    fn module_rebinding_headers_star_dynamic_and_competing_guards_refuse() {
        let cases: &[&[u8]] = &[
            b"def run(): pass\nrun = other\nif __name__ == '__main__': run()\n",
            b"def run(): pass\ndef run(): pass\nif __name__ == '__main__': run()\n",
            b"import run\ndef run(): pass\nif __name__ == '__main__': run()\n",
            b"from lib import *\ndef run(): pass\nif __name__ == '__main__': run()\n",
            b"@wrap\ndef run(): pass\nif __name__ == '__main__': run()\n",
            b"def run(value=effect()): pass\nif __name__ == '__main__': run()\n",
            b"def run() -> Type: pass\nif __name__ == '__main__': run()\n",
            b"def run(): pass\nimport worker\nif __name__ == '__main__': run()\n",
            b"def run(): pass\nprepare()\nif __name__ == '__main__': run()\n",
            b"def run():\n    global run\nif __name__ == '__main__': run()\n",
            b"def run():\n    exec(payload)\nif __name__ == '__main__': run()\n",
            b"def run():\n    globals()['run'] = other\nif __name__ == '__main__': run()\n",
            b"def run(): pass\nif __name__ == '__ main__': run()\n",
            b"def run(): pass\nif __name__ == '__main__': run(value)\n",
            b"def run(): pass\nif __name__ == '__main__': run()\nelse: run()\n",
            b"def run(): pass\nif __name__ == '__main__':\n    run()\n    run()\n",
            b"def run(): pass\nif __name__ == '__main__' run()\n",
        ];
        for source in cases {
            assert_eq!(
                source_proof(source, 16_384, &Cancellation::new()).unwrap(),
                None,
                "{}",
                String::from_utf8_lossy(source)
            );
        }
    }

    #[test]
    fn incomplete_input_and_work_or_cancel_never_supply_partial_authority() {
        for maximum in [0, 1, 8] {
            assert_eq!(
                source_proof(ORIGINAL, maximum, &Cancellation::new()).unwrap(),
                None
            );
        }
        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            source_proof(ORIGINAL, 16_384, &cancelled),
            Err(AdapterError::Cancelled { .. })
        ));
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(ORIGINAL, None).unwrap();
        let site = tree
            .root_node()
            .descendant_for_byte_range(1810, 1814)
            .unwrap();
        let mut cache = Cache::new(ORIGINAL, 16_384, false);
        assert!(!cache.certified(site, &Cancellation::new()).unwrap());
        assert!(
            cache.proof.is_none(),
            "partial input cannot allocate or replay the owner cache"
        );
    }
}
