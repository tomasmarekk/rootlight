//! Common Lisp captures describe complete written owners, not evaluated bindings.
//! Reader selection, package identity and macro expansion remain explicit gaps.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operator {
    Function,
    Macro,
    Generic,
    Method,
    Class,
    Package,
    Variable,
    Constant,
    Lambda,
    Quote,
    Progn,
    LocalMacro,
    PackageEnvironment,
    Binding,
    Unsupported,
}

impl Operator {
    const fn syntax(self) -> Option<&'static str> {
        Some(match self {
            Self::Function => "common_lisp.function",
            Self::Macro => "common_lisp.macro",
            Self::Generic => "common_lisp.generic_function",
            Self::Method => "common_lisp.method",
            Self::Class => "common_lisp.class",
            Self::Package => "common_lisp.package",
            Self::Variable => "common_lisp.variable",
            Self::Constant => "common_lisp.constant",
            _ => return None,
        })
    }

    const fn has_parameters(self) -> bool {
        matches!(
            self,
            Self::Function | Self::Macro | Self::Generic | Self::Method
        )
    }

    const fn has_body_reads(self) -> bool {
        matches!(self, Self::Function | Self::Macro | Self::Method)
    }
}

#[derive(Clone)]
struct OwnerProof {
    name: Range<usize>,
    header: Range<usize>,
    lambda: Option<Range<usize>>,
    required_end: usize,
    complex_parameters: bool,
    operator: Operator,
}

#[derive(Clone)]
struct FormProof {
    operator: Option<Operator>,
    owner: Option<OwnerProof>,
    reader_prefix: bool,
    reader_content: bool,
    reader_head: bool,
}

#[derive(Clone, Copy)]
struct BodyProof {
    header_end: usize,
    reads: bool,
}

#[derive(Clone, Copy)]
struct ContextProof {
    data: bool,
    blocked: bool,
    declarations: bool,
    body: Option<BodyProof>,
}

#[derive(Default)]
struct CacheEntry {
    form: Option<FormProof>,
    context: Option<ContextProof>,
    prefix: Option<bool>,
}

/// Reuses the existing capture/body bound; no source-sized auxiliary index exists.
pub(super) struct CaptureCache {
    entries: HashMap<usize, CacheEntry>,
    maximum_entries: usize,
    exhausted: bool,
}

