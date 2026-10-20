//! Certify one direct written call in an ordinary top-level Scheme procedure.
//! Complete native binding census and mutation/reader barriers precede admission;
//! no library resolution, macro expansion or runtime target is inferred.

use std::collections::{HashMap, HashSet};

use super::*;

pub(super) struct Census {
    sites: HashSet<(usize, usize)>,
}

impl Census {
    fn empty() -> Self {
        Self {
            sites: HashSet::new(),
        }
    }

    pub(super) fn contains(&self, node: Node<'_>) -> bool {
        self.sites.contains(&(node.start_byte(), node.end_byte()))
    }

    pub(super) fn new(
        cache: &mut CaptureCache,
        root: Node<'_>,
        source: &[u8],
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        if !cache.complete_source
            || root.kind() != "program"
            || !clean(root, source)
            || !(root.start_byte() == 0
                || root.start_byte() == 3 && source.starts_with(b"\xef\xbb\xbf"))
            || root.end_byte() != source.len()
        {
            return Ok(Self::empty());
        }
        let mut bindings = HashMap::<String, Option<Span>>::new();
        let mut procedures = Vec::new();
        let mut cursor = root.walk();
        for (index, form) in root.named_children(&mut cursor).enumerate() {
            cancel.check()?;
            if index >= cache.maximum_entries {
                cache.exhausted = true;
                return Ok(Self::empty());
            }
            if intertoken(form) || data_node(form) {
                continue;
            }
            let Some(proof) = cache.form(form, source, cancel)? else {
                return Ok(Self::empty());
            };
            if proof.operation == Operation::Data {
                continue;
            }
            if !proof.valid || !cache.owner_allowed(form, proof, source, cancel)? {
                return Ok(Self::empty());
            }
            let Some(owner) = proof.owner else {
                return Ok(Self::empty());
            };
            let Some(name) = bare(source.get(owner.name.start..owner.name.end)) else {
                return Ok(Self::empty());
            };
            if intrinsic_name(name) {
                return Ok(Self::empty());
            }
            match proof.operation {
                Operation::Define => {
                    let procedure = owner.syntax == "scheme.procedure"
                        && procedure_formals(cache, form, source, cancel)?.is_some();
                    if owner.syntax == "scheme.procedure" && !procedure {
                        return Ok(Self::empty());
                    }
                    if !insert(
                        &mut bindings,
                        &[name],
                        procedure.then_some(Span::of(form)),
                        cache,
                    )? {
                        return Ok(Self::empty());
                    }
                    if procedure {
                        procedures
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        procedures.push((form, name));
                    }
                }
                Operation::Record => {
                    if !record_names(cache, form, source, &mut bindings, cancel)? {
                        return Ok(Self::empty());
                    }
                }
                _ => return Ok(Self::empty()),
            }
        }
        // Reader data is opaque. Active mutation or expansion may hide a donor,
        // so these barriers run over the complete native tree before any site is kept.
        let mut visits = 0usize;
        let mut cursor = root.walk();
        loop {
            cancel.check()?;
            visits = visits
                .checked_add(1)
                .ok_or_else(|| query_failure("query-scheme-call-work"))?;
            if visits > cache.maximum_entries {
                cache.exhausted = true;
                return Ok(Self::empty());
            }
            let current = cursor.node();
            let mut opaque = data_node(current);
            if current.kind() == "directive" || current.is_error() || current.is_missing() {
                return Ok(Self::empty());
            }
            let executable = cache
                .context(current, source, cancel)?
                .is_some_and(|context| !context.data && !context.blocked && context.executable);
            // Evaluated templates and reader graph nodes need evidence this
            // finite proof cannot follow; refuse them before data opacity.
            if executable
                && matches!(
                    current.kind(),
                    "quasiquote"
                        | "unquote"
                        | "unquote_splicing"
                        | "quasisyntax"
                        | "unsyntax"
                        | "unsyntax_splicing"
                        | "datum_label"
                        | "datum_reference"
                )
            {
                return Ok(Self::empty());
            }
            if current.kind() == "symbol"
                && cache.name(current, source, cancel)?.is_some_and(|proof| {
                    matches!(proof.syntax, "scheme.parameter" | "scheme.variable")
                })
                && bare(source.get(current.byte_range())).is_none_or(intrinsic_name)
            {
                return Ok(Self::empty());
            }
            if current.kind() == "list" && !opaque && executable {
                let Some(parts) = cache.direct(current, source, cancel)? else {
                    return Ok(Self::empty());
                };
                let operation = parts
                    .first()
                    .map_or(Operation::Other, |head| operation(*head, source));
                let Some(form) = cache.form(current, source, cancel)? else {
                    return Ok(Self::empty());
                };
                if !form.valid
                    || form.owner.is_some_and(|owner| {
                        bare(source.get(owner.name.start..owner.name.end))
                            .is_none_or(intrinsic_name)
                    })
                    || operation == Operation::Define
                        && form
                            .owner
                            .is_some_and(|owner| owner.syntax == "scheme.procedure")
                        && procedure_formals(cache, current, source, cancel)?.is_none()
                {
                    return Ok(Self::empty());
                }
                if operation == Operation::Lambda {
                    let Some(formals) = parts.get(1) else {
                        return Ok(Self::empty());
                    };
                    if formal_names(cache, *formals, false, source, cancel)?.is_none() {
                        return Ok(Self::empty());
                    }
                }
                opaque |= matches!(operation, Operation::Data | Operation::Template);
                if matches!(
                    operation,
                    Operation::Macro | Operation::LocalMacro | Operation::Unsupported
                ) || operation == Operation::Record && current.parent() != Some(root)
                {
                    return Ok(Self::empty());
                }
                let head = parts
                    .first()
                    .and_then(|head| bare(source.get(head.byte_range())));
                // These standard binders have no reviewed native owner proof
                // here; their bodies cannot inherit the default intrinsic names.
                if head.is_some_and(unsupported_binding_name) {
                    return Ok(Self::empty());
                }
                if matches!(
                    head,
                    Some(
                        "quasiquote"
                            | "unquote"
                            | "unquote-splicing"
                            | "quasisyntax"
                            | "unsyntax"
                            | "unsyntax-splicing"
                    )
                ) {
                    return Ok(Self::empty());
                }
                if matches!(head, Some("eval" | "load" | "include" | "include-ci")) {
                    return Ok(Self::empty());
                }
                if head == Some("set!") {
                    let Some(name) = parts
                        .get(1)
                        .and_then(|name| bare(source.get(name.byte_range())))
                    else {
                        return Ok(Self::empty());
                    };
                    if let Some(binding) = bindings.get_mut(name) {
                        *binding = None;
                    }
                }
            }
            if !opaque && cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    if cache.exhausted {
                        return Ok(Self::empty());
                    }
                    let mut sites = HashSet::new();
                    sites
                        .try_reserve(procedures.len())
                        .map_err(|_| SinkError::AllocationFailed)?;
                    for (procedure, name) in procedures {
                        cancel.check()?;
                        if bindings.get(name).copied().flatten().is_none_or(|span| {
                            span.start != procedure.start_byte() || span.end != procedure.end_byte()
                        }) {
                            continue;
                        }
                        if let Some(head) =
                            wrapper_head(cache, procedure, source, &bindings, cancel)?
                        {
                            sites.insert((head.start_byte(), head.end_byte()));
                        }
                        if cache.exhausted {
                            return Ok(Self::empty());
                        }
                    }
                    return Ok(Self { sites });
                }
            }
        }
    }
}

