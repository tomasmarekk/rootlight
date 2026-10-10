//! Scheme reader nodes become written identities only after direct-shape proof.
//! Data/template ancestry remains opaque; executable symbol uses stay unresolved.
//! The cache shares caller capture/body limits and never evaluates reader state.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Define,
    Macro,
    Lambda,
    Let,
    LetStar,
    LetRec,
    LetRecStar,
    LetValues,
    LetStarValues,
    Record,
    Begin,
    Data,
    Template,
    LocalMacro,
    Unsupported,
    Other,
}

impl Operation {
    const fn is_binding(self) -> bool {
        matches!(
            self,
            Self::Let
                | Self::LetStar
                | Self::LetRec
                | Self::LetRecStar
                | Self::LetValues
                | Self::LetStarValues
        )
    }

    const fn is_multiple_binding(self) -> bool {
        matches!(self, Self::LetValues | Self::LetStarValues)
    }

    const fn introduces_owner(self) -> bool {
        matches!(self, Self::Define | Self::Macro | Self::Record)
    }
}

#[derive(Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
}

impl Span {
    fn of(node: Node<'_>) -> Self {
        Self {
            start: node.start_byte(),
            end: node.end_byte(),
        }
    }
}

#[derive(Clone, Copy)]
struct Owner {
    name: Span,
    header: Span,
    syntax: &'static str,
}

#[derive(Clone, Copy)]
struct Formals {
    container: usize,
    skipped_name: Option<usize>,
    single: bool,
}

impl Formals {
    fn contains(self, node: Node<'_>) -> bool {
        self.skipped_name != Some(node.id())
            && if self.single {
                self.container == node.id()
            } else {
                node.parent()
                    .is_some_and(|parent| parent.id() == self.container)
            }
    }
}

#[derive(Clone, Copy)]
struct Bindings {
    container: usize,
    multiple: bool,
}

#[derive(Clone, Copy)]
struct FormProof {
    operation: Operation,
    head: Option<usize>,
    valid: bool,
    owner: Option<Owner>,
    formals: Option<Formals>,
    bindings: Option<Bindings>,
    body_start: Option<usize>,
    header_end: Option<usize>,
    initializer: Option<usize>,
    nested_lambda: Option<usize>,
    r7_record: bool,
}

impl FormProof {
    const fn empty(operation: Operation) -> Self {
        Self {
            operation,
            head: None,
            valid: false,
            owner: None,
            formals: None,
            bindings: None,
            body_start: None,
            header_end: None,
            initializer: None,
            nested_lambda: None,
            r7_record: false,
        }
    }
}

#[derive(Clone, Copy)]
struct BindingProof {
    outer: usize,
    names: Formals,
    initializer: usize,
}

#[derive(Clone, Copy)]
struct NameProof {
    syntax: &'static str,
}

#[derive(Clone, Copy)]
struct ContextProof {
    data: bool,
    blocked: bool,
    executable: bool,
    declarations: bool,
}

#[derive(Default)]
struct CacheEntry {
    form: Option<FormProof>,
    context: Option<ContextProof>,
    binding: Option<Option<BindingProof>>,
    name: Option<Option<NameProof>>,
}