impl CaptureCache {
    pub(super) fn new(maximum_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            maximum_entries,
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn slot(&mut self, id: usize) -> Result<bool, AdapterError> {
        if self.entries.contains_key(&id) {
            return Ok(true);
        }
        if self.entries.len() >= self.maximum_entries {
            self.exhausted = true;
            return Ok(false);
        }
        self.entries
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.entries.insert(id, CacheEntry::default());
        Ok(true)
    }

    fn form(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<FormProof>, AdapterError> {
        cancellation.check()?;
        if let Some(proof) = self
            .entries
            .get(&node.id())
            .and_then(|entry| entry.form.as_ref())
        {
            return Ok(Some(proof.clone()));
        }
        if !self.slot(node.id())? {
            return Ok(None);
        }
        let prefix = self.prefix(node, source, cancellation)?;
        let proof = form_proof(node, source, cancellation, prefix)?;
        self.entries
            .get_mut(&node.id())
            .ok_or_else(|| query_failure("query-common-lisp-cache-slot"))?
            .form = Some(proof.clone());
        Ok(Some(proof))
    }

    fn prefix(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        if let Some(prefix) = self.entries.get(&node.id()).and_then(|entry| entry.prefix) {
            return Ok(prefix);
        }
        if !self.slot(node.id())? {
            return Ok(true);
        }
        let prefix = reader_prefix(node, source, cancellation)?;
        self.entries
            .get_mut(&node.id())
            .ok_or_else(|| query_failure("query-common-lisp-prefix-slot"))?
            .prefix = Some(prefix);
        Ok(prefix)
    }

    fn context(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<ContextProof>, AdapterError> {
        cancellation.check()?;
        let mut chain = Vec::new();
        let mut current = Some(node);
        let mut inherited = ContextProof {
            data: false,
            blocked: false,
            declarations: true,
            body: None,
        };
        while let Some(ancestor) = current {
            cancellation.check()?;
            if let Some(context) = self
                .entries
                .get(&ancestor.id())
                .and_then(|entry| entry.context)
            {
                inherited = context;
                break;
            }
            if chain.len() >= self.maximum_entries {
                self.exhausted = true;
                return Ok(None);
            }
            chain
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            chain.push(ancestor);
            current = ancestor.parent();
        }
        for current in chain.into_iter().rev() {
            cancellation.check()?;
            if let Some(parent) = current.parent() {
                if parent.is_missing() || parent.kind() == "ERROR" || data_node(parent) {
                    inherited.data = true;
                    inherited.blocked = true;
                    inherited.declarations = false;
                }
                if self.prefix(parent, source, cancellation)? {
                    inherited.blocked = true;
                    inherited.declarations = false;
                }
                if parent.kind() == "include_reader_macro"
                    && single_field(parent, "target", cancellation)? != Some(current)
                {
                    inherited.data = true;
                    inherited.blocked = true;
                    inherited.declarations = false;
                }
                if matches!(parent.kind(), "defun_header" | "loop_macro") {
                    inherited.blocked = true;
                }
                if parent.kind() == "list_lit" {
                    let Some(form) = self.form(parent, source, cancellation)? else {
                        return Ok(None);
                    };
                    inherited.declarations &= form.operator == Some(Operator::Progn);
                    inherited.data |=
                        matches!(form.operator, Some(Operator::Quote | Operator::LocalMacro));
                    inherited.blocked |= form.reader_prefix
                        || form.reader_head
                        || matches!(
                            form.operator,
                            Some(
                                Operator::Quote
                                    | Operator::LocalMacro
                                    | Operator::Binding
                                    | Operator::Unsupported
                                    | Operator::Package
                                    | Operator::Class
                            )
                        );
                    if let Some(owner) = form.owner {
                        inherited.body = Some(BodyProof {
                            header_end: owner.header.end,
                            reads: owner.operator.has_body_reads(),
                        });
                    } else if form
                        .operator
                        .is_some_and(|op| op.syntax().is_some() || op == Operator::Lambda)
                    {
                        inherited.blocked = true;
                    }
                }
            }
            if !self.slot(current.id())? {
                return Ok(None);
            }
            self.entries
                .get_mut(&current.id())
                .ok_or_else(|| query_failure("query-common-lisp-context-slot"))?
                .context = Some(inherited);
        }
        Ok(Some(inherited))
    }
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}

fn atomic_name(node: Node<'_>, source: &[u8], package: bool) -> bool {
    clean(node, source)
        && (matches!(node.kind(), "sym_lit" | "fancy_literal" | "package_lit")
            || package && node.kind() == "kwd_lit")
        && source.get(node.byte_range()).is_some_and(|bytes| {
            !bytes.is_empty() && bytes.first() != Some(&b'#') && !bytes.contains(&0)
        })
}

fn operator(node: Node<'_>, source: &[u8]) -> Option<Operator> {
    if !matches!(node.kind(), "sym_lit" | "package_lit" | "defun_keyword") {
        return None;
    }
    let mut bytes = source.get(node.byte_range())?;
    if let Some(colon) = bytes.iter().position(|byte| *byte == b':') {
        let package = bytes.get(..colon)?;
        if !package.eq_ignore_ascii_case(b"cl") && !package.eq_ignore_ascii_case(b"common-lisp") {
            return None;
        }
        bytes = bytes.get(colon.checked_add(1)?..)?;
        bytes = bytes.strip_prefix(b":").unwrap_or(bytes);
    }
    for (name, op) in [
        (b"defun".as_slice(), Operator::Function),
        (b"defmacro".as_slice(), Operator::Macro),
        (b"defgeneric".as_slice(), Operator::Generic),
        (b"defmethod".as_slice(), Operator::Method),
        (b"defclass".as_slice(), Operator::Class),
        (b"defpackage".as_slice(), Operator::Package),
        (b"defvar".as_slice(), Operator::Variable),
        (b"defparameter".as_slice(), Operator::Variable),
        (b"defconstant".as_slice(), Operator::Constant),
        (b"lambda".as_slice(), Operator::Lambda),
        (b"quote".as_slice(), Operator::Quote),
        (b"function".as_slice(), Operator::Quote),
        (b"progn".as_slice(), Operator::Progn),
        (b"macrolet".as_slice(), Operator::LocalMacro),
        (b"symbol-macrolet".as_slice(), Operator::LocalMacro),
        (b"in-package".as_slice(), Operator::PackageEnvironment),
        (b"let".as_slice(), Operator::Binding),
        (b"let*".as_slice(), Operator::Binding),
        (b"flet".as_slice(), Operator::Binding),
        (b"labels".as_slice(), Operator::Binding),
        (b"destructuring-bind".as_slice(), Operator::Binding),
        (b"multiple-value-bind".as_slice(), Operator::Binding),
        (b"do".as_slice(), Operator::Binding),
        (b"do*".as_slice(), Operator::Binding),
        (b"dolist".as_slice(), Operator::Binding),
        (b"dotimes".as_slice(), Operator::Binding),
        (b"declare".as_slice(), Operator::Binding),
        (b"loop".as_slice(), Operator::Binding),
        (b"prog".as_slice(), Operator::Binding),
        (b"prog*".as_slice(), Operator::Binding),
        (b"handler-case".as_slice(), Operator::Binding),
        (b"handler-bind".as_slice(), Operator::Binding),
        (b"restart-case".as_slice(), Operator::Binding),
        (b"restart-bind".as_slice(), Operator::Binding),
        (b"with-slots".as_slice(), Operator::Binding),
        (b"with-accessors".as_slice(), Operator::Binding),
        (b"with-open-file".as_slice(), Operator::Binding),
        (b"with-open-stream".as_slice(), Operator::Binding),
        (b"with-input-from-string".as_slice(), Operator::Binding),
        (b"with-output-to-string".as_slice(), Operator::Binding),
        (b"defstruct".as_slice(), Operator::Unsupported),
        (b"deftype".as_slice(), Operator::Unsupported),
        (b"defsetf".as_slice(), Operator::Unsupported),
        (b"define-setf-expander".as_slice(), Operator::Unsupported),
        (b"define-condition".as_slice(), Operator::Unsupported),
        (
            b"define-method-combination".as_slice(),
            Operator::Unsupported,
        ),
        (b"define-compiler-macro".as_slice(), Operator::Unsupported),
    ] {
        if bytes.eq_ignore_ascii_case(name) {
            return Some(op);
        }
    }
    None
}

fn single_field<'tree>(
    node: Node<'tree>,
    field: &str,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    let mut found = None;
    for child in node.children_by_field_name(field, &mut cursor) {
        cancellation.check()?;
        if !child.is_named() || found.replace(child).is_some() {
            return Ok(None);
        }
    }
    Ok(found)
}

fn value<'tree>(
    node: Node<'tree>,
    index: usize,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    for (offset, child) in node
        .children_by_field_name("value", &mut cursor)
        .enumerate()
    {
        cancellation.check()?;
        // Anonymous dispatch pieces are separate native children, not a clean atom.
        if !child.is_named() {
            return Ok(None);
        }
        if offset == index {
            return Ok(Some(child));
        }
    }
    Ok(None)
}

fn method_lambda<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    for (offset, child) in node
        .children_by_field_name("value", &mut cursor)
        .enumerate()
    {
        cancellation.check()?;
        if !child.is_named() {
            return Ok(None);
        }
        if offset >= 2 && !matches!(child.kind(), "kwd_lit" | "sym_lit") {
            return Ok(Some(child));
        }
    }
    Ok(None)
}