fn bare(bytes: Option<&[u8]>) -> Option<&str> {
    let text = std::str::from_utf8(bytes?).ok()?;
    (text.is_ascii()
        && !text.bytes().any(|byte| matches!(byte, b'|' | b'\\'))
        && structural_captured_name_for_language("scheme", text, text.len()).is_some())
    .then_some(text)
}

fn intrinsic_name(name: &str) -> bool {
    name == "fields"
        || unsupported_binding_name(name)
        || operation_bytes(Some(name.as_bytes())) != Operation::Other
}

fn unsupported_binding_name(name: &str) -> bool {
    matches!(name, "do" | "case-lambda" | "guard")
}

fn insert(
    bindings: &mut HashMap<String, Option<Span>>,
    pieces: &[&str],
    value: Option<Span>,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let length = pieces
        .iter()
        .try_fold(0usize, |length, piece| length.checked_add(piece.len()))
        .ok_or(SinkError::AccountingOverflow)?;
    let mut name = String::new();
    name.try_reserve_exact(length)
        .map_err(|_| SinkError::AllocationFailed)?;
    for piece in pieces {
        name.push_str(piece);
    }
    if intrinsic_name(&name) {
        return Ok(false);
    }
    if let Some(existing) = bindings.get_mut(&name) {
        *existing = None;
    } else {
        if bindings.len() >= cache.maximum_entries {
            cache.exhausted = true;
            return Ok(false);
        }
        bindings
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        bindings.insert(name, value);
    }
    Ok(true)
}

