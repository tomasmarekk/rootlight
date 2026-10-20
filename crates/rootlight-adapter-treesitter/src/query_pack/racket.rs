//! Admit written Racket bindings from clean default-reader lists only.
//! Reader data and unmodeled lexical forms cannot manufacture owners or calls;
//! all memoized proofs share the caller's existing capture/body limit.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

#[derive(Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
}

#[derive(Clone, Copy)]
struct ReaderHeader {
    span: Span,
    base_reader: bool,
}

#[derive(Clone, Copy)]
struct Owner {
    name: Span,
    header_end: usize,
    body_start: usize,
    header: Option<usize>,
    function: bool,
}

pub(super) struct CaptureCache {
    forms: HashMap<usize, Option<Owner>>,
    maximum: usize,
    exhausted: bool,
    reader_allowed: Option<bool>,
    header: Option<ReaderHeader>,
}

impl CaptureCache {
    pub(super) fn new(maximum: usize) -> Self {
        Self {
            forms: HashMap::new(),
            maximum,
            exhausted: false,
            reader_allowed: None,
            header: None,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn direct<'tree>(
        &mut self,
        node: Node<'tree>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<Vec<Node<'tree>>>, AdapterError> {
        if node.kind() != "list" || node.has_error() || node.is_missing() {
            return Ok(None);
        }
        let raw = source.get(node.byte_range()).unwrap_or_default();
        if !matches!(
            (raw.first(), raw.last()),
            (Some(b'('), Some(b')')) | (Some(b'['), Some(b']')) | (Some(b'{'), Some(b'}'))
        ) {
            return Ok(None);
        }
        let mut values = Vec::new();
        let mut cursor = node.walk();
        for (index, child) in node.named_children(&mut cursor).enumerate() {
            cancellation.check()?;
            if index >= self.maximum {
                self.exhausted = true;
                return Ok(None);
            }
            if documentary(child) {
                continue;
            }
            if child.has_error() || child.is_missing() {
                return Ok(None);
            }
            values
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            values.push(child);
        }
        Ok(Some(values))
    }

    fn reader(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if let Some(allowed) = self.reader_allowed {
            return Ok(allowed);
        }
        self.header = first_reader_header(source, cancellation)?;
        if self.header.is_some_and(|header| !header.base_reader) {
            self.reader_allowed = Some(false);
            return Ok(false);
        }
        let mut root = node;
        while let Some(parent) = root.parent() {
            cancellation.check()?;
            root = parent;
        }
        let mut cursor = root.walk();
        let mut visits = 0usize;
        let mut extensions = 0usize;
        let mut allowed = true;
        loop {
            cancellation.check()?;
            visits = visits
                .checked_add(1)
                .ok_or_else(|| query_failure("query-racket-node-count"))?;
            if visits > self.maximum {
                self.exhausted = true;
                return Ok(false);
            }
            let current = cursor.node();
            if current.kind() == "extension" {
                extensions += 1;
                let mut children = root.walk();
                let first = root
                    .named_children(&mut children)
                    .find(|child| !documentary(*child));
                allowed &= current.parent() == Some(root)
                    && first == Some(current)
                    && matches!(
                        text(current, source),
                        Some("#lang racket/base" | "#lang racket" | "#!racket/base" | "#!racket")
                    );
            }
            if case_directive(current, source) {
                allowed = false;
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    allowed &= extensions <= 1;
                    self.reader_allowed = Some(allowed);
                    return Ok(allowed);
                }
            }
        }
    }

    fn form(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<Owner>, AdapterError> {
        cancellation.check()?;
        if let Some(owner) = self.forms.get(&node.id()) {
            return Ok(*owner);
        }
        if self.forms.len() >= self.maximum {
            self.exhausted = true;
            return Ok(None);
        }
        let mut owner = None;
        if let Some(values) = self.direct(node, source, cancellation)?
            && values.first().and_then(|head| text(*head, source)) == Some("define")
            && let Some(binding) = values.get(1).copied()
            && let Some(body) = values.get(2).copied()
        {
            if binding.kind() == "list" {
                if let Some(arguments) = self.direct(binding, source, cancellation)?
                    && let Some(name) = arguments
                        .first()
                        .copied()
                        .filter(|name| atomic(*name, source))
                    && self.formals(&arguments, source, cancellation)?
                {
                    owner = Some(Owner {
                        name: Span {
                            start: name.start_byte(),
                            end: name.end_byte(),
                        },
                        header_end: binding.end_byte(),
                        body_start: body.start_byte(),
                        header: Some(binding.id()),
                        function: true,
                    });
                }
            } else if values.len() == 3 && atomic(binding, source) {
                owner = Some(Owner {
                    name: Span {
                        start: binding.start_byte(),
                        end: binding.end_byte(),
                    },
                    header_end: binding.end_byte(),
                    body_start: body.start_byte(),
                    header: None,
                    function: false,
                });
            }
        }
        self.forms
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.forms.insert(node.id(), owner);
        Ok(owner)
    }

    fn formals(
        &self,
        arguments: &[Node<'_>],
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut keyword = false;
        let mut rest = false;
        for (index, argument) in arguments.iter().enumerate().skip(1) {
            cancellation.check()?;
            if argument.kind() == "keyword" && !keyword && !rest {
                keyword = true;
            } else if argument.kind() == "dot" && !keyword && !rest && index + 2 == arguments.len()
            {
                rest = true;
            } else if atomic(*argument, source) {
                keyword = false;
            } else {
                return Ok(false);
            }
        }
        Ok(!keyword)
    }

    fn owner_visible(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.reader(node, source, cancellation)? {
            return Ok(false);
        }
        let mut child = node;
        let mut count = 0;
        while let Some(parent) = child.parent() {
            cancellation.check()?;
            count += 1;
            if count > self.maximum {
                self.exhausted = true;
                return Ok(false);
            }
            match parent.kind() {
                "program" => return Ok(true),
                "list" => {
                    let own = self.form(parent, source, cancellation)?;
                    if !own.is_some_and(|own| own.function && child.start_byte() >= own.body_start)
                        && head(parent, source) != Some("begin")
                    {
                        return Ok(false);
                    }
                }
                _ => return Ok(false),
            }
            child = parent;
        }
        Ok(false)
    }

    fn parameter(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !atomic(node, source) {
            return Ok(false);
        }
        let Some(header) = node.parent().filter(|parent| parent.kind() == "list") else {
            return Ok(false);
        };
        let Some(definition) = header.parent().filter(|parent| parent.kind() == "list") else {
            return Ok(false);
        };
        let Some(owner) = self.form(definition, source, cancellation)? else {
            return Ok(false);
        };
        Ok(owner.header == Some(header.id())
            && node.start_byte() != owner.name.start
            && self.owner_visible(definition, source, cancellation)?)
    }

    fn call(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !atomic(node, source) || !self.reader(node, source, cancellation)? {
            return Ok(false);
        }
        let Some(application) = node.parent().filter(|parent| parent.kind() == "list") else {
            return Ok(false);
        };
        let Some(values) = self.direct(application, source, cancellation)? else {
            return Ok(false);
        };
        if values.first() != Some(&node)
            || special(text(node, source).unwrap_or_default())
            || self.form(application, source, cancellation)?.is_some()
        {
            return Ok(false);
        }
        let mut child = application;
        let mut count = 0;
        while let Some(parent) = child.parent() {
            cancellation.check()?;
            count += 1;
            if count > self.maximum {
                self.exhausted = true;
                return Ok(false);
            }
            match parent.kind() {
                "program" => return Ok(true),
                "list" => {
                    if let Some(owner) = self.form(parent, source, cancellation)? {
                        if child.start_byte() < owner.body_start {
                            return Ok(false);
                        }
                    } else if !head(parent, source).is_some_and(transparent_application) {
                        return Ok(false);
                    }
                }
                _ => return Ok(false),
            }
            child = parent;
        }
        Ok(false)
    }
}

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}

fn atomic(node: Node<'_>, source: &[u8]) -> bool {
    node.kind() == "symbol"
        && !node.has_error()
        && !node.is_missing()
        && text(node, source).is_some_and(|raw| {
            structural_captured_name_for_language("racket", raw, raw.len()).is_some()
        })
}

fn documentary(node: Node<'_>) -> bool {
    matches!(node.kind(), "comment" | "block_comment" | "sexp_comment")
}

fn case_directive(node: Node<'_>, source: &[u8]) -> bool {
    node.kind() == "symbol"
        && text(node, source).is_some_and(|text| {
            text.eq_ignore_ascii_case("#ci") || text.eq_ignore_ascii_case("#cs")
        })
}

fn reader_data(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut parent = node.parent();
    while let Some(ancestor) = parent {
        cancellation.check()?;
        if matches!(
            ancestor.kind(),
            "quote"
                | "quasiquote"
                | "syntax"
                | "quasisyntax"
                | "unquote"
                | "unquote_splicing"
                | "unsyntax"
                | "unsyntax_splicing"
                | "sexp_comment"
                | "vector"
                | "hash"
                | "box"
                | "structure"
                | "graph"
        ) {
            return Ok(true);
        }
        if ancestor.kind() == "list"
            && matches!(
                head(ancestor, source),
                Some("quote" | "quote-syntax" | "quasiquote" | "syntax" | "quasisyntax")
            )
        {
            return Ok(true);
        }
        parent = ancestor.parent();
    }
    Ok(false)
}

fn head<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| !documentary(*child))
        .and_then(|child| {
            (child.kind() == "symbol")
                .then(|| text(child, source))
                .flatten()
        })
}