fn contents<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Node<'tree>, AdapterError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        cancellation.check()?;
        if child.kind() == "defun" && child.byte_range() == node.byte_range() {
            return Ok(child);
        }
    }
    Ok(node)
}

fn reader_prefix(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    let mut previous = node.prev_sibling();
    while let Some(child) = previous {
        cancellation.check()?;
        if matches!(child.kind(), "comment" | "block_comment") {
            previous = child.prev_sibling();
            continue;
        }
        // An anonymous read-eval prefix can leave a feature marker folded into
        // a native symbol. Its following target is not an independent form.
        if child.kind() == "sym_lit"
            && source
                .get(child.byte_range())
                .is_some_and(|bytes| bytes.starts_with(b".#+") || bytes.starts_with(b".#-"))
            && child.prev_sibling().is_some_and(|prefix| {
                !prefix.is_named() && source.get(prefix.byte_range()) == Some(b"#".as_slice())
            })
        {
            return Ok(true);
        }
        return Ok(!child.is_named()
            && source.get(child.byte_range()).is_some_and(|bytes| {
                bytes == b"#"
                    || bytes == b"."
                        && child.prev_sibling().is_some_and(|prefix| {
                            !prefix.is_named()
                                && source.get(prefix.byte_range()) == Some(b"#".as_slice())
                        })
            }));
    }
    Ok(false)
}

