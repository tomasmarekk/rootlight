//! Native target certificates for ordinary frontmatter const reads in templates.
//! Server declarations and template references remain separate native trees;
//! certificates carry exact declaration/read endpoints rather than a name join.

use std::mem::size_of;

use super::*;

#[derive(Clone, Copy)]
struct Binding {
    start: usize,
    end: usize,
    variable: bool,
}

pub(super) struct ReadPlan {
    bindings: Vec<Binding>,
    witnesses: Vec<QueryCandidate>,
    frontmatter: Option<(usize, usize)>,
    admitted: bool,
    memory_limit: usize,
}

struct Work {
    used: usize,
    limit: usize,
}

impl Work {
    fn visit(&mut self, cancel: &Cancellation) -> Result<(), AdapterError> {
        cancel.check()?;
        let next = self
            .used
            .checked_add(1)
            .ok_or(SinkError::AccountingOverflow)?;
        require_provider_limit(ResourceKind::SyntaxNodes, next, self.limit)?;
        self.used = next;
        Ok(())
    }

    fn name<'a>(
        &mut self,
        node: Node<'_>,
        source: &'a [u8],
        cancel: &Cancellation,
    ) -> Result<Option<&'a [u8]>, AdapterError> {
        let Some(bytes) = source.get(node.byte_range()) else {
            return Ok(None);
        };
        for _ in bytes.chunks(64) {
            self.visit(cancel)?;
        }
        Ok(bytes
            .first()
            .is_some_and(|first| first.is_ascii_alphabetic() || *first == b'_' || *first == b'$')
            .then_some(bytes)
            .filter(|bytes| {
                bytes
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'$'))
            }))
    }
}

impl ReadPlan {
    /// Bound owned proof buffers within the caller's remaining source-memory admission.
    pub(super) fn new(memory_limit: usize) -> Self {
        Self {
            bindings: Vec::new(),
            witnesses: Vec::new(),
            frontmatter: None,
            admitted: true,
            memory_limit,
        }
    }

    fn memory(&self, bindings: usize, witnesses: usize) -> Result<(), AdapterError> {
        let bytes = bindings
            .checked_mul(size_of::<Binding>())
            .and_then(|left| {
                witnesses
                    .checked_mul(size_of::<QueryCandidate>())
                    .and_then(|right| left.checked_add(right))
            })
            .ok_or(SinkError::AccountingOverflow)?;
        require_provider_limit(ResourceKind::ReportedMemoryBytes, bytes, self.memory_limit)
    }