/// Memoizes native proofs under the existing caller capture/body bound.
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

    fn direct<'tree>(
        &mut self,
        node: Node<'tree>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<Vec<Node<'tree>>>, AdapterError> {
        cancellation.check()?;
        if !clean(node, source) || node.kind() != "list" || !list_delimiters(node, source) {
            return Ok(None);
        }
        let mut values = Vec::new();
        let mut previous = None;
        let mut count = 0usize;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            cancellation.check()?;
            count = count
                .checked_add(1)
                .ok_or_else(|| query_failure("query-scheme-child-count"))?;
            if count > self.maximum_entries {
                self.exhausted = true;
                return Ok(None);
            }
            if !child.is_named() || intertoken(child) {
                continue;
            }
            if !clean(child, source) {
                return Ok(None);
            }
            if let Some(previous) = previous
                && !separated(previous, child, source)
            {
                return Ok(None);
            }
            values
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            values.push(child);
            previous = Some(child);
        }
        Ok(Some(values))
    }

    fn formals(
        &mut self,
        node: Node<'_>,
        skip_first: bool,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<Formals>, AdapterError> {
        if !skip_first && atomic_name(node, source) {
            return Ok(Some(Formals {
                container: node.id(),
                skipped_name: None,
                single: true,
            }));
        }
        let Some(values) = self.direct(node, source, cancellation)? else {
            return Ok(None);
        };
        let skipped_name = if skip_first {
            let Some(name) = values.first().filter(|name| atomic_name(**name, source)) else {
                return Ok(None);
            };
            Some(name.id())
        } else {
            None
        };
        let mut dot = None;
        for (index, value) in values.iter().enumerate() {
            cancellation.check()?;
            if value.kind() == "dot" {
                if dot.replace(index).is_some() || index == 0 {
                    return Ok(None);
                }
            } else if !atomic_name(*value, source) {
                return Ok(None);
            }
        }
        if let Some(dot) = dot
            && dot.checked_add(2) != Some(values.len())
        {
            return Ok(None);
        }
        Ok(Some(Formals {
            container: node.id(),
            skipped_name,
            single: false,
        }))
    }

    fn binding_shape(
        &mut self,
        pair: Node<'_>,
        multiple: bool,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<(Formals, usize)>, AdapterError> {
        let Some(values) = self.direct(pair, source, cancellation)? else {
            return Ok(None);
        };
        let [names, initializer] = values.as_slice() else {
            return Ok(None);
        };
        let names = if multiple {
            self.formals(*names, false, source, cancellation)?
        } else {
            atomic_name(*names, source).then_some(Formals {
                container: names.id(),
                skipped_name: None,
                single: true,
            })
        };
        Ok(names.map(|names| (names, initializer.id())))
    }

    fn form(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<FormProof>, AdapterError> {
        cancellation.check()?;
        if let Some(proof) = self.entries.get(&node.id()).and_then(|entry| entry.form) {
            return Ok(Some(proof));
        }
        if !self.slot(node.id())? {
            return Ok(None);
        }
        let proof = if let Some(values) = self.direct(node, source, cancellation)? {
            self.form_from_values(node, &values, source, cancellation)?
        } else {
            let mut operation = Operation::Other;
            let mut cursor = node.walk();
            for (index, child) in node.named_children(&mut cursor).enumerate() {
                cancellation.check()?;
                if index >= self.maximum_entries {
                    self.exhausted = true;
                    break;
                }
                if !intertoken(child) {
                    operation = operation_bytes(
                        atomic_name(child, source)
                            .then(|| source.get(child.byte_range()))
                            .flatten(),
                    );
                    break;
                }
            }
            FormProof::empty(operation)
        };
        if self.exhausted {
            return Ok(None);
        }
        self.entries
            .get_mut(&node.id())
            .ok_or_else(|| query_failure("query-scheme-form-slot"))?
            .form = Some(proof);
        Ok(Some(proof))
    }

    fn form_from_values(
        &mut self,
        node: Node<'_>,
        values: &[Node<'_>],
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<FormProof, AdapterError> {
        let written_operation = values
            .first()
            .map_or(Operation::Other, |head| operation(*head, source));
        let mut proof = FormProof::empty(written_operation);
        proof.head = values.first().map(Node::id);
        if values.iter().any(|value| value.kind() == "dot") {
            return Ok(proof);
        }
        let owner = |name: Node<'_>, end, syntax| Owner {
            name: Span::of(name),
            header: Span {
                start: node.start_byte(),
                end,
            },
            syntax,
        };
        match written_operation {
            Operation::Define => {
                let Some(name) = values.get(1).copied() else {
                    return Ok(proof);
                };
                if name.kind() == "list" && values.len() >= 3 {
                    let Some(formals) = self.formals(name, true, source, cancellation)? else {
                        return Ok(proof);
                    };
                    let Some(header) = self.direct(name, source, cancellation)? else {
                        return Ok(proof);
                    };
                    let Some(written_name) = header.first().copied() else {
                        return Ok(proof);
                    };
                    proof.owner = Some(owner(written_name, name.end_byte(), "scheme.procedure"));
                    proof.formals = Some(formals);
                    proof.body_start = values.get(2).map(Node::start_byte);
                } else if atomic_name(name, source) && values.len() == 3 {
                    let initializer = values
                        .get(2)
                        .copied()
                        .ok_or_else(|| query_failure("query-scheme-define-initializer"))?;
                    proof.initializer = Some(initializer.id());
                    let lambda = if let Some(parts) =
                        self.direct(initializer, source, cancellation)?
                        && parts
                            .first()
                            .is_some_and(|head| operation(*head, source) == Operation::Lambda)
                    {
                        self.form(initializer, source, cancellation)?
                    } else {
                        None
                    };
                    if let Some(lambda) = lambda.filter(|lambda| lambda.valid) {
                        let Some(end) = lambda.header_end else {
                            return Ok(proof);
                        };
                        proof.owner = Some(owner(name, end, "scheme.procedure"));
                        proof.formals = lambda.formals;
                        proof.nested_lambda = Some(initializer.id());
                    } else {
                        proof.owner = Some(owner(name, name.end_byte(), "scheme.variable"));
                    }
                }
                proof.valid = proof.owner.is_some();
            }
            Operation::Macro => {
                if let [_, name, _] = values
                    && atomic_name(*name, source)
                {
                    proof.owner = Some(owner(*name, name.end_byte(), "scheme.macro"));
                    proof.valid = true;
                }
            }
            Operation::Lambda => {
                if let Some(written_formals) = values.get(1).copied()
                    && values.len() >= 3
                    && let Some(formals) =
                        self.formals(written_formals, false, source, cancellation)?
                {
                    proof.formals = Some(formals);
                    proof.header_end = Some(written_formals.end_byte());
                    proof.body_start = values.get(2).map(Node::start_byte);
                    proof.valid = true;
                }
            }
            op if op.is_binding() => {
                let mut binding_index = 1;
                if op == Operation::Let
                    && values.get(1).is_some_and(|name| atomic_name(*name, source))
                {
                    binding_index = 2;
                }
                if let Some(bindings) = values.get(binding_index).copied()
                    && let Some(body) = values.get(binding_index + 1)
                    && let Some(pairs) = self.direct(bindings, source, cancellation)?
                {
                    for pair in pairs {
                        if self
                            .binding_shape(pair, op.is_multiple_binding(), source, cancellation)?
                            .is_none()
                        {
                            return Ok(proof);
                        }
                    }
                    proof.bindings = Some(Bindings {
                        container: bindings.id(),
                        multiple: op.is_multiple_binding(),
                    });
                    proof.body_start = Some(body.start_byte());
                    if binding_index == 2 {
                        let name = values
                            .get(1)
                            .copied()
                            .ok_or_else(|| query_failure("query-scheme-named-let"))?;
                        proof.owner = Some(owner(name, bindings.end_byte(), "scheme.procedure"));
                    }
                    proof.valid = true;
                }
            }
            Operation::Record => {
                if let Some(name) = values.get(1).copied() {
                    let written_name = if atomic_name(name, source) {
                        Some(name)
                    } else if let Some(names) = self.direct(name, source, cancellation)? {
                        if names.len() == 3 && names.iter().all(|name| atomic_name(*name, source)) {
                            names.first().copied()
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(written_name) = written_name {
                        let Some(r7_record) =
                            self.record_tail(name, values, source, cancellation)?
                        else {
                            return Ok(proof);
                        };
                        proof.owner =
                            Some(owner(written_name, name.end_byte(), "scheme.record_type"));
                        proof.valid = true;
                        proof.r7_record = r7_record;
                    }
                }
            }
            Operation::Begin | Operation::Other | Operation::Data | Operation::Template => {
                proof.valid = true
            }
            Operation::LocalMacro | Operation::Unsupported => {}
            _ => {}
        }
        Ok(proof)
    }

    fn record_tail(
        &mut self,
        name: Node<'_>,
        values: &[Node<'_>],
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<bool>, AdapterError> {
        // R6RS permits no clauses, including the abbreviated name. Present
        // tails must prove clauses or the distinct R7RS constructor shape.
        let mut r6_record = true;
        for clause in values.iter().skip(2) {
            cancellation.check()?;
            if !self
                .direct(*clause, source, cancellation)?
                .is_some_and(|parts| r6_record_clause(&parts, source))
            {
                r6_record = false;
                break;
            }
        }
        if r6_record {
            return Ok(Some(false));
        }
        if !atomic_name(name, source) {
            return Ok(None);
        }
        let (Some(constructor), Some(predicate)) = (values.get(2), values.get(3)) else {
            return Ok(None);
        };
        let Some(arguments) = self.direct(*constructor, source, cancellation)? else {
            return Ok(None);
        };
        if !atomic_name(*predicate, source)
            || arguments.is_empty()
            || !arguments.iter().all(|name| atomic_name(*name, source))
        {
            return Ok(None);
        }
        for field in values.iter().skip(4) {
            cancellation.check()?;
            let Some(parts) = self.direct(*field, source, cancellation)? else {
                return Ok(None);
            };
            if !(2..=3).contains(&parts.len())
                || !parts.iter().all(|name| atomic_name(*name, source))
            {
                return Ok(None);
            }
        }
        Ok(Some(true))
    }

    fn binding(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<BindingProof>, AdapterError> {
        if let Some(proof) = self.entries.get(&node.id()).and_then(|entry| entry.binding) {
            return Ok(proof);
        }
        if !self.slot(node.id())? {
            return Ok(None);
        }
        let mut proof = None;
        if let Some(container) = node.parent().filter(|parent| parent.kind() == "list")
            && let Some(outer) = container.parent().filter(|parent| parent.kind() == "list")
            && let Some(form) = self.form(outer, source, cancellation)?
            && form.valid
            && let Some(bindings) = form
                .bindings
                .filter(|bindings| bindings.container == container.id())
            && let Some((names, initializer)) =
                self.binding_shape(node, bindings.multiple, source, cancellation)?
        {
            proof = Some(BindingProof {
                outer: outer.id(),
                names,
                initializer,
            });
        }
        self.entries
            .get_mut(&node.id())
            .ok_or_else(|| query_failure("query-scheme-binding-slot"))?
            .binding = Some(proof);
        Ok(proof)
    }

    fn context(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<ContextProof>, AdapterError> {
        let mut chain = Vec::new();
        let mut current = Some(node);
        let mut inherited = ContextProof {
            data: false,
            blocked: false,
            executable: true,
            declarations: true,
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
                inherited = self.edge(parent, current, inherited, source, cancellation)?;
            }
            if !self.slot(current.id())? {
                return Ok(None);
            }
            self.entries
                .get_mut(&current.id())
                .ok_or_else(|| query_failure("query-scheme-context-slot"))?
                .context = Some(inherited);
        }
        Ok(Some(inherited))
    }

    fn edge(
        &mut self,
        parent: Node<'_>,
        child: Node<'_>,
        mut context: ContextProof,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<ContextProof, AdapterError> {
        if context.data || data_node(parent) {
            context.data = true;
            context.executable = false;
            context.declarations = false;
            return Ok(context);
        }
        if context.blocked || parent.is_missing() || parent.kind() == "ERROR" {
            context.blocked = true;
            context.executable = false;
            context.declarations = false;
            return Ok(context);
        }
        if parent.kind() != "list" {
            return Ok(context);
        }
        if let Some(binding) = self.binding(parent, source, cancellation)? {
            let active = self
                .entries
                .get(&binding.outer)
                .and_then(|entry| entry.context)
                .is_some_and(|outer| !outer.data && !outer.blocked && outer.executable);
            context.executable = active && child.id() == binding.initializer;
            context.declarations = false;
            return Ok(context);
        }
        // Formal and binding headers are not expression lists. Their direct
        // symbols acquire identity only through the separately proved owner.
        if !context.executable {
            context.declarations = false;
            return Ok(context);
        }
        let Some(form) = self.form(parent, source, cancellation)? else {
            context.blocked = true;
            context.executable = false;
            context.declarations = false;
            return Ok(context);
        };
        if matches!(
            form.operation,
            Operation::Data | Operation::Template | Operation::Macro
        ) {
            context.data = true;
            context.executable = false;
            context.declarations = false;
        } else if !form.valid {
            context.blocked = true;
            context.executable = false;
            context.declarations = false;
        } else if form.operation == Operation::Begin {
            // Begin is transparent only in the declaration position inherited
            // from its enclosing body, never in an arbitrary initializer.
            context.executable &= form.head != Some(child.id());
            context.declarations &= context.executable;
        } else if form.operation == Operation::Define {
            if form
                .body_start
                .is_some_and(|start| child.start_byte() >= start)
            {
                context.executable &= context.declarations;
                context.declarations = context.executable;
            } else {
                context.executable &= form.initializer == Some(child.id()) && context.declarations;
                context.declarations = false;
            }
        } else if form.operation == Operation::Lambda || form.operation.is_binding() {
            context.executable &= form
                .body_start
                .is_some_and(|start| child.start_byte() >= start);
            context.declarations = context.executable;
        } else if form.operation == Operation::Record {
            context.executable = false;
            context.declarations = false;
        } else {
            context.declarations = false;
        }
        Ok(context)
    }

    fn owner_allowed(
        &mut self,
        node: Node<'_>,
        form: FormProof,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        Ok(form.valid
            && self
                .context(node, source, cancellation)?
                .is_some_and(|context| {
                    !context.data
                        && !context.blocked
                        && if form.operation == Operation::Let {
                            context.executable
                        } else {
                            context.declarations
                        }
                }))
    }

    fn name(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<NameProof>, AdapterError> {
        cancellation.check()?;
        if let Some(proof) = self.entries.get(&node.id()).and_then(|entry| entry.name) {
            return Ok(proof);
        }
        if !self.slot(node.id())? || !atomic_name(node, source) {
            return Ok(None);
        }
        let proof = self.name_uncached(node, source, cancellation)?;
        self.entries
            .get_mut(&node.id())
            .ok_or_else(|| query_failure("query-scheme-name-slot"))?
            .name = Some(proof);
        Ok(proof)
    }

    fn name_uncached(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<NameProof>, AdapterError> {
        let Some(context) = self.context(node, source, cancellation)? else {
            return Ok(None);
        };
        if context.data || context.blocked {
            return Ok(None);
        }
        let mut current = node.parent();
        for _ in 0..3 {
            let Some(parent) = current else {
                break;
            };
            cancellation.check()?;
            if parent.kind() != "list" {
                break;
            }
            if let Some(form) = self.form(parent, source, cancellation)?
                && form.valid
                && form.formals.is_some_and(|formals| formals.contains(node))
                && self
                    .context(parent, source, cancellation)?
                    .is_some_and(|context| !context.data && !context.blocked && context.executable)
                && (!form.operation.introduces_owner()
                    || self.owner_allowed(parent, form, source, cancellation)?)
            {
                return Ok(Some(NameProof {
                    syntax: "scheme.parameter",
                }));
            }
            if let Some(binding) = self.binding(parent, source, cancellation)?
                && binding.names.contains(node)
                && self
                    .entries
                    .get(&binding.outer)
                    .and_then(|entry| entry.context)
                    .is_some_and(|context| !context.data && !context.blocked && context.executable)
            {
                return Ok(Some(NameProof {
                    syntax: "scheme.variable",
                }));
            }
            current = parent.parent();
        }
        Ok(self
            .record_field(node, source, cancellation)?
            .then_some(NameProof {
                syntax: "scheme.field",
            }))
    }

    fn record_field(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let Some(parent) = node.parent().filter(|parent| parent.kind() == "list") else {
            return Ok(false);
        };
        let Some(values) = self.direct(parent, source, cancellation)? else {
            return Ok(false);
        };
        if let Some(record) = parent.parent().filter(|record| record.kind() == "list")
            && let Some(form) = self.form(record, source, cancellation)?
            && form.operation == Operation::Record
            && form.r7_record
        {
            let Some(parts) = self.direct(record, source, cancellation)? else {
                return Ok(false);
            };
            return Ok(self.owner_allowed(record, form, source, cancellation)?
                && (2..=3).contains(&values.len())
                && values.iter().all(|name| atomic_name(*name, source))
                && values.first().is_some_and(|name| name.id() == node.id())
                && parts.iter().skip(4).any(|field| field.id() == parent.id()));
        }
        let head = values
            .first()
            .and_then(|head| source.get(head.byte_range()));
        let (record, explicit) = if head == Some(b"fields".as_slice()) {
            (
                parent.parent(),
                values.iter().skip(1).any(|name| name.id() == node.id()),
            )
        } else if matches!(head, Some(b"mutable" | b"immutable")) {
            let Some(fields) = parent.parent().filter(|parent| parent.kind() == "list") else {
                return Ok(false);
            };
            if self
                .direct(fields, source, cancellation)?
                .and_then(|parts| parts.first().and_then(|head| source.get(head.byte_range())))
                != Some(b"fields".as_slice())
            {
                return Ok(false);
            }
            (
                fields.parent(),
                values.get(1).is_some_and(|name| name.id() == node.id())
                    && r6_field_shape(&values, source)
                    && values.iter().all(|name| atomic_name(*name, source)),
            )
        } else {
            (
                parent.parent(),
                values.first().is_some_and(|name| name.id() == node.id())
                    && (2..=3).contains(&values.len())
                    && values.iter().all(|name| atomic_name(*name, source)),
            )
        };
        let Some(record) = record.filter(|record| record.kind() == "list") else {
            return Ok(false);
        };
        let Some(form) = self.form(record, source, cancellation)? else {
            return Ok(false);
        };
        if form.operation != Operation::Record
            || !self.owner_allowed(record, form, source, cancellation)?
        {
            return Ok(false);
        }
        if head == Some(b"fields".as_slice()) || matches!(head, Some(b"mutable" | b"immutable")) {
            return Ok(explicit);
        }
        let Some(parts) = self.direct(record, source, cancellation)? else {
            return Ok(false);
        };
        Ok(form.r7_record
            && explicit
            && parts.iter().skip(4).any(|field| field.id() == parent.id()))
    }
}

fn r6_record_clause(values: &[Node<'_>], source: &[u8]) -> bool {
    let Some(head) = values.first().filter(|head| atomic_name(**head, source)) else {
        return false;
    };
    if values.iter().any(|value| value.kind() == "dot") {
        return false;
    }
    match source.get(head.byte_range()) {
        Some(b"fields") => true,
        Some(b"parent") => values.len() == 2 && atomic_name(values[1], source),
        Some(b"protocol") => values.len() == 2,
        Some(b"sealed" | b"opaque") => values.len() == 2 && values[1].kind() == "boolean",
        Some(b"nongenerative") => {
            values.len() == 1 || values.len() == 2 && atomic_name(values[1], source)
        }
        Some(b"parent-rtd") => values.len() == 3,
        _ => false,
    }
}

fn r6_field_shape(values: &[Node<'_>], source: &[u8]) -> bool {
    match values
        .first()
        .and_then(|head| source.get(head.byte_range()))
    {
        Some(b"mutable") => matches!(values.len(), 2 | 4),
        Some(b"immutable") => matches!(values.len(), 2 | 3),
        _ => false,
    }
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}

fn atomic_name(node: Node<'_>, source: &[u8]) -> bool {
    clean(node, source)
        && node.kind() == "symbol"
        && node.child_count() == 0
        && node
            .prev_named_sibling()
            .is_none_or(|previous| intertoken(previous) || separated(previous, node, source))
        && node
            .next_named_sibling()
            .is_none_or(|next| intertoken(next) || separated(node, next, source))
        && source
            .get(node.byte_range())
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .is_some_and(|text| {
                structural_captured_name_for_language("scheme", text, text.len()).is_some()
            })
}

fn intertoken(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "comment" | "block_comment" | "sexp_comment" | "directive"
    )
}

fn list_delimiters(node: Node<'_>, source: &[u8]) -> bool {
    source.get(node.byte_range()).is_some_and(|bytes| {
        matches!(
            (bytes.first(), bytes.last()),
            (Some(b'('), Some(b')')) | (Some(b'['), Some(b']'))
        )
    })
}

fn separated(previous: Node<'_>, next: Node<'_>, source: &[u8]) -> bool {
    if previous.end_byte() < next.start_byte() {
        return true;
    }
    if previous.end_byte() != next.start_byte() {
        return false;
    }
    let delimiter =
        |byte: &u8| matches!(byte, b'(' | b')' | b'[' | b']' | b'"' | b'\'' | b'`' | b',');
    source
        .get(previous.byte_range())
        .and_then(|bytes| bytes.last())
        .is_some_and(delimiter)
        || source
            .get(next.byte_range())
            .and_then(|bytes| bytes.first())
            .is_some_and(delimiter)
}

fn operation(node: Node<'_>, source: &[u8]) -> Operation {
    operation_bytes(
        atomic_name(node, source)
            .then(|| source.get(node.byte_range()))
            .flatten(),
    )
}

fn operation_bytes(bytes: Option<&[u8]>) -> Operation {
    match bytes {
        Some(b"define") => Operation::Define,
        Some(b"define-syntax") => Operation::Macro,
        Some(b"lambda") => Operation::Lambda,
        Some(b"let") => Operation::Let,
        Some(b"let*") => Operation::LetStar,
        Some(b"letrec") => Operation::LetRec,
        Some(b"letrec*") => Operation::LetRecStar,
        Some(b"let-values") => Operation::LetValues,
        Some(b"let*-values") => Operation::LetStarValues,
        Some(b"define-record-type") => Operation::Record,
        Some(b"begin") => Operation::Begin,
        Some(
            b"quote" | b"quasiquote" | b"unquote" | b"unquote-splicing" | b"syntax"
            | b"quasisyntax" | b"unsyntax" | b"unsyntax-splicing",
        ) => Operation::Data,
        Some(b"syntax-rules" | b"syntax-case") => Operation::Template,
        Some(b"let-syntax" | b"letrec-syntax") => Operation::LocalMacro,
        Some(b"library" | b"define-library" | b"module" | b"define-values") => {
            Operation::Unsupported
        }
        _ => Operation::Other,
    }
}

fn data_node(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "quote"
            | "quasiquote"
            | "unquote"
            | "unquote_splicing"
            | "syntax_quote"
            | "quasisyntax"
            | "unsyntax"
            | "unsyntax_splicing"
            | "sexp_comment"
            | "vector"
            | "byte_vector"
            | "datum_label"
            | "datum_reference"
            | "string"
            | "comment"
            | "block_comment"
    )
}

fn reference(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    if !atomic_name(node, source) {
        return Ok(false);
    }
    Ok(cache
        .context(node, source, cancellation)?
        .is_some_and(|context| !context.data && !context.blocked && context.executable))
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
    if node.kind() == "directive" {
        return Ok(Some("scheme.reader_context"));
    }
    if node.kind() == "ERROR" {
        return Ok(Some("scheme.unsupported_owner"));
    }
    if node.kind() == "symbol" {
        return Ok(
            reference(node, source, cancellation, cache)?.then_some("scheme.source_reference_gap")
        );
    }
    if node.kind() != "list" {
        return Ok(None);
    }
    let Some(form) = cache.form(node, source, cancellation)? else {
        return Ok(None);
    };
    if matches!(form.operation, Operation::Data | Operation::Template) {
        return Ok(None);
    }
    if form.operation == Operation::LocalMacro {
        return Ok(Some("scheme.local_macro_context"));
    }
    if form.operation == Operation::Unsupported {
        return Ok(Some("scheme.unsupported_owner"));
    }
    if form.operation.introduces_owner() {
        return Ok(Some(
            if !cache.owner_allowed(node, form, source, cancellation)? {
                "scheme.unsupported_owner"
            } else if form.operation == Operation::Record {
                "scheme.record_context"
            } else if form.operation == Operation::Macro {
                "scheme.macro_context"
            } else {
                "scheme.body_semantics"
            },
        ));
    }
    if form.operation == Operation::Lambda || form.operation.is_binding() {
        return Ok(Some(
            if form.valid && !context.blocked && context.executable {
                "scheme.body_semantics"
            } else {
                "scheme.parameter_context"
            },
        ));
    }
    Ok(None)
}

/// Admits complete written owners, local names and unresolved executable uses.
pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if node.kind() == "program" {
        return Ok(matches!(
            role,
            StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope
        ));
    }
    if role == StructuralRole::Expression {
        return Ok(expression(node, source, cancellation, cache)?.is_some());
    }
    if role == StructuralRole::Comment {
        return Ok(intertoken(node) && node.kind() != "directive" && clean(node, source));
    }
    if role == StructuralRole::StringLiteral {
        return Ok(node.kind() == "string" && clean(node, source));
    }
    if role == StructuralRole::Reference {
        return reference(node, source, cancellation, cache);
    }
    if !clean(node, source) {
        return Ok(false);
    }
    if node.kind() == "symbol"
        && matches!(
            role,
            StructuralRole::Declaration | StructuralRole::Definition
        )
    {
        return Ok(cache.name(node, source, cancellation)?.is_some());
    }
    if node.kind() != "list" {
        return Ok(false);
    }
    let Some(form) = cache.form(node, source, cancellation)? else {
        return Ok(false);
    };
    if form.owner.is_some() && cache.owner_allowed(node, form, source, cancellation)? {
        return Ok(matches!(
            role,
            StructuralRole::Declaration
                | StructuralRole::Definition
                | StructuralRole::Signature
                | StructuralRole::Scope
        ));
    }
    if role == StructuralRole::Scope
        && form.valid
        && (form.operation == Operation::Lambda || form.operation.is_binding())
        && cache
            .context(node, source, cancellation)?
            .is_some_and(|context| !context.data && !context.blocked && context.executable)
    {
        if form.operation == Operation::Lambda
            && let Some(parent) = node.parent().filter(|parent| parent.kind() == "list")
            && cache
                .form(parent, source, cancellation)?
                .is_some_and(|parent| parent.nested_lambda == Some(node.id()))
        {
            return Ok(false);
        }
        return Ok(true);
    }
    Ok(false)
}

/// Projects raw names and written headers without normalization or target claims.
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<QueryCandidate, AdapterError> {
    cancellation.check()?;
    let mut span = Span::of(node);
    let syntax = if node.kind() == "program" {
        span = Span {
            start: 0,
            end: source.len(),
        };
        "scheme.file"
    } else if role == StructuralRole::Expression {
        expression(node, source, cancellation, cache)?
            .ok_or_else(|| query_failure("query-scheme-expression-proof-missing"))?
    } else if role == StructuralRole::Reference {
        "scheme.symbol"
    } else if role == StructuralRole::Comment {
        "scheme.comment"
    } else if role == StructuralRole::StringLiteral {
        "scheme.string"
    } else if node.kind() == "symbol" {
        let proof = cache
            .name(node, source, cancellation)?
            .ok_or_else(|| query_failure("query-scheme-name-proof-missing"))?;
        if role == StructuralRole::Definition {
            "scheme.name"
        } else {
            proof.syntax
        }
    } else {
        let form = cache
            .form(node, source, cancellation)?
            .ok_or_else(|| query_failure("query-scheme-form-proof-missing"))?;
        if let Some(owner) = form.owner {
            match role {
                StructuralRole::Definition => {
                    span = owner.name;
                    "scheme.name"
                }
                StructuralRole::Signature => {
                    span = owner.header;
                    "scheme.header"
                }
                StructuralRole::Declaration | StructuralRole::Scope => owner.syntax,
                _ => return Err(query_failure("query-scheme-unreviewed-owner-role")),
            }
        } else if role == StructuralRole::Scope {
            if form.operation == Operation::Lambda {
                "scheme.lambda"
            } else {
                "scheme.local_binding"
            }
        } else {
            return Err(query_failure("query-scheme-owner-proof-missing"));
        }
    };
    let depth = native_depth(node, cancellation)?;
    Ok(QueryCandidate {
        start: span.start,
        end: span.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: if role == StructuralRole::Scope && node.kind() != "program" {
            depth
                .checked_add(1)
                .ok_or_else(|| query_failure("query-scheme-depth-overflow"))?
        } else {
            depth
        },
    })
}

#[cfg(test)]
mod tests {
    use rootlight_cancel::CancellationReason;
    use tree_sitter::{Query, QueryCursor, StreamingIterator};

    use super::*;

    fn parse(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_scheme::LANGUAGE_SCHEME.into())
            .unwrap();
        parser.parse(source.as_bytes(), None).unwrap()
    }

    fn extract(source: &str) -> (tree_sitter::Tree, Vec<QueryCandidate>) {
        let tree = parse(source);
        let query = Query::new(
            &tree_sitter_scheme::LANGUAGE_SCHEME.into(),
            include_str!("../../queries/scheme.scm"),
        )
        .unwrap();
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(10_000);
        let mut candidates = Vec::new();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
        while let Some(found) = matches.next() {
            for capture in found.captures {
                let role = StructuralRole::from_capture_name(
                    query.capture_names()[usize::try_from(capture.index).unwrap()],
                )
                .unwrap();
                if retain_capture(
                    capture.node,
                    role,
                    source.as_bytes(),
                    &cancellation,
                    &mut cache,
                )
                .unwrap()
                {
                    candidates.push(
                        candidate(
                            capture.node,
                            role,
                            source.as_bytes(),
                            &cancellation,
                            &mut cache,
                        )
                        .unwrap(),
                    );
                }
                assert!(!cache.exhausted());
            }
        }
        drop(matches);
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
            if matches!(
                candidate.role,
                StructuralRole::Declaration
                    | StructuralRole::Definition
                    | StructuralRole::Signature
            ) {
                assert!(candidate.required);
            }
        }
        (tree, candidates)
    }

    fn captures<'a>(
        source: &'a str,
        candidates: &[QueryCandidate],
        role: StructuralRole,
    ) -> Vec<&'a str> {
        let mut selected = candidates
            .iter()
            .filter(|candidate| candidate.role == role)
            .collect::<Vec<_>>();
        selected.sort_unstable_by_key(|candidate| (candidate.start, candidate.end));
        selected
            .into_iter()
            .map(|candidate| &source[candidate.start..candidate.end])
            .collect()
    }

    fn definitions<'a>(source: &'a str, candidates: &[QueryCandidate]) -> Vec<&'a str> {
        assert!(
            candidates
                .iter()
                .filter(|candidate| candidate.role == StructuralRole::Definition)
                .all(|candidate| candidate.syntax == "scheme.name")
        );
        captures(source, candidates, StructuralRole::Definition)
    }

    #[test]
    fn direct_owners_keep_exact_headers_and_proper_dotted_or_single_rest_formals() {
        let source = "(define value 1)\n(define (pick first . rest) first)\n(define delayed (lambda args args))\n(define (empty) value)\n";
        let (tree, candidates) = extract(source);
        assert!(!tree.root_node().has_error());
        assert_eq!(
            definitions(source, &candidates),
            ["value", "pick", "first", "rest", "delayed", "args", "empty"]
        );
        assert_eq!(
            captures(source, &candidates, StructuralRole::Signature),
            [
                "(define value",
                "(define (pick first . rest)",
                "(define delayed (lambda args",
                "(define (empty)"
            ]
        );
        assert_eq!(
            captures(source, &candidates, StructuralRole::Reference),
            ["first", "args", "value"]
        );
        let owners = candidates
            .iter()
            .filter(|candidate| {
                candidate.role == StructuralRole::Declaration
                    && candidate.syntax == "scheme.procedure"
            })
            .collect::<Vec<_>>();
        assert_eq!(owners.len(), 3);
        assert_eq!(
            &source[owners[1].start..owners[1].end],
            "(define delayed (lambda args args))"
        );
        assert!(
            !candidates
                .iter()
                .any(|candidate| candidate.role == StructuralRole::Scope
                    && candidate.syntax == "scheme.lambda")
        );
        assert_eq!(
            candidates
                .iter()
                .filter(|candidate| candidate.syntax == "scheme.file")
                .count(),
            3
        );
    }

    #[test]
    fn internal_and_local_multiple_binding_names_keep_distinct_written_owners() {
        let source = "(define (outer item) (define copy item) (let loop ((next item)) (let* ((temp next)) temp)))\n\
            (define (other item) (define copy item) (letrec ((again item)) (letrec* ((last again)) last)))\n\
            (let-values (((left right) (values 1 2)) ((head . tail) (values 3 4))) left)\n\
            (let*-values ((remaining (values 1 2))) remaining)\n";
        let (tree, candidates) = extract(source);
        assert!(!tree.root_node().has_error());
        assert_eq!(
            definitions(source, &candidates),
            [
                "outer",
                "item",
                "copy",
                "loop",
                "next",
                "temp",
                "other",
                "item",
                "copy",
                "again",
                "last",
                "left",
                "right",
                "head",
                "tail",
                "remaining"
            ]
        );
        let copies = candidates
            .iter()
            .filter(|candidate| {
                candidate.role == StructuralRole::Declaration
                    && candidate.syntax == "scheme.variable"
                    && source
                        .get(candidate.start..candidate.end)
                        .is_some_and(|text| text.starts_with("(define copy"))
            })
            .collect::<Vec<_>>();
        assert_eq!(copies.len(), 2);
        assert_ne!(copies[0].start, copies[1].start);
        let scopes = candidates
            .iter()
            .filter(|candidate| candidate.role == StructuralRole::Scope)
            .take(12)
            .collect::<Vec<_>>();
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.role == StructuralRole::Scope
                    && candidate.syntax == "scheme.local_binding"),
            "scopes={scopes:?}"
        );
        assert!(captures(source, &candidates, StructuralRole::Reference).contains(&"values"));
        assert!(
            candidates
                .iter()
                .filter(|candidate| candidate.role == StructuralRole::Expression)
                .any(|candidate| candidate.syntax == "scheme.source_reference_gap")
        );
    }

    #[test]
    fn macros_and_reader_data_do_not_leak_definitions_or_references() {
        let source = "(define-syntax build (syntax-rules () ((_ name) (define name hidden))))\n\
            '(define quoted secret)\n(quote (define explicit secret))\n\
            `(define template ,(define unquoted secret))\n#;(define discarded secret)\n\
            #((define vector-data secret))\n#1=(define labelled secret)\n#1#\n\
            (syntax-case input () ((_ name) (define name hidden)))\n\
            (define live visible)\n";
        let (tree, candidates) = extract(source);
        assert!(!tree.root_node().has_error());
        assert_eq!(definitions(source, &candidates), ["build", "live"]);
        assert_eq!(
            captures(source, &candidates, StructuralRole::Reference),
            ["visible"]
        );
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.role == StructuralRole::Declaration
                    && candidate.syntax == "scheme.macro")
        );
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.syntax == "scheme.macro_context")
        );
        let comment = candidates
            .iter()
            .find(|candidate| {
                candidate.role == StructuralRole::Comment
                    && source
                        .get(candidate.start..candidate.end)
                        .is_some_and(|text| text.starts_with("#;"))
            })
            .unwrap();
        assert_eq!(
            &source[comment.start..comment.end],
            "#;(define discarded secret)"
        );
    }

    #[test]
    fn raw_unicode_bar_and_hex_spelling_survives_without_reader_equivalence() {
        let source = "#!fold-case\n(define λ 1)\n(define (|Mixed Name| |arg space|) |arg space|)\n(define \\x61;Name 2)\n(define Mixed 3)\n(DEFINE not-recognized 4)\n";
        let (tree, candidates) = extract(source);
        assert!(!tree.root_node().has_error());
        assert_eq!(
            definitions(source, &candidates),
            ["λ", "|Mixed Name|", "|arg space|", r"\x61;Name", "Mixed"]
        );
        assert!(
            captures(source, &candidates, StructuralRole::Signature)
                .contains(&"(define (|Mixed Name| |arg space|)")
        );
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.syntax == "scheme.reader_context"
                    && &source[candidate.start..candidate.end] == "#!fold-case")
        );
        for candidate in candidates
            .iter()
            .filter(|candidate| candidate.role == StructuralRole::Definition)
        {
            let spelling = &source[candidate.start..candidate.end];
            assert_eq!(
                structural_captured_name_for_language("scheme", spelling, spelling.len())
                    .as_deref(),
                Some(spelling)
            );
        }
        assert!(captures(source, &candidates, StructuralRole::Reference).contains(&"DEFINE"));
    }

    #[test]
    fn clean_siblings_and_explicit_record_fields_survive_scoped_unknown_structure() {
        let source = ")\n(define stable 1)\n(library (unit) (define hidden 2))\n\
            (define-record-type node (fields e o (mutable count get-count set-count)))\n\
            (define-record-type <pair> (make-pair left) pair? (left pair-left))\n\
            (define-record-type awkward (fields (computed field) (mutable malformed accessor)))\n\
            (define-record-type row)\n(define-record-type (named make-named named?))\n\
            (define-record-type numeric-tail 123)\n";
        let (tree, candidates) = extract(source);
        assert!(tree.root_node().has_error());
        assert_eq!(
            definitions(source, &candidates),
            [
                "stable", "node", "e", "o", "count", "<pair>", "left", "awkward", "row", "named"
            ]
        );
        assert_eq!(
            candidates
                .iter()
                .filter(|candidate| candidate.role == StructuralRole::Declaration
                    && candidate.syntax == "scheme.field")
                .count(),
            4
        );
        assert!(candidates.iter().any(|candidate| {
            candidate.syntax == "scheme.unsupported_owner"
                && source
                    .get(candidate.start..candidate.end)
                    .is_some_and(|text| text.starts_with("(library"))
        }));
        assert_eq!(
            candidates
                .iter()
                .filter(|candidate| candidate.syntax == "scheme.record_context")
                .count(),
            5
        );
        for role in [
            StructuralRole::Declaration,
            StructuralRole::Definition,
            StructuralRole::Signature,
            StructuralRole::Scope,
        ] {
            assert!(!candidates.iter().any(|candidate| {
                candidate.role == role
                    && candidate.syntax != "scheme.file"
                    && source
                        .get(candidate.start..candidate.end)
                        .is_some_and(|text| text.contains("numeric-tail"))
            }));
        }
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.syntax == "scheme.unsupported_owner"
                    && &source[candidate.start..candidate.end]
                        == "(define-record-type numeric-tail 123)")
        );
        assert!(
            !definitions(source, &candidates).iter().any(|name| matches!(
                *name,
                "make-pair"
                    | "pair?"
                    | "pair-left"
                    | "get-count"
                    | "set-count"
                    | "computed"
                    | "field"
                    | "malformed"
                    | "accessor"
                    | "numeric-tail"
                    | "hidden"
            ))
        );
    }

    #[test]
    fn adjacent_atoms_and_expression_positions_cannot_invent_declarations() {
        let source = "(define bad 1value)\n(consume (define phantom 1))\n\
            (define result (begin (define hidden 1) visible))\n\
            (begin (define retained 1))\n(lambda (. bad-rest) (define leaked 1))\n123trailing\n";
        let (tree, candidates) = extract(source);
        assert!(!tree.root_node().has_error());
        assert_eq!(definitions(source, &candidates), ["result", "retained"]);
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.syntax == "scheme.unsupported_owner"
                    && &source[candidate.start..candidate.end] == "(define bad 1value)")
        );
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.syntax == "scheme.parameter_context")
        );
        assert!(captures(source, &candidates, StructuralRole::Reference).contains(&"visible"));
        assert!(!captures(source, &candidates, StructuralRole::Reference).contains(&"trailing"));
    }

    #[test]
    fn memoization_cancellation_and_body_changes_preserve_identity_headers() {
        let before = "(define (pick item) item)";
        let after = "(define (pick item) (begin item item))";
        let (tree, first) = extract(before);
        let (_, second) = extract(after);
        assert_eq!(definitions(before, &first), definitions(after, &second));
        assert_eq!(
            captures(before, &first, StructuralRole::Signature),
            captures(after, &second, StructuralRole::Signature)
        );
        let node = tree.root_node().named_child(0).unwrap();
        let cancellation = Cancellation::new();
        let mut cache = CaptureCache::new(128);
        for _ in 0..8 {
            assert!(
                cache
                    .form(node, before.as_bytes(), &cancellation)
                    .unwrap()
                    .unwrap()
                    .owner
                    .is_some()
            );
            assert_eq!(cache.entries.len(), 1);
        }
        assert!(!cache.exhausted());
        let mut bounded = CaptureCache::new(0);
        assert!(
            !retain_capture(
                node,
                StructuralRole::Declaration,
                before.as_bytes(),
                &cancellation,
                &mut bounded
            )
            .unwrap()
        );
        assert!(bounded.exhausted());
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(
            retain_capture(
                node,
                StructuralRole::Declaration,
                before.as_bytes(),
                &cancellation,
                &mut cache
            )
            .is_err()
        );
        assert!(
            candidate(
                node,
                StructuralRole::Declaration,
                before.as_bytes(),
                &cancellation,
                &mut cache
            )
            .is_err()
        );
    }
}