fn parameters(
    lambda: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<(usize, bool), AdapterError> {
    let mut end = lambda.start_byte();
    let mut cursor = lambda.walk();
    for child in lambda.children_by_field_name("value", &mut cursor) {
        cancellation.check()?;
        if !atomic_name(child, source, false)
            || source
                .get(child.byte_range())
                .is_some_and(|bytes| bytes.starts_with(b"&"))
        {
            return Ok((end, true));
        }
        end = child.end_byte();
    }
    Ok((end, false))
}

fn form_proof(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    prefix: bool,
) -> Result<FormProof, AdapterError> {
    let inner = contents(node, cancellation)?;
    let mut cursor = inner.walk();
    let mut header = None;
    let mut reader_content = false;
    let mut reader_head = false;
    let mut head_seen = false;
    for child in inner.children(&mut cursor) {
        cancellation.check()?;
        let dispatch = !child.is_named() && source.get(child.byte_range()) == Some(b"#".as_slice());
        reader_content |= dispatch;
        reader_head |= dispatch && !head_seen;
        head_seen |= child.is_named() && !matches!(child.kind(), "comment" | "block_comment");
        if child.kind() == "defun_header" {
            header = Some(child);
        }
    }
    let op_node = if let Some(header) = header {
        single_field(header, "keyword", cancellation)?
    } else {
        value(inner, 0, cancellation)?
    };
    let op = op_node.and_then(|node| operator(node, source));
    let mut result = FormProof {
        operator: op,
        owner: None,
        reader_prefix: prefix,
        reader_content,
        reader_head,
    };
    let Some(op) = op.filter(|op| op.syntax().is_some()) else {
        return Ok(result);
    };
    if prefix
        || !clean(node, source)
        || !source
            .get(node.byte_range())
            .is_some_and(|bytes| bytes.first() == Some(&b'(') && bytes.last() == Some(&b')'))
        || inner
            .child_by_field_name("open")
            .is_some_and(|open| open.kind() == "vec_lit")
    {
        return Ok(result);
    }
    let name = if let Some(header) = header {
        single_field(header, "function_name", cancellation)?
    } else {
        value(inner, 1, cancellation)?
    };
    let Some(name) = name.filter(|name| atomic_name(*name, source, op == Operator::Package)) else {
        return Ok(result);
    };
    let mut lambda = None;
    let mut header_end = name.end_byte();
    let mut required_end = name.end_byte();
    let mut complex_parameters = false;
    if op.has_parameters() {
        lambda = if let Some(header) = header {
            single_field(header, "lambda_list", cancellation)?
        } else if op == Operator::Method {
            method_lambda(inner, cancellation)?
        } else {
            value(inner, 2, cancellation)?
        };
        let Some(args) = lambda.filter(|args| args.kind() == "list_lit" && clean(*args, source))
        else {
            return Ok(result);
        };
        if reader_prefix(args, source, cancellation)? {
            return Ok(result);
        }
        (required_end, complex_parameters) = parameters(args, source, cancellation)?;
        header_end = args.end_byte();
    } else if op == Operator::Class {
        let Some(superclasses) = value(inner, 2, cancellation)?
            .filter(|value| value.kind() == "list_lit" && clean(*value, source))
        else {
            return Ok(result);
        };
        header_end = superclasses.end_byte();
    }
    result.owner = Some(OwnerProof {
        name: name.byte_range(),
        header: node.start_byte()..header_end,
        lambda: lambda.map(|node| node.byte_range()),
        required_end,
        complex_parameters,
        operator: op,
    });
    Ok(result)
}

fn data_node(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "quoting_lit"
            | "var_quoting_lit"
            | "syn_quoting_lit"
            | "unquoting_lit"
            | "unquote_splicing_lit"
            | "dis_expr"
            | "str_lit"
            | "comment"
            | "block_comment"
            | "vec_lit"
            | "set_lit"
            | "meta_lit"
            | "read_cond_lit"
            | "splicing_read_cond_lit"
    )
}

fn allowed_context(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
    declaration: bool,
) -> Result<bool, AdapterError> {
    Ok(cache
        .context(node, source, cancellation)?
        .is_some_and(|context| !context.blocked && (!declaration || context.declarations)))
}