fn record_names(
    cache: &mut CaptureCache,
    form: Node<'_>,
    source: &[u8],
    bindings: &mut HashMap<String, Option<Span>>,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let Some(parts) = cache.direct(form, source, cancel)? else {
        return Ok(false);
    };
    let [_, name, fields] = parts.as_slice() else {
        return Ok(false);
    };
    let Some(name) = bare(source.get(name.byte_range())) else {
        return Ok(false);
    };
    let Some(fields) = cache.direct(*fields, source, cancel)? else {
        return Ok(false);
    };
    if fields
        .first()
        .and_then(|head| source.get(head.byte_range()))
        != Some(b"fields".as_slice())
    {
        return Ok(false);
    }
    // R6RS implicit record names are bindings too, even though they are not
    // source-authored procedure declarations: https://r6rs.org/final/html/r6rs-lib/r6rs-lib-Z-H-7.html
    if !insert(bindings, &[name], None, cache)?
        || !insert(bindings, &["make-", name], None, cache)?
        || !insert(bindings, &[name, "?"], None, cache)?
    {
        return Ok(false);
    }
    for field in fields.iter().skip(1) {
        cancel.check()?;
        let Some(field) = bare(source.get(field.byte_range())) else {
            return Ok(false);
        };
        if !insert(bindings, &[name, "-", field], None, cache)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn formal_names<'source>(
    cache: &mut CaptureCache,
    formals: Node<'_>,
    skip_name: bool,
    source: &'source [u8],
    cancel: &Cancellation,
) -> Result<Option<HashSet<&'source str>>, AdapterError> {
    let Some(formals) = cache.direct(formals, source, cancel)? else {
        return Ok(None);
    };
    if skip_name && formals.is_empty() {
        return Ok(None);
    }
    let mut names = HashSet::new();
    names
        .try_reserve(formals.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for formal in formals.iter().skip(usize::from(skip_name)) {
        cancel.check()?;
        let Some(name) = atomic_name(*formal, source)
            .then(|| bare(source.get(formal.byte_range())))
            .flatten()
        else {
            return Ok(None);
        };
        if intrinsic_name(name) || !names.insert(name) {
            return Ok(None);
        }
    }
    Ok(Some(names))
}

fn procedure_formals<'source>(
    cache: &mut CaptureCache,
    procedure: Node<'_>,
    source: &'source [u8],
    cancel: &Cancellation,
) -> Result<Option<HashSet<&'source str>>, AdapterError> {
    let Some(parts) = cache.direct(procedure, source, cancel)? else {
        return Ok(None);
    };
    let Some(binding) = parts.get(1) else {
        return Ok(None);
    };
    if binding.kind() == "list" {
        return formal_names(cache, *binding, true, source, cancel);
    }
    let [_, _, initializer] = parts.as_slice() else {
        return Ok(None);
    };
    let Some(lambda) = cache.direct(*initializer, source, cancel)? else {
        return Ok(None);
    };
    let [head, formals, _, ..] = lambda.as_slice() else {
        return Ok(None);
    };
    if !atomic_name(*head, source) || source.get(head.byte_range()) != Some(b"lambda".as_slice()) {
        return Ok(None);
    }
    formal_names(cache, *formals, false, source, cancel)
}