fn blocks_execution(operator: &str) -> bool {
    matches!(
        operator,
        "quote"
            | "quote-syntax"
            | "quasiquote"
            | "syntax"
            | "quasisyntax"
            | "lambda"
            | "λ"
            | "case-lambda"
            | "let"
            | "let*"
            | "letrec"
            | "letrec*"
            | "let-values"
            | "let*-values"
            | "letrec-values"
            | "let-syntax"
            | "letrec-syntax"
            | "define-syntax"
            | "define-syntaxes"
            | "define-syntax-rule"
            | "define-for-syntax"
            | "begin-for-syntax"
            | "syntax-rules"
            | "syntax-case"
            | "match"
            | "match*"
            | "for"
            | "for/list"
            | "for/fold"
            | "module"
            | "module*"
            | "module+"
            | "parameterize"
            | "with-handlers"
    )
}

fn special(operator: &str) -> bool {
    blocks_execution(operator)
        || matches!(
            operator,
            "define"
                | "define-values"
                | "require"
                | "provide"
                | "set!"
                | "if"
                | "cond"
                | "case"
                | "and"
                | "or"
                | "begin"
                | "begin0"
                | "else"
                | "struct"
                | "define-struct"
        )
}

fn transparent_application(operator: &str) -> bool {
    // Unknown application heads may be imported macros whose arguments are data.
    // These base forms have reviewed expression positions; dynamic local forms
    // are withheld before the lexical target resolver can publish a call edge.
    matches!(operator, "not" | "if" | "and" | "or" | "begin" | "begin0")
}