    fn binding(
        &mut self,
        node: Node<'_>,
        variable: bool,
        source: &[u8],
        work: &mut Work,
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        let Some(name) = work.name(node, source, cancel)? else {
            self.admitted = false;
            return Ok(());
        };
        for old in &self.bindings {
            work.visit(cancel)?;
            if source.get(old.start..old.end) == Some(name) {
                self.admitted = false;
                return Ok(());
            }
        }
        let next = self
            .bindings
            .len()
            .checked_add(1)
            .ok_or(SinkError::AccountingOverflow)?;
        self.memory(
            next.max(self.bindings.capacity()),
            self.witnesses.capacity(),
        )?;
        self.bindings
            .try_reserve_exact(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.memory(self.bindings.capacity(), self.witnesses.capacity())?;
        self.bindings.push(Binding {
            start: node.start_byte(),
            end: node.end_byte(),
            variable,
        });
        Ok(())
    }

    /// Census one complete server module; unsupported or duplicate bindings withhold all certificates.
    /// Return charged native work; work, allocation and cancellation errors abort the caller's transaction.
    pub(super) fn frontmatter(
        &mut self,
        tree: &Tree,
        span: (usize, usize),
        source: &[u8],
        limit: usize,
        cancel: &Cancellation,
    ) -> Result<usize, AdapterError> {
        cancel.check()?;
        let mut work = Work { used: 0, limit };
        if self.frontmatter.is_some()
            || tree.root_node().kind() != "program"
            || tree.root_node().has_error()
            || tree.root_node().is_missing()
        {
            self.admitted = false;
            return Ok(0);
        }
        self.frontmatter = Some(span);
        let root = tree.root_node();
        let mut cursor = root.walk();
        for statement in root.named_children(&mut cursor) {
            work.visit(cancel)?;
            match statement.kind() {
                "comment"
                | "empty_statement"
                | "interface_declaration"
                | "type_alias_declaration" => {}
                "export_statement"
                    if statement.child_by_field_name("declaration").is_some_and(
                        |declaration| {
                            matches!(
                                declaration.kind(),
                                "interface_declaration" | "type_alias_declaration"
                            )
                        },
                    ) => {}
                "import_statement" => {
                    let mut nested = statement.walk();
                    loop {
                        work.visit(cancel)?;
                        let node = nested.node();
                        if node.kind() == "identifier" {
                            self.binding(node, false, source, &mut work, cancel)?;
                        }
                        if nested.goto_first_child() {
                            continue;
                        }
                        loop {
                            if nested.goto_next_sibling() {
                                break;
                            }
                            if !nested.goto_parent() {
                                break;
                            }
                        }
                        if nested.node() == statement {
                            break;
                        }
                    }
                }
                "lexical_declaration"
                    if statement
                        .child_by_field_name("kind")
                        .is_some_and(|kind| kind.kind() == "const") =>
                {
                    let mut declarations = statement.walk();
                    for declaration in statement.named_children(&mut declarations) {
                        work.visit(cancel)?;
                        if declaration.kind() == "comment" {
                            continue;
                        }
                        let Some(name) = declaration.child_by_field_name("name") else {
                            self.admitted = false;
                            continue;
                        };
                        if declaration.kind() != "variable_declarator"
                            || declaration.child_by_field_name("value").is_none()
                        {
                            self.admitted = false;
                            continue;
                        }
                        match name.kind() {
                            "identifier" => self.binding(name, true, source, &mut work, cancel)?,
                            "object_pattern" => {
                                let mut names = name.walk();
                                for binding in name.named_children(&mut names) {
                                    work.visit(cancel)?;
                                    if binding.kind() == "comment" {
                                        continue;
                                    }
                                    if binding.kind() != "shorthand_property_identifier_pattern" {
                                        self.admitted = false;
                                        continue;
                                    }
                                    self.binding(binding, true, source, &mut work, cancel)?;
                                }
                            }
                            _ => self.admitted = false,
                        }
                    }
                }
                _ => self.admitted = false,
            }
        }
        // A complete native walk keeps dynamic namespace operations from hiding
        // in an initializer, including one inside an otherwise opaque closure.
        let mut cursor = root.walk();
        loop {
            work.visit(cancel)?;
            let node = cursor.node();
            if node.is_missing() || node.is_error() {
                self.admitted = false;
            }
            if node.kind() == "identifier" {
                let name = work.name(node, source, cancel)?;
                if name.is_none() || name.is_some_and(|name| name == b"eval" || name == b"Function")
                {
                    self.admitted = false;
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
                    return Ok(work.used);
                }
            }
        }
    }

    /// Stage only outer expression reads; nested lexical scopes and computed reads remain unproved.
    /// Names select native lexical bindings here, while the consumer receives exact source endpoints.
    pub(super) fn expression(
        &mut self,
        tree: &Tree,
        source: &[u8],
        limit: usize,
        cancel: &Cancellation,
    ) -> Result<usize, AdapterError> {
        cancel.check()?;
        if !self.admitted || self.frontmatter.is_none() || tree.root_node().has_error() {
            return Ok(0);
        }
        let mut work = Work { used: 0, limit };
        let mut cursor = tree.root_node().walk();
        loop {
            work.visit(cancel)?;
            let node = cursor.node();
            if node.kind() == "identifier"
                && ordinary_read(node, &mut work, cancel)?
                && let Some(name) = work.name(node, source, cancel)?
            {
                let mut target = None;
                for binding in &self.bindings {
                    work.visit(cancel)?;
                    if binding.variable && source.get(binding.start..binding.end) == Some(name) {
                        target = Some(*binding);
                    }
                }
                if let Some(target) = target {
                    let next = self
                        .witnesses
                        .len()
                        .checked_add(2)
                        .ok_or(SinkError::AccountingOverflow)?;
                    self.memory(
                        self.bindings.capacity(),
                        next.max(self.witnesses.capacity()),
                    )?;
                    self.witnesses
                        .try_reserve_exact(2)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    self.memory(self.bindings.capacity(), self.witnesses.capacity())?;
                    self.witnesses.push(QueryCandidate {
                        start: target.start,
                        end: node.end_byte(),
                        role: StructuralRole::Signature,
                        syntax: "astro.template_read_target",
                        required: true,
                        native_depth: 0,
                    });
                    self.witnesses.push(QueryCandidate {
                        start: node.start_byte(),
                        end: node.end_byte(),
                        role: StructuralRole::Reference,
                        syntax: "astro.template_binding_read",
                        required: true,
                        native_depth: 0,
                    });
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
                    return Ok(work.used);
                }
            }
        }
    }

    /// Replace duplicate optional reads with a mandatory certificate only after full-host admission.
    /// The caller must abort its syntax transaction on any returned error.
    pub(super) fn append(
        self,
        candidates: &mut Vec<QueryCandidate>,
        source_length: usize,
        limit: usize,
        cancel: &Cancellation,
    ) -> Result<usize, AdapterError> {
        cancel.check()?;
        if !self.admitted || self.witnesses.is_empty() {
            return Ok(0);
        }
        let Some((start, end)) = self.frontmatter else {
            return Ok(0);
        };
        let mut work = Work { used: 0, limit };
        // The exact target marker is mandatory beside its read. Existing runtime
        // and lowerer preflights must therefore admit the complete certificate.
        let mut index = 0;
        while let Some(candidate) = candidates.get(index).copied() {
            work.visit(cancel)?;
            let mut replace = false;
            if candidate.role == StructuralRole::Reference {
                for witness in &self.witnesses {
                    work.visit(cancel)?;
                    replace |= witness.role == StructuralRole::Reference
                        && candidate.start == witness.start
                        && candidate.end == witness.end;
                }
            }
            if replace {
                candidates.swap_remove(index);
            } else {
                index += 1;
            }
        }
        let extra = self
            .witnesses
            .len()
            .checked_add(2)
            .ok_or(SinkError::AccountingOverflow)?;
        let total = candidates
            .len()
            .checked_add(extra)
            .ok_or(SinkError::AccountingOverflow)?;
        let staged = self
            .witnesses
            .capacity()
            .checked_add(total.max(candidates.capacity()))
            .ok_or(SinkError::AccountingOverflow)?;
        self.memory(self.bindings.capacity(), staged)?;
        try_reserve_cancellable(
            candidates,
            extra,
            cancel,
            "astro-read-candidates-allocation",
        )?;
        let staged = self
            .witnesses
            .capacity()
            .checked_add(candidates.capacity())
            .ok_or(SinkError::AccountingOverflow)?;
        self.memory(self.bindings.capacity(), staged)?;
        candidates.extend(self.witnesses);
        candidates.push(QueryCandidate {
            start: 0,
            end: source_length,
            role: StructuralRole::Signature,
            syntax: "astro.template_clean_host",
            required: true,
            native_depth: 0,
        });
        candidates.push(QueryCandidate {
            start,
            end,
            role: StructuralRole::Signature,
            syntax: "astro.template_clean_server",
            required: true,
            native_depth: 0,
        });
        Ok(work.used)
    }
}

fn ordinary_read(
    mut node: Node<'_>,
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    while let Some(parent) = node.parent() {
        work.visit(cancel)?;
        match parent.kind() {
            "member_expression"
                if parent.child_by_field_name("object") == Some(node)
                    && parent
                        .child_by_field_name("property")
                        .is_some_and(|property| property.kind() == "property_identifier") => {}
            "call_expression"
                if parent.child_by_field_name("function") == Some(node)
                    && node.kind() == "member_expression" => {}
            "parenthesized_expression" | "expression_statement" => {}
            "program" => return Ok(true),
            _ => return Ok(false),
        }
        node = parent;
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(source: &[u8], start: usize, end: usize, typescript: bool) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(if typescript {
                GrammarFamily::TypeScript
            } else {
                GrammarFamily::JavaScript
            }))
            .unwrap();
        let point = |offset: usize| {
            let prefix = &source[..offset];
            let row = prefix.iter().filter(|byte| **byte == b'\n').count();
            let column = prefix
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(offset, |last| offset - last - 1);
            tree_sitter::Point::new(row, column)
        };
        parser
            .set_included_ranges(&[tree_sitter::Range {
                start_byte: start,
                end_byte: end,
                start_point: point(start),
                end_point: point(end),
            }])
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        tree
    }