fn formal_owner(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<OwnerProof>, AdapterError> {
    let Some(args) = node.parent().filter(|parent| parent.kind() == "list_lit") else {
        return Ok(None);
    };
    let mut parent = args.parent();
    while let Some(current) = parent {
        cancellation.check()?;
        if current.kind() == "list_lit" {
            let Some(form) = cache.form(current, source, cancellation)? else {
                return Ok(None);
            };
            return Ok(form.owner.filter(|proof| {
                proof.lambda.as_ref() == Some(&args.byte_range())
                    && node.end_byte() <= proof.required_end
            }));
        }
        if !matches!(current.kind(), "defun" | "defun_header") {
            return Ok(None);
        }
        parent = current.parent();
    }
    Ok(None)
}

fn reference(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    if !atomic_name(node, source, false)
        || cache.prefix(node, source, cancellation)?
        || node
            .parent()
            .is_some_and(|parent| parent.kind() == "package_lit")
    {
        return Ok(false);
    }
    Ok(cache
        .context(node, source, cancellation)?
        .is_some_and(|context| {
            !context.blocked
                && context
                    .body
                    .is_some_and(|body| body.reads && node.start_byte() >= body.header_end)
        }))
}

fn expression(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<&'static str>, AdapterError> {
    let Some(context) = cache.context(node, source, cancellation)? else {
        return Ok(None);
    };
    if context.data {
        return Ok(None);
    }
    if matches!(
        node.kind(),
        "read_cond_lit" | "splicing_read_cond_lit" | "dis_expr" | "meta_lit"
    ) {
        return Ok(Some("common_lisp.reader_context"));
    }
    if node.kind() == "ERROR" {
        return Ok(Some("common_lisp.unsupported_owner"));
    }
    if node.kind() == "include_reader_macro" {
        return Ok(Some(if cache.prefix(node, source, cancellation)? {
            "common_lisp.reader_context"
        } else {
            "common_lisp.conditional_selection"
        }));
    }
    if node.kind() != "list_lit" {
        return Ok(None);
    }
    let Some(form) = cache.form(node, source, cancellation)? else {
        return Ok(None);
    };
    if form.reader_prefix || form.reader_content {
        return Ok(Some("common_lisp.reader_context"));
    }
    if matches!(form.operator, Some(Operator::Quote)) {
        return Ok(None);
    }
    if form.operator == Some(Operator::LocalMacro) {
        return Ok(Some("common_lisp.local_macro_context"));
    }
    if form.operator == Some(Operator::PackageEnvironment)
        || form.operator == Some(Operator::Package)
    {
        return Ok(Some(
            if form.operator == Some(Operator::Package) && form.owner.is_none() {
                "common_lisp.unsupported_owner"
            } else {
                "common_lisp.package_environment"
            },
        ));
    }
    if form.operator == Some(Operator::Lambda) {
        return Ok(Some("common_lisp.parameter_context"));
    }
    if let Some(formal) = node.parent() {
        let mut parent = Some(formal);
        while let Some(current) = parent {
            cancellation.check()?;
            if current.kind() == "list_lit" {
                if let Some(owner) = cache
                    .form(current, source, cancellation)?
                    .and_then(|form| form.owner)
                {
                    if owner.lambda.as_ref() == Some(&node.byte_range()) {
                        return Ok(owner
                            .complex_parameters
                            .then_some("common_lisp.parameter_context"));
                    }
                    if owner.operator == Operator::Class && node.start_byte() >= owner.header.end {
                        return Ok(Some("common_lisp.unsupported_owner"));
                    }
                }
                break;
            }
            if !matches!(current.kind(), "defun_header" | "defun") {
                break;
            }
            parent = current.parent();
        }
    }
    if form.operator.is_some_and(|op| op.syntax().is_some()) {
        return Ok(Some(
            if form.owner.is_none() || !allowed_context(node, source, cancellation, cache, true)? {
                "common_lisp.unsupported_owner"
            } else {
                "common_lisp.body_semantics"
            },
        ));
    }
    Ok(match form.operator {
        Some(Operator::Binding) => Some("common_lisp.parameter_context"),
        Some(Operator::Unsupported) => Some("common_lisp.unsupported_owner"),
        _ => None,
    })
}

/// Admits only complete source identities and body reads with physical context.
pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if node.kind() == "source" {
        return Ok(true);
    }
    if role == StructuralRole::Expression {
        return Ok(expression(node, source, cancellation, cache)?.is_some());
    }
    if matches!(
        role,
        StructuralRole::Comment | StructuralRole::StringLiteral
    ) {
        return Ok(clean(node, source));
    }
    if role == StructuralRole::Reference {
        return reference(node, source, cancellation, cache);
    }
    if !clean(node, source) {
        return Ok(false);
    }
    if node.kind() == "list_lit" {
        return Ok(cache
            .form(node, source, cancellation)?
            .is_some_and(|form| form.owner.is_some())
            && allowed_context(node, source, cancellation, cache, true)?);
    }
    if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition
    ) && atomic_name(node, source, false)
        && let Some(owner) = formal_owner(node, source, cancellation, cache)?
    {
        let mut current = node.parent();
        while let Some(parent) = current {
            cancellation.check()?;
            if parent.kind() == "list_lit" && parent.start_byte() == owner.header.start {
                return allowed_context(parent, source, cancellation, cache, true);
            }
            current = parent.parent();
        }
    }
    Ok(false)
}