fn first_reader_header(
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<ReaderHeader>, AdapterError> {
    let start = if source.starts_with(b"\xef\xbb\xbf") {
        3
    } else {
        0
    };
    let Some(prefix) = source.get(start..) else {
        return Ok(None);
    };
    if !prefix.starts_with(b"#lang") {
        return Ok(None);
    }
    let mut end = source.len();
    for (index, byte) in prefix.iter().enumerate() {
        if index % 1024 == 0 {
            cancellation.check()?;
        }
        if *byte == b'\n' {
            end = start + index;
            break;
        }
    }
    if end > start && source.get(end - 1) == Some(&b'\r') {
        end -= 1;
    }
    let header = source
        .get(start..end)
        .and_then(|raw| std::str::from_utf8(raw).ok());
    let base_reader = header
        .is_some_and(|header| matches!(header.trim_end(), "#lang racket" | "#lang racket/base"));
    Ok(Some(ReaderHeader {
        span: Span { start, end },
        base_reader,
    }))
}

pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    let reader_allowed = cache.reader(node, source, cancellation)?;
    if node.kind() == "program" {
        return Ok(match role {
            StructuralRole::Expression => cache.header.is_some_and(|header| !header.base_reader),
            StructuralRole::Scope => reader_allowed,
            _ => true,
        });
    }
    if !reader_allowed {
        return Ok(role == StructuralRole::Expression
            && !cache.header.is_some_and(|header| !header.base_reader)
            && (node.kind() == "extension" || case_directive(node, source)));
    }
    if node.has_error() || node.is_missing() {
        return Ok(role == StructuralRole::Expression);
    }
    if role == StructuralRole::Comment {
        return Ok(documentary(node));
    }
    if role == StructuralRole::StringLiteral {
        return Ok(true);
    }
    if role == StructuralRole::Expression {
        if reader_data(node, source, cancellation)? {
            return Ok(false);
        }
        if matches!(
            head(node, source),
            Some("quote" | "quote-syntax" | "quasiquote" | "syntax" | "quasisyntax")
        ) {
            return Ok(false);
        }
        if node.kind() == "list"
            && let Some(values) = cache.direct(node, source, cancellation)?
            && let Some(first) = values.first().copied()
            && cache.call(first, source, cancellation)?
            && !head(node, source).is_some_and(transparent_application)
        {
            return Ok(true);
        }
        return Ok(
            node.kind() == "extension" && !cache.reader(node, source, cancellation)?
                || case_directive(node, source)
                || node.kind() == "list"
                    && head(node, source).is_some_and(|name| {
                        blocks_execution(name)
                            || matches!(
                                name,
                                "define" | "define-values" | "struct" | "define-struct"
                            )
                    })
                    && (cache.form(node, source, cancellation)?.is_none()
                        || !cache.owner_visible(node, source, cancellation)?),
        );
    }
    if role == StructuralRole::Reference {
        return cache.call(node, source, cancellation);
    }
    if node.kind() == "symbol"
        && matches!(
            role,
            StructuralRole::Declaration | StructuralRole::Definition
        )
    {
        return cache.parameter(node, source, cancellation);
    }
    if node.kind() != "list" {
        return Ok(false);
    }
    Ok(cache
        .form(node, source, cancellation)?
        .is_some_and(|owner| role != StructuralRole::Scope || owner.function)
        && cache.owner_visible(node, source, cancellation)?)
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<QueryCandidate, AdapterError> {
    cancellation.check()?;
    let mut start = node.start_byte();
    let mut end = node.end_byte();
    let syntax = if node.kind() == "program" && role == StructuralRole::Expression {
        let header = cache
            .header
            .filter(|header| !header.base_reader)
            .ok_or_else(|| query_failure("query-racket-reader-header"))?;
        start = header.span.start;
        end = header.span.end;
        "racket.reader_unavailable"
    } else if node.kind() == "program" {
        start = 0;
        end = source.len();
        "racket.file"
    } else if role == StructuralRole::Expression {
        if node.kind() == "extension" || case_directive(node, source) {
            "racket.reader_unavailable"
        } else if head(node, source).is_some_and(|operator| {
            matches!(
                operator,
                "define"
                    | "define-values"
                    | "define-syntax"
                    | "define-syntaxes"
                    | "define-syntax-rule"
            )
        }) {
            "racket.binding_unavailable"
        } else if head(node, source).is_some_and(|operator| !special(operator)) {
            "racket.application_unavailable"
        } else {
            "racket.unavailable"
        }
    } else if role == StructuralRole::Comment {
        "racket.comment"
    } else if role == StructuralRole::StringLiteral {
        "racket.string"
    } else if role == StructuralRole::Reference {
        "racket.call"
    } else if node.kind() == "symbol" {
        if role == StructuralRole::Definition {
            "racket.name"
        } else {
            "racket.parameter"
        }
    } else {
        let owner = cache
            .form(node, source, cancellation)?
            .ok_or_else(|| query_failure("query-racket-owner"))?;
        match role {
            StructuralRole::Definition => {
                start = owner.name.start;
                end = owner.name.end;
                "racket.name"
            }
            StructuralRole::Signature => {
                end = owner.header_end;
                "racket.header"
            }
            _ => {
                if owner.function {
                    "racket.function"
                } else {
                    "racket.variable"
                }
            }
        }
    };
    let depth = native_depth(node, cancellation)?;
    Ok(QueryCandidate {
        start,
        end,
        syntax,
        role,
        required: role.belongs_to_identity_closure(),
        native_depth: if role == StructuralRole::Scope && node.kind() != "program" {
            depth
                .checked_add(1)
                .ok_or_else(|| query_failure("query-racket-depth"))?
        } else {
            depth
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    #[test]
    fn racket_native_proofs_obey_cache_admission_and_typed_cancellation() {
        let source = b"#lang racket/base\n(define (literal x) x)";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_racket::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let mut cursor = tree.root_node().walk();
        let definition = tree
            .root_node()
            .named_children(&mut cursor)
            .find(|node| node.kind() == "list")
            .unwrap();
        let cancellation = Cancellation::new();
        let mut bounded = CaptureCache::new(0);
        assert!(
            !retain_capture(
                definition,
                StructuralRole::Declaration,
                source,
                &cancellation,
                &mut bounded
            )
            .unwrap()
        );
        assert!(bounded.exhausted());
        let mut admitted = CaptureCache::new(128);
        assert!(
            retain_capture(
                definition,
                StructuralRole::Declaration,
                source,
                &cancellation,
                &mut admitted
            )
            .unwrap()
        );
        assert!(!admitted.exhausted());
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert_eq!(
            retain_capture(
                definition,
                StructuralRole::Declaration,
                source,
                &cancellation,
                &mut admitted
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        );
        assert_eq!(
            candidate(
                definition,
                StructuralRole::Definition,
                source,
                &cancellation,
                &mut admitted
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        );
    }
}