fn wrapper_head<'tree>(
    cache: &mut CaptureCache,
    procedure: Node<'tree>,
    source: &[u8],
    bindings: &HashMap<String, Option<Span>>,
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let Some(names) = procedure_formals(cache, procedure, source, cancel)? else {
        return Ok(None);
    };
    let Some(parts) = cache.direct(procedure, source, cancel)? else {
        return Ok(None);
    };
    let [_, binding, body] = parts.as_slice() else {
        return Ok(None);
    };
    let body = if binding.kind() == "list" {
        *body
    } else {
        let Some(lambda) = cache.direct(*body, source, cancel)? else {
            return Ok(None);
        };
        let [head, _, body] = lambda.as_slice() else {
            return Ok(None);
        };
        if source.get(head.byte_range()) != Some(b"lambda".as_slice()) {
            return Ok(None);
        }
        *body
    };
    let Some(call) = cache.direct(body, source, cancel)? else {
        return Ok(None);
    };
    let Some(head) = call.first().copied() else {
        return Ok(None);
    };
    let Some(name) = atomic_name(head, source)
        .then(|| bare(source.get(head.byte_range())))
        .flatten()
    else {
        return Ok(None);
    };
    if names.contains(name) || bindings.get(name).copied().flatten().is_none() {
        return Ok(None);
    }
    for argument in call.iter().skip(1) {
        cancel.check()?;
        if !atomic_name(*argument, source) || bare(source.get(argument.byte_range())).is_none() {
            return Ok(None);
        }
    }
    Ok(Some(head))
}

#[cfg(test)]
mod qualification_tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn parse(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_scheme::LANGUAGE_SCHEME.into())
            .unwrap();
        parser.parse(source.as_bytes(), None).unwrap()
    }

    #[test]
    fn admitted_bom_root_keeps_the_exact_direct_call_certificate() {
        let text = "\u{feff}; \u{394}\u{1f642}\n(define caller (lambda (value) (later value)))\n(define later (lambda (arg) arg))\n";
        let tree = parse(text);
        let root = tree.root_node();
        assert!(!root.has_error());
        assert_eq!(root.start_byte(), 3, "native lexer skips only the BOM");
        assert_eq!(root.end_byte(), text.len());
        let mut cache = CaptureCache::new(4096).with_complete_source(true);
        let census = Census::new(&mut cache, root, text.as_bytes(), &Cancellation::new()).unwrap();
        assert!(!cache.exhausted());
        let start = text.find("(later value)").unwrap() + 1;
        assert_eq!(census.sites.len(), 1);
        assert!(census.sites.contains(&(start, start + "later".len())));
    }

    #[test]
    fn shared_native_work_exhaustion_never_keeps_a_partial_direct_call_set() {
        let text = "(define (caller value) (target value)) (define (target arg) arg)";
        let tree = parse(text);
        let mut cache = CaptureCache::new(3).with_complete_source(true);
        let census = Census::new(
            &mut cache,
            tree.root_node(),
            text.as_bytes(),
            &Cancellation::new(),
        )
        .unwrap();
        assert!(cache.exhausted());
        assert!(census.sites.is_empty());
    }

    #[test]
    fn incomplete_source_and_cancellation_do_not_create_call_certificates() {
        let text = "(define (caller value) (target value)) (define (target arg) arg)";
        let tree = parse(text);
        let mut cache = CaptureCache::new(4096);
        assert!(
            Census::new(
                &mut cache,
                tree.root_node(),
                text.as_bytes(),
                &Cancellation::new()
            )
            .unwrap()
            .sites
            .is_empty()
        );
        let cancelled = Cancellation::new();
        assert!(cancelled.cancel(CancellationReason::ClientRequest));
        let mut cache = CaptureCache::new(4096).with_complete_source(true);
        assert!(matches!(
            Census::new(&mut cache, tree.root_node(), text.as_bytes(), &cancelled),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