/// Keeps byte-exact names and headers; no normalized reader spelling is created.
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<QueryCandidate, AdapterError> {
    cancellation.check()?;
    let mut range = node.byte_range();
    let syntax = if node.kind() == "source" {
        range = 0..source.len();
        "common_lisp.file"
    } else if role == StructuralRole::Expression {
        expression(node, source, cancellation, cache)?
            .ok_or_else(|| query_failure("query-common-lisp-expression-proof-missing"))?
    } else if role == StructuralRole::Reference {
        "common_lisp.symbol"
    } else if matches!(role, StructuralRole::Comment) {
        "common_lisp.comment"
    } else if role == StructuralRole::StringLiteral {
        "common_lisp.string"
    } else if node.kind() == "list_lit" {
        let proof = cache
            .form(node, source, cancellation)?
            .and_then(|form| form.owner)
            .ok_or_else(|| query_failure("query-common-lisp-owner-proof-missing"))?;
        match role {
            StructuralRole::Definition => {
                range = proof.name;
                "common_lisp.name"
            }
            StructuralRole::Signature => {
                range = proof.header;
                "common_lisp.header"
            }
            StructuralRole::Declaration | StructuralRole::Scope => proof
                .operator
                .syntax()
                .ok_or_else(|| query_failure("query-common-lisp-kind-proof-missing"))?,
            _ => return Err(query_failure("query-common-lisp-unreviewed-role")),
        }
    } else {
        formal_owner(node, source, cancellation, cache)?
            .ok_or_else(|| query_failure("query-common-lisp-parameter-proof-missing"))?;
        if role == StructuralRole::Definition {
            "common_lisp.name"
        } else {
            "common_lisp.parameter"
        }
    };
    let depth = native_depth(node, cancellation)?;
    Ok(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: if role == StructuralRole::Scope && node.kind() != "source" {
            depth
                .checked_add(1)
                .ok_or_else(|| query_failure("query-common-lisp-depth-overflow"))?
        } else {
            depth
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn extract(source: &str) -> (tree_sitter::Tree, Vec<QueryCandidate>) {
        let family = crate::GrammarFamily::CommonLisp;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::registry::language_for(family))
            .unwrap();
        let tree = parser.parse(source.as_bytes(), None).unwrap();
        let pack =
            super::super::QueryPack::compile(family, include_str!("../../queries/common_lisp.scm"))
                .unwrap();
        let extraction = pack
            .extract(
                family,
                &tree,
                source.as_bytes(),
                10_000,
                1024,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(extraction.limit, None);
        let mut candidates = extraction.candidates;
        candidates.sort_unstable_by_key(|candidate| candidate.retention_rank());
        candidates.dedup_by(|left, right| left.same_fact(*right));
        for candidate in &candidates {
            assert!(source.get(candidate.start..candidate.end).is_some());
            assert!(!matches!(
                candidate.role,
                StructuralRole::Call
                    | StructuralRole::CallName
                    | StructuralRole::ScopedCall
                    | StructuralRole::Import
            ));
        }
        (tree, candidates)
    }

    fn definitions<'a>(source: &'a str, candidates: &[QueryCandidate]) -> Vec<&'a str> {
        candidates
            .iter()
            .filter(|candidate| candidate.role == StructuralRole::Definition)
            .map(|candidate| {
                assert_eq!(candidate.syntax, "common_lisp.name");
                &source[candidate.start..candidate.end]
            })
            .collect()
    }

    #[test]
    fn physical_specialized_and_generic_owners_project_complete_headers_once() {
        let source =
            "(defun |Mixed Name| (argument) argument)\n(CL:DEFMACRO BUILD (INPUT) INPUT)\n";
        let (tree, captures) = extract(source);
        assert!(!tree.root_node().has_error());
        let names = definitions(source, &captures);
        for name in ["|Mixed Name|", "argument", "BUILD", "INPUT"] {
            assert_eq!(
                names.iter().filter(|candidate| **candidate == name).count(),
                1
            );
        }
        assert_eq!(names.len(), 4);
        let headers = captures
            .iter()
            .filter(|capture| capture.role == StructuralRole::Signature)
            .map(|capture| {
                assert_eq!(capture.syntax, "common_lisp.header");
                &source[capture.start..capture.end]
            })
            .collect::<Vec<_>>();
        assert_eq!(
            headers,
            [
                "(defun |Mixed Name| (argument)",
                "(CL:DEFMACRO BUILD (INPUT)"
            ]
        );
        assert_eq!(
            captures
                .iter()
                .filter(|capture| capture.role == StructuralRole::Declaration
                    && capture.syntax == "common_lisp.function")
                .count(),
            1
        );
    }

    #[test]
    fn discarded_quoted_and_local_macro_data_cannot_leak_written_definitions() {
        let source = "'(defun quoted () nil)\n(quote (defun explicit () nil))\n\
            `(defun template (,name) ,body)\n#_(defun discarded () nil)\n\
            (macrolet ((defun (name args &body body) `(list ',name ',args))) (defun shadowed () nil))\n\
            (defun live () nil)\n";
        let (tree, captures) = extract(source);
        assert!(!tree.root_node().has_error());
        assert_eq!(definitions(source, &captures), ["live"]);
        let entity_gaps = captures
            .iter()
            .filter(|capture| {
                capture.role == StructuralRole::Expression
                    && matches!(
                        capture.syntax,
                        "common_lisp.unsupported_owner"
                            | "common_lisp.local_macro_context"
                            | "common_lisp.parameter_context"
                            | "common_lisp.reader_context"
                    )
            })
            .collect::<Vec<_>>();
        assert_eq!(entity_gaps.len(), 2);
        assert!(
            entity_gaps
                .iter()
                .any(|capture| capture.syntax == "common_lisp.local_macro_context")
        );
    }

    #[test]
    fn reader_conditions_retain_only_physical_targets_and_preserve_selection_gap() {
        let source = "#+feature (defun positive () nil)\n#-(defun condition-data () nil) (defun negative () nil)\n";
        let (tree, captures) = extract(source);
        assert!(!tree.root_node().has_error());
        assert_eq!(definitions(source, &captures), ["positive", "negative"]);
        assert_eq!(
            captures
                .iter()
                .filter(|capture| capture.role == StructuralRole::Expression
                    && capture.syntax == "common_lisp.conditional_selection")
                .count(),
            2
        );
    }

    #[test]
    fn complex_formals_specializers_and_aliases_are_not_body_reads() {
        let source = "(defun combine (first &optional (second default supplied) &key ((:external internal) initial)) first)\n\
            (defmethod measure :around ((item box)) (call-next-method))\n";
        let (tree, captures) = extract(source);
        assert!(!tree.root_node().has_error());
        let names = definitions(source, &captures);
        assert_eq!(names, ["combine", "first", "measure"]);
        let reads = captures
            .iter()
            .filter(|capture| capture.role == StructuralRole::Reference)
            .map(|capture| &source[capture.start..capture.end])
            .collect::<Vec<_>>();
        assert_eq!(reads, ["first", "call-next-method"]);
        assert_eq!(
            captures
                .iter()
                .filter(|capture| capture.syntax == "common_lisp.parameter_context")
                .count(),
            2
        );
    }

    #[test]
    fn invisible_dispatch_non_atomic_names_and_recovery_cannot_borrow_clean_children() {
        for rejected in [
            "#.(defun evaluated () nil)",
            "(defun #:uninterned () nil)",
            "(defun (setf cell) (value) value)",
            "(defpackage \"string-package\" (:export :item))",
        ] {
            let source = format!("{rejected}\n(defun stable () nil)\n");
            let (_, captures) = extract(&source);
            assert_eq!(definitions(&source, &captures), ["stable"], "{rejected}");
            assert!(
                captures
                    .iter()
                    .any(|capture| capture.role == StructuralRole::Expression
                        && matches!(
                            capture.syntax,
                            "common_lisp.unsupported_owner" | "common_lisp.reader_context"
                        )),
                "{rejected}"
            );
        }
        let source = "(defun broken (item\n(defun borrowed (value) value)\n";
        let (tree, captures) = extract(source);
        assert!(tree.root_node().has_error());
        assert!(definitions(source, &captures).is_empty());
        assert!(
            captures
                .iter()
                .any(|capture| capture.syntax == "common_lisp.unsupported_owner")
        );
    }

    #[test]
    fn dispatch_prefix_propagates_through_feature_reader_wrappers() {
        let source = "#.#+feature (defun leaked () nil)\n(defun stable () nil)\n";
        let (tree, captures) = extract(source);
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let wrapper = tree.root_node().named_child(0).unwrap();
        assert_eq!(
            definitions(source, &captures),
            ["stable"],
            "tree={} wrapper={:?} marker={:?}",
            tree.root_node().to_sexp(),
            wrapper.byte_range(),
            wrapper
                .child_by_field_name("marker")
                .map(|node| (node.byte_range(), &source[node.byte_range()]))
        );
        assert!(
            captures
                .iter()
                .any(|capture| capture.syntax == "common_lisp.reader_context")
        );
    }

    #[test]
    fn internal_standard_package_quote_and_local_macro_forms_fence_reads() {
        let source = "(defun live () (CL::QUOTE secret)\n\
            (COMMON-LISP::MACROLET ((local (formal) formal)) (local 1)) visible)\n";
        let (tree, captures) = extract(source);
        assert!(!tree.root_node().has_error());
        assert_eq!(definitions(source, &captures), ["live"]);
        let reads = captures
            .iter()
            .filter(|capture| capture.role == StructuralRole::Reference)
            .map(|capture| &source[capture.start..capture.end])
            .collect::<Vec<_>>();
        assert_eq!(reads, ["visible"]);
        assert!(
            captures
                .iter()
                .any(|capture| capture.syntax == "common_lisp.local_macro_context")
        );
    }

    #[test]
    fn reader_evaluated_list_heads_do_not_turn_data_into_body_reads() {
        let source = "(defun live () (#.'quote secret) visible)\n";
        let (tree, captures) = extract(source);
        assert!(!tree.root_node().has_error());
        assert_eq!(definitions(source, &captures), ["live"]);
        let reads = captures
            .iter()
            .filter(|capture| capture.role == StructuralRole::Reference)
            .map(|capture| &source[capture.start..capture.end])
            .collect::<Vec<_>>();
        assert_eq!(reads, ["visible"]);
        assert!(
            captures
                .iter()
                .any(|capture| capture.syntax == "common_lisp.reader_context")
        );
    }

    #[test]
    fn reader_prefix_cache_reuses_comment_scans_and_checks_cancellation() {
        let source = format!("#.{}(defun target () nil)", "; prefix trivia\n".repeat(64));
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::registry::language_for(
                crate::GrammarFamily::CommonLisp,
            ))
            .unwrap();
        let tree = parser.parse(source.as_bytes(), None).unwrap();
        assert!(!tree.root_node().has_error());
        let mut cursor = tree.root_node().walk();
        let form = tree
            .root_node()
            .named_children(&mut cursor)
            .find(|node| node.kind() == "list_lit")
            .unwrap();
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(1);
        for _ in 0..8 {
            assert!(
                cache
                    .prefix(form, source.as_bytes(), &cancellation)
                    .unwrap()
            );
            assert_eq!(cache.entries.len(), 1);
            assert_eq!(cache.entries[&form.id()].prefix, Some(true));
        }
        assert!(!cache.exhausted());
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(
            cache
                .prefix(form, source.as_bytes(), &cancellation)
                .is_err()
        );
        assert!(reader_prefix(form, source.as_bytes(), &cancellation).is_err());
    }

    #[test]
    fn context_memo_reuses_each_native_node_and_shares_the_existing_bound() {
        let source = format!("{}nil{}", "(list ".repeat(64), ")".repeat(64));
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::registry::language_for(
                crate::GrammarFamily::CommonLisp,
            ))
            .unwrap();
        let tree = parser.parse(source.as_bytes(), None).unwrap();
        assert!(!tree.root_node().has_error());
        let mut forms = Vec::new();
        let mut current = tree.root_node().named_child(0).unwrap();
        loop {
            forms.push(current);
            let Some(next) = current
                .named_child(1)
                .filter(|node| node.kind() == "list_lit")
            else {
                break;
            };
            current = next;
        }
        assert_eq!(forms.len(), 64);
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(forms.len() + 1);
        for form in forms.iter().rev() {
            assert!(
                cache
                    .context(*form, source.as_bytes(), &cancellation)
                    .unwrap()
                    .is_some()
            );
        }
        assert_eq!(cache.entries.len(), forms.len() + 1);
        assert!(cache.entries.values().all(|entry| entry.context.is_some()));
        for _ in 0..8 {
            for form in &forms {
                assert!(
                    cache
                        .context(*form, source.as_bytes(), &cancellation)
                        .unwrap()
                        .is_some()
                );
            }
            assert_eq!(cache.entries.len(), forms.len() + 1);
        }
        assert!(!cache.exhausted());
        let mut bounded = CaptureCache::new(1);
        assert!(
            bounded
                .context(tree.root_node(), source.as_bytes(), &cancellation)
                .unwrap()
                .is_some()
        );
        assert!(
            bounded
                .context(forms[0], source.as_bytes(), &cancellation)
                .unwrap()
                .is_none()
        );
        assert!(bounded.exhausted());
        assert_eq!(bounded.entries.len(), 1);
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(
            cache
                .context(forms[0], source.as_bytes(), &cancellation)
                .is_err()
        );
    }

    #[test]
    fn cache_reuses_native_proofs_and_fails_closed_at_the_capture_bound() {
        let source = b"(defun first () nil) (defun second () nil)";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::registry::language_for(
                crate::GrammarFamily::CommonLisp,
            ))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let mut cursor = tree.root_node().walk();
        let forms = tree
            .root_node()
            .named_children(&mut cursor)
            .filter(|node| node.kind() == "list_lit")
            .collect::<Vec<_>>();
        assert_eq!(forms.len(), 2);
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(1);
        for _ in 0..8 {
            assert!(
                cache
                    .form(forms[0], source, &cancellation)
                    .unwrap()
                    .unwrap()
                    .owner
                    .is_some()
            );
            assert_eq!(cache.entries.len(), 1);
        }
        assert!(
            cache
                .form(forms[1], source, &cancellation)
                .unwrap()
                .is_none()
        );
        assert!(cache.exhausted());
        assert_eq!(cache.entries.len(), 1);
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(cache.form(forms[0], source, &cancellation).is_err());
    }
}