    #[test]
    fn native_const_target_endpoints_exclude_properties_and_charge_every_phase() {
        let source = b"const {posts} = props;\n(posts.map(item => item))";
        let boundary = source.iter().position(|byte| *byte == b'\n').unwrap() + 1;
        let server = tree(source, 0, boundary, true);
        let expression = tree(source, boundary, source.len(), false);
        let cancel = Cancellation::new();
        let mut plan = ReadPlan::new(1024 * 1024);
        assert!(
            plan.frontmatter(&server, (0, boundary), source, 4096, &cancel)
                .unwrap()
                > 0
        );
        assert!(plan.expression(&expression, source, 4096, &cancel).unwrap() > 0);
        let mut candidates = Vec::new();
        assert_eq!(
            plan.append(&mut candidates, source.len(), 4096, &cancel)
                .unwrap(),
            0
        );
        let target = candidates
            .iter()
            .find(|candidate| candidate.syntax == "astro.template_read_target")
            .unwrap();
        assert_eq!(target.start, 7);
        assert_eq!(target.end, boundary + 6);
        assert!(candidates.iter().all(|candidate| candidate.required));
        assert_eq!(
            candidates
                .iter()
                .filter(|candidate| candidate.role == StructuralRole::Reference)
                .count(),
            1
        );
    }

    #[test]
    fn native_frontmatter_proof_work_and_cancellation_refuse_before_publication() {
        let source = b"const {posts} = props;\n(posts)";
        let boundary = source.iter().position(|byte| *byte == b'\n').unwrap() + 1;
        let server = tree(source, 0, boundary, true);
        let expression = tree(source, boundary, source.len(), false);
        let cancel = Cancellation::new();
        let mut limited = ReadPlan::new(1024 * 1024);
        assert!(
            limited
                .frontmatter(&server, (0, boundary), source, 0, &cancel)
                .is_err()
        );
        let mut admitted = ReadPlan::new(1024 * 1024);
        admitted
            .frontmatter(&server, (0, boundary), source, 4096, &cancel)
            .unwrap();
        assert!(
            admitted
                .expression(&expression, source, 0, &cancel)
                .is_err()
        );
        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            admitted.expression(&expression, source, 4096, &cancelled),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
